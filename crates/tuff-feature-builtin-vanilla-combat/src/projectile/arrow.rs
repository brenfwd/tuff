//! Arrows: fired by players' bows and by skeletons. An arrow flies with
//! gravity and drag, slows in water, sticks in the first block it hits
//! (and can be picked up there, if its shooter allows), and hurts the
//! first living thing its path crosses by its speed times its damage,
//! with the bow's Power, Punch and Flame.

use rand::Rng;
use tuff_core::world::block_contact;
use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{BlockState, Item};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext, take, take_bool, take_number,
};
use tuff_core::world::physics::{self, Aabb};
use tuff_core::world::projectile::{
    ArrowShot, Owner, Pickup, Target, block_hit, border_stop, bounce_off_border, hit_source,
    lerp_rotation, margin, rotation_of, targets_along,
};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_common::combat::{self, DamageContext};

const BASE_DAMAGE: f64 = 2.0;
const SHAKE_TIME: u8 = 7;
const WATER_INERTIA: f64 = 0.6;
const AIR_DRAG: f64 = 0.99;
const GRAVITY: f64 = 0.05;
/// An arrow stuck in a block disappears after a minute.
const DESPAWN_TICKS: i16 = 1200;
const FLAG_CRIT: i8 = 1;
const DATA_FLAGS: u8 = 8;
const DATA_PIERCE: u8 = 9;
const DATA_IN_GROUND: u8 = 10;
const DATA_COLOR: u8 = 11;
const DATA_LOYALTY: u8 = 11;
const DATA_FOIL: u8 = 12;
/// A trident's damage before enchantments, whatever its speed.
const TRIDENT_DAMAGE: f32 = 8.0;
const TRIDENT_WATER_INERTIA: f64 = 0.99;
/// What a trident keeps of its speed bouncing off what it hit, reversed.
const TRIDENT_BOUNCE: [f64; 3] = [-0.01, -0.1, -0.01];
/// Seconds of fire a burning arrow sets its victim alight for.
const BURNING_ARROW_FIRE: f32 = 5.0;
/// Ticks a spectral arrow makes its victim glow for, unless saved
/// otherwise.
const SPECTRAL_GLOW_TICKS: i32 = 200;
/// Ticks a tipped arrow lies in the ground before its potion wears off.
const EXPOSED_POTION_DECAY_TICKS: i16 = 600;
/// The entity event that puffs a tipped arrow's potion away.
const EVENT_POTION_PUFF: i8 = 0;
/// What a bounced arrow keeps of its speed, reversed.
const REBOUND_SPEED: f64 = -0.1;

#[derive(tuff_item::item::DupClone, Debug)]
pub struct Arrow {
    pub base: EntityBase,
    pub owner: Owner,
    pub pickup: Pickup,
    pub damage: f64,
    pub crit: bool,
    pub pierce: u8,
    pub in_ground: bool,
    pub shake: u8,
    life: i16,
    in_block: Option<BlockState>,
    pub sound: SoundEvent,
    /// What a pickup gives back.
    pub item: ItemStack,
    /// The bow it was shot from; its enchantments act on the hit.
    pub weapon: Option<ItemStack>,
    ticks: u32,
    pierced: Vec<i32>,
    changed: Vec<u8>,
    /// Held for a player who was offered it, until they answer.
    pub reserved: Option<tuff_core::world::entity::Reservation>,
    /// Players who could not take it; not saved.
    pub refused_by: Vec<i32>,
    /// Effects it gives what it hits (a stray's slowness); not saved.
    pub effects: Vec<tuff_core::world::effects::ActiveEffect>,
    /// Ticks a spectral arrow makes its victim glow for.
    pub glow_ticks: i32,
    /// A thrown trident's own state.
    pub trident: Option<Trident>,
    /// Set once a piercing arrow's failed hit sent a bounced copy back in
    /// its place; not saved.
    bounced: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// What a thrown trident adds to an arrow.
#[derive(Debug, Clone, Default)]
pub struct Trident {
    /// How hard it pulls back to its thrower (Loyalty).
    pub loyalty: u8,
    /// The enchantment glint.
    pub foil: bool,
    /// It hit something (or lay in the ground a moment): it hits nothing
    /// more and, with Loyalty, flies home.
    pub dealt_damage: bool,
    /// Flying home through everything.
    pub returning: bool,
    return_ticks: u32,
}

impl Arrow {
    /// An arrow leaving `shooter`'s eyes (0.1 below them), owned by it.
    pub fn new(id: i32, position: [f64; 3], velocity: [f64; 3], owner: Owner) -> Self {
        Self::of(EntityType::Arrow, id, position, velocity, owner)
    }

    /// The arrow entity an arrow item flies as: spectral arrows are their
    /// own type; plain and tipped ones are arrows.
    pub fn from_item(
        item: &ItemStack,
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        owner: Owner,
    ) -> Self {
        let kind = if item.item == Item::SpectralArrow {
            EntityType::SpectralArrow
        } else {
            EntityType::Arrow
        };
        let mut arrow = Self::of(kind, id, position, velocity, owner);
        arrow.item = item.dup_count_unchecked(1);
        arrow
    }

    /// The arrow or trident `shot` describes.
    pub fn shot(id: i32, position: [f64; 3], owner: Owner, shot: ArrowShot) -> Self {
        let mut arrow = if shot.kind() == EntityType::Trident {
            Self::trident(&shot.item, id, position, shot.velocity, owner)
        } else {
            Self::from_item(&shot.item, id, position, shot.velocity, owner)
        };
        if let Some(pickup) = shot.pickup {
            arrow.pickup = pickup;
        }
        if let Some(damage) = shot.damage {
            arrow.damage = damage;
        }
        if let Some(sound) = shot.sound {
            arrow.sound = sound;
        }
        if shot.weapon.is_some() {
            arrow.weapon = shot.weapon;
        }
        if shot.fire_ticks > 0 {
            arrow.base.fire = shot.fire_ticks;
        }
        arrow.crit = shot.crit;
        arrow.pierce = shot.pierce;
        arrow.effects.extend(shot.effects);
        arrow
    }

    /// A trident `item` thrown along `velocity`.
    pub fn trident(
        item: &ItemStack,
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        owner: Owner,
    ) -> Self {
        let mut arrow = Self::of(EntityType::Trident, id, position, velocity, owner);
        arrow.item = item.dup_count_unchecked(1);
        arrow.weapon = Some(arrow.item.dup_unchecked());
        arrow.trident = Some(Trident {
            loyalty: tuff_vanilla_common::ranged::trident_loyalty(item),
            foil: item
                .components
                .enchantments
                .as_ref()
                .is_some_and(|e| !e.entries().is_empty())
                || item
                    .components
                    .other
                    .get("minecraft:enchantment_glint_override")
                    .is_some(),
            ..Default::default()
        });
        arrow
    }

    fn default_sound(kind: EntityType) -> SoundEvent {
        if kind == EntityType::Trident {
            SoundEvent::ItemTridentHitGround
        } else {
            SoundEvent::EntityArrowHit
        }
    }

    fn of(kind: EntityType, id: i32, position: [f64; 3], velocity: [f64; 3], owner: Owner) -> Self {
        let uuid = rand::random();
        let mut base = EntityBase::new(kind, id, uuid, position, velocity);
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        let pickup = if owner.culprit.as_ref().is_some_and(|c| c.is_player()) {
            Pickup::Allowed
        } else {
            Pickup::Disallowed
        };
        Self {
            base,
            owner,
            pickup,
            damage: BASE_DAMAGE,
            crit: false,
            pierce: 0,
            in_ground: false,
            shake: 0,
            life: 0,
            in_block: None,
            sound: Self::default_sound(kind),
            item: ItemStack::mint(
                match kind {
                    EntityType::SpectralArrow => Item::SpectralArrow,
                    EntityType::Trident => Item::Trident,
                    _ => Item::Arrow,
                },
                1,
                MintReason::EntityDrop,
            ),
            weapon: None,
            ticks: 0,
            pierced: Vec::new(),
            changed: Vec::new(),
            reserved: None,
            refused_by: Vec::new(),
            effects: Vec::new(),
            glow_ticks: SPECTRAL_GLOW_TICKS,
            trident: (kind == EntityType::Trident).then(Trident::default),
            bounced: None,
        }
    }

    /// The potion color tipped arrows trail, or -1.
    fn color(&self) -> i32 {
        self.item
            .components
            .potion_contents
            .as_ref()
            .map_or(-1, |contents| {
                tuff_core::world::effects::potion_color(contents)
            })
    }

    /// What the arrow does to a victim it hurts on top of the damage: its
    /// potion, a spectral arrow's glow, a stray's slowness.
    fn hit_effects(&self) -> Vec<tuff_core::world::effects::ActiveEffect> {
        let mut effects = self.effects.clone();
        if let Some(contents) = &self.item.components.potion_contents {
            let scale = self.item.item.potion_duration_scale();
            effects.extend(tuff_core::world::effects::potion_effects(contents, scale));
        }
        if self.base.kind == EntityType::SpectralArrow {
            effects.push(tuff_core::world::effects::ActiveEffect::timed(
                tuff_vanilla_data::registry::MobEffect::Glowing,
                i64::from(self.glow_ticks),
                0,
            ));
        }
        effects
    }

    /// This arrow bounced back off a victim it failed to hurt.
    fn rebounded(&self, cx: &mut TickContext) -> Self {
        let mut arrow = self.clone();
        arrow.base.id = cx.shared.next_entity_id();
        arrow.base.uuid = cx.rng.r#gen();
        arrow.base.velocity = self.base.velocity.map(|c| c * REBOUND_SPEED);
        let turn = 170.0 + cx.rng.r#gen::<f32>() * 20.0;
        arrow.base.yaw += turn;
        arrow.changed.clear();
        arrow.bounced = None;
        arrow
    }

    fn flags(&self) -> i8 {
        if self.crit { FLAG_CRIT } else { 0 }
    }

    fn set_in_ground(&mut self, in_ground: bool) {
        if self.in_ground != in_ground {
            self.in_ground = in_ground;
            self.changed.push(DATA_IN_GROUND);
        }
    }

    fn set_crit(&mut self, crit: bool) {
        if self.crit != crit {
            self.crit = crit;
            self.changed.push(DATA_FLAGS);
        }
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_FLAGS => MetadataValue::Byte(self.flags()),
            DATA_PIERCE => MetadataValue::Byte(self.pierce as i8),
            DATA_IN_GROUND => MetadataValue::Boolean(self.in_ground),
            DATA_COLOR if self.base.kind == EntityType::Arrow => {
                MetadataValue::VarInt(self.color())
            }
            DATA_LOYALTY if self.trident.is_some() => {
                MetadataValue::Byte(self.trident.as_ref()?.loyalty as i8)
            }
            DATA_FOIL if self.trident.is_some() => {
                MetadataValue::Boolean(self.trident.as_ref()?.foil)
            }
            _ => return None,
        })
    }

    /// The stack a player picking it up gets, when it may be picked up by
    /// anyone: stuck in the ground and done shaking.
    pub fn pickup_stack(&self) -> Option<&ItemStack> {
        (self.touchable() && self.pickup == Pickup::Allowed).then_some(&self.item)
    }

    /// Whether a creative player touching it takes it without an item.
    pub fn creative_pickup(&self) -> bool {
        self.touchable() && self.pickup == Pickup::CreativeOnly
    }

    /// Lying still enough to pick up: stuck and done shaking, or a trident
    /// flying home.
    fn touchable(&self) -> bool {
        let returning = self.trident.as_ref().is_some_and(|t| t.returning);
        (self.in_ground || returning) && self.shake == 0
    }

    fn sound_at(&self, sound: SoundEvent, cx: &mut TickContext) {
        let pitch = 1.2 / (cx.rng.r#gen::<f32>() * 0.2 + 0.9);
        let packet = framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::NEUTRAL,
            position: self.base.position,
            volume: 1.0,
            pitch,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
    }

    fn bounds(&self) -> Aabb {
        self.base.bounds()
    }

    fn should_fall(&self, cx: &TickContext) -> bool {
        let [x, y, z] = self.base.position;
        let area = [x - 0.06, y - 0.06, z - 0.06, x + 0.06, y + 0.06, z + 0.06];
        self.in_ground && physics::no_collision(area, cx.block)
    }

    fn hit_block(&mut self, hit: tuff_core::world::raycast::Hit, cx: &mut TickContext) {
        tuff_core::world::projectile::struck_block(hit, &self.base, &self.owner, cx);
        self.in_block = (cx.block)(hit.pos);
        let v = self.base.velocity;
        let offset = v.map(|c| c.signum() * 0.050_000_000_745_058_06);
        for axis in 0..3 {
            if v[axis] != 0.0 {
                self.base.position[axis] -= offset[axis];
            }
        }
        self.base.velocity = [0.0; 3];
        self.sound_at(self.sound, cx);
        self.set_in_ground(true);
        self.shake = SHAKE_TIME;
        self.set_crit(false);
        if self.pierce != 0 {
            self.pierce = 0;
            self.changed.push(DATA_PIERCE);
        }
        self.sound = Self::default_sound(self.base.kind);
        self.pierced.clear();
        if let Some(weapon) = self.weapon.dup_unchecked() {
            self.hit_block_effects(&weapon, hit, v, cx);
        }
    }

    /// The weapon's `hit_block` enchantment effects (Channeling calling
    /// lightning onto a rod), at the hit clamped inside the block.
    fn hit_block_effects(
        &mut self,
        weapon: &ItemStack,
        hit: tuff_core::world::raycast::Hit,
        movement: [f64; 3],
        cx: &mut TickContext,
    ) {
        use tuff_vanilla_common::enchant_effects::{self as fx, Action};
        let at = [0, 1, 2].map(|i| {
            let min = f64::from([hit.pos.x, hit.pos.y, hit.pos.z][i]);
            hit.location[i].clamp(min, min + 1.0)
        });
        let actions = fx::projectile_hit_block(weapon, self.base.kind, at, movement, cx);
        let mut world = Vec::new();
        for action in actions {
            match action {
                Action::Ignite(seconds) => {
                    let ticks = (seconds * 20.0) as i16;
                    if self.base.fire < ticks {
                        self.base.fire = ticks;
                    }
                }
                Action::World(action) => world.push(action),
                _ => {}
            }
        }
        let executor = fx::Executor {
            entity: self.base.id,
            uuid: self.owner.uuid,
            kind: self.base.kind,
            position: at,
        };
        fx::push_world(cx, executor, world);
    }

    /// A trident's hit: a flat 8 (plus Impaling and the like), Channeling's
    /// lightning in a storm, then it bounces off whatever happened.
    fn trident_hit(&mut self, target: &Target, cx: &mut TickContext) {
        let attacker = self.owner.culprit(cx);
        tuff_core::world::projectile::landed(
            &self.base,
            attacker.clone(),
            None,
            Some(target.id),
            cx,
        );
        let context = DamageContext {
            kind: &damage_type::TRIDENT,
            victim: target.kind,
            attacker: Some(attacker.as_ref().map_or(self.base.kind, |a| a.kind)),
            direct_attacker: Some(self.base.kind),
            is_direct: false,
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        };
        let damage = self.weapon.as_ref().map_or(TRIDENT_DAMAGE, |weapon| {
            combat::modify_damage(weapon, &context, TRIDENT_DAMAGE, cx.rng)
        });
        if let Some(trident) = &mut self.trident {
            trident.dealt_damage = true;
        }
        let mut hit = EntityHit::new(damage, &damage_type::TRIDENT);
        hit.attacker = attacker;
        hit.projectile = Some(self.base.kind);
        hit.projectile_id = Some(self.base.id);
        hit.projectile_velocity = Some(self.base.velocity);
        hit.position = Some(hit_source(target.position, self.base.velocity));
        if self.base.on_fire() {
            hit.ignite = BURNING_ARROW_FIRE;
        }
        cx.effects.push(Effect::Hurt {
            target: target.id,
            hit: Box::new(hit),
        });
        if let Some(weapon) = &self.weapon
            && tuff_vanilla_common::ranged::channeling(weapon)
            && tuff_core::world::projectile::thundering(cx)
            && tuff_core::world::projectile::sees_sky(
                tuff_core::world::physics::block_pos(target.position),
                cx,
            )
        {
            tuff_core::world::projectile::call_lightning(target.position, cx);
            if let Some(uuid) = self.owner.uuid {
                cx.effects.push(Effect::Advancement {
                    player: uuid,
                    event: Box::new(
                        tuff_core::world::messages::AdvancementEvent::ChanneledLightning {
                            victims: vec![tuff_core::world::predicate::EntityFacts {
                                kind: Some(target.kind),
                                position: target.position,
                                ..Default::default()
                            }],
                        },
                    ),
                });
            }
        }
        self.sound_at(SoundEvent::ItemTridentHit, cx);
        let v = self.base.velocity;
        self.base.velocity = [0, 1, 2].map(|i| v[i] * TRIDENT_BOUNCE[i]);
        let turn = 170.0 + cx.rng.r#gen::<f32>() * 20.0;
        self.base.yaw += turn;
    }

    /// A Loyalty trident that hit something flies back to its thrower, or
    /// drops (or vanishes) once its thrower is gone. True while it flies
    /// home, which it does through everything.
    fn come_home(&mut self, cx: &mut TickContext) -> Option<Tick> {
        let trident = self.trident.as_ref()?;
        if trident.loyalty == 0 || !(trident.dealt_damage || trident.returning) {
            return None;
        }
        let loyalty = f64::from(trident.loyalty);
        let uuid = self.owner.uuid?;
        let eye = if let Some(player) = cx.players.iter().find(|p| p.uuid == uuid) {
            player.eye()
        } else {
            let entity = cx.entities.iter().find(|e| e.uuid == uuid)?;
            if !entity.living.is_some_and(|l| l.alive) {
                if self.pickup == Pickup::Allowed {
                    cx.effects.push(Effect::DropItem {
                        stack: self.item.dup_unchecked(),
                        position: self.base.position,
                        velocity: [0.0; 3],
                        pickup_delay: 10,
                    });
                }
                return Some(Tick::Discard);
            }
            let eye_height = entity.living.map_or(0.0, |l| f64::from(l.eye_height));
            let eye = [
                entity.position[0],
                entity.position[1] + eye_height,
                entity.position[2],
            ];
            let width = entity.bounds[3] - entity.bounds[0];
            if tuff_core::world::distance_sq(self.base.position, eye).sqrt() < width + 1.0 {
                return Some(Tick::Discard);
            }
            eye
        };
        let to = [0, 1, 2].map(|i| eye[i] - self.base.position[i]);
        self.base.position[1] += to[1] * 0.015 * loyalty;
        let length = to.iter().map(|c| c * c).sum::<f64>().sqrt();
        let pull = 0.05 * loyalty;
        let v = self.base.velocity;
        self.base.velocity = [0, 1, 2].map(|i| {
            v[i] * 0.95
                + if length > 0.0 {
                    to[i] / length * pull
                } else {
                    0.0
                }
        });
        let first = self.trident.as_ref().is_some_and(|t| t.return_ticks == 0);
        if let Some(trident) = &mut self.trident {
            trident.returning = true;
            trident.return_ticks += 1;
        }
        if first {
            let packet = framed(&play::Sound {
                sound_event: SoundEvent::ItemTridentReturn.id(),
                category: play::sound_category::NEUTRAL,
                position: self.base.position,
                volume: 10.0,
                pitch: 1.0,
                seed: cx.rng.r#gen(),
            });
            cx.effects.push(Effect::ChunkPackets(packets([packet])));
        }
        self.set_in_ground(false);
        let v = self.base.velocity;
        for axis in 0..3 {
            self.base.position[axis] += v[axis];
        }
        let (yaw, pitch) = rotation_of(v);
        self.base.yaw = yaw;
        self.base.pitch = pitch;
        self.base.velocity = v.map(|c| c * AIR_DRAG);
        Some(Tick::Alive)
    }

    /// Hits `target`; returns whether the arrow is used up.
    fn hit_entity(&mut self, target: &Target, cx: &mut TickContext) -> bool {
        if self.trident.is_some() {
            self.trident_hit(target, cx);
            return false;
        }
        let speed = self.base.velocity.iter().map(|c| c * c).sum::<f64>().sqrt() as f32;
        let attacker = self.owner.culprit(cx);
        tuff_core::world::projectile::landed(
            &self.base,
            attacker.clone(),
            None,
            Some(target.id),
            cx,
        );
        let context = DamageContext {
            kind: &damage_type::ARROW,
            victim: target.kind,
            attacker: Some(attacker.as_ref().map_or(self.base.kind, |a| a.kind)),
            direct_attacker: Some(self.base.kind),
            is_direct: false,
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        };
        let mut base_damage = self.damage as f32;
        if let Some(weapon) = &self.weapon {
            base_damage = combat::modify_damage(weapon, &context, base_damage, cx.rng);
        }
        let mut damage = (f64::from(speed) * f64::from(base_damage))
            .clamp(0.0, f64::from(i32::MAX))
            .ceil() as i64;
        if self.pierce > 0 {
            if self.pierced.len() > self.pierce as usize {
                return true;
            }
            self.pierced.push(target.id);
        }
        if self.crit {
            damage += cx.rng.gen_range(0..damage / 2 + 2);
        }
        let mut hit = EntityHit::new(damage.min(i64::from(i32::MAX)) as f32, &damage_type::ARROW);
        hit.attacker = attacker;
        hit.projectile = Some(self.base.kind);
        hit.projectile_id = Some(self.base.id);
        hit.projectile_velocity = Some(self.base.velocity);
        hit.weapon = self.weapon.dup_unchecked();
        hit.position = Some(hit_source(target.position, self.base.velocity));
        hit.effects = self.hit_effects();
        hit.sticks = self.pierce == 0;
        if target.kind != EntityType::Enderman {
            let bounced = Box::new(self.rebounded(cx));
            hit.rebound = Some(if self.pierce == 0 {
                tuff_core::world::entity::Rebound::new(bounced)
            } else {
                let claim = self.bounced.get_or_insert_with(Default::default).clone();
                tuff_core::world::entity::Rebound::claiming(bounced, claim)
            });
        }
        if self.base.on_fire() {
            hit.ignite = BURNING_ARROW_FIRE;
        }
        if let Some(weapon) = &self.weapon {
            let punch = combat::modify_knockback(weapon, &context, 0.0, cx.rng);
            let [vx, _, vz] = self.base.velocity;
            let horizontal = (vx * vx + vz * vz).sqrt();
            if punch > 0.0 && horizontal > 0.0 {
                let scale = f64::from(punch) * 0.6 / horizontal;
                hit.push = Some([vx * scale, 0.1, vz * scale]);
            }
        }
        cx.effects.push(Effect::Hurt {
            target: target.id,
            hit: Box::new(hit),
        });
        if target.kind == EntityType::Enderman {
            let v = self.base.velocity;
            self.base.velocity = [v[0] * -0.2, v[1] * -0.2, v[2] * -0.2];
            self.base.yaw += 180.0;
            return false;
        }
        self.sound_at(self.sound, cx);
        self.pierce == 0
    }

    /// Moves along this tick's path, hitting entities on the way (several
    /// if it pierces) or the block it runs into. Returns false once used.
    fn step_and_hit(&mut self, movement: [f64; 3], cx: &mut TickContext) -> bool {
        let start = self.base.position;
        let end = [
            start[0] + movement[0],
            start[1] + movement[1],
            start[2] + movement[2],
        ];
        let block = block_hit(start, end, cx);
        let border = border_stop(start, block.map_or(end, |hit| hit.location), cx);
        let block_end = border.unwrap_or_else(|| block.map_or(end, |hit| hit.location));
        loop {
            let from = self.base.position;
            let owner = self.owner.find(cx).map(|(id, _)| id);
            let left = self.owner.left;
            let pierced = self.pierced.clone();
            let spent = self.trident.as_ref().is_some_and(|t| t.dealt_damage);
            let skip = |id: i32| spent || (!left && Some(id) == owner) || pierced.contains(&id);
            let hits = targets_along(
                &self.base,
                self.bounds(),
                from,
                block_end,
                margin(self.ticks),
                skip,
                cx,
            );
            let Some(first) = hits.first() else {
                self.base.position = block_end;
                if border.is_some() {
                    bounce_off_border(&mut self.base, cx.rng);
                } else if let Some(hit) = block {
                    self.hit_block(hit, cx);
                }
                return true;
            };
            self.base.position = first.location;
            for target in &hits {
                if tuff_core::world::projectile::deflect_off(&mut self.base, target, cx) {
                    return true;
                }
                if self.hit_entity(target, cx) {
                    return false;
                }
                if self.pierce == 0 || self.trident.is_some() {
                    return true;
                }
            }
            if self.pierce == 0 {
                return true;
            }
        }
    }
}

impl RegionEntity for Arrow {
    fn ground_pickup(&mut self) -> Option<tuff_core::world::entity::GroundPickup<'_>> {
        let creative_only = self.creative_pickup();
        let stack = self.pickup_stack().map(DupUnchecked::dup_unchecked);
        if !creative_only && stack.is_none() {
            return None;
        }
        Some(tuff_core::world::entity::GroundPickup {
            bounds: self.base.bounds(),
            reserved: &mut self.reserved,
            refused_by: &mut self.refused_by,
            creative_only,
            stack,
            only: self.trident.as_ref().and(self.owner.uuid),
        })
    }

    fn presses_buttons(&self) -> bool {
        true
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        out.insert("life", self.life);
        if let Some(state) = self.in_block {
            out.insert(
                "inBlockState",
                tuff_core::storage::chunk_nbt::encode_block_state(state),
            );
        }
        out.insert("shake", self.shake as i8);
        out.insert("inGround", self.in_ground);
        out.insert("pickup", self.pickup.byte());
        out.insert("damage", self.damage);
        out.insert("crit", self.crit);
        out.insert("PierceLevel", self.pierce as i8);
        out.insert("SoundEvent", self.sound.name());
        out.insert("item", self.item.to_nbt());
        if let Some(weapon) = &self.weapon {
            out.insert("weapon", weapon.to_nbt());
        }
        if self.base.kind == EntityType::SpectralArrow {
            out.insert("Duration", self.glow_ticks);
        }
        if let Some(trident) = &self.trident {
            out.insert("DealtDamage", trident.dealt_damage);
        }
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        if self.flags() != 0 {
            values.push((DATA_FLAGS, MetadataValue::Byte(self.flags())));
        }
        if self.pierce != 0 {
            values.push((DATA_PIERCE, MetadataValue::Byte(self.pierce as i8)));
        }
        if self.in_ground {
            values.push((DATA_IN_GROUND, MetadataValue::Boolean(true)));
        }
        if self.base.kind == EntityType::Arrow {
            values.push((DATA_COLOR, MetadataValue::VarInt(self.color())));
        }
        if let Some(trident) = &self.trident {
            if trident.loyalty > 0 {
                values.push((DATA_LOYALTY, MetadataValue::Byte(trident.loyalty as i8)));
            }
            if trident.foil {
                values.push((DATA_FOIL, MetadataValue::Boolean(true)));
            }
        }
        values
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|index| self.value(index).map(|v| (index, v)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        if self
            .bounced
            .as_ref()
            .is_some_and(|b| b.load(std::sync::atomic::Ordering::Acquire))
        {
            return Tick::Discard;
        }
        self.ticks += 1;
        self.owner.has_been_shot = true;
        if self.life > 4
            && let Some(trident) = &mut self.trident
        {
            trident.dealt_damage = true;
        }
        if let Some(tick) = self.come_home(cx) {
            self.shake = self.shake.saturating_sub(1);
            return tick;
        }
        let movement = self.base.velocity;
        let pos = physics::block_pos(self.base.position);
        let state = (cx.block)(pos);
        if let Some(state) = state.filter(|s| !tuff_world_model::chunk::is_air(*s))
            && physics::block_boxes(Some(state), pos).any(|b| {
                let p = self.base.position;
                (0..3).all(|i| p[i] >= b[i] && p[i] < b[i + 3])
            })
        {
            self.base.velocity = [0.0; 3];
            self.set_in_ground(true);
        }
        self.shake = self.shake.saturating_sub(1);
        let water = tuff_core::world::projectile::in_water(self.bounds(), cx);
        if water {
            self.base.fire = 0;
        }
        if self.in_ground {
            if self.in_block != state && self.should_fall(cx) {
                self.set_in_ground(false);
                let v = self.base.velocity;
                self.base.velocity = [
                    v[0] * f64::from(cx.rng.r#gen::<f32>() * 0.2),
                    v[1] * f64::from(cx.rng.r#gen::<f32>() * 0.2),
                    v[2] * f64::from(cx.rng.r#gen::<f32>() * 0.2),
                ];
                self.life = 0;
            } else {
                self.life += 1;
                if self.life >= EXPOSED_POTION_DECAY_TICKS
                    && self.item.components.potion_contents.is_some()
                {
                    cx.effects.push(Effect::EntityPackets(packets([framed(
                        &play::EntityEvent {
                            entity_id: self.base.id,
                            status: EVENT_POTION_PUFF,
                        },
                    )])));
                    self.item = ItemStack::mint(Item::Arrow, 1, MintReason::EntityDrop);
                    self.changed.push(DATA_COLOR);
                }
                let kept = self.pickup == Pickup::Allowed
                    && self.trident.as_ref().is_some_and(|t| t.loyalty > 0);
                if self.life >= DESPAWN_TICKS && !kept {
                    return Tick::Discard;
                }
            }
            return Tick::Alive;
        }
        if water {
            let inertia = if self.trident.is_some() {
                TRIDENT_WATER_INERTIA
            } else {
                WATER_INERTIA
            };
            self.base.velocity = self.base.velocity.map(|c| c * inertia);
        }
        let (yaw, pitch) = rotation_of(movement);
        self.base.pitch = lerp_rotation(self.base.pitch, pitch);
        self.base.yaw = lerp_rotation(self.base.yaw, yaw);
        self.owner.check_left(self.bounds(), self.base.velocity, cx);
        if !self.step_and_hit(movement, cx) {
            return Tick::Discard;
        }
        if !self.in_ground
            && block_contact::projectile_bubble_column_push(
                self.bounds(),
                &mut self.base.velocity,
                cx.block,
            )
        {
            self.base.fall_distance = 0.0;
        }
        if !water {
            self.base.velocity = self.base.velocity.map(|c| c * AIR_DRAG);
        }
        if !self.in_ground && !self.base.no_gravity {
            self.base.velocity[1] -= GRAVITY;
        }
        Tick::Alive
    }
}

/// Loads a saved arrow.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let position = base.position;
    let velocity = base.velocity;
    let mut arrow = Arrow::of(base.kind, base.id, position, velocity, Owner::default());
    arrow.base = base;
    arrow.owner.load(nbt);
    arrow.life = take_number(nbt, "life").map_or(0, |v| v as i16);
    arrow.in_block = take(nbt, "inBlockState", |tag| {
        Some(tuff_core::storage::chunk_nbt::decode_block_state(tag))
    });
    arrow.shake = take_number(nbt, "shake").map_or(0, |v| v as u8);
    arrow.in_ground = take_bool(nbt, "inGround").unwrap_or(false);
    arrow.pickup =
        take_number(nbt, "pickup").map_or(Pickup::Disallowed, |v| Pickup::from_byte(v as i8));
    arrow.damage = take_number(nbt, "damage").unwrap_or(BASE_DAMAGE);
    arrow.crit = take_bool(nbt, "crit").unwrap_or(false);
    arrow.pierce = take_number(nbt, "PierceLevel").map_or(0, |v| v as u8);
    if let Some(sound) = take(nbt, "SoundEvent", |tag| {
        SoundEvent::from_name(tag.as_str()?)
    }) {
        arrow.sound = sound;
    }
    if let Some(item) = take(nbt, "item", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    }) {
        arrow.item = item;
    }
    arrow.weapon = take(nbt, "weapon", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    });
    if let Some(ticks) = take_number(nbt, "Duration") {
        arrow.glow_ticks = ticks as i32;
    }
    if arrow.base.kind == EntityType::Trident {
        let dealt_damage = take_bool(nbt, "DealtDamage").unwrap_or(false);
        let mut trident = Arrow::trident(
            &arrow.item,
            arrow.base.id,
            arrow.base.position,
            arrow.base.velocity,
            Owner::default(),
        )
        .trident
        .unwrap_or_default();
        trident.dealt_damage = dealt_damage;
        arrow.trident = Some(trident);
        arrow.weapon = Some(arrow.item.dup_unchecked());
    }
    Some(Box::new(arrow))
}

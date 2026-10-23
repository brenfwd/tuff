//! Sulfur cubes: harmless hopping cubes that swallow a block. The block's
//! archetype (data pack `sulfur_cube_archetype`) turns the cube into a
//! ball that flies when hit, slides, bounces or floats, burns what it
//! touches or primes and blows up. Players push a cube holding a block
//! by walking into it; shears pop the block out, an empty bucket picks
//! the cube up. Adults are size 2 and split into two babies when killed.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::item::{EquipmentSlot, ModifierOperation};
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::sulfur_cube::{
    self as archetypes, Archetype, ContactDamage, ExplosionData, KnockbackModifiers, SoundSettings,
};
use tuff_vanilla_data::{Item, damage_type, tag};

use super::control::{MoveStyle, Operation};
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::slime::{CubeFloat, CubeKeepJumping, CubeRandomDirection, Hop};
use super::{Aim, Damage, Knockback, Mob, MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Culprit, Effect, EntityBase, EntityHit, EntityInteraction, InteractOutcome, TickContext,
    take_bool, take_number,
};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::game_event::GameEvent;
use tuff_core::world::messages::{HandEffect, PlayerEvent};
use tuff_core::world::region_api::{DispenseOutcome, RegionWorld};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

type Touching = (i32, [f64; 3], f64, [f64; 3], f32);

const CUBE_SIZE: u8 = data::SPECIES;
const MAX_FUSE: u8 = data::SPECIES + 1;
const FROM_BUCKET: u8 = data::SPECIES + 2;
const ADULT_SIZE: i32 = 2;
const PICKUP_TIMER: i32 = 100;
const PUSH_DISTANCE: f64 = 1.299_999_952_316_284;
const MAX_PLAYER_PUSH_SPEED: f64 = 0.5;
const PLAYER_PUSH_SCALE: f32 = 0.3;
/// A riding player pushes from its vehicle, more gently.
const VEHICLE_PUSH_SCALE: f32 = 0.16;
const VERTICAL_PUSH: f64 = 0.300_000_011_920_929;
const HORIZONTAL_HIT_ANGLE_SCALE: f32 = 1.6;
const VERTICAL_HIT_ANGLE_SCALE: f32 = 0.5;
const VERTICAL_POSITION_ANGLE_SCALE: f32 = 0.8;
const ITEM_SEARCH_RANGE: f64 = 8.0;
const BODY_STEP: &str = "minecraft:sulfur_cube_body_step";

/// What the swallowed block's archetypes make of the cube.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Effects {
    floats: bool,
    explosion: Option<ExplosionData>,
    contact: [Option<ContactDamage>; 4],
    knockback: KnockbackModifiers,
    sounds: SoundSettings,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            floats: false,
            explosion: None,
            contact: [None; 4],
            knockback: archetypes::DEFAULT_KNOCKBACK,
            sounds: archetypes::DEFAULT_SOUNDS,
        }
    }
}

/// Whether `archetype` applies to `item`.
fn applies(archetype: &Archetype, item: Item) -> bool {
    match archetype.items {
        archetypes::Items::Tag(name) => {
            tuff_datapack::tags::with(|tags| tags.contains("minecraft:item", name, item.name()))
        }
        archetypes::Items::List(items) => items.contains(&item),
    }
}

fn matching(item: Option<Item>) -> impl Iterator<Item = &'static Archetype> {
    archetypes::ALL
        .iter()
        .filter(move |a| item.is_some_and(|item| applies(a, item)))
}

fn swallowable(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::SULFUR_CUBE_SWALLOWABLE, item)
}

fn food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::SULFUR_CUBE_FOOD, item)
}

fn body(core: &MobCore) -> Option<&ItemStack> {
    core.equipment.get(EquipmentSlot::Body)
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1.0e-5 {
        [0.0; 3]
    } else {
        v.map(|c| c / length)
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn rotate((x, y): (f32, f32), angle: f64) -> (f32, f32) {
    let (cos, sin) = (tuff_core::mth::cos(angle), tuff_core::mth::sin(angle));
    (x * cos - y * sin, y * cos + x * sin)
}

fn clamped_map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let t = (value - from_min) / (from_max - from_min);
    if t < 0.0 {
        to_min
    } else if t > 1.0 {
        to_max
    } else {
        to_min + t * (to_max - to_min)
    }
}

/// The velocity change a hit with `push` from an attacker aiming `aim`
/// from `feet` gives a cube holding a block: the hit's angle steers it,
/// aiming high or low trades height for distance, and hitting it from
/// below or above lifts or flattens it.
#[allow(clippy::too_many_arguments)]
fn ball_velocity(
    velocity: [f64; 3],
    modifiers: KnockbackModifiers,
    push: &Knockback,
    aim: Aim,
    feet: [f64; 3],
    position: [f64; 3],
    center: [f64; 3],
    height: f32,
    resistance: f64,
) -> [f64; 3] {
    let (horizontal, vertical) = (modifiers.horizontal_power, modifiers.vertical_power);
    let look = normalize(aim.look);
    let to_target = normalize(sub(center, aim.eye));
    let angle = (look[0] * to_target[2] - look[2] * to_target[0])
        .atan2(look[0] * to_target[0] + look[2] * to_target[2]) as f32;
    let direction = rotate(
        (push.dx as f32, push.dz as f32),
        f64::from(angle * HORIZONTAL_HIT_ANGLE_SCALE),
    );
    let half = 0.5 * f64::from(height);
    let to_top = normalize(sub([center[0], center[1] + half, center[2]], aim.eye));
    let to_bottom = normalize(sub([center[0], center[1] - half, center[2]], aim.eye));
    let factor = clamped_map(look[1], to_top[1], to_bottom[1], -1.0, 1.0) as f32;
    let mut ratio = (factor * VERTICAL_HIT_ANGLE_SCALE).abs();
    if factor < 0.0 {
        ratio = -ratio;
    }
    let power = (horizontal * (1.0 - ratio), vertical * (1.0 + ratio));
    let rise = sub(position, feet);
    let slope = (-rise[1]).atan2((rise[0] * rise[0] + rise[2] * rise[2]).sqrt()) as f32;
    let mut rotated = rotate(power, f64::from(-slope * VERTICAL_POSITION_ANGLE_SCALE));
    let ratio_of = |value: f32, original: f32| {
        if original > 0.0 {
            value.abs() / original
        } else {
            0.0
        }
    };
    let max_ratio = ratio_of(rotated.0, horizontal).max(ratio_of(rotated.1, vertical));
    if max_ratio > 1.0 {
        rotated = (rotated.0 / max_ratio, rotated.1 / max_ratio);
    }
    let multiplier = push.damage.sqrt()
        * if push.extra {
            push.power as f32 * 0.25
        } else {
            1.0
        };
    let keep = (1.0 - resistance) as f32;
    let horizontal = (rotated.0 * multiplier * keep * 0.4).clamp(-128.0, 128.0);
    let vertical = (rotated.1 * multiplier * keep).clamp(-128.0, 128.0);
    let along = normalize([f64::from(direction.0), 0.0, f64::from(direction.1)]);
    [
        velocity[0] - along[0] * f64::from(horizontal),
        velocity[1] + f64::from(vertical) * 1.2,
        velocity[2] - along[2] * f64::from(horizontal),
    ]
}

/// Ids of sulfur cubes that could blow up, published each tick so the
/// region can prime the ones standing in redstone power.
#[derive(Debug, Default)]
pub struct Primable(pub Vec<(i32, BlockPos)>);

/// Primes the cubes that stood in powered blocks this tick.
pub fn prime_powered(world: &mut dyn RegionWorld) {
    let Primable(candidates) = std::mem::take(world.state().get::<Primable>());
    for (id, pos) in candidates {
        if world.best_neighbour_signal(pos) == 0 {
            continue;
        }
        let rules_allow = world.shared().rule_bool("tnt_explodes");
        let Some(tracked) = world.entity_mut(id) else {
            continue;
        };
        let Some(mob) = tracked.entity.downcast_mut::<Mob<SulfurCube>>() else {
            continue;
        };
        if rules_allow {
            mob.species.pending_prime = true;
        }
    }
}

#[derive(Debug, Default)]
pub struct SulfurCube {
    size: i32,
    was_on_ground: bool,
    was_baby: bool,
    push_sound_cooldown: i32,
    fuse: i32,
    max_fuse: i32,
    from_bucket: bool,
    /// The body item whose archetypes are applied.
    applied: Option<tuff_item::item::ItemSnapshot>,
    effects: Effects,
    tick_count: i32,
    /// Redstone powered it last tick; it primes at the start of this one.
    pending_prime: bool,
    /// It swallowed a loose item since its last tick.
    pending_absorb_sound: bool,
}

impl SulfurCube {
    fn is_tiny(&self) -> bool {
        self.size <= 1
    }

    fn is_primed(&self) -> bool {
        self.fuse >= 0
    }

    fn can_explode(&self, core: &MobCore) -> bool {
        self.effects.explosion.is_some() && core.is_alive() && !self.is_primed()
    }

    fn sound_volume(&self) -> f32 {
        0.4 * self.size as f32
    }

    fn sound_pitch(&self, rng: &mut StdRng) -> f32 {
        let adjust = if self.is_tiny() { 1.4 } else { 0.8 };
        ((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + 1.0) * adjust
    }

    /// Sets the size and what follows from it; size 1 makes it a baby.
    pub fn set_size(&mut self, core: &mut MobCore, size: i32, heal: bool) {
        let size = size.clamp(1, ADULT_SIZE);
        self.size = size;
        core.size_scale = size as f32;
        core.refresh_size();
        core.set_attribute_base(Attribute::MaxHealth, f64::from(4 * size));
        core.set_attribute_base(
            Attribute::MovementSpeed,
            f64::from(0.2f32 + 0.1f32 * size as f32),
        );
        if heal {
            core.health = core.max_health();
            if size == 1 && !core.is_baby() {
                core.set_baby(true);
            }
        }
        self.was_baby = core.is_baby();
        core.changed(CUBE_SIZE);
    }

    fn goals() -> GoalSelector {
        let mut goals = GoalSelector::default();
        goals.add(1, CubeFloat);
        goals.add(2, CubeTempt::default());
        goals.add(3, SearchForItems::default());
        goals.add(4, CubeRandomDirection::default());
        goals.add(5, CubeKeepJumping);
        goals
    }

    /// Follows a change of the swallowed block: its archetypes' modifiers
    /// and effects, and the goals it only has while empty.
    fn sync_body(&mut self, core: &mut MobCore) {
        if tuff_item::item::same_slot(body(core), self.applied.as_deref()) {
            return;
        }
        let current = body(core).map(tuff_item::item::ItemSnapshot::of);
        if current.is_some() {
            core.goals = GoalSelector::default();
            core.set_speed(0.0);
            core.move_style = MoveStyle::Walk;
            core.movement.operation = Operation::Wait;
        } else {
            core.goals = Self::goals();
            core.move_style = MoveStyle::Hop(Hop {
                yaw: core.base.yaw,
                ..Hop::default()
            });
        }
        for archetype in matching(self.applied.as_ref().map(|s| s.item())) {
            for modifier in archetype.attribute_modifiers {
                core.put_modifier(modifier.attribute, modifier.id.into(), None, false);
            }
        }
        let mut effects = Effects::default();
        let mut contacts = 0;
        for archetype in matching(current.as_ref().map(|s| s.item())) {
            effects.floats |= archetype.buoyant;
            if archetype.explosion.is_some() {
                effects.explosion = archetype.explosion;
            }
            if let Some(contact) = archetype.contact_damage
                && contacts < effects.contact.len()
            {
                effects.contact[contacts] = Some(contact);
                contacts += 1;
            }
            effects.knockback = archetype.knockback;
            effects.sounds = archetype.sounds;
            for modifier in archetype.attribute_modifiers {
                core.put_modifier(
                    modifier.attribute,
                    modifier.id.into(),
                    Some((modifier.amount, modifier.operation)),
                    false,
                );
            }
        }
        let holding = current.is_some();
        core.put_modifier(
            Attribute::StepHeight,
            BODY_STEP.into(),
            holding.then_some((-1.0, ModifierOperation::AddMultipliedTotal)),
            false,
        );
        core.omnidirectional_air = holding;
        self.effects = effects;
        self.applied = current;
    }

    /// Swallows one of `stack`, spitting out a different block it held;
    /// false for a baby or the block it already holds.
    fn equip(
        &mut self,
        core: &mut MobCore,
        item: Item,
        one: impl FnOnce() -> Option<ItemStack>,
        cx: &mut TickContext,
    ) -> bool {
        if core.is_baby() {
            return false;
        }
        if body(core).is_some_and(|held| held.item() == item) {
            return false;
        }
        let Some(one) = one() else {
            return false;
        };
        if let Some(held) = core.equipment.take(EquipmentSlot::Body) {
            self.drop_at_top(core, held, cx);
        }
        self.hold(core, one);
        core.persistence_required = true;
        core.sound(SoundEvent::EntitySulfurCubeAbsorb, 1.0, 1.0, cx);
        true
    }

    fn hold(&mut self, core: &mut MobCore, stack: ItemStack) {
        core.equipment.set(EquipmentSlot::Body, Some(stack));
        core.equipment
            .set_drop_chance(EquipmentSlot::Body, crate::equipment::PRESERVED_DROP_CHANCE);
        self.sync_body(core);
    }

    /// Drops `stack` from where a rider would sit.
    fn drop_at_top(&self, core: &MobCore, stack: ItemStack, cx: &mut TickContext) {
        let offset = f64::from(core.size.height) - 0.015_625 * f64::from(self.size);
        core.drop_item(stack, offset, cx);
    }

    /// Spits the block out; the cube leaves loose items alone for a while.
    fn eject(&mut self, core: &mut MobCore, category: i32, cx: &mut TickContext) -> bool {
        let Some(held) = core.equipment.take(EquipmentSlot::Body) else {
            return false;
        };
        self.sync_body(core);
        self.drop_at_top(core, held, cx);
        core.sound_as(SoundEvent::EntitySulfurCubeEject, category, 1.0, 1.0, cx);
        core.signal = PICKUP_TIMER;
        true
    }

    /// Lights the fuse: the archetype's own, or a short random one for a
    /// chain reaction. False when it cannot blow up or TNT is off.
    fn prime(&mut self, core: &mut MobCore, imminent: bool, cx: &mut TickContext) -> bool {
        let Some(explosion) = self.effects.explosion else {
            return false;
        };
        if !core.is_alive() || !cx.shared.rule_bool("tnt_explodes") || self.is_primed() {
            return false;
        }
        let fuse = explosion.fuse as i32;
        let fuse = if imminent {
            cx.rng.gen_range(0..(fuse / 4).max(1)) + fuse / 8
        } else {
            fuse
        };
        core.base.invulnerable = true;
        self.fuse = fuse;
        self.max_fuse = fuse;
        core.changed(MAX_FUSE);
        let pitch = core.voice_pitch(cx.rng);
        core.sound(SoundEvent::EntityTntPrimed, self.sound_volume(), pitch, cx);
        cx.effects.push(Effect::GameEvent {
            event: GameEvent::PrimeFuse,
            position: core.base.position,
            source: Some(core.base.id),
        });
        true
    }

    fn tick_fuse(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.fuse > 0 {
            self.fuse -= 1;
        }
        let Some(explosion) = self.effects.explosion else {
            return;
        };
        if self.fuse != 0 {
            return;
        }
        core.dead = true;
        if cx.shared.rule_bool("tnt_explodes") {
            let rules = cx.shared.game_rules.borrow();
            let blocks = if !rules.bool("mob_griefing") {
                BlockInteraction::Keep
            } else if rules.bool("tnt_explosion_drop_decay") {
                BlockInteraction::DestroyWithDecay
            } else {
                BlockInteraction::Destroy
            };
            drop(rules);
            let [x, y, z] = core.base.position;
            cx.effects.push(Effect::Explode(Explosion {
                dangerous_wither_skull: false,
                spares_nether_portals: false,
                center: [x, y + 0.0625, z],
                radius: explosion.power as f32,
                fire: explosion.causes_fire,
                blocks,
                damage_type: &damage_type::PLAYER_EXPLOSION,
                igniter: None,
                culprit: Some(Box::new(Culprit::entity(&core.base))),
            }));
        }
        super::death_effects::removed(core, cx);
        core.discarded = true;
    }

    /// Hurts what it touches with its archetype's contact damage.
    fn contact(&self, core: &MobCore, target: i32, player: bool, cx: &mut TickContext) {
        for contact in self.effects.contact.iter().flatten() {
            let amount = if contact.max > contact.min {
                cx.rng.gen_range(contact.min..contact.max)
            } else {
                contact.min
            };
            let mut hit = EntityHit::new(amount, contact.damage_type);
            if contact.attribute_to_source || player {
                hit.attacker = Some(Culprit::entity(&core.base));
            }
            hit.position = Some(core.base.position);
            cx.effects.push(Effect::Hurt {
                target,
                hit: Box::new(hit),
            });
        }
    }

    /// Players walking into a cube holding a block shove it along, and
    /// it burns them if hot.
    fn touch_players(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if body(core).is_none() {
            return;
        }
        let bounds = core.bounds();
        let height = f64::from(core.size.height);
        let resistance = core.attribute(Attribute::KnockbackResistance);
        let root = |mut id: i32| {
            for _ in 0..16 {
                match cx
                    .entities
                    .iter()
                    .find(|e| e.id == id)
                    .and_then(|e| e.vehicle)
                {
                    Some(vehicle) => id = vehicle,
                    None => break,
                }
            }
            cx.entities.iter().find(|e| e.id == id)
        };
        let touching: Vec<Touching> = cx
            .players
            .iter()
            .filter_map(|p| {
                let ridden = p.vehicle.and_then(root);
                let reach = match ridden {
                    Some(vehicle) => {
                        let own = p.bounds();
                        let joined = [0, 1, 2, 3, 4, 5].map(|i| {
                            if i < 3 {
                                own[i].min(vehicle.bounds[i])
                            } else {
                                own[i].max(vehicle.bounds[i])
                            }
                        });
                        super::physics::inflate(joined, [1.0, 0.0, 1.0])
                    }
                    None => super::physics::inflate(p.bounds(), [1.0, 0.5, 1.0]),
                };
                if !super::physics::intersects(reach, bounds) {
                    return None;
                }
                Some(match ridden {
                    Some(vehicle) => {
                        let moved = if p.movement == [0.0; 3] {
                            vehicle.velocity
                        } else {
                            p.movement
                        };
                        (
                            p.id,
                            vehicle.position,
                            vehicle.bounds[4] - vehicle.bounds[1],
                            moved,
                            VEHICLE_PUSH_SCALE,
                        )
                    }
                    None => (p.id, p.position, p.height, p.movement, PLAYER_PUSH_SCALE),
                })
            })
            .collect();
        for (id, feet, player_height, movement, push_scale) in touching {
            let to_cube = sub(core.base.position, feet);
            let bottom = core.base.position[1];
            if (to_cube[0] * to_cube[0] + to_cube[2] * to_cube[2]).sqrt() >= PUSH_DISTANCE
                || feet[1] > bottom + height
                || feet[1] + player_height <= bottom
            {
                continue;
            }
            let knockback = (1.0 - resistance).max(0.0);
            let along = normalize([to_cube[0], 0.0, to_cube[2]]).map(|c| c * knockback);
            let speed =
                (movement[0] * movement[0] + movement[1] * movement[1] + movement[2] * movement[2])
                    .sqrt()
                    * 2.0
                    * f64::from(push_scale);
            let scale = speed.clamp(0.0, MAX_PLAYER_PUSH_SPEED);
            let lift = if core.base.on_ground {
                knockback * VERTICAL_PUSH
            } else {
                0.0
            };
            let push = [along[0] * scale, lift * scale, along[2] * scale];
            let threshold = f64::from(self.effects.sounds.push_threshold);
            if push.iter().map(|c| c * c).sum::<f64>() > threshold * threshold
                && self.push_sound_cooldown <= 0
            {
                self.push_sound_cooldown = (self.effects.sounds.push_cooldown * 20.0) as i32;
                core.sound(self.effects.sounds.push, 1.0, 1.0, cx);
            }
            for (axis, delta) in push.into_iter().enumerate() {
                core.base.velocity[axis] += delta;
            }
            core.impulse = true;
            self.contact(core, id, true, cx);
        }
    }

    /// Burns the living things it overlaps, as pushing them does.
    fn touch_entities(&self, core: &MobCore, cx: &mut TickContext) {
        if self.effects.contact.iter().all(Option::is_none) {
            return;
        }
        let bounds = core.bounds();
        let touching: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                e.id != core.base.id
                    && e.living.is_some_and(|l| l.alive)
                    && super::physics::intersects(e.bounds, bounds)
            })
            .map(|e| e.id)
            .collect();
        for id in touching {
            self.contact(core, id, false, cx);
        }
        let players: Vec<i32> = cx
            .players
            .iter()
            .filter(|p| super::physics::intersects(p.bounds(), bounds))
            .map(|p| p.id)
            .collect();
        for id in players {
            self.contact(core, id, true, cx);
        }
    }

    /// Takes one swallowable item it touches, unless it holds a block.
    fn pick_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if body(core).is_some()
            || core.is_baby()
            || core.signal > 0
            || !cx.shared.rule_bool("mob_griefing")
        {
            return;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some(id) = cx.entities.iter().find_map(|e| {
            let (item, count, takeable) = e.item?;
            (takeable
                && count > 0
                && swallowable(item)
                && super::physics::intersects(e.bounds, reach))
            .then_some(e.id)
        }) else {
            return;
        };
        cx.effects.push(Effect::TakeItem { item: id, count: 1 });
    }

    /// The push a hit gives while it holds a block, scaled by how the
    /// attacker aimed.
    fn ball_knockback(
        &self,
        core: &mut MobCore,
        push: &Knockback,
        damage: &Damage,
        cx: &mut TickContext,
    ) -> bool {
        if body(core).is_none() || damage.attacker.is_none() {
            return false;
        }
        let bounds = core.bounds();
        let center = [
            (bounds[0] + bounds[3]) / 2.0,
            (bounds[1] + bounds[4]) / 2.0,
            (bounds[2] + bounds[5]) / 2.0,
        ];
        let feet = damage.position.unwrap_or(core.base.position);
        let aim = damage.aim.unwrap_or(Aim {
            eye: feet,
            look: sub(center, feet),
        });
        let resistance = core.attribute(Attribute::KnockbackResistance);
        core.base.velocity = ball_velocity(
            core.base.velocity,
            self.effects.knockback,
            push,
            aim,
            feet,
            core.base.position,
            center,
            core.size.height,
            resistance,
        );
        core.impulse = true;
        core.sound(self.effects.sounds.hit, 1.0, 1.0, cx);
        true
    }

    /// The bucket a player scoops it into.
    fn bucket(&self, core: &mut MobCore) -> ItemStack {
        let age = core.age.as_ref().map_or(0, |a| a.age);
        let locked = core.age.as_ref().is_some_and(|a| a.locked);
        let nbt = super::fish::capture(core, |nbt| {
            nbt.insert("age", age);
            nbt.insert("age_locked", locked);
        });
        let mut bucket = ItemStack::mint(Item::SulfurCubeBucket, 1, MintReason::Spawn);
        bucket
            .components
            .other
            .insert("minecraft:bucket_entity_data", Tag::Compound(nbt));
        if let Some(held) = body(core) {
            bucket.components.other.insert(
                CONTENT,
                Tag::Compound(
                    tuff_item::item::ItemSnapshot::of(held)
                        .with_count(1)
                        .to_nbt(),
                ),
            );
        }
        bucket
    }
}

/// The item component a sulfur cube bucket keeps its swallowed block in.
pub const CONTENT: &str = "minecraft:sulfur_cube_content";

impl Species for SulfurCube {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            size: ADULT_SIZE,
            fuse: -1,
            max_fuse: -1,
            ..Self::default()
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.hostile = false;
        core.move_style = MoveStyle::Hop(Hop::default());
        core.navigation.can_float = true;
        core.max_head_x_rot = 0.0;
        core.goals = Self::goals();
        core.baby_keeps_size = true;
        core.size_scale = ADULT_SIZE as f32;
        core.refresh_size();
        core.set_attribute_base(Attribute::MaxHealth, f64::from(4 * ADULT_SIZE));
        core.set_attribute_base(
            Attribute::MovementSpeed,
            f64::from(0.2f32 + 0.1f32 * ADULT_SIZE as f32),
        );
        core.health = core.max_health();
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let size = take_number(nbt, "Size").map_or(0, |v| v as i32);
        let health = core.health;
        self.set_size(core, size.saturating_add(1), false);
        core.health = health.min(core.max_health());
        if let Some(saved) = take_number(nbt, "Health") {
            core.health = (saved as f32).clamp(0.0, core.max_health());
        }
        self.was_on_ground = take_bool(nbt, "wasOnGround").unwrap_or(false);
        core.signal = take_number(nbt, "pickup_timer").map_or(0, |v| v as i32);
        self.from_bucket = take_bool(nbt, "from_bucket").unwrap_or(false);
        self.fuse = take_number(nbt, "fuse").map_or(-1, |v| v as i32);
        self.max_fuse = self.fuse;
        self.was_baby = core.is_baby();
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("Size", self.size - 1);
        out.insert("wasOnGround", self.was_on_ground);
        out.insert("pickup_timer", core.signal.max(0));
        out.insert("from_bucket", self.from_bucket);
        out.insert("fuse", self.fuse);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (CUBE_SIZE, MetadataValue::VarInt(self.size)),
            (MAX_FUSE, MetadataValue::VarInt(self.max_fuse)),
            (FROM_BUCKET, MetadataValue::Boolean(self.from_bucket)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        let (hurt, death) = if self.is_tiny() {
            (
                SoundEvent::EntitySmallSulfurCubeHurt,
                SoundEvent::EntitySmallSulfurCubeDeath,
            )
        } else {
            (
                SoundEvent::EntitySulfurCubeHurt,
                SoundEvent::EntitySulfurCubeDeath,
            )
        };
        MobSounds {
            ambient: None,
            hurt: Some(hurt),
            death: Some(death),
            step: None,
            volume: self.sound_volume(),
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntitySmallSulfurCubeEat)
    }

    fn is_food<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        food(stack.item())
    }

    fn can_fall_in_love(&self, _core: &MobCore) -> bool {
        false
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if core.is_baby() || self.is_primed() {
            return None;
        }
        if self.can_explode(core) && matches!(stack.item(), Item::FlintAndSteel | Item::FireCharge)
        {
            if !cx.shared.rule_bool("tnt_explodes") {
                cx.effects.push(Effect::TellPlayer {
                    player: interaction.player,
                    event: Box::new(PlayerEvent::Overlay(
                        tuff_item::text::TextComponent::translate(
                            "block.minecraft.tnt.disabled",
                            [],
                        ),
                    )),
                });
                return None;
            }
            self.prime(core, false, cx);
            return Some(if stack.item() == Item::FlintAndSteel {
                HandEffect::Damage(1)
            } else {
                HandEffect::Consume(1)
            });
        }
        if stack.item() == Item::Shears && body(core).is_some() {
            self.eject(core, play::sound_category::PLAYERS, cx);
            cx.effects.push(Effect::GameEvent {
                event: GameEvent::Shear,
                position: core.base.position,
                source: Some(interaction.player),
            });
            return Some(HandEffect::Damage(1));
        }
        if swallowable(stack.item()) {
            if !self.equip(core, stack.item(), || interaction.hand.take(1), cx) {
                return None;
            }
            cx.effects.push(Effect::GameEvent {
                event: GameEvent::EntityInteract,
                position: core.base.position,
                source: Some(interaction.player),
            });
            return Some(HandEffect::None);
        }
        if stack.item() == Item::Bucket {
            let bucket = self.bucket(core);
            core.sound_as(
                SoundEvent::ItemBucketFillSulfurCube,
                play::sound_category::NEUTRAL,
                1.0,
                1.0,
                cx,
            );
            super::leash::untie(&mut core.leash, &core.base, true, cx);
            cx.effects.push(Effect::Discard);
            return Some(HandEffect::ExchangeStack(Box::new(bucket)));
        }
        None
    }

    fn shear(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        self.eject(core, play::sound_category::BLOCKS, cx)
    }

    fn dispensed_on(
        &mut self,
        core: &mut MobCore,
        stack: &tuff_core::world::region_api::Fired,
        cx: &mut TickContext,
    ) -> Option<DispenseOutcome> {
        if stack.item() == Item::FlintAndSteel {
            if !self.can_explode(core) {
                return None;
            }
            self.prime(core, false, cx);
            return Some(DispenseOutcome::Worn(1));
        }
        if !swallowable(stack.item()) || !self.equip(core, stack.item(), || stack.take(), cx) {
            return None;
        }
        Some(DispenseOutcome::Dispensed)
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if body(core).is_none() && swallowable(stack.item()) {
            self.hold(core, stack.keep(1, BurnReason::Consumed));
            self.pending_absorb_sound = true;
        }
    }

    fn intercept_hurt(
        &mut self,
        core: &mut MobCore,
        damage: &Damage,
        amount: f32,
        cx: &mut TickContext,
    ) -> Option<bool> {
        body(core)?;
        if self.can_explode(core) {
            if damage.is("is_fire") {
                self.prime(core, false, cx);
            } else {
                if damage.burning_projectile {
                    self.prime(core, false, cx);
                }
                if damage.is("is_explosion") {
                    self.prime(core, true, cx);
                }
            }
        }
        if !damage.is("sulfur_cube_with_block_immune_to") {
            return None;
        }
        if !damage.is("no_knockback") {
            let (dx, dz) = match damage.position {
                Some(from) => (
                    from[0] - core.base.position[0],
                    from[2] - core.base.position[2],
                ),
                None => (0.0, 0.0),
            };
            let push = Knockback {
                power: 0.4,
                dx,
                dz,
                damage: amount,
                extra: false,
            };
            if !self.ball_knockback(core, &push, damage, cx) {
                core.knockback(0.4, dx, dz, cx.rng);
            }
        }
        Some(true)
    }

    fn knockback(
        &mut self,
        core: &mut MobCore,
        push: &Knockback,
        damage: &Damage,
        cx: &mut TickContext,
    ) -> bool {
        self.ball_knockback(core, push, damage, cx)
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.tick_count += 1;
        self.sync_body(core);
        self.tick_fuse(core, cx);
        if std::mem::take(&mut self.pending_prime) && self.can_explode(core) {
            self.prime(core, false, cx);
        }
        if self.can_explode(core) {
            let pos = super::physics::block_pos(core.base.position);
            cx.state.get::<Primable>().0.push((core.base.id, pos));
        }
        if std::mem::take(&mut self.pending_absorb_sound) {
            core.sound(SoundEvent::EntitySulfurCubeAbsorb, 1.0, 1.0, cx);
        }
        if self.was_baby && !core.is_baby() {
            self.set_size(core, ADULT_SIZE, true);
        }
        self.was_baby = core.is_baby();
        if body(core).is_some() {
            let yaw = core.base.yaw;
            let close = yaw % 90.0;
            let close = if close >= 45.0 {
                close - 90.0
            } else if close < -45.0 {
                close + 90.0
            } else {
                close
            };
            core.base.yaw = yaw - close;
            core.head_yaw = core.base.yaw;
            core.body_yaw = core.base.yaw;
            if self.effects.floats && core.in_liquid() {
                let wobble = 0.2 * tuff_core::mth::sin(f64::from(self.tick_count as f32 * 0.4));
                let height = if core.body.in_water {
                    core.body.water_height
                } else {
                    core.body.lava_height
                };
                let immersion = height - f64::from(core.size.height) * 0.2 + f64::from(wobble);
                if immersion > 0.0 {
                    core.base.velocity[1] += immersion.min(1.0) * 0.039_999_999_105_930_33;
                }
            }
        }
        if core.base.on_ground && !self.was_on_ground {
            let squish = if self.is_tiny() {
                SoundEvent::EntitySmallSulfurCubeSquish
            } else if body(core).is_some() {
                SoundEvent::EntitySulfurCubeBounce
            } else {
                SoundEvent::EntitySulfurCubeSquish
            };
            let pitch = ((cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0) / 0.8;
            core.sound(squish, self.sound_volume(), pitch, cx);
        }
        self.was_on_ground = core.base.on_ground;
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let MoveStyle::Hop(hop) = &mut core.move_style
            && std::mem::take(&mut hop.jumped)
        {
            let sound = if self.is_tiny() {
                SoundEvent::EntitySmallSulfurCubeJump
            } else {
                SoundEvent::EntitySulfurCubeJump
            };
            let pitch = self.sound_pitch(cx.rng);
            core.sound(sound, self.sound_volume(), pitch, cx);
        }
        if core.signal > 0 {
            core.signal -= 1;
        }
        if self.push_sound_cooldown > 0 {
            self.push_sound_cooldown -= 1;
        }
        self.pick_up(core, cx);
        self.touch_entities(core, cx);
        self.touch_players(core, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let size = if core.is_baby() { 1 } else { ADULT_SIZE };
        self.set_size(core, size, true);
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        if core.is_baby() {
            0
        } else {
            1 + rng.gen_range(0..2)
        }
    }

    fn remove_dead(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.size <= 1 || self.is_primed() {
            return;
        }
        let offset = core.size.width / 2.0;
        let half = self.size / 2;
        for i in 0..2 {
            let dx = ((i % 2) as f32 - 0.5) * offset;
            let dz = ((i / 2) as f32 - 0.5) * offset;
            let [x, y, z] = core.base.position;
            let position = [x + f64::from(dx), y + 0.5, z + f64::from(dz)];
            let mut base = EntityBase::new(
                core.kind(),
                cx.shared.next_entity_id(),
                cx.rng.r#gen(),
                position,
                [0.0; 3],
            );
            base.yaw = cx.rng.r#gen::<f32>() * 360.0;
            base.custom_name = core.base.custom_name.clone();
            base.invulnerable = core.base.invulnerable;
            base.silent = core.base.silent;
            let mut child = Mob::<SulfurCube>::new(base, cx.rng);
            child.core.persistence_required = core.persistence_required;
            child.core.no_ai = core.no_ai;
            child.core.set_baby(true);
            child.species.set_size(&mut child.core, half, true);
            child.core.changed.clear();
            cx.effects.push(Effect::Spawn(Box::new(child)));
        }
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        self.applied
            .is_some()
            .then_some(OwnAirRules::NEVER_LOSES_AIR)
    }

    fn remove_when_far_away(&self) -> bool {
        self.applied.is_none() && !self.from_bucket
    }
}

/// Lures it towards a player holding what it wants: food for a baby, a
/// block to swallow for an adult.
#[derive(Debug, Default)]
struct CubeTempt {
    player: Option<i32>,
    calm_down: i32,
}

impl Goal for CubeTempt {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.calm_down > 0 {
            self.calm_down -= 1;
            return false;
        }
        let range = mob.attribute(Attribute::TemptRange);
        let wants = if mob.is_baby() { food } else { swallowable };
        let from = mob.base.position;
        self.player = cx
            .players
            .iter()
            .filter(|p| p.held.iter().flatten().any(|item| wants(*item)))
            .map(|p| (tuff_core::world::distance_sq(from, p.position), p.id))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        self.player.is_some()
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.player = None;
        mob.movement.operation = Operation::Wait;
        self.calm_down = reduced_delay(100);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(player) = self
            .player
            .and_then(|id| cx.players.iter().find(|p| p.id == id))
        else {
            return;
        };
        let eye = player.eye();
        mob.look
            .set_look_at(eye, mob.max_head_y_rot + 20.0, mob.max_head_x_rot);
        if mob.distance_sq(player.position) < 1.0 {
            mob.want_hop(0.0);
        } else {
            mob.turn_to(eye, 10.0, 10.0);
            let yaw = mob.base.yaw;
            mob.set_hop_direction(yaw, true);
        }
    }
}

/// Turns towards the nearest loose block it could swallow within eight
/// blocks.
#[derive(Debug, Default)]
struct SearchForItems {
    target: Option<[f64; 3]>,
}

impl Goal for SearchForItems {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.is_baby() || mob.signal > 0 {
            return false;
        }
        let area = super::physics::inflate(mob.bounds(), [ITEM_SEARCH_RANGE; 3]);
        let from = mob.base.position;
        self.target = cx
            .entities
            .iter()
            .filter(|e| {
                e.item.is_some_and(|(item, count, takeable)| {
                    takeable && count > 0 && swallowable(item)
                }) && super::physics::intersects(e.bounds, area)
            })
            .map(|e| (tuff_core::world::distance_sq(from, e.position), e.position))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p);
        self.target.is_some()
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        let Some(target) = self.target else {
            return;
        };
        mob.turn_to(target, 10.0, 10.0);
        let yaw = mob.base.yaw;
        mob.set_hop_direction(yaw, true);
    }
}

/// A sulfur cube let out of a bucket: its saved data, and the block the
/// bucket's `minecraft:sulfur_cube_content` says it holds.
pub fn from_bucket(
    base: EntityBase,
    data: Option<Compound>,
    components: Option<&Compound>,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    let mut nbt = data.unwrap_or_default();
    if let Some(age) = nbt.remove("age") {
        nbt.insert("Age", age);
    }
    if let Some(locked) = nbt.remove("age_locked") {
        nbt.insert("AgeLocked", locked);
    }
    nbt.insert("from_bucket", true);
    let mut entity = super::load::<SulfurCube>(base, &mut nbt)?;
    let content = components
        .and_then(|c| c.get_compound(CONTENT))
        .and_then(ItemStack::from_storage);
    if let Some(content) = content
        && let Some(mob) = entity.downcast_mut::<Mob<SulfurCube>>()
    {
        mob.species
            .hold(&mut mob.core, content.keep(1, BurnReason::Destroyed));
    }
    Some(entity)
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::SulfurCube, super::load::<SulfurCube>);
    registry.register_spawn_rule(EntityType::SulfurCube, |_, _, _, _, _| true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Mob<SulfurCube> {
        let base = EntityBase::new(EntityType::SulfurCube, 1, 7, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        Mob::<SulfurCube>::new(base, &mut rng)
    }

    #[tuff_core::watchdog]
    #[test]
    fn data_indices_match_the_dumped_layout() {
        let int = MetadataValue::VarInt(0).type_id() as u8;
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::SulfurCube.data_layout();
        assert_eq!(layout.len(), FROM_BUCKET as usize + 1);
        assert_eq!(
            layout[data::BABY as usize..],
            [boolean, boolean, int, int, boolean]
        );
        assert_eq!(layout[CUBE_SIZE as usize], int);
        assert_eq!(layout[MAX_FUSE as usize], int);
    }

    #[tuff_core::watchdog]
    #[test]
    fn adults_are_twice_a_babys_size() {
        let mut mob = cube();
        mob.species.set_size(&mut mob.core, 2, true);
        assert!((mob.core.size.width - 0.98).abs() < 1.0e-6);
        assert_eq!(mob.core.max_health(), 8.0);
        assert!(!mob.core.is_baby());
        mob.species.set_size(&mut mob.core, 1, true);
        assert!(mob.core.is_baby());
        assert!((mob.core.size.width - 0.49).abs() < 1.0e-6);
        assert_eq!(mob.core.max_health(), 4.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_swallowed_block_brings_its_archetype() {
        let mut mob = cube();
        assert!(!mob.core.goals.is_empty());
        mob.species.hold(
            &mut mob.core,
            ItemStack::mint(Item::OakPlanks, 1, MintReason::Test),
        );
        assert!(mob.core.goals.is_empty(), "a full cube does nothing itself");
        assert!(mob.core.omnidirectional_air);
        assert!((mob.core.attribute(Attribute::Bounciness) - 0.9).abs() < 1.0e-6);
        assert_eq!(mob.core.max_up_step(), 0.0);
        assert_eq!(
            mob.species.effects.sounds.hit,
            SoundEvent::EntitySulfurCubeBouncyHit
        );
        mob.core.equipment.set(EquipmentSlot::Body, None);
        mob.species.sync_body(&mut mob.core);
        assert!(!mob.core.goals.is_empty());
        assert_eq!(mob.core.attribute(Attribute::Bounciness), 0.0);
        assert_eq!(mob.species.effects, Effects::default());
        mob.species.hold(
            &mut mob.core,
            ItemStack::mint(Item::Tnt, 1, MintReason::Test),
        );
        assert_eq!(
            mob.species.effects.explosion.map(|e| e.fuse),
            Some(120),
            "TNT makes it explosive"
        );
        super::super::tests::with_context(|_, cx| {
            let tnt = ItemStack::mint(Item::Tnt, 1, MintReason::Test);
            assert!(
                !mob.species
                    .equip(&mut mob.core, Item::Tnt, || Some(tnt), cx),
                "it holds TNT already"
            );
            let magma = ItemStack::mint(Item::MagmaBlock, 1, MintReason::Test);
            assert!(
                mob.species
                    .equip(&mut mob.core, Item::MagmaBlock, || Some(magma), cx)
            );
            assert!(
                cx.effects.iter().any(
                    |e| matches!(e, Effect::DropItem { stack, .. } if stack.item() == Item::Tnt)
                ),
                "the old block is spat out"
            );
        });
        assert!(mob.species.effects.explosion.is_none());
        assert_eq!(
            mob.species.effects.contact[0].map(|c| c.damage_type.name),
            Some("minecraft:sulfur_cube_hot")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_level_swing_sends_a_held_cube_straight_on_and_up() {
        let push = Knockback {
            power: 0.4,
            dx: -1.0,
            dz: 0.0,
            damage: 4.0,
            extra: false,
        };
        let aim = Aim {
            eye: [-2.0, 64.245, 0.0],
            look: [1.0, 0.0, 0.0],
        };
        let velocity = ball_velocity(
            [0.0; 3],
            archetypes::DEFAULT_KNOCKBACK,
            &push,
            aim,
            [-2.0, 64.0, 0.0],
            [0.0, 64.0, 0.0],
            [0.0, 64.245, 0.0],
            0.49,
            0.0,
        );
        let horizontal = 0.33f32 * 2.0 * 0.4;
        let vertical = 0.06f32 * 2.0 * 1.2;
        assert!(
            (velocity[0] - f64::from(horizontal)).abs() < 1.0e-4,
            "{velocity:?}"
        );
        assert!(velocity[2].abs() < 1.0e-6);
        assert!(
            (velocity[1] - f64::from(vertical)).abs() < 1.0e-4,
            "{velocity:?}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn aiming_or_hitting_off_level_trades_distance_for_height() {
        let push = Knockback {
            power: 0.4,
            dx: -1.0,
            dz: 0.0,
            damage: 4.0,
            extra: false,
        };
        let level = |eye_y: f64, look_y: f64, feet_y: f64| {
            ball_velocity(
                [0.0; 3],
                archetypes::DEFAULT_KNOCKBACK,
                &push,
                Aim {
                    eye: [-2.0, eye_y, 0.0],
                    look: [1.0, look_y, 0.0],
                },
                [-2.0, feet_y, 0.0],
                [0.0, 64.0, 0.0],
                [0.0, 64.245, 0.0],
                0.49,
                0.0,
            )
        };
        let flat = level(64.245, 0.0, 64.0);
        let low = level(64.245, -0.12, 64.0);
        assert!(low[0] < flat[0], "{low:?} vs {flat:?}");
        assert!((low[1] - flat[1]).abs() < 1.0e-6, "capped at its power");
        let high = level(64.245, 0.12, 64.0);
        assert!(high[1] < flat[1], "{high:?} vs {flat:?}");
        assert!((high[0] - flat[0]).abs() < 1.0e-6, "capped at its power");
        let below = level(62.245, 0.0, 62.0);
        assert!(below[0] < flat[0], "{below:?} vs {flat:?}");
        assert!((below[1] - flat[1]).abs() < 1.0e-6, "capped at its power");
    }
}

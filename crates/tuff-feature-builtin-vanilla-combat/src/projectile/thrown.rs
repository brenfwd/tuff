//! Thrown items: snowballs (which hurt blazes), eggs (which sometimes hatch
//! chicks) and ender pearls (which carry their thrower to where they
//! land). They fall faster than arrows and break on the first thing they
//! hit.

use rand::Rng;
use tuff_core::world::block_contact;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::BASE_POTION_COLOR;
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext, take,
};
use tuff_core::world::projectile::{
    Owner, block_hit, hit_source, lerp_rotation, margin, rotation_of, targets_along,
};
use tuff_core::world::{framed, packets};
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const GRAVITY: f64 = 0.03;
const POTION_GRAVITY: f64 = 0.05;
const BOTTLE_GRAVITY: f64 = 0.07;
const AIR_DRAG: f64 = 0.99;
const WATER_INERTIA: f64 = 0.8;
const DATA_ITEM: u8 = 8;
/// The entity event that shows a thrown item breaking.
const BREAK_EVENT: i8 = 3;
/// A snowball's damage to blazes; everything else takes none.
const SNOWBALL_BLAZE_DAMAGE: f32 = 3.0;
const ENDERMITE_CHANCE: f32 = 0.05;
const PEARL_DAMAGE: f32 = 5.0;
const CHICK_AGE: i32 = -24000;

#[derive(Debug)]
pub struct Thrown {
    pub base: EntityBase,
    pub owner: Owner,
    pub item: ItemStack,
    ticks: u32,
}

impl Thrown {
    pub fn new(
        kind: EntityType,
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        item: ItemStack,
        owner: Owner,
    ) -> Self {
        let mut base = EntityBase::new(kind, id, rand::random(), position, velocity);
        let (yaw, pitch) = rotation_of(velocity);
        base.yaw = yaw;
        base.pitch = pitch;
        Self {
            base,
            owner,
            item: item.with_count(1),
            ticks: 0,
        }
    }

    fn event(&self, cx: &mut TickContext) {
        cx.effects.push(Effect::EntityPackets(packets([framed(
            &play::EntityEvent {
                entity_id: self.base.id,
                status: BREAK_EVENT,
            },
        )])));
    }

    /// What landing does; `hit` is the living thing it struck, if any, and
    /// `from` where the throw was at the start of this tick.
    fn land(
        &mut self,
        hit: Option<tuff_core::world::projectile::Target>,
        block: Option<tuff_core::world::raycast::Hit>,
        from: [f64; 3],
        cx: &mut TickContext,
    ) {
        if let Some(block) = block.filter(|_| hit.is_none()) {
            tuff_core::world::projectile::struck_block(block, &self.base, &self.owner, cx);
        }
        if self.is_potion() {
            if let Some(block) = block
                && hit.is_none()
                && self
                    .item
                    .components
                    .potion_contents
                    .as_ref()
                    .is_some_and(|c| {
                        c.custom_effects.is_empty()
                            && c.potion.is_some_and(|p| {
                                tuff_datapack::tags::is(
                                    tuff_vanilla_data::tag::potion::key::DOUSES_FIRE,
                                    p,
                                )
                            })
                    })
            {
                super::potion::douse(block.pos, block.face, cx);
            }
            let owner = self.owner.culprit(cx);
            let position = self.base.position;
            let at = tuff_world_model::BlockPos::new(
                position[0].floor() as i32,
                position[1].floor() as i32,
                position[2].floor() as i32,
            );
            super::potion::land(
                self.base.kind,
                &self.item,
                self.base.bounds(),
                at,
                position,
                hit.map(|target| target.position),
                margin(self.ticks),
                owner,
                cx,
            );
            return;
        }
        if self.base.kind == EntityType::ExperienceBottle {
            self.break_bottle(cx);
            return;
        }
        if let Some(target) = hit {
            let damage =
                if self.base.kind == EntityType::Snowball && target.kind == EntityType::Blaze {
                    SNOWBALL_BLAZE_DAMAGE
                } else {
                    0.0
                };
            let mut hit = EntityHit::new(damage, &damage_type::THROWN);
            hit.attacker = self.owner.culprit(cx);
            tuff_core::world::projectile::landed(
                &self.base,
                hit.attacker.clone(),
                None,
                Some(target.id),
                cx,
            );
            hit.projectile = Some(self.base.kind);
            hit.position = Some(hit_source(target.position, self.base.velocity));
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
        }
        match self.base.kind {
            EntityType::Egg => self.hatch(cx),
            EntityType::EnderPearl => self.teleport_owner(from, cx),
            _ => {}
        }
        self.event(cx);
    }

    fn is_potion(&self) -> bool {
        matches!(
            self.base.kind,
            EntityType::SplashPotion | EntityType::LingeringPotion
        )
    }

    /// A bottle o' enchanting shatters into the splash-potion particle and
    /// sound, then awards 3-11 experience (two rolls of 0..5 above a floor
    /// of 3, matching the game's spread).
    fn break_bottle(&self, cx: &mut TickContext) {
        let position = self.base.position;
        let at = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let event = |event, data| {
            framed(&play::LevelEvent {
                event,
                position: at,
                data,
                global: false,
            })
        };
        cx.effects.push(Effect::ChunkPackets(packets([
            event(
                play::level_event::PARTICLES_SPELL_POTION_SPLASH,
                BASE_POTION_COLOR,
            ),
            event(play::level_event::SOUND_SPELL_POTION_SPLASH, 0),
        ])));
        let amount = tuff_core::world::experience::bottle_amount(cx.rng);
        cx.effects.push(Effect::Experience { position, amount });
    }

    fn hatch(&self, cx: &mut TickContext) {
        if cx.rng.gen_range(0..8) != 0 {
            return;
        }
        let count = if cx.rng.gen_range(0..32) == 0 { 4 } else { 1 };
        for _ in 0..count {
            let nbt = Compound::new()
                .with("id", EntityType::Chicken.name())
                .with(
                    "Pos",
                    Tag::List(self.base.position.map(Tag::Double).to_vec()),
                )
                .with(
                    "Rotation",
                    Tag::List(vec![Tag::Float(self.base.yaw), Tag::Float(0.0)]),
                )
                .with("Age", CHICK_AGE);
            if let Some(chick) = cx.shared.entity_types.load(&nbt, 0) {
                cx.effects.push(Effect::Spawn(chick));
            }
        }
    }

    fn teleport_owner(&self, to: [f64; 3], cx: &mut TickContext) {
        let Some((player, _)) = self.owner.find(cx) else {
            return;
        };
        if !self.owner.is_player(cx) {
            return;
        }
        self.maybe_leave_endermite(player, cx);
        cx.effects.push(Effect::TeleportPlayer {
            player,
            handle: self.owner.handle.clone(),
            position: to,
            fall_damage: PEARL_DAMAGE,
        });
        let packet = framed(&play::Sound {
            sound_event: SoundEvent::EntityPlayerTeleport.id(),
            category: play::sound_category::PLAYERS,
            position: to,
            volume: 1.0,
            pitch: 1.0,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::ChunkPackets(packets([packet])));
    }
}

impl Thrown {
    /// One pearl in twenty leaves an endermite where its thrower stood,
    /// when monsters may spawn.
    fn maybe_leave_endermite(&self, player: i32, cx: &mut TickContext) {
        if cx.rng.r#gen::<f32>() >= ENDERMITE_CHANCE
            || !cx.shared.game_rules.borrow().bool("spawn_monsters")
            || *cx.shared.difficulty.borrow() == tuff_core::world::player::Difficulty::Peaceful
        {
            return;
        }
        let Some(thrower) = cx.players.iter().find(|p| p.id == player) else {
            return;
        };
        let (position, yaw, pitch) = (thrower.position, thrower.yaw, thrower.pitch);
        let id = cx.shared.next_entity_id();
        let Some(mut endermite) =
            cx.shared
                .entity_types
                .create(EntityType::Endermite, id, position, yaw)
        else {
            return;
        };
        endermite.base_mut().pitch = pitch;
        cx.effects.push(Effect::Spawn(endermite));
    }
}

impl RegionEntity for Thrown {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        self.owner.save(out);
        out.insert("Item", self.item.to_nbt());
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(DATA_ITEM, MetadataValue::Slot(Some(self.item.clone())))]
    }

    fn spawn_data(&self) -> i32 {
        self.owner.spawn_data()
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        self.owner.has_been_shot = true;
        let from = self.base.position;
        if !self.base.no_gravity {
            self.base.velocity[1] -= if self.base.kind == EntityType::ExperienceBottle {
                BOTTLE_GRAVITY
            } else if self.is_potion() {
                POTION_GRAVITY
            } else {
                GRAVITY
            };
        }
        let inertia = if tuff_core::world::projectile::in_water(self.base.bounds(), cx) {
            WATER_INERTIA
        } else {
            AIR_DRAG
        };
        self.base.velocity = self.base.velocity.map(|c| c * inertia);
        let velocity = self.base.velocity;
        let end = [
            from[0] + velocity[0],
            from[1] + velocity[1],
            from[2] + velocity[2],
        ];
        let block = block_hit(from, end, cx);
        let to = block.map_or(end, |hit| hit.location);
        let owner = self.owner.find(cx).map(|(id, _)| id);
        let left = self.owner.left;
        let target = targets_along(
            &self.base,
            self.base.bounds(),
            from,
            to,
            margin(self.ticks),
            |id| !left && Some(id) == owner,
            cx,
        )
        .into_iter()
        .next()
        .filter(|t| !tuff_core::world::projectile::deflect_off(&mut self.base, t, cx));
        self.base.position = match (&target, block) {
            (Some(target), _) => target.location,
            (None, Some(hit)) => hit.location,
            (None, None) => end,
        };
        let (yaw, pitch) = rotation_of(velocity);
        self.base.pitch = lerp_rotation(self.base.pitch, pitch);
        self.base.yaw = lerp_rotation(self.base.yaw, yaw);
        self.owner.check_left(self.base.bounds(), velocity, cx);
        let push = if self.base.kind == EntityType::EnderPearl {
            block_contact::bubble_column_push
        } else {
            block_contact::projectile_bubble_column_push
        };
        if push(self.base.bounds(), &mut self.base.velocity, cx.block) {
            self.base.fall_distance = 0.0;
        }
        if target.is_some() || block.is_some() {
            self.land(target, block, from, cx);
            return Tick::Discard;
        }
        if self.base.kind == EntityType::EnderPearl && self.owner.is_player(cx) {
            cx.state
                .get::<crate::world::region::pearls::PearlTickets>()
                .report(self.base.chunk());
        }
        Tick::Alive
    }
}

/// Loads a saved snowball, egg or ender pearl.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let default = match base.kind {
        EntityType::Snowball => Item::Snowball,
        EntityType::Egg => Item::Egg,
        EntityType::SplashPotion => Item::SplashPotion,
        EntityType::LingeringPotion => Item::LingeringPotion,
        EntityType::ExperienceBottle => Item::ExperienceBottle,
        _ => Item::EnderPearl,
    };
    let item = take(nbt, "Item", |tag| ItemStack::from_nbt(tag.as_compound()?))
        .unwrap_or_else(|| ItemStack::new(default, 1));
    let mut owner = Owner::default();
    owner.load(nbt);
    Some(Box::new(Thrown {
        base,
        owner,
        item,
        ticks: 0,
    }))
}

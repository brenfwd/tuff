//! The state every mob shares: health, physics, goals, pathfinding,
//! and the ageable/animal parts common to many species.

use std::fmt::Debug;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::entity::{PathType, Size};
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};

use super::animal::{Age, Love};
use super::control::{BodyRotation, JumpControl, LookControl, MoveControl};
use super::equipment::Equipment;
use super::goal::{Flags, GoalSelector};
use super::navigation::Navigation;
use super::physics::Aabb;
use super::{Damage, MOB_FLAG_AGGRESSIVE, MOB_FLAG_LEFT_HANDED, MOB_FLAG_NO_AI, data};
use super::{attack, control, target};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::{self};
use tuff_core::world::entity::{Effect, EntityBase, LivingSummary, TickContext};
use tuff_core::world::{framed, packets};
use tuff_item::item::ItemSnapshot;
use tuff_item::item::ItemStack;
use tuff_vanilla_common::combat;
use tuff_vanilla_common::shadow::{self, SHADOW_RANGE, distance};
use tuff_world_model::BlockPos;

/// An attribute modifier a mob carries; permanent ones are saved.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeciesModifier {
    pub attribute: Attribute,
    pub id: std::borrow::Cow<'static, str>,
    pub amount: f64,
    pub operation: tuff_vanilla_data::item::ModifierOperation,
    pub permanent: bool,
}

/// The state every mob has.
#[derive(Debug)]
pub struct MobCore {
    pub base: EntityBase,
    pub size: Size,
    pub health: f32,
    pub absorption: f32,
    /// Base attribute values that differ from the type's, as saved.
    pub attribute_bases: Vec<(Attribute, f64)>,
    pub hurt_time: i16,
    pub death_time: i16,
    pub stuck_arrows: tuff_core::world::entity::StuckArrows,
    pub last_hurt: f32,
    pub dead: bool,
    pub last_hurt_by_player: Option<u128>,
    pub last_hurt_by_player_time: i32,
    /// The last damage taken and when.
    pub last_damage: Option<(Damage, u64)>,
    pub body: physics_state::Body,
    pub head_yaw: f32,
    pub body_yaw: f32,
    pub goals: GoalSelector,
    pub look: LookControl,
    pub movement: MoveControl,
    pub jump: JumpControl,
    pub body_rotation: BodyRotation,
    pub navigation: Navigation,
    pub path_malus: Vec<(PathType, f32)>,
    pub no_action_time: i32,
    pub ambient_sound_time: i32,
    pub persistence_required: bool,
    /// A living entity that is not a mob (mannequins): no mob flags byte
    /// in its data (the species owns index 15) and no mob-only save keys.
    pub not_a_mob: bool,
    /// Never moves, falls or is pushed (a marker or gravity-free armor
    /// stand).
    pub no_physics: bool,
    /// Other bodies never push it (an armor stand).
    pub unpushable: bool,
    /// Bumping an enemy now and then makes it the target (an iron golem).
    pub retargets_on_push: bool,
    /// While it has a target, it plans falls its health could take (a creeper).
    pub fall_by_health: bool,
    /// The face of the block it clings to (a shulker): its head turns in
    /// that face's frame and never pitches, and its body never turns.
    pub attached: Option<tuff_vanilla_data::block::Facing>,
    /// Its box relative to its position, in place of one from its width
    /// and height (a shulker's opened shell).
    pub box_offset: Option<Aabb>,
    /// Its acceleration in flight, in place of the common 0.02 (a happy
    /// ghast's flying speed).
    pub flying_speed: Option<f32>,
    /// Knockback and shoves leave it where it is (a creaking frozen by a
    /// gaze).
    pub no_knockback: bool,
    /// Moves through blocks, is never in a wall and only others push it
    /// (a vex).
    pub phasing: bool,
    /// Others did not push or count it last tick: it climbed, or a horse
    /// carried a rider.
    pub pushed_off: bool,
    /// Its region was in crowd mode last tick (see `crowd`), so this tick
    /// runs simplified.
    pub crowded: bool,
    pub crowd_marked: bool,
    /// Sees nothing (a stunned or roaring ravager).
    pub blind: bool,
    pub no_ai: bool,
    pub left_handed: bool,
    pub can_pick_up_loot: bool,
    pub age: Option<Age>,
    pub love: Option<Love>,
    /// Owner and sit order, for a tameable species (`make_tameable`).
    pub tame: Option<super::animal::Tame>,
    /// The data index `tame`'s flags byte uses; the owner follows at +1.
    pub(super) tame_flags_index: u8,
    /// This tick's steering from a driving rider (see
    /// `RegionEntity::rideable_by_input`); latched by `steer_with_input`
    /// until the next call, cleared on a fresh mount so a stale value
    /// from a previous rider can't move it. Only consulted for a species
    /// whose `Species::drivable` says yes.
    pub rider_input: Option<tuff_core::world::entity::RiderInput>,
    /// Where the server's own simulation of a client-driven mount stands.
    pub shadow: Option<DrivenShadow>,
    /// Set by a goal when the mob ate (a sheep eating grass).
    pub ate: bool,
    pub tick_count: u64,
    pub min_y: i32,
    pub max_y: i32,
    /// Set by a panic goal while it runs.
    pub panicking: bool,
    /// The baby villager a play-tag chase is after.
    pub chasing: Option<i32>,
    /// A species-driven behavior is running and holds the mob in place.
    pub busy: bool,
    /// A baby of a type that never grows up on its own (zombies).
    pub baby: bool,
    /// Lasting effects it is under: they tick down, apply their periodic
    /// action (poison, regeneration, wither, hunger) and feed into
    /// [`MobCore::attribute`] like a player's.
    pub effects: effects::ActiveEffects,
    /// Willing to breed, for types that court without love mode
    /// (villagers); others see it as being in love.
    pub breedable: bool,
    /// The player or entity it attacks, by id.
    pub target: Option<i32>,
    /// Who last hurt it, and at which tick.
    pub last_hurt_by_mob: Option<(i32, u64)>,
    /// Arms raised for an attack (mob flag).
    pub aggressive: bool,
    /// Plays its sounds as a hostile mob.
    pub hostile: bool,
    /// Experience dropped when killed by a player, before species bonuses.
    pub xp_reward: i32,
    pub equipment: Equipment,
    /// Modifiers a species adds to its attributes (a baby's speed), by id.
    pub modifiers: Vec<SpeciesModifier>,
    /// What its gear's `location_changed` enchantment effects last saw.
    pub enchanted_location: Option<Box<crate::instance::LocationEffects>>,
    /// It is gone at the end of this tick (an exploded creeper).
    pub discarded: bool,
    /// Adjusts its melee hits.
    pub attack_hook: Option<attack::AttackHook>,
    /// Adjusts the arrows it shoots (a stray's slowness).
    pub arrow_hook: Option<fn(&MobCore, &mut tuff_core::world::projectile::ArrowShot)>,
    /// Ticks it has been using the item in its main hand (drawing a bow).
    pub using_item: Option<u32>,
    /// It clings to the wall it bumped into (spiders).
    pub climbing: bool,
    /// It flies: no gravity and no fall damage (ghasts).
    pub flying: bool,
    /// Air drags it the same way up and down as sideways (a sulfur cube
    /// holding a block).
    pub omnidirectional_air: bool,
    /// The power of the explosions it causes (a ghast's fireballs).
    pub explosion_power: i32,
    /// A position it keeps away from (a hoglin's repellent).
    pub avoid: Option<BlockPos>,
    /// Its own wander preference, in place of the animal or monster one.
    pub walk_value: Option<fn(&MobCore, BlockPos, &TickContext) -> f32>,
    /// Its type's size is scaled by this (a slime's size).
    pub size_scale: f32,
    /// Being a baby does not shrink its box (a sulfur cube's size alone
    /// sets it).
    pub baby_keeps_size: bool,
    /// Added to its jump power (big magma cubes jump higher).
    pub jump_boost: f64,
    /// Ticks a rabbit stays full after raiding carrots.
    pub more_carrot_ticks: i32,
    /// The upward speed a jump in lava sets, instead of swimming up.
    pub lava_jump: Option<f64>,
    /// How its move control turns a wanted position into motion.
    pub move_style: control::MoveStyle,
    /// How its look control turns its head.
    pub look_style: control::LookStyle,
    /// How far its head may pitch, and turn from its body.
    pub max_head_x_rot: f32,
    pub max_head_y_rot: f32,
    /// How it moves itself while in water.
    pub water_travel: super::movement::WaterTravel,
    /// Its move control is driving it this tick (a guardian's spikes
    /// retract while it swims).
    pub moving: bool,
    /// Swimming under water (a drowned chasing something): currents do not
    /// push it.
    pub swimming: bool,
    /// Lying asleep (a fox): shown in the Sleeping pose.
    pub sleeping: bool,
    /// A frog croaking: shown in the Croaking pose.
    pub croaking: bool,
    /// A frog's tongue is out for this entity.
    pub tongue_target: Option<i32>,
    /// Entities a frog could not reach, with the tick it forgets them.
    pub unreachable_until: Vec<(i32, u64)>,
    /// A cat curled up on a bed, or settling there.
    pub lying: bool,
    pub relaxed: bool,
    /// A panda's flags byte (sitting, lying on its back) and whether its
    /// genes make it lazy.
    pub panda_flags: u8,
    pub panda_lazy: bool,
    pub panda_weak: bool,
    pub panda_playful: bool,
    pub panda_worried: bool,
    /// A panda's unhappy counter (it refuses to breed without bamboo) and
    /// sneeze counter.
    pub panda_unhappy: i32,
    pub panda_sneeze: i32,
    pub panda_roll: i32,
    pub panda_roll_delta: [f64; 2],
    /// A panda's eat counter (zero when not eating).
    pub panda_eat: i32,
    /// Other mobs may target it (false while an axolotl plays dead).
    pub seen_as_enemy: bool,
    /// On its way back to `bound_origin` (a turtle going home).
    pub homeward: bool,
    /// A velocity its goals pick for its species to apply (a squid's next
    /// push).
    pub drift: [f64; 3],
    /// A value a species' own goals set for its tick to read (a
    /// creeper's swell direction).
    pub signal: i32,
    /// A pose its species forces over the usual ones (a breeze inhaling).
    pub pose: Option<tuff_vanilla_data::entity::Pose>,
    /// Ground friction is ignored, so a leap keeps its speed (a breeze).
    pub discard_friction: bool,
    /// A neutral mob's grudge.
    pub anger: Option<target::Anger>,
    /// Kinds it never attacks (an iron golem spares creepers).
    pub never_attacks: &'static [EntityType],
    /// A saved grudge `(end game time, target uuid)` waiting for the first tick.
    pub(super) anger_loaded: Option<(i64, u128)>,
    pub(super) world_tick: u64,
    /// Hits it asked to hear about that hurt their victim since its last tick.
    pub landed_hits: u32,
    pub(super) piglin: super::piglin::PiglinBrain,
    /// A block it carries, which its goals pick up and put down (endermen).
    pub carried_block: Option<tuff_core::world::block_ledger::HeldBlock>,
    /// Who summoned it, for goals that copy an owner's target (an
    /// evoker's vexes); not saved once the owning entity is gone.
    pub owner: Option<u128>,
    /// Where it was bound (a vex's summon position), for goals that wander
    /// near it rather than its current position.
    pub bound_origin: Option<BlockPos>,
    /// A flower it remembers pollinating (a bee); its goal sets this and
    /// `nectar` and species save/load them.
    /// The lead on it.
    pub leash: Option<tuff_core::world::leash::Leash>,
    /// Where it keeps to (the holder of its lead, a wandering trader's
    /// meeting point); goals avoid targets outside it.
    pub home: Option<Home>,
    /// It wore a lead at the end of its last tick.
    pub(super) was_leashed: bool,
    /// Raid and patrol membership, for raid mobs.
    pub raider: Option<super::raider::RaiderState>,
    pub flower: Option<BlockPos>,
    /// Whether it still carries nectar from `flower` (a bee).
    pub nectar: bool,
    /// Crops it grew since it last pollinated (a bee).
    pub crops_grown: i32,
    /// The hive it lives in and the ticks before it may enter one again (a bee).
    pub hive: Option<BlockPos>,
    pub cannot_enter_hive: i32,
    /// Ticks since a bee last left a hive without carrying nectar.
    pub ticks_without_nectar: i32,
    /// Ticks a bee has been under water.
    pub underwater_ticks: i32,
    /// A block change spoiled the path; recompute it next tick.
    pub(super) recompute_requested: bool,
    /// Damage from the world (fire, lava, explosions), applied next tick.
    pub(super) pending_hurts: Vec<(f32, &'static DamageType)>,
    pub(super) pending_blamed_hurts:
        Vec<(f32, &'static DamageType, tuff_core::world::entity::Culprit)>,
    /// Effect changes and instant effects from outside its tick (commands,
    /// splash potions), applied and announced next tick.
    pub(super) pending_effects: Vec<effects::EffectChange>,
    pub(super) pending_effect_actions: Vec<effects::EffectAction>,
    /// Fall damage from landing this tick.
    pub fall_damage: Option<(f32, &'static tuff_vanilla_data::damage_type::DamageType)>,
    /// How far it fell before landing this tick, for species that react
    /// (a creeper swells).
    pub landed_fall: Option<f64>,
    /// It bounced since the region last looked: viewers get its stops.
    pub position_sync: bool,
    /// Knocked back since the region last looked: its velocity goes out.
    pub impulse: bool,
    /// Horizontal distance walked, for step sounds.
    pub move_distance: f32,
    pub next_step: f32,
    /// A step sound is due.
    pub stepped: bool,
    pub(super) changed: Vec<u8>,
    /// A mob riding it steers it (see `mount`).
    pub(super) steered: bool,
    /// Whether its goals were last set up for being steered.
    pub(super) goals_steered: bool,
    /// Paths its goals asked for while steering what it rides, for the
    /// vehicle to search.
    pub(super) steer_requests: Option<Vec<super::mount::PathRequest>>,
    /// The box of what it rides as of its last AI step, which widens its
    /// melee reach.
    pub(super) vehicle_bounds: Option<Aabb>,
}

pub mod physics_state {
    /// Movement state that is not saved.
    #[derive(Debug, Clone, Default)]
    pub struct Body {
        pub horizontal_collision: bool,
        pub vertical_collision: bool,
        pub supporting: Option<tuff_world_model::BlockPos>,
        pub water_height: f64,
        pub lava_height: f64,
        pub in_water: bool,
        pub eyes_in_water: bool,
        pub jumping: bool,
        pub no_jump_delay: i32,
        pub xxa: f32,
        pub yya: f32,
        pub zza: f32,
        pub speed: f32,
        /// Where it was at the start of the tick.
        pub previous: [f64; 3],
        pub first_tick: bool,
        /// It was inside powder snow last tick.
        pub in_powder_snow: bool,
        /// The per-axis scale of its next move, set by a block holding it
        /// back (a cobweb).
        pub stuck: Option<[f64; 3]>,
    }
}

/// The area a mob keeps to: within `radius` blocks of `pos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Home {
    pub pos: BlockPos,
    pub radius: i32,
}

impl Home {
    /// Whether the block `pos` is inside (compared corner to corner).
    pub fn contains(&self, pos: BlockPos) -> bool {
        let d = [pos.x - self.pos.x, pos.y - self.pos.y, pos.z - self.pos.z].map(f64::from);
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < f64::from(self.radius * self.radius)
    }

    /// Whether the point `at` is inside (measured from the home block's
    /// centre).
    pub fn contains_point(&self, at: [f64; 3]) -> bool {
        tuff_core::world::distance_sq(self.pos.center(), at) < f64::from(self.radius * self.radius)
    }
}

impl MobCore {
    pub fn new(base: EntityBase) -> Self {
        let kind = base.kind;
        let mut core = Self {
            size: kind.size(),
            health: 0.0,
            absorption: 0.0,
            attribute_bases: Vec::new(),
            hurt_time: 0,
            death_time: 0,
            stuck_arrows: Default::default(),
            last_hurt: 0.0,
            dead: false,
            last_hurt_by_player: None,
            last_hurt_by_player_time: 0,
            last_damage: None,
            body: physics_state::Body {
                first_tick: true,
                previous: base.position,
                ..Default::default()
            },
            head_yaw: base.yaw,
            body_yaw: base.yaw,
            goals: GoalSelector::default(),
            look: LookControl::default(),
            movement: MoveControl::default(),
            jump: JumpControl::default(),
            body_rotation: BodyRotation::default(),
            navigation: Navigation::default(),
            path_malus: Vec::new(),
            no_action_time: 0,
            ambient_sound_time: 0,
            persistence_required: false,
            not_a_mob: false,
            no_physics: false,
            unpushable: false,
            retargets_on_push: false,
            fall_by_health: false,
            attached: None,
            box_offset: None,
            no_knockback: false,
            flying_speed: None,
            phasing: false,
            pushed_off: false,
            crowded: false,
            crowd_marked: false,
            blind: false,
            no_ai: false,
            left_handed: false,
            can_pick_up_loot: false,
            age: None,
            love: None,
            tame: None,
            tame_flags_index: 0,
            rider_input: None,
            pose: None,
            discard_friction: false,
            shadow: None,
            ate: false,
            tick_count: 0,
            min_y: -64,
            max_y: 319,
            panicking: false,
            chasing: None,
            busy: false,
            baby: false,
            breedable: false,
            effects: effects::ActiveEffects::default(),
            target: None,
            last_hurt_by_mob: None,
            aggressive: false,
            hostile: false,
            xp_reward: 0,
            equipment: Equipment::default(),
            modifiers: Vec::new(),
            enchanted_location: None,
            discarded: false,
            attack_hook: None,
            arrow_hook: None,
            using_item: None,
            climbing: false,
            flying: false,
            omnidirectional_air: false,
            explosion_power: 1,
            avoid: None,
            walk_value: None,
            size_scale: 1.0,
            baby_keeps_size: false,
            jump_boost: 0.0,
            more_carrot_ticks: 0,
            lava_jump: None,
            move_style: control::MoveStyle::Walk,
            look_style: control::LookStyle::Default,
            max_head_x_rot: control::MAX_HEAD_X_ROT,
            max_head_y_rot: control::MAX_HEAD_Y_ROT,
            water_travel: super::movement::WaterTravel::Standard,
            moving: false,
            swimming: false,
            sleeping: false,
            lying: false,
            croaking: false,
            tongue_target: None,
            unreachable_until: Vec::new(),
            relaxed: false,
            panda_flags: 0,
            panda_eat: 0,
            panda_lazy: false,
            panda_weak: false,
            panda_playful: false,
            panda_worried: false,
            panda_unhappy: 0,
            panda_sneeze: 0,
            panda_roll: 0,
            panda_roll_delta: [0.0; 2],
            seen_as_enemy: true,
            homeward: false,
            drift: [0.0; 3],
            signal: 0,
            anger: None,
            never_attacks: &[],
            anger_loaded: None,
            world_tick: 0,
            landed_hits: 0,
            piglin: Default::default(),
            carried_block: None,
            owner: None,
            bound_origin: None,
            leash: None,
            home: None,
            was_leashed: false,
            raider: None,
            flower: None,
            nectar: false,
            crops_grown: 0,
            hive: None,
            cannot_enter_hive: 0,
            ticks_without_nectar: 0,
            underwater_ticks: 0,
            recompute_requested: false,
            pending_hurts: Vec::new(),
            pending_blamed_hurts: Vec::new(),
            pending_effects: Vec::new(),
            pending_effect_actions: Vec::new(),
            fall_damage: None,
            landed_fall: None,
            position_sync: false,
            impulse: false,
            move_distance: 0.0,
            next_step: 1.0,
            stepped: false,
            changed: Vec::new(),
            steered: false,
            goals_steered: false,
            steer_requests: None,
            vehicle_bounds: None,
            base,
        };
        core.health = core.max_health();
        core
    }

    pub fn kind(&self) -> EntityType {
        self.base.kind
    }

    /// Whether the block `pos` lies within its home, if it has one.
    pub fn within_home(&self, pos: BlockPos) -> bool {
        self.home.is_none_or(|home| home.contains(pos))
    }

    /// Whether the point `at` lies within its home, if it has one.
    pub fn within_home_point(&self, at: [f64; 3]) -> bool {
        self.home.is_none_or(|home| home.contains_point(at))
    }

    /// Whether a search reaching `horizontal` blocks around it may leave
    /// its home, so candidates must be checked against it.
    pub fn home_restricts(&self, horizontal: f64) -> bool {
        self.home.is_some_and(|home| {
            let reach = f64::from(home.radius) + horizontal + 1.0;
            tuff_core::world::distance_sq(home.pos.center(), self.base.position) < reach * reach
        })
    }

    pub fn attribute_base(&self, attribute: Attribute) -> f64 {
        self.attribute_bases
            .iter()
            .find(|(a, _)| *a == attribute)
            .map(|(_, v)| *v)
            .or_else(|| self.base.kind.base_attribute(attribute))
            .unwrap_or_else(|| attribute.range().default)
    }

    /// An attribute's value: its base with the equipment's, the species'
    /// and the active effects' modifiers (Speed, Strength, ...).
    pub fn attribute(&self, attribute: Attribute) -> f64 {
        let base = self.attribute_base(attribute);
        if self.equipment.is_empty() && self.modifiers.is_empty() && self.effects.is_empty() {
            return attribute.sanitize(base);
        }
        let worn = self.equipment.worn();
        let extra: Vec<(&str, f64, tuff_vanilla_data::item::ModifierOperation)> = self
            .modifiers
            .iter()
            .filter(|m| m.attribute == attribute)
            .map(|m| (m.id.as_ref(), m.amount, m.operation))
            .collect();
        let levels = self.effect_levels();
        combat::Modifiers {
            equipment: &worn,
            effects: &levels,
        }
        .value_from(attribute, base, &extra)
    }

    /// The active effects and their amplifiers, for attribute values.
    fn effect_levels(&self) -> Vec<(MobEffect, u8)> {
        self.effects
            .iter()
            .map(|effect| (effect.effect, effect.amplifier))
            .collect()
    }

    /// Adds or replaces the species modifier `id` on `attribute`; it is
    /// not saved.
    pub fn set_modifier(
        &mut self,
        attribute: Attribute,
        id: &'static str,
        amount: Option<(f64, tuff_vanilla_data::item::ModifierOperation)>,
    ) {
        self.put_modifier(attribute, id.into(), amount, false);
    }

    /// Like [`Self::set_modifier`], but the modifier is saved with the mob.
    pub fn set_permanent_modifier(
        &mut self,
        attribute: Attribute,
        id: &'static str,
        amount: Option<(f64, tuff_vanilla_data::item::ModifierOperation)>,
    ) {
        self.put_modifier(attribute, id.into(), amount, true);
    }

    pub fn put_modifier(
        &mut self,
        attribute: Attribute,
        id: std::borrow::Cow<'static, str>,
        amount: Option<(f64, tuff_vanilla_data::item::ModifierOperation)>,
        permanent: bool,
    ) {
        self.modifiers
            .retain(|m| !(m.attribute == attribute && m.id == id));
        if let Some((amount, operation)) = amount {
            self.modifiers.push(SpeciesModifier {
                attribute,
                id,
                amount,
                operation,
                permanent,
            });
        }
    }

    /// The amount of the modifier `id` on `attribute`, if it has one.
    pub fn modifier_amount(&self, attribute: Attribute, id: &str) -> Option<f64> {
        self.modifiers
            .iter()
            .find(|m| m.attribute == attribute && m.id == id)
            .map(|m| m.amount)
    }

    /// The attributes the client syncs for this mob, with their values.
    pub fn synced_attributes(&self) -> Vec<(Attribute, f64)> {
        Attribute::ALL
            .into_iter()
            .filter(|a| {
                !tuff_core::world::attributes::NOT_CLIENT_SYNCED.contains(a)
                    && (self.base.kind.base_attribute(*a).is_some()
                        || self.attribute_bases.iter().any(|(b, _)| b == a))
            })
            .map(|a| (a, self.attribute(a)))
            .collect()
    }

    /// `/attribute` on this mob.
    pub fn edit_attribute(
        &mut self,
        command: &tuff_core::world::messages::AttributeCommand,
    ) -> tuff_core::world::messages::AttributeAnswer {
        use tuff_core::world::messages::{AttributeAnswer, AttributeOp};
        let attribute = command.attribute;
        let has = self.base.kind.base_attribute(attribute).is_some()
            || self.attribute_bases.iter().any(|(a, _)| *a == attribute);
        if !has {
            return AttributeAnswer::NoAttribute;
        }
        let present = |this: &Self, id: &str| {
            this.modifiers
                .iter()
                .any(|m| m.attribute == attribute && m.id == id)
        };
        match &command.op {
            AttributeOp::Value => AttributeAnswer::Number(self.attribute(attribute)),
            AttributeOp::Base => AttributeAnswer::Number(self.attribute_base(attribute)),
            AttributeOp::SetBase(value) => {
                self.set_attribute_base(attribute, *value);
                self.clamp_health_to_max();
                AttributeAnswer::Done
            }
            AttributeOp::ResetBase => {
                self.attribute_bases.retain(|(a, _)| *a != attribute);
                self.clamp_health_to_max();
                AttributeAnswer::Number(self.attribute_base(attribute))
            }
            AttributeOp::AddModifier {
                id,
                amount,
                operation,
            } => {
                if present(self, id) {
                    return AttributeAnswer::DuplicateModifier;
                }
                self.put_modifier(
                    attribute,
                    std::borrow::Cow::Owned(id.clone()),
                    Some((*amount, *operation)),
                    true,
                );
                self.clamp_health_to_max();
                AttributeAnswer::Done
            }
            AttributeOp::RemoveModifier(id) => {
                if !present(self, id) {
                    return AttributeAnswer::NoModifier;
                }
                self.put_modifier(attribute, std::borrow::Cow::Owned(id.clone()), None, true);
                self.clamp_health_to_max();
                AttributeAnswer::Done
            }
            AttributeOp::ModifierValue(id) => self
                .modifier_amount(attribute, id)
                .map_or(AttributeAnswer::NoModifier, AttributeAnswer::Number),
        }
    }

    fn clamp_health_to_max(&mut self) {
        let max = self.max_health();
        if self.health > max {
            self.health = max;
        }
    }

    pub fn set_attribute_base(&mut self, attribute: Attribute, value: f64) {
        self.attribute_bases.retain(|(a, _)| *a != attribute);
        self.attribute_bases.push((attribute, value));
    }

    pub fn max_health(&self) -> f32 {
        self.attribute(Attribute::MaxHealth) as f32
    }

    /// Regeneration's periodic heal; a no-op once dead or at full health.
    pub fn heal(&mut self, amount: f32) {
        if amount > 0.0 && self.is_alive() && self.health < self.max_health() {
            self.health = (self.health + amount).min(self.max_health());
            self.changed(data::HEALTH);
        }
    }

    pub fn max_up_step(&self) -> f32 {
        self.attribute(Attribute::StepHeight) as f32
    }

    /// This mob as advancement predicates see it.
    pub fn facts(&self) -> tuff_core::world::predicate::EntityFacts {
        use tuff_core::world::predicate::{EntityFacts, Flags};
        use tuff_vanilla_data::item::EquipmentSlot;
        let equipment = [
            EquipmentSlot::Mainhand,
            EquipmentSlot::Offhand,
            EquipmentSlot::Feet,
            EquipmentSlot::Legs,
            EquipmentSlot::Chest,
            EquipmentSlot::Head,
            EquipmentSlot::Body,
        ]
        .into_iter()
        .filter_map(|slot| Some((slot, ItemSnapshot::of(self.equipment.get(slot)?))))
        .collect();
        EntityFacts {
            kind: Some(self.base.kind),
            position: self.base.position,
            flags: Flags {
                baby: self.is_baby(),
                on_fire: self.base.on_fire(),
                on_ground: self.base.on_ground,
                ..Flags::default()
            },
            equipment,
            ..EntityFacts::default()
        }
    }

    pub fn is_baby(&self) -> bool {
        self.age.as_ref().map_or(self.baby, Age::is_baby)
    }

    pub fn is_alive(&self) -> bool {
        !self.dead && self.health > 0.0
    }

    pub fn bounds(&self) -> Aabb {
        let [x, y, z] = self.base.position;
        if let Some(offset) = self.box_offset {
            return [
                x + offset[0],
                y + offset[1],
                z + offset[2],
                x + offset[3],
                y + offset[4],
                z + offset[5],
            ];
        }
        let half = self.size.width as f64 / 2.0;
        [
            x - half,
            y,
            z - half,
            x + half,
            y + self.size.height as f64,
            z + half,
        ]
    }

    pub fn eye_position(&self) -> [f64; 3] {
        let [x, y, z] = self.base.position;
        [x, y + self.size.eye_height as f64, z]
    }

    pub fn in_liquid(&self) -> bool {
        self.body.in_water || self.body.lava_height > 0.0
    }

    pub fn in_lava(&self) -> bool {
        self.body.lava_height > 0.0
    }

    pub fn in_floatable_fluid(&self) -> bool {
        self.body.water_height > 0.0
    }

    pub fn fluid_jump_threshold(&self) -> f64 {
        if (self.size.eye_height as f64) < 0.4 {
            0.0
        } else {
            0.4
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.body.speed = speed;
        self.body.zza = speed;
    }

    pub fn path_malus(&self, kind: PathType) -> f32 {
        self.path_malus
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or_else(|| kind.malus(), |(_, m)| *m)
    }

    pub fn set_path_malus(&mut self, kind: PathType, malus: f32) {
        self.path_malus.retain(|(k, _)| *k != kind);
        self.path_malus.push((kind, malus));
    }

    /// The longest fall pathfinding may plan.
    pub fn max_fall_distance(&self) -> i32 {
        if self.fall_by_health && self.target.is_some() {
            return (self.health - 1.0 + 3.0).floor() as i32;
        }
        let safe = self.attribute(Attribute::SafeFallDistance);
        (safe + 0.0).ceil() as i32
    }

    pub fn distance_sq(&self, to: [f64; 3]) -> f64 {
        tuff_core::world::distance_sq(self.base.position, to)
    }

    /// Damage from its own doing (a water-sensitive mob in rain), taken at
    /// the start of its next tick.
    pub fn hurt_later(&mut self, amount: f32, kind: &'static DamageType) {
        self.pending_hurts.push((amount, kind));
    }

    pub fn set_sleeping(&mut self, sleeping: bool) {
        if self.sleeping != sleeping {
            self.sleeping = sleeping;
            self.changed(play::metadata::POSE);
        }
    }

    pub fn set_croaking(&mut self, croaking: bool) {
        if self.croaking != croaking {
            self.croaking = croaking;
            self.changed(play::metadata::POSE);
        }
    }

    pub fn set_tongue_target(&mut self, target: Option<i32>) {
        if self.tongue_target != target {
            self.tongue_target = target;
            self.changed(data::SPECIES + 1);
        }
    }

    pub fn set_pose(&mut self, pose: Option<tuff_vanilla_data::entity::Pose>) {
        if self.pose != pose {
            self.pose = pose;
            self.changed(play::metadata::POSE);
        }
    }

    /// Marks entity data index `index` as changed for the viewers.
    pub fn changed(&mut self, index: u8) {
        if !self.changed.contains(&index) {
            self.changed.push(index);
        }
    }

    /// Recomputes the size after growing up or down.
    pub fn refresh_size(&mut self) {
        let size = if self.is_baby() && !self.baby_keeps_size {
            self.base.kind.baby_size()
        } else {
            self.base.kind.size()
        };
        let scale = self.size_scale;
        self.size = Size {
            width: size.width * scale,
            height: size.height * scale,
            eye_height: size.eye_height * scale,
            ..size
        };
    }

    pub fn is_panicking(&self) -> bool {
        self.panicking
    }

    /// How much bigger than its type's usual size it is (a baby is
    /// smaller), which scales where it sits and seats others.
    pub fn size_ratio(&self) -> f64 {
        let usual = self.base.kind.size().height;
        if usual > 0.0 {
            f64::from(self.size.height / usual)
        } else {
            1.0
        }
    }

    pub(super) fn update_control_flags(&mut self) {
        self.goals_steered = self.steered;
        let mut flags = Flags::MOVE | Flags::JUMP | Flags::LOOK;
        if self.base.kind == EntityType::Ravager {
            flags = flags | Flags::TARGET;
        }
        self.goals.set_disabled(flags, self.steered);
    }

    fn mob_flags(&self) -> i8 {
        let mut flags = 0;
        if self.no_ai {
            flags |= MOB_FLAG_NO_AI;
        }
        if self.left_handed {
            flags |= MOB_FLAG_LEFT_HANDED;
        }
        if self.aggressive {
            flags |= MOB_FLAG_AGGRESSIVE;
        }
        flags
    }

    /// Gives it a lasting effect, which viewers learn of on its next tick.
    pub fn add_effect(&mut self, effect: effects::ActiveEffect) -> bool {
        if !self.can_be_affected(effect.effect) {
            return false;
        }
        match self.effects.add(effect) {
            Some(change) => {
                self.pending_effects.push(change);
                true
            }
            None => false,
        }
    }

    /// Whether its type lets `effect` apply: the tagged immunities, and the
    /// wither skeleton, parched and spiders' own.
    pub fn can_be_affected(&self, effect: tuff_vanilla_data::registry::MobEffect) -> bool {
        use tuff_vanilla_data::registry::{EntityType, MobEffect};
        let kind = self.kind();
        if tuff_core::world::entity_in_tag(kind, "immune_to_infested") {
            return effect != MobEffect::Infested;
        }
        if tuff_core::world::entity_in_tag(kind, "immune_to_oozing") {
            return effect != MobEffect::Oozing;
        }
        if tuff_core::world::entity_in_tag(kind, "ignores_poison_and_regen")
            && matches!(effect, MobEffect::Regeneration | MobEffect::Poison)
        {
            return false;
        }
        match kind {
            EntityType::WitherSkeleton => effect != MobEffect::Wither,
            EntityType::Parched => effect != MobEffect::Weakness,
            EntityType::Spider | EntityType::CaveSpider => effect != MobEffect::Poison,
            _ => true,
        }
    }

    pub fn set_aggressive(&mut self, aggressive: bool) {
        if self.aggressive != aggressive {
            self.aggressive = aggressive;
            self.changed(data::MOB_FLAGS);
        }
    }

    pub(crate) fn sound_category(&self) -> i32 {
        if self.hostile {
            play::sound_category::HOSTILE
        } else {
            play::sound_category::NEUTRAL
        }
    }

    /// The common entity data from the living entity down, full. The baby
    /// and age lock entries are only shared by ageable types; everywhere
    /// else those indices belong to the species.
    pub(super) fn core_metadata(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            play::metadata::AIR_SUPPLY => MetadataValue::VarInt(self.base.air.into()),
            play::metadata::TICKS_FROZEN => MetadataValue::VarInt(self.base.ticks_frozen),
            play::metadata::POSE => {
                let pose = if let Some(pose) = self.pose.filter(|_| !self.dead) {
                    pose
                } else if self.dead {
                    tuff_vanilla_data::entity::Pose::Dying
                } else if self.sleeping {
                    tuff_vanilla_data::entity::Pose::Sleeping
                } else if self.swimming {
                    tuff_vanilla_data::entity::Pose::Swimming
                } else if self.croaking {
                    tuff_vanilla_data::entity::Pose::Croaking
                } else {
                    tuff_vanilla_data::entity::Pose::Standing
                };
                MetadataValue::Pose(i32::from(pose.id()))
            }
            data::LIVING_FLAGS => MetadataValue::Byte(i8::from(self.using_item.is_some())),
            data::HEALTH => MetadataValue::Float(self.health),
            play::metadata::ARROW_COUNT => MetadataValue::VarInt(self.stuck_arrows.count),
            data::MOB_FLAGS if !self.not_a_mob => MetadataValue::Byte(self.mob_flags()),
            data::BABY if self.age.is_some() => MetadataValue::Boolean(self.is_baby()),
            data::AGE_LOCKED => MetadataValue::Boolean(self.age.as_ref()?.locked),
            _ => return None,
        })
    }

    pub fn sound(&self, sound: SoundEvent, volume: f32, pitch: f32, cx: &mut TickContext) {
        if !self.base.silent {
            self.sound_as(sound, self.sound_category(), volume, pitch, cx);
        }
    }

    /// A sound at the mob in `category`, for everyone nearby.
    pub fn sound_as(
        &self,
        sound: SoundEvent,
        category: i32,
        volume: f32,
        pitch: f32,
        cx: &mut TickContext,
    ) {
        let packet = framed(&play::Sound {
            sound_event: sound.id(),
            category,
            position: self.base.position,
            volume,
            pitch,
            seed: cx.rng.r#gen(),
        });
        cx.effects.push(Effect::NearbyPackets {
            center: self.base.position,
            range: 16.0 * f64::from(volume.max(1.0)),
            packets: packets([packet]),
        });
    }

    /// How much a wandering mob likes `pos`: animals like grass under
    /// them, then light; monsters like the dark; others do not care.
    pub fn walk_target_value(&self, pos: BlockPos, cx: &TickContext) -> f32 {
        if let Some(value) = self.walk_value {
            return value(self, pos, cx);
        }
        if self.hostile {
            return self.monster_walk_target_value(pos, cx);
        }
        if self.love.is_none() {
            return 0.0;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if (cx.block)(below).is_some_and(|s| s.block() == tuff_vanilla_data::Block::GrassBlock) {
            return 10.0;
        }
        let (sky, block) = (cx.light)(pos).unwrap_or((0, 0));
        let raw = (sky.saturating_sub(cx.sky_darken)).max(block) as f32;
        let v = raw / 15.0;
        v / (4.0 - 3.0 * v) - 0.5
    }

    pub fn voice_pitch(&self, rng: &mut StdRng) -> f32 {
        voice_pitch_for(self.base.kind, self.is_baby(), rng)
    }

    pub fn entity_event(&self, event: i8, cx: &mut TickContext) {
        let packet = framed(&play::EntityEvent {
            entity_id: self.base.id,
            status: event,
        });
        cx.effects.push(Effect::EntityPackets(packets([packet])));
    }

    pub fn drop_item(&self, stack: ItemStack, offset_y: f64, cx: &mut TickContext) {
        let [x, y, z] = self.base.position;
        let velocity = [
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
            0.2,
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
        ];
        cx.effects.push(Effect::DropItem {
            stack,
            position: [x, y + offset_y, z],
            velocity,
            pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
        });
    }

    pub(super) fn summary(&self) -> LivingSummary {
        LivingSummary {
            alive: self.is_alive(),
            target: self.target,
            age: self.age.as_ref().map_or(-i32::from(self.baby), |a| a.age),
            in_love: self.love.as_ref().is_some_and(Love::is_in_love) || self.breedable,
            panicking: self.panicking,
            eye_height: self.size.eye_height,
            in_water: self.body.in_water,
            seen_as_enemy: self.seen_as_enemy,
            enemy: self.hostile,
            health: self.health,
            tamed: self.tame.as_ref().is_some_and(|t| t.is_tamed()),
            owner: self.tame.as_ref().and_then(|t| t.owner),
            chasing: self.chasing,
            unpushable: self.unpushable || self.pushed_off,
        }
    }
}

pub fn voice_pitch_for(kind: EntityType, baby: bool, rng: &mut StdRng) -> f32 {
    let jitter = (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2;
    match kind {
        EntityType::HappyGhast => 1.0,
        EntityType::Bat => (jitter + 1.0) * 0.95,
        EntityType::ZombieVillager if baby => jitter + 2.0,
        _ if baby => jitter + 1.5,
        _ => jitter + 1.0,
    }
}

#[cfg(test)]
mod voice_pitch_tests {
    use super::*;
    use rand::SeedableRng;

    fn range(kind: EntityType, baby: bool) -> (f32, f32) {
        let mut rng = StdRng::seed_from_u64(7);
        let samples: Vec<f32> = (0..2000)
            .map(|_| voice_pitch_for(kind, baby, &mut rng))
            .collect();
        let lo = samples.iter().cloned().fold(f32::MAX, f32::min);
        let hi = samples.iter().cloned().fold(f32::MIN, f32::max);
        (lo, hi)
    }

    #[tuff_core::watchdog]
    #[test]
    fn adult_and_baby_ranges_match_vanilla() {
        let (lo, hi) = range(EntityType::Pig, false);
        assert!(lo >= 0.8 && hi <= 1.2 && lo < 0.95 && hi > 1.05);
        let (lo, hi) = range(EntityType::Pig, true);
        assert!(lo >= 1.3 && hi <= 1.7 && lo < 1.45 && hi > 1.55);
        let (lo, hi) = range(EntityType::ZombieVillager, true);
        assert!(lo >= 1.8 && hi <= 2.2);
        let (lo, hi) = range(EntityType::Bat, false);
        assert!(lo >= 0.76 && hi <= 1.14);
        assert_eq!(range(EntityType::HappyGhast, true), (1.0, 1.0));
    }
}

/// The server's own run of a mount whose driver's client moves it: the
/// driver's reported positions are held within `SHADOW_RANGE` of it.
#[derive(Debug, Clone, Copy)]
pub struct DrivenShadow {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
    pub fall_distance: f64,
}

impl MobCore {
    /// Starts or keeps the shadow and swaps it with the live state, so the
    /// mob simulates from the shadow. Called again to swap back.
    pub fn swap_shadow(&mut self) {
        let shadow = self.shadow.get_or_insert(DrivenShadow {
            position: self.base.position,
            velocity: [0.0; 3],
            on_ground: self.base.on_ground,
            fall_distance: self.base.fall_distance,
        });
        std::mem::swap(&mut self.base.position, &mut shadow.position);
        std::mem::swap(&mut self.base.velocity, &mut shadow.velocity);
        std::mem::swap(&mut self.base.on_ground, &mut shadow.on_ground);
        std::mem::swap(&mut self.base.fall_distance, &mut shadow.fall_distance);
    }

    /// Drops a shadow the live position has left behind (the mob was moved
    /// by something other than its driver).
    pub fn resync_shadow(&mut self) {
        let position = self.base.position;
        if self
            .shadow
            .is_some_and(|s| distance(s.position, position) > SHADOW_RANGE + 0.5)
        {
            self.shadow = None;
        }
    }

    /// Holds a driver-reported position within range of the shadow.
    pub fn clamp_to_shadow(&mut self, reported: [f64; 3]) -> [f64; 3] {
        let Some(shadow) = self.shadow.as_mut() else {
            return reported;
        };
        shadow::hold(&mut shadow.position, reported)
    }
}

//! Mobs: living region entities with health, physics, goal-driven AI and
//! pathfinding. [`MobCore`] is the state every mob shares (the game's
//! living entity, mob and pathfinder mob layers, plus the optional ageable
//! and animal parts); a [`Species`] adds a type's own state and behaviour,
//! and [`Mob`] joins the two into a [`RegionEntity`], so new mob types
//! (vanilla or feature) are one `Species` impl and a registry entry.

pub mod allay;
pub mod animal;
pub mod armadillo;
pub mod armor_stand;
pub mod attack;
pub mod axolotl;
pub mod bat;
pub mod bee;
pub mod blaze;
pub mod brain_debug;
pub mod breeze;
pub mod calendar;
pub mod camel;
pub mod cat;
pub mod control;
pub mod creaking;
pub mod creeper;
pub mod crowd;
pub mod death_effects;
pub mod debugmobspawning_command;
pub mod dolphin;
pub mod door;
pub mod drowned;
pub mod enderman;
pub mod equine;
pub mod equipment;
pub mod evoker;
pub mod fangs;
pub mod farm;
pub mod fish;
pub mod fox;
pub mod frog;
pub mod ghast;
pub mod giant;
pub mod goal;
pub mod goals;
pub mod goat_ram;
pub mod golem;
pub mod guardian;
pub mod hive;
pub mod hoglin;
pub mod illager;
pub mod illusioner;
pub mod leash;
pub mod llama;
pub mod llama_spit;
pub mod long_jump;
pub mod mannequin;
pub mod merchant_events;
pub mod monster;
pub mod mount;
pub mod movement;
pub mod nautilus;
pub mod navigation;
pub mod ocelot;
pub mod pack_variants;
pub mod panda;
pub mod parrot;
pub mod path;
pub mod phantom;
pub mod physics;
pub mod piglin;
pub mod pillager;
pub mod polar_bear;
pub mod rabbit;
pub mod raid;
pub mod raid_command;
pub mod raider;
pub mod random_pos;
pub mod ranged;
pub mod ravager;
pub mod school;
pub mod shulker;
pub mod shulker_bullet;
pub mod silverfish;
pub mod skeleton;
pub mod skeleton_trap;
pub mod slime;
pub mod sniffer;
pub mod spawning;
pub mod spear;
pub mod spider;
pub mod squid;
pub mod steering;
pub mod strider;
pub mod sulfur_cube;
pub mod sweep;
pub mod tadpole;
pub mod target;
pub mod turtle;
pub mod vex;
pub mod village_walk;
pub mod villager;
pub mod vindicator;
pub mod voiced_variant;
pub mod wandering_trader;
pub mod warden;
pub mod witch;
pub mod wither;
pub mod wolf;
pub mod world;
pub mod zombie;
pub mod zombie_villager;

use std::fmt::Debug;

use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::world::entity::EntityHit;
use tuff_item::item::ItemStack;
use tuff_vanilla_common::combat;

mod core;
mod instance;

pub use core::*;
pub use instance::*;

/// Entity data indices shared by every mob; tests check them against the
/// dumped layouts.
pub mod data {
    pub const LIVING_FLAGS: u8 = 8;
    pub const HEALTH: u8 = 9;
    pub const MOB_FLAGS: u8 = 10 + 5;
    pub const BABY: u8 = 16;
    pub const AGE_LOCKED: u8 = 17;
    /// The first index a species' own data uses.
    pub const SPECIES: u8 = 18;
}

pub mod event {
    pub const DEATH: i8 = 3;
    pub const EAT_GRASS: i8 = 10;
    pub const IN_LOVE: i8 = 18;
    pub const POOF: i8 = 60;
    pub const DROWN_PARTICLES: i8 = tuff_core::protocol::play::entity_event::DROWN_PARTICLES;
    /// A tameable animal's interaction, shared by every species that uses
    /// [`MobCore::try_tame`]: smoke particles on failure, hearts on success.
    pub const TAME_FAILED: i8 = 6;
    pub const TAME_SUCCEEDED: i8 = 7;
}

pub(crate) const MOB_FLAG_NO_AI: i8 = 0x01;
pub(crate) const MOB_FLAG_LEFT_HANDED: i8 = 0x02;
pub(crate) const MOB_FLAG_AGGRESSIVE: i8 = 0x04;
pub(crate) const DEATH_TICKS: i16 = 20;
/// Air gained back each tick out of water.
pub(crate) const AIR_REFILL: i16 = 4;
/// The air at which a submerged mob takes drowning damage and gets a
/// little breath back.
pub(crate) const DROWNING_AIR: i16 = -20;
pub(crate) const HURT_DURATION: i16 = 10;
pub(crate) const DAMAGE_COOLDOWN: i32 = 20;
pub(crate) const PLAYER_MEMORY_TICKS: i32 = 100;
pub(crate) const DEFAULT_AMBIENT_INTERVAL: i32 = 80;
pub(crate) const NO_ACTION_DESPAWN_TICKS: i32 = 600;

/// A species' own breathing rule, in place of the common under-water one
/// (see [`Species::air_rules`] and [`Mob::breathe`]).
#[derive(Debug, Clone, Copy)]
pub struct OwnAirRules {
    /// Air while wet; also what it refills to.
    pub max: i16,
    /// Rain counts as wet too (axolotls), not just being in water.
    pub in_rain: bool,
    pub damage_kind: &'static DamageType,
    /// Damage when air runs out; a rule that does none never loses air.
    pub damage: f32,
}

impl OwnAirRules {
    /// Air stays full anywhere: iron golems, and happy ghasts.
    pub const NEVER_LOSES_AIR: Self = Self {
        max: tuff_vanilla_data::entity::MAX_AIR_SUPPLY as i16,
        in_rain: false,
        damage_kind: &tuff_vanilla_data::damage_type::DROWN,
        damage: 0.0,
    };
}

/// The sounds a mob makes, and how loud.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobSounds {
    pub ambient: Option<SoundEvent>,
    pub hurt: Option<SoundEvent>,
    pub death: Option<SoundEvent>,
    pub step: Option<SoundEvent>,
    pub volume: f32,
}

/// What hurt a mob.
#[derive(Debug, Clone)]
pub struct Damage {
    pub kind: &'static DamageType,
    /// The player responsible, by entity id and UUID.
    pub player: Option<(i32, u128)>,
    /// The entity responsible (a player too), by id and type.
    pub attacker: Option<(i32, EntityType)>,
    /// The head drop of a charged creeper attacker.
    pub attacker_charged: Option<tuff_core::world::entity::HeadDrop>,
    /// The name of the entity responsible, for death messages.
    pub attacker_name: Option<tuff_item::text::TextComponent>,
    /// The projectile that hit, if it was one.
    pub projectile: Option<EntityType>,
    pub projectile_id: Option<i32>,
    pub position: Option<[f64; 3]>,
    pub weapon: Option<ItemStack>,
    /// Where the responsible entity's eyes were and where it looked, when
    /// known (a player's swing).
    pub aim: Option<Aim>,
    /// The projectile that hit was burning.
    pub burning_projectile: bool,
}

/// An attacker's eye position and look direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aim {
    pub eye: [f64; 3],
    pub look: [f64; 3],
}

/// A push a hit gives: `power` along `(dx, dz)` pointing from the victim
/// toward the source, as the common knockback takes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Knockback {
    pub power: f64,
    pub dx: f64,
    pub dz: f64,
    /// The damage of the hit.
    pub damage: f32,
    /// An attacker's extra knockback (sprinting, the enchantment) rather
    /// than the hit's own push.
    pub extra: bool,
}

impl Damage {
    pub fn of(kind: &'static DamageType) -> Self {
        Self {
            kind,
            player: None,
            attacker: None,
            attacker_charged: None,
            attacker_name: None,
            projectile: None,
            projectile_id: None,
            position: None,
            weapon: None,
            aim: None,
            burning_projectile: false,
        }
    }

    /// What enchantment effects see of this damage to a `victim`.
    pub fn context(&self, victim: EntityType) -> combat::DamageContext<'static> {
        let attacker = self.attacker.map(|(_, kind)| kind);
        combat::DamageContext {
            kind: self.kind,
            victim,
            attacker,
            direct_attacker: self.projectile.or(attacker),
            is_direct: self.projectile.is_none(),
            direct_attacker_fall_distance: None,
            direct_attacker_flying: None,
        }
    }

    /// Damage from a mob's or projectile's hit.
    pub fn from_hit(hit: &EntityHit) -> Self {
        let attacker = hit.attacker.as_ref();
        Self {
            kind: hit.kind,
            player: attacker.filter(|a| a.is_player()).map(|a| (a.id, a.uuid)),
            attacker: attacker.map(|a| (a.id, a.kind)),
            attacker_charged: attacker.and_then(|a| a.charged.clone()),
            attacker_name: attacker.map(|a| a.name.clone()),
            projectile: hit.projectile,
            projectile_id: hit.projectile_id,
            position: hit.position,
            weapon: hit.weapon.clone(),
            aim: None,
            burning_projectile: hit.projectile.is_some() && hit.ignite > 0.0,
        }
    }

    pub fn is(&self, tag: &str) -> bool {
        tuff_core::world::damage_type_in_tag(self.kind, &format!("minecraft:{tag}"))
    }
}

/// Registers every vanilla mob species (and its spawn rule) into core.
pub struct MobsFeature;

impl tuff_core::feature::Feature for MobsFeature {
    fn id(&self) -> &'static str {
        "vanilla-mobs"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let entities = &mut cx.registries().entity_types;
            crate::farm::register(entities);
            crate::zombie::register(entities);
            crate::villager::register(entities);
            crate::zombie_villager::register(entities);
            crate::skeleton::register(entities);
            crate::creaking::register(entities);
            crate::creeper::register(entities);
            crate::spider::register(entities);
            crate::enderman::register(entities);
            crate::blaze::register(entities);
            crate::shulker::register(entities);
            crate::breeze::register(entities);
            crate::piglin::register(entities);
            crate::ghast::register(entities);
            crate::golem::register(entities);
            crate::slime::register(entities);
            crate::sulfur_cube::register(entities);
            crate::hoglin::register(entities);
            crate::silverfish::register(entities);
            crate::giant::register(entities);
            crate::witch::register(entities);
            crate::fish::register(entities);
            crate::bat::register(entities);
            crate::squid::register(entities);
            crate::polar_bear::register(entities);
            crate::dolphin::register(entities);
            crate::strider::register(entities);
            crate::equine::register(entities);
            crate::llama::register(entities);
            crate::leash::register(entities);
            crate::wandering_trader::register(entities);
            crate::evoker::register(entities);
            crate::vex::register(entities);
            crate::vindicator::register(entities);
            crate::pillager::register(entities);
            crate::ravager::register(entities);
            crate::illusioner::register(entities);
            crate::guardian::register(entities);
            crate::phantom::register(entities);
            crate::wolf::register(entities);
            crate::cat::register(entities);
            crate::bee::register(entities);
            crate::turtle::register(entities);
            crate::ocelot::register(entities);
            crate::parrot::register(entities);
            crate::allay::register(entities);
            crate::fox::register(entities);
            crate::panda::register(entities);
            crate::frog::register(entities);
            crate::armadillo::register(entities);
            crate::sniffer::register(entities);
            crate::camel::register(entities);
            crate::warden::register(entities);
            crate::nautilus::register(entities);
            crate::wither::register(entities);
            crate::mannequin::register(entities);
            crate::armor_stand::register(entities);
            cx.registries()
                .config_keys
                .extend(world::region::spawning::cap_keys());
            cx.registries().config_keys.extend(crowd::config_keys());
            cx.registries()
                .region_hooks
                .push(std::sync::Arc::new(MobRegionHooks));
            cx.registries()
                .services
                .push(std::sync::Arc::new(world::raid_store::RaidStoreFactory));
            cx.registries()
                .services
                .push(std::sync::Arc::new(world::trader_store::TraderStoreFactory));
            cx.registries()
                .services
                .push(std::sync::Arc::new(world::spawn_census::SpawnCensusFactory));
        }
        Ok(())
    }
}

/// The entity type a spawn egg stack hatches: its `entity_data`
/// component's id, else the item's default.
fn egg_entity_type(
    stack: &tuff_item::item::ItemStack,
) -> Option<tuff_vanilla_data::registry::EntityType> {
    match stack.components.other.get("minecraft:entity_data") {
        Some(tuff_nbt::Tag::Compound(data)) => data
            .get_str("id")
            .and_then(tuff_vanilla_data::registry::EntityType::from_name),
        Some(_) => None,
        None => stack.item.spawns_entity(),
    }
}

/// How far a box one block above `target` falls onto what the block
/// holds, at most a block (zero or negative).
fn settle_drop(
    block: tuff_core::world::physics::BlockLookup<'_>,
    target: tuff_world_model::BlockPos,
    bounds: [f64; 6],
) -> f64 {
    let cell = [
        f64::from(target.x),
        f64::from(target.y),
        f64::from(target.z),
        f64::from(target.x) + 1.0,
        f64::from(target.y) + 1.0,
        f64::from(target.z) + 1.0,
    ];
    let shapes = tuff_core::world::physics::colliders(cell, block);
    tuff_core::world::physics::collide_with_shapes([0.0, -1.0, 0.0], bounds, &shapes)[1]
}

/// The `entity_data` compound laid over a new entity's saved form.
fn merge_entity_data(
    world: &mut dyn tuff_core::world::region_api::RegionWorld,
    entity: Box<dyn tuff_core::world::entity::RegionEntity>,
    data: &tuff_nbt::Compound,
) -> Box<dyn tuff_core::world::entity::RegionEntity> {
    let mut saved = tuff_core::world::entity::save(entity.as_ref());
    let id = entity.base().id;
    for (key, value) in data.iter() {
        if !matches!(key, "id" | "Pos" | "UUID") {
            saved.insert(key, value.clone());
        }
    }
    world
        .shared()
        .entity_types
        .load(&saved, id)
        .unwrap_or(entity)
}

/// A dispenser hatches a spawn egg's mob in the block in front of it.
fn dispense_spawn_egg(
    world: &mut dyn tuff_core::world::region_api::RegionWorld,
    pos: tuff_world_model::BlockPos,
    facing: tuff_vanilla_data::block::Facing,
    stack: &tuff_item::item::ItemStack,
) -> Option<tuff_core::world::region_api::DispenseOutcome> {
    use tuff_core::world::region_api::DispenseOutcome;
    let kind = egg_entity_type(stack)?;
    let (sx, sy, sz) = facing.step();
    let target = tuff_world_model::BlockPos::new(pos.x + sx, pos.y + sy, pos.z + sz);
    let mut position = [
        f64::from(target.x) + 0.5,
        f64::from(target.y),
        f64::from(target.z) + 0.5,
    ];
    if !world.owns_chunk(tuff_world_model::pos::ChunkPos::of_position(position)) {
        return Some(DispenseOutcome::Kept);
    }
    let id = world.shared().next_entity_id();
    let yaw = rand::Rng::gen_range(world.rng(), 0.0..360.0);
    let Some(mut entity) = world.shared().entity_types.create(kind, id, position, yaw) else {
        return Some(DispenseOutcome::Kept);
    };
    if facing != tuff_vanilla_data::block::Facing::Up {
        entity.base_mut().position = [position[0], position[1] + 1.0, position[2]];
        let bounds = entity.bounds();
        position[1] += 1.0 + settle_drop(&|p| world.block(p), target, bounds);
        entity.base_mut().position = position;
    }
    apply_egg_components(&mut entity, &stack.components);
    if let Some(tuff_nbt::Tag::Compound(data)) = stack.components.other.get("minecraft:entity_data")
    {
        entity = merge_entity_data(world, entity, data);
    }
    world.finalize_spawn(&mut entity, 1);
    world.add_entity(entity);
    world.game_event(
        tuff_core::world::game_event::GameEvent::EntityPlace,
        position,
        tuff_core::world::game_event::Context::default(),
    );
    Some(DispenseOutcome::Dispensed)
}

/// What a spawn egg's stack gives the mob it hatches: its custom name, and
/// the components the mob takes on (a mannequin's profile).
pub fn apply_egg_components(
    entity: &mut Box<dyn tuff_core::world::entity::RegionEntity>,
    components: &tuff_item::item::Components,
) {
    if let Some(name) = &components.custom_name {
        entity.base_mut().custom_name = Some(name.to_nbt());
    }
    if let Some(color) = components
        .other
        .get_str("minecraft:shulker/color")
        .and_then(tuff_vanilla_data::item::DyeColor::from_name)
        && let Some(mob) = entity.downcast_mut::<Mob<shulker::Shulker>>()
    {
        mob.species.set_color(Some(color.id() as u8));
    }
    if let Some(profile) = components.other.get("minecraft:profile")
        && let Some(mob) = entity.downcast_mut::<Mob<mannequin::Mannequin>>()
    {
        mob.species.take_profile(&mut mob.core, profile);
    }
}

/// Mobs' part of every region tick: natural spawning, spawner blocks,
/// phantoms, and sculk shriekers hearing disturbances.
struct MobRegionHooks;

impl tuff_core::world::region_api::RegionHooks for MobRegionHooks {
    fn dispense(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        pos: tuff_world_model::BlockPos,
        facing: tuff_vanilla_data::block::Facing,
        _origin: [f64; 3],
        stack: &tuff_item::item::ItemStack,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        let target = tuff_vanilla_common::placement::step(pos, facing);
        hive::dispensed_at(world, target, stack.item)
            .or_else(|| world::region::assembly::dispensed(world, target, facing, stack))
            .or_else(|| dispense_spawn_egg(world, pos, facing, stack))
    }

    fn tick(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        phase: tuff_core::world::region_api::TickPhase,
    ) {
        use tuff_core::world::region_api::TickPhase;
        match phase {
            TickPhase::Spawning => world::region::spawning::spawn_mobs(world),
            TickPhase::Late => {
                world::region::spawners::tick_spawners(world);
                world::region::phantoms::tick_phantoms(world);
                world::region::raids::tick(world);
                world::region::patrols::tick(world);
                world::region::cats::tick(world);
                world::region::sieges::tick(world);
                world::region::traders::tick(world);
                world::region::shriekers::tick(world, phase);
                world::region::creaking_heart::tick(world);
                sulfur_cube::prime_powered(world);
                world::region::hearing::tick(world);
                leash::tick_region(world);
                hive::tick(world);
                world::region::cramming::tick(world);
                crowd::tick(world);
            }
            TickPhase::Weather => {}
        }
    }

    fn game_event(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        emission: &tuff_core::world::game_event::Emission,
    ) {
        world::region::shriekers::hear(world, emission);
        world::region::hearing::hear(world, emission);
    }

    fn player_moved(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        update: &tuff_core::world::messages::PlayerUpdate,
    ) {
        world::region::shriekers::stepped(world, update);
    }

    fn player_breaking_block(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        action: &tuff_core::world::region_api::PlayerBlockAction,
        tool: Option<&tuff_item::item::ItemStack>,
    ) {
        hive::broken(world, action, tool);
    }

    fn player_block_action(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        action: &tuff_core::world::region_api::PlayerBlockAction,
    ) {
        use tuff_core::world::region_api::BlockAction;
        world::region::creaking_heart::broken(world, action);
        if action.action == BlockAction::Placed {
            world::region::assembly::placed(world, action.pos, action.state);
        }
        if action.action != BlockAction::Placed
            && tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::GUARDED_BY_PIGLINS,
                action.state.block(),
            )
        {
            piglin::anger_piglins(world, action.player, action.action == BlockAction::Opened);
        }
    }

    fn block_exploded(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        pos: tuff_world_model::BlockPos,
        state: tuff_vanilla_data::BlockState,
        cause: &tuff_core::world::explosion::ExplosionCause,
    ) {
        world::region::creaking_heart::exploded(world, pos, state, cause);
        hive::exploded(world, pos, state, cause);
    }

    fn guarded_container_disturbed(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        player: tuff_core::world::messages::PlayerId,
    ) {
        piglin::anger_piglins(world, player, true);
    }

    fn interact_entity(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        player: &tuff_core::world::messages::PlayerHandle,
        target: i32,
        interaction: &tuff_core::world::entity::EntityInteraction,
    ) -> tuff_core::world::entity::InteractOutcome {
        leash::interact(world, player, target, interaction)
    }

    fn use_block(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        place: &tuff_core::world::messages::Place,
        state: tuff_vanilla_data::BlockState,
    ) -> Option<tuff_core::world::messages::ActionOutcome> {
        leash::bind_to_fence(world, place, state)
    }

    fn use_item_on(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        place: &tuff_core::world::messages::Place,
        state: tuff_vanilla_data::BlockState,
    ) -> Option<tuff_core::world::messages::ActionOutcome> {
        if let Some(outcome) = hive::harvest(world, place, state) {
            return Some(outcome);
        }
        let item = place.held?;
        if item == tuff_vanilla_data::Item::Lead
            && let Some(outcome) = leash::bind_to_fence(world, place, state)
        {
            return Some(outcome);
        }
        let kind = item.spawns_entity()?;
        let pos = place.target;
        let entity_data = place
            .components
            .other
            .get_compound("minecraft:entity_data")
            .filter(|_| place.game_master);
        if world::region::spawners::retarget(world, pos, kind, entity_data) {
            return Some(tuff_core::world::messages::ActionOutcome::Placed {
                off_hand: place.off_hand,
                item,
                placement: None,
            });
        }
        let at = if state.collision_boxes().is_empty() {
            pos
        } else {
            pos.offset(place.face)
        };
        let lower = at != pos && place.face == tuff_world_model::Face::Up;
        world::region::spawning::spawn_from_egg(world, kind, at, lower, &place.components)
            .then_some(tuff_core::world::messages::ActionOutcome::Placed {
                off_hand: place.off_hand,
                item,
                placement: None,
            })
    }

    fn player_effect(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        event: &tuff_core::world::region_api::PlayerEffectEvent,
    ) {
        world.with_tick_context(event.position, &mut |cx| death_effects::player(event, cx));
    }

    fn use_bucket(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        request: &tuff_core::world::messages::BucketUse,
    ) {
        use tuff_core::world::messages::{ActionOutcome, PlayerEvent};
        use tuff_core::world::raycast::{self, FluidMode};
        let Some(kind) = request.item.spawns_entity() else {
            return;
        };
        let to = [0, 1, 2].map(|i| request.eye[i] + request.look[i] * request.reach);
        let hit = raycast::cast(request.eye, to, FluidMode::SourceOnly, &|p| {
            world
                .block(p)
                .unwrap_or(tuff_vanilla_data::Block::Air.default_state())
        });
        let components = request
            .mob_components
            .as_deref()
            .map(tuff_item::item::Components::from_nbt)
            .unwrap_or_default();
        let spawned = hit
            .filter(|hit| world.block(hit.pos).is_some_and(|s| s.is_liquid()))
            .is_some_and(|hit| {
                world::region::spawning::spawn_from_egg(world, kind, hit.pos, false, &components)
            });
        let outcome = if spawned {
            ActionOutcome::Placed {
                off_hand: request.off_hand,
                item: request.item,
                placement: None,
            }
        } else {
            ActionOutcome::Rejected(std::sync::Arc::from(Vec::new()))
        };
        request.player.send(PlayerEvent::ActionResult {
            sequence: request.sequence,
            outcome,
        });
    }

    fn chunk_generated(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        chunk: tuff_world_model::pos::ChunkPos,
    ) {
        world::region::spawning::spawn_generation_mobs(world, chunk);
    }

    fn natural_lightning(
        &self,
        world: &mut dyn tuff_core::world::region_api::RegionWorld,
        target: tuff_world_model::BlockPos,
    ) -> bool {
        skeleton_trap::natural_strike(world, target)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_vanilla_data::Block;
    use tuff_vanilla_data::registry::{Attribute, MobEffect};

    use super::*;
    use tuff_core::protocol::play::MetadataValue;
    use tuff_core::world::effects;
    use tuff_core::world::entity::EntityBase;
    use tuff_core::world::entity::TickContext;
    use tuff_world_model::BlockPos;

    #[tuff_core::watchdog]
    #[test]
    fn a_spawn_egg_hatches_the_type_its_entity_data_names() {
        use tuff_item::item::ItemStack;
        use tuff_vanilla_data::registry::EntityType;
        let mut egg = ItemStack::new(tuff_vanilla_data::Item::CowSpawnEgg, 1);
        assert_eq!(egg_entity_type(&egg), Some(EntityType::Cow));
        egg.components.other.insert(
            "minecraft:entity_data",
            tuff_nbt::Compound::new().with("id", "minecraft:pig"),
        );
        assert_eq!(egg_entity_type(&egg), Some(EntityType::Pig));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dispensed_mob_settles_onto_a_slab_in_front() {
        use tuff_vanilla_data::Block;
        let target = BlockPos::new(0, 64, 0);
        let bounds = [0.2, 65.0, 0.2, 0.8, 66.0, 0.8];
        let open = |_: BlockPos| Some(Block::Air.default_state());
        assert!((settle_drop(&open, target, bounds) + 1.0).abs() < 1e-9);
        let slab = |p: BlockPos| {
            Some(if p == target {
                Block::StoneSlab.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert!((settle_drop(&slab, target, bounds) + 0.5).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn swimmers_steer_against_currents_but_others_are_pushed() {
        assert!(!fish::Cod::default().pushed_by_fluid());
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
        assert!(!<squid::Squid as Species>::create(&mut rng).pushed_by_fluid());
        assert!(guardian::Guardian::default().pushed_by_fluid());
    }

    /// Runs `f` with a cow and a context over a flat grass world at y 64.
    pub(crate) fn with_context(f: impl FnOnce(&mut MobCore, &mut TickContext)) {
        with_entities(&[], f);
    }

    /// [`with_context`] with these other entities in the region.
    pub(crate) fn with_entities(
        entities: &[tuff_core::world::entity::EntitySummary],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        with_world(&block, entities, f);
    }

    /// [`with_entities`] over a world of `block`s.
    pub(crate) fn with_world(
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        entities: &[tuff_core::world::entity::EntitySummary],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        with_players(block, entities, &[], f);
    }

    /// [`with_world`] with these players nearby.
    pub(crate) fn with_players(
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        entities: &[tuff_core::world::entity::EntitySummary],
        players: &[tuff_core::world::entity::NearbyPlayer],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        with_shared(
            tuff_test_support::bare_shared(),
            block,
            entities,
            players,
            f,
        );
    }

    /// [`with_context`] with the game rule `name` set to `value`.
    pub(crate) fn with_rule(
        name: &str,
        value: bool,
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let mut rules = tuff_core::world::game_rules::GameRules::default();
        rules
            .set(name, tuff_core::world::game_rules::RuleValue::Bool(value))
            .expect("known rule");
        let mut shared = tuff_test_support::bare_shared();
        shared.game_rules = tuff_test_support::watch::channel(rules).1;
        let block = |_: BlockPos| Some(Block::Air.default_state());
        with_shared(shared, &block, &[], &[], f);
    }

    /// [`with_world`] over this store of points of interest.
    pub(crate) fn with_pois(
        pois: &crate::world::poi::PoiStore,
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        light: &dyn Fn(BlockPos) -> Option<(u8, u8)>,
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        with_shared_pois(
            tuff_test_support::bare_shared(),
            block,
            &[],
            &[],
            pois,
            light,
            f,
        );
    }

    pub(crate) fn with_shared(
        shared: tuff_core::world::Shared,
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        entities: &[tuff_core::world::entity::EntitySummary],
        players: &[tuff_core::world::entity::NearbyPlayer],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let pois = crate::world::poi::PoiStore::default();
        with_shared_pois(
            shared,
            block,
            entities,
            players,
            &pois,
            &|_| Some((15, 0)),
            f,
        );
    }

    fn with_shared_pois(
        shared: tuff_core::world::Shared,
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        entities: &[tuff_core::world::entity::EntitySummary],
        players: &[tuff_core::world::entity::NearbyPlayer],
        pois: &crate::world::poi::PoiStore,
        light: &dyn Fn(BlockPos) -> Option<(u8, u8)>,
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = StdRng::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let mut cx = TickContext {
            tick: 1,
            dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
            block,
            light,
            sky_darken: 0,
            ambient_light: 0.0,
            monsters_burn: true,
            raining: false,
            biome: &biome,
            container: &|_| None,
            container_info: &|_| Default::default(),
            inhabited_time: &|_| 0,
            min_y: -64,
            max_y: 319,
            players,
            players_in_dimension: 0,
            entities,
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        let base = EntityBase::new(EntityType::Cow, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut core = MobCore::new(base);
        f(&mut core, &mut cx);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_players_hits_credit_it_with_resisted_and_absorbed_damage() {
        use tuff_core::world::entity::Effect;
        use tuff_core::world::messages::PlayerEvent;
        use tuff_vanilla_data::registry::CustomStat;
        let credited = |effects: &[Effect]| {
            effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::TellPlayer { player: 7, event } => match **event {
                        PlayerEvent::AwardStat(stat, amount) => Some((stat, amount)),
                        _ => None,
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Cow, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
            let mut cow = Mob::<farm::Cow>::new(base, cx.rng);
            cow.core
                .effects
                .add(effects::ActiveEffect::timed(MobEffect::Resistance, 200, 0));
            cow.core.absorption = 2.0;
            let mut damage = Damage::of(&tuff_vanilla_data::damage_type::GENERIC);
            cow.hurt_by(&damage, 1.0, cx);
            assert!(credited(cx.effects).is_empty(), "nobody to credit");
            cow.core.base.invulnerable_time = 0;
            cow.core.absorption = 2.0;
            cx.effects.clear();
            damage.player = Some((7, 7));
            cow.hurt_by(&damage, 10.0, cx);
            assert_eq!(
                credited(cx.effects),
                [
                    (CustomStat::DamageDealtResisted, 20),
                    (CustomStat::DamageDealtAbsorbed, 20)
                ]
            );
            assert_eq!(cow.core.health, cow.core.max_health() - 6.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn lasting_effects_feed_into_attribute_values() {
        with_context(|core, _cx| {
            let base_speed = core.attribute(Attribute::MovementSpeed);
            core.effects
                .add(effects::ActiveEffect::timed(MobEffect::Speed, 200, 0));
            assert!(
                core.attribute(Attribute::MovementSpeed) > base_speed,
                "speed I raises movement speed"
            );

            let base_damage = core.attribute(Attribute::AttackDamage);
            core.effects
                .add(effects::ActiveEffect::timed(MobEffect::Weakness, 200, 0));
            assert!(
                core.attribute(Attribute::AttackDamage) < base_damage,
                "weakness lowers attack damage"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn heal_caps_at_max_health_and_marks_it_changed() {
        with_context(|core, _cx| {
            core.health = core.max_health() - 1.0;
            core.changed.clear();
            core.heal(5.0);
            assert_eq!(core.health, core.max_health());
            assert!(core.changed.contains(&data::HEALTH));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn shared_data_indices_match_the_dumped_layouts() {
        use tuff_vanilla_data::entity::MobCategory;
        let byte = EntityType::Cow.data_serializer(0).unwrap();
        let float = EntityType::Cow.data_serializer(data::HEALTH).unwrap();
        let boolean = EntityType::Cow.data_serializer(3).unwrap();
        for kind in [
            EntityType::Cow,
            EntityType::Pig,
            EntityType::Sheep,
            EntityType::Chicken,
        ] {
            assert_eq!(kind.category(), MobCategory::Creature);
            assert_eq!(kind.data_serializer(data::LIVING_FLAGS), Some(byte));
            assert_eq!(kind.data_serializer(data::HEALTH), Some(float));
            assert_eq!(kind.data_serializer(data::MOB_FLAGS), Some(byte));
            assert_eq!(kind.data_serializer(data::BABY), Some(boolean));
            assert_eq!(kind.data_serializer(data::AGE_LOCKED), Some(boolean));
        }
        assert_eq!(EntityType::Sheep.data_serializer(data::SPECIES), Some(byte));
        assert_eq!(
            EntityType::Sheep.data_layout().len(),
            data::SPECIES as usize + 1
        );
        assert_eq!(
            EntityType::Pig.data_layout().len(),
            data::SPECIES as usize + 3
        );
        assert_eq!(
            EntityType::Cow.data_layout().len(),
            data::SPECIES as usize + 2
        );
        assert_eq!(
            EntityType::Chicken.data_layout().len(),
            data::SPECIES as usize + 2
        );
        assert_eq!(byte as i32, MetadataValue::Byte(0).type_id());
        assert_eq!(float as i32, MetadataValue::Float(0.0).type_id());
        assert_eq!(boolean as i32, MetadataValue::Boolean(false).type_id());
    }

    #[tuff_core::watchdog]
    #[test]
    fn cows_walk_where_they_are_sent() {
        with_context(|cow, cx| {
            let mut sent = false;
            for _ in 0..200 {
                if !sent {
                    sent = cow.move_to_position([6.5, 64.0, 0.5], 1.0, cx);
                }
                cow.body.previous = cow.base.position;
                cow.tick_navigation(cx);
                cow.tick_move_control(cx);
                cow.tick_jump_control();
                cow.apply_jump(cx);
                let input = [cow.body.xxa as f64, 0.0, cow.body.zza as f64];
                cow.travel(input, cx);
            }
            assert!(sent);
            assert!(cow.base.on_ground);
            assert!(
                (cow.base.position[0] - 6.5).abs() < 1.5,
                "{:?}",
                cow.base.position
            );
        });
    }
}

//! Wandering traders: every 24000 ticks a trader may turn up near a
//! player, more likely each time one did not (25% up to 75%), and a tenth
//! of the tries actually find a player to visit. It walks to the nearest
//! meeting point within 48 blocks of the player (or the player), with two
//! trader llamas, and leaves after 48000 ticks.
//!
//! The spawn delay and chance are level-wide and saved (see
//! `trader_store`); each region counts the 1200-tick period itself,
//! asks the store to step, and acts only when the picked player stands
//! here.

use rand::Rng;
use tokio::sync::oneshot;
use tuff_vanilla_data::entity::Heightmap;
use tuff_vanilla_data::registry::{EntityType, PointOfInterestType};

use crate::llama::TraderLlama;
use crate::physics::block_pos;
use crate::spawning;
use crate::wandering_trader::{self, WanderingTrader};
use crate::world::trader_store::TraderStore;
use crate::{Mob, MobCore};
use tuff_core::world::poi::Occupancy;
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;

const TICK_DELAY: i32 = 1200;
const SEARCH_RADIUS: i32 = 48;
const LLAMA_RADIUS: i32 = 4;

pub struct Timer {
    tick_delay: i32,
    pending: Option<oneshot::Receiver<Option<i32>>>,
}

impl Default for Timer {
    fn default() -> Self {
        Self {
            tick_delay: TICK_DELAY,
            pending: None,
        }
    }
}

pub fn tick(world: &mut dyn RegionWorld) {
    if !world.shared().rule_bool("spawn_wandering_traders")
        || !world.dimension_info().special_spawners()
    {
        return;
    }
    let shared = world.shared().clone();
    let Some(store) = shared.services.get::<TraderStore>() else {
        return;
    };
    let dimension = world.dimension_info().key.clone();
    let timer = world.state().get::<Timer>();
    if let Some(answer) = timer.pending.as_mut() {
        let due = match answer.try_recv() {
            Ok(due) => due,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => None,
        };
        timer.pending = None;
        timer.tick_delay = TICK_DELAY;
        if let Some(chance) = due
            && world.rng().gen_range(0..100) <= chance
            && visit(world)
        {
            store.visited(&dimension);
        }
        return;
    }
    timer.tick_delay -= 1;
    if timer.tick_delay > 0 {
        return;
    }
    timer.tick_delay = 1;
    let now = *shared.clock.borrow();
    let pending = store.step(&dimension, now);
    world.state().get::<Timer>().pending = pending;
}

/// Picks a player and maybe sends a trader to it; true when the try
/// counts as done (a trader came, or nobody was around).
fn visit(world: &mut dyn RegionWorld) -> bool {
    let total = world
        .shared()
        .players
        .borrow()
        .in_dimension(world.dimension_id());
    if total == 0 {
        return true;
    }
    let pick = world.rng().gen_range(0..total);
    let here: Vec<[f64; 3]> = world
        .seen_player_details()
        .into_iter()
        .map(|p| p.position)
        .filter(|p| world.owns(block_pos(*p)))
        .collect();
    let Some(&player) = here.get(pick) else {
        return false;
    };
    if world.rng().gen_range(0..10) != 0 {
        return false;
    }
    spawn_trader_near(world, block_pos(player))
}

/// Sends a trader and its llamas to the meeting point nearest `player`
/// (or `player` itself). False when there was no room.
pub fn spawn_trader_near(world: &mut dyn RegionWorld, player: BlockPos) -> bool {
    let meeting = world
        .pois()
        .closest_first(player, SEARCH_RADIUS, Occupancy::Any, |kind| {
            kind == PointOfInterestType::Meeting
        })
        .first()
        .map(|(pos, _)| *pos);
    let reference = meeting.unwrap_or(player);
    let Some(pos) = spawn_position_near(world, reference, SEARCH_RADIUS) else {
        return false;
    };
    if !enough_space(world, pos) || !biome_allows(world, pos) {
        return false;
    }
    let Some(mut trader) = create(world, EntityType::WanderingTrader, pos) else {
        return false;
    };
    let (trader_id, uuid) = (trader.base().id, trader.base().uuid);
    if let Some(mob) = trader.downcast_mut::<Mob<WanderingTrader>>() {
        let Mob { core, species } = mob;
        species.arrive(core, reference, wandering_trader::DESPAWN_DELAY);
        core.homeward = true;
    }
    let at = trader.base().position;
    world.add_entity(trader);
    for _ in 0..2 {
        let Some(spot) = spawn_position_near(world, block_pos(at), LLAMA_RADIUS) else {
            continue;
        };
        let Some(mut llama) = create(world, EntityType::TraderLlama, spot) else {
            continue;
        };
        if let Some(mob) = llama.downcast_mut::<Mob<TraderLlama>>() {
            let Mob { core, species } = mob;
            species.follow(core, trader_id, uuid);
        }
        world.add_entity(llama);
    }
    true
}

fn create(
    world: &mut dyn RegionWorld,
    kind: EntityType,
    pos: BlockPos,
) -> Option<Box<dyn tuff_core::world::entity::RegionEntity>> {
    let id = world.shared().next_entity_id();
    let at = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ];
    let mut entity = world.shared().entity_types.create(kind, id, at, 0.0)?;
    world.finalize_spawn(&mut entity, 0);
    if let Some(core) = mob_core(entity.as_mut()) {
        core.persistence_required = false;
    }
    Some(entity)
}

fn mob_core(entity: &mut dyn tuff_core::world::entity::RegionEntity) -> Option<&mut MobCore> {
    if let Some(mob) = entity.downcast_mut::<Mob<WanderingTrader>>() {
        return Some(&mut mob.core);
    }
    entity
        .downcast_mut::<Mob<TraderLlama>>()
        .map(|mob| &mut mob.core)
}

fn spawn_position_near(
    world: &mut dyn RegionWorld,
    reference: BlockPos,
    radius: i32,
) -> Option<BlockPos> {
    let kind = EntityType::WanderingTrader;
    for _ in 0..10 {
        let rng = world.rng();
        let x = reference.x + rng.gen_range(0..radius * 2) - radius;
        let z = reference.z + rng.gen_range(0..radius * 2) - radius;
        let pos = BlockPos::new(x, surface(world, x, z), z);
        if !world.owns(pos) {
            continue;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
        if spawning::valid_empty_spawn_block(world.block(pos), kind)
            && spawning::valid_empty_spawn_block(world.block(above), kind)
            && world.block(below).is_some_and(|s| s.is_valid_spawn(kind))
        {
            return Some(pos);
        }
    }
    None
}

fn enough_space(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    (0..=1).all(|dx| {
        (0..=2).all(|dy| {
            (0..=1).all(|dz| {
                world
                    .block(BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz))
                    .is_some_and(|s| s.collision_boxes().is_empty())
            })
        })
    })
}

fn biome_allows(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world.biome_name(pos).is_none_or(|biome| {
        !tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::WITHOUT_WANDERING_TRADER_SPAWNS,
            biome,
        )
    })
}

fn surface(world: &dyn RegionWorld, x: i32, z: i32) -> i32 {
    let shape = &world.dimension_info().shape;
    spawning::heightmap_top(
        Heightmap::MotionBlockingNoLeaves,
        x,
        z,
        shape.min_y + shape.height - 1,
        shape.min_y,
        &|p| world.block(p),
    )
}

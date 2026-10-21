//! Pillager patrols: every ten minutes or so, in daylight, one player in
//! five away from villages gets a patrol of pillagers spawned 24 to 48
//! blocks off, led by a banner carrier heading somewhere far away.
//!
//! The game keeps one timer for the whole dimension and picks one of its
//! players. Each region keeps its own timer instead and draws from all of
//! the dimension's players, acting only when the pick is one standing
//! here, so a player sees patrols as often as in the game.

use rand::Rng;
use tuff_vanilla_data::entity::Heightmap;
use tuff_vanilla_data::registry::EntityType;

use crate::monster::LocalDifficulty;
use crate::physics::block_pos;
use crate::raider;
use crate::spawning;
use tuff_core::world::player::Difficulty;
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;

const BASE_DELAY: i64 = 12_000;
const DELAY_SPREAD: i64 = 1_200;

#[derive(Default)]
struct Timer {
    next: i64,
}

fn allowed(world: &dyn RegionWorld) -> bool {
    let shared = world.shared();
    *shared.difficulty.borrow() != Difficulty::Peaceful
        && world.dimension_info().special_spawners()
        && shared.rule_bool("spawn_mobs")
        && shared.rule_bool("spawn_monsters")
        && shared.rule_bool("spawn_patrols")
}

pub fn tick(world: &mut dyn RegionWorld) {
    if !allowed(world) {
        return;
    }
    let timer = world.state().get::<Timer>();
    timer.next -= 1;
    if timer.next > 0 {
        return;
    }
    let delay = BASE_DELAY + world.rng().gen_range(0..DELAY_SPREAD);
    world.state().get::<Timer>().next += delay;
    let bright = !world.dimension_info().environment().has_fixed_time && world.sky_darken() < 4;
    if !bright || world.rng().gen_range(0..5) != 0 {
        return;
    }
    let total = world
        .shared()
        .players
        .borrow()
        .in_dimension(world.dimension_id());
    if total == 0 {
        return;
    }
    let pick = world.rng().gen_range(0..total);
    let here: Vec<[f64; 3]> = world
        .seen_player_details()
        .into_iter()
        .map(|p| p.position)
        .filter(|p| world.owns(block_pos(*p)))
        .collect();
    let Some(&player) = here.get(pick) else {
        return;
    };
    spawn_patrol_near(world, block_pos(player));
}

/// Spawns a patrol somewhere around `player`, unless it is close to a
/// village. False when none spawned.
pub fn spawn_patrol_near(world: &mut dyn RegionWorld, player: BlockPos) -> bool {
    if world.pois().sections_to_village(player) <= 2 {
        return false;
    }
    let mut offset = || {
        let rng = world.rng();
        let d = 24 + rng.gen_range(0..24);
        if rng.r#gen::<bool>() { -d } else { d }
    };
    let (dx, dz) = (offset(), offset());
    let mut pos = BlockPos::new(player.x + dx, player.y, player.z + dz);
    let loaded = [(-10, -10), (-10, 10), (10, -10), (10, 10)]
        .iter()
        .all(|(x, z)| {
            world
                .chunk(tuff_world_model::pos::ChunkPos::of_block(BlockPos::new(
                    pos.x + x,
                    0,
                    pos.z + z,
                )))
                .is_some()
        });
    if !loaded || !patrols_may_spawn(world, pos) {
        return false;
    }
    let effective = LocalDifficulty::in_region(
        world,
        [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)],
    )
    .effective;
    let size = effective.ceil() as i32 + 1;
    for i in 0..size {
        pos.y = surface(world, pos.x, pos.z);
        let spawned = spawn_member(world, pos, i == 0);
        if i == 0 && !spawned {
            return false;
        }
        let rng = world.rng();
        pos.x += rng.gen_range(0..5) - rng.gen_range(0..5);
        pos.z += rng.gen_range(0..5) - rng.gen_range(0..5);
    }
    true
}

fn patrols_may_spawn(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .biome_name(pos)
        .and_then(tuff_vanilla_data::biome::Biome::get)
        .and_then(|b| b.attribute("minecraft:gameplay/can_pillager_patrol_spawn"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
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

fn spawn_member(world: &mut dyn RegionWorld, pos: BlockPos, leader: bool) -> bool {
    if !world.owns(pos) {
        return false;
    }
    let kind = EntityType::Pillager;
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let dark_enough = world.engine_light(pos).is_none_or(|(_, block)| block <= 8);
    if !spawning::valid_empty_spawn_block(world.block(pos), kind)
        || !dark_enough
        || !world.block(below).is_some_and(|s| s.is_valid_spawn(kind))
    {
        return false;
    }
    let id = world.shared().next_entity_id();
    let at = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
    let Some(mut entity) = world.shared().entity_types.create(kind, id, at, 0.0) else {
        return false;
    };
    let target = leader.then(|| {
        let rng = world.rng();
        BlockPos::new(
            pos.x - 500 + rng.gen_range(0..1000),
            pos.y,
            pos.z - 500 + rng.gen_range(0..1000),
        )
    });
    let Some(state) = raider::core_mut(entity.as_mut()).and_then(|c| c.raider.as_mut()) else {
        return false;
    };
    state.spawned_for_group = true;
    state.patrolling = true;
    if leader {
        state.patrol_leader = true;
        state.patrol_target = target;
    }
    world.finalize_spawn(&mut entity, 0);
    if let Some(state) = raider::core_mut(entity.as_mut()).and_then(|c| c.raider.as_mut()) {
        state.spawned_for_group = false;
    }
    world.add_entity(entity);
    true
}

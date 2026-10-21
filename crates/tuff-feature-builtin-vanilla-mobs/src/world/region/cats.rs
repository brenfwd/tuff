//! Village cats: about once a minute a random player may get a cat
//! spawned 8 to 31 blocks off, near a village with more than four occupied
//! homes (and fewer than five cats around), or inside a swamp hut with no
//! cat nearby, where it stays for good.
//!
//! The game keeps one timer for the whole dimension and picks one of its
//! players. Each region keeps its own timer instead and draws from all of
//! the dimension's players, acting only when the pick is one standing
//! here.

use rand::Rng;
use tuff_core::world::poi::Occupancy;
use tuff_core::world::region_api::RegionWorld;
use tuff_vanilla_data::registry::{EntityType, PointOfInterestType};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use crate::cat::Cat;
use crate::physics::block_pos;
use crate::spawning;
use crate::{Mob, physics};

const DELAY: i64 = 1200;
const VILLAGE_HOMES: usize = 4;
const VILLAGE_CATS: usize = 5;
const VILLAGE_RANGE: i32 = 48;
const HUT_RANGE: i32 = 16;
const SWAMP_HUT: &str = "minecraft:swamp_hut";

#[derive(Default)]
struct Timer {
    next: i64,
}

pub fn tick(world: &mut dyn RegionWorld) {
    if !world.dimension_info().special_spawners() || !world.shared().rule_bool("spawn_mobs") {
        return;
    }
    let timer = world.state().get::<Timer>();
    timer.next -= 1;
    if timer.next > 0 {
        return;
    }
    world.state().get::<Timer>().next = DELAY;
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
    spawn_near(world, block_pos(player));
}

fn spawn_near(world: &mut dyn RegionWorld, player: BlockPos) {
    let mut offset = || {
        let rng = world.rng();
        let d = 8 + rng.gen_range(0..24);
        if rng.r#gen::<bool>() { -d } else { d }
    };
    let (dx, dz) = (offset(), offset());
    let pos = BlockPos::new(player.x + dx, player.y, player.z + dz);
    let loaded = [(-10, -10), (-10, 10), (10, -10), (10, 10)]
        .iter()
        .all(|(x, z)| {
            world
                .chunk(ChunkPos::of_block(BlockPos::new(pos.x + x, 0, pos.z + z)))
                .is_some()
        });
    if loaded {
        spawn_at(world, pos);
    }
}

pub fn spawn_at(world: &mut dyn RegionWorld, pos: BlockPos) {
    if !world.owns(pos) || !position_ok(world, pos) {
        return;
    }
    if world.pois().sections_to_village(pos) <= 2 {
        let homes = world
            .pois()
            .closest_first(pos, VILLAGE_RANGE, Occupancy::IsOccupied, |kind| {
                kind == PointOfInterestType::Home
            })
            .len();
        if homes > VILLAGE_HOMES && cats_within(world, pos, VILLAGE_RANGE, 8) < VILLAGE_CATS {
            spawn_cat(world, pos, false);
        }
    } else if super::spawners::in_structure_piece(world, pos, SWAMP_HUT)
        && cats_within(world, pos, HUT_RANGE, 8) == 0
    {
        spawn_cat(world, pos, true);
    }
}

fn position_ok(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    spawning::valid_empty_spawn_block(world.block(pos), EntityType::Cat)
        && world
            .block(below)
            .is_some_and(|s| s.is_valid_spawn(EntityType::Cat))
}

fn cats_within(world: &dyn RegionWorld, pos: BlockPos, horizontal: i32, vertical: i32) -> usize {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let area = [
        x - f64::from(horizontal),
        y - f64::from(vertical),
        z - f64::from(horizontal),
        x + 1.0 + f64::from(horizontal),
        y + 1.0 + f64::from(vertical),
        z + 1.0 + f64::from(horizontal),
    ];
    world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .filter(|tracked| {
            tracked.entity.base().kind == EntityType::Cat
                && physics::intersects(tracked.entity.bounds(), area)
        })
        .count()
}

fn spawn_cat(world: &mut dyn RegionWorld, pos: BlockPos, persistent: bool) {
    let id = world.shared().next_entity_id();
    let at = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ];
    let Some(mut entity) = world
        .shared()
        .entity_types
        .create(EntityType::Cat, id, at, 0.0)
    else {
        return;
    };
    world.finalize_spawn(&mut entity, 0);
    if persistent && let Some(cat) = entity.downcast_mut::<Mob<Cat>>() {
        cat.core.persistence_required = true;
    }
    world.add_entity(entity);
}

//! Phantoms' natural spawning: an insomnia-driven spawner independent of
//! the generic biome/structure spawn cycle in `spawning.rs` (phantoms are
//! in no biome's spawn list). The game runs this once per level; here it
//! runs once per region, off the region's own view of nearby players (see
//! `nearby.rs`), which is the same simplification the rest of natural
//! spawning already makes.

use rand::Rng;
use tuff_vanilla_data::registry::EntityType;

use crate::{physics, spawning};
use tuff_core::world::player::Difficulty;
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Roughly 60 to 119 seconds between attempts, like the game's own timer.
const MIN_INTERVAL: i64 = 60 * 20;
const RANDOM_INTERVAL: i64 = 60 * 20;
/// A player's `time_since_rest` roll (clamped to at least 1) must reach
/// this before a batch has any chance to spawn over them.
const INSOMNIA_THRESHOLD: i32 = 72_000;

/// Ticks left before a region's next phantom spawn attempt.
#[derive(Default)]
struct PhantomTimer(i64);

/// One tick of the phantom timer; a no-op except once every one to two
/// minutes, and even then usually a no-op (bright out, no insomniac
/// players, or the roll misses).
pub fn tick_phantoms(world: &mut dyn RegionWorld) {
    // Checked before touching the timer or drawing anything from the
    // region's RNG, exactly like the game's own gate order, so a disabled
    // rule (the default in tests) never perturbs the RNG sequence other,
    // unrelated behaviour in the same region depends on.
    let shared = world.shared().clone();
    let peaceful = *shared.difficulty.borrow() == Difficulty::Peaceful;
    if peaceful
        || !shared.game_rules.borrow().bool("spawn_phantoms")
        || !world.dimension_info().special_spawners()
    {
        return;
    }
    let timer = world.state().get::<PhantomTimer>();
    timer.0 -= 1;
    if timer.0 > 0 {
        return;
    }
    let next = MIN_INTERVAL + world.rng().gen_range(0..RANDOM_INTERVAL);
    world.state().get::<PhantomTimer>().0 = next;
    let dimension = world.dimension_info();
    let sky_darken = world.sky_darken();
    if dimension.kind.has_skylight && sky_darken < 5 {
        return;
    }
    let sea_level = dimension.sea_level();
    let has_skylight = dimension.kind.has_skylight;
    let top = dimension.shape.min_y + dimension.shape.height;
    let base = *shared.difficulty.borrow();
    let difficulty = crate::monster::LocalDifficulty::new(base, world.tick(), 0, 0.0);
    let players: Vec<_> = shared
        .players
        .borrow()
        .near(world.dimension_id(), world.region_pos())
        .to_vec();
    for player in players {
        if has_skylight {
            let pos = physics::block_pos(player.position);
            if pos.y < sea_level || !sees_sky_above(world, pos, top) {
                continue;
            }
        }
        if difficulty.effective <= world.rng().r#gen::<f32>() * 3.0 {
            continue;
        }
        let value = player.time_since_rest.max(1);
        if world.rng().gen_range(0..value) < INSOMNIA_THRESHOLD {
            continue;
        }
        spawn_phantoms(world, player.position, difficulty.base);
    }
}

/// Whether nothing but air stands above `pos`, up to `top`, as far as
/// this region can see (an unloaded chunk simply stops the check, the
/// same simplification the monster-under-sky rule makes).
fn sees_sky_above(world: &dyn RegionWorld, pos: BlockPos, top: i32) -> bool {
    ((pos.y + 1)..top)
        .map_while(|y| world.block(BlockPos::new(pos.x, y, pos.z)))
        .all(tuff_world_model::chunk::is_air)
}

fn spawn_phantoms(world: &mut dyn RegionWorld, near: [f64; 3], difficulty: Difficulty) {
    let base = physics::block_pos(near);
    let rng = world.rng();
    let spawn_pos = BlockPos::new(
        base.x + rng.gen_range(-10..=10),
        base.y + 20 + rng.gen_range(0..15),
        base.z + rng.gen_range(-10..=10),
    );
    if !world.owns_chunk(ChunkPos::of_block(spawn_pos)) {
        return;
    }
    if !spawning::valid_empty_spawn_block(world.block(spawn_pos), EntityType::Phantom) {
        return;
    }
    let position = [
        f64::from(spawn_pos.x) + 0.5,
        f64::from(spawn_pos.y),
        f64::from(spawn_pos.z) + 0.5,
    ];
    let group = 1 + world.rng().gen_range(0..=difficulty.id());
    for index in 0..group {
        let id = world.shared().next_entity_id();
        let Some(mut entity) =
            world
                .shared()
                .entity_types
                .create(EntityType::Phantom, id, position, 0.0)
        else {
            return;
        };
        world.finalize_spawn(&mut entity, u32::from(index));
        world.add_entity(entity);
    }
}

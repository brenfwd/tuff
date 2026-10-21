//! Frosted ice: ages while it stays lit and crowded less than four deep, then
//! melts, and melts at once when a neighbour melts or breaks away and fewer
//! than two frosted neighbours remain. A neighbour freezing never melts it,
//! so a Frost Walker disk placed block by block stays.

use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, DIRECTIONS, GrowthLevel, GrowthLevelExt, aged};
use tuff_core::world::redstone::UPDATE_CLIENTS;
use tuff_vanilla_common::placement::step;
use tuff_vanilla_data::block::Facing;
use tuff_world_model::BlockPos;

const MAX_AGE: u8 = 3;
const NEIGHBOURS_TO_AGE: u32 = 4;
const NEIGHBOURS_TO_MELT: u32 = 2;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_placed(Block::FrostedIce, placed);
    behaviours.register_scheduled(Block::FrostedIce, tick);
    behaviours.register_neighbour(Block::FrostedIce, neighbour_changed);
}

fn schedule(level: &mut dyn GrowthLevel, pos: BlockPos, min: i32, max: i32) {
    let delay = min + level.random_int(max - min + 1);
    level.schedule_tick(pos, Block::FrostedIce, delay as u32);
}

fn placed(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    schedule(level, pos, 60, 120);
}

fn frosted_around(level: &dyn GrowthLevel, pos: BlockPos, cap: u32) -> u32 {
    let mut found = 0;
    for direction in DIRECTIONS {
        found += u32::from(level.state(step(pos, direction)).block() == Block::FrostedIce);
        if found == cap {
            break;
        }
    }
    found
}

fn melt(level: &mut dyn GrowthLevel, pos: BlockPos) {
    super::weather::melt(level, pos);
}

/// Advances a block one thaw stage; returns true once it has turned to water.
fn thaw(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    match state.age().unwrap_or(0) {
        age if age < MAX_AGE => {
            level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
            false
        }
        _ => {
            melt(level, pos);
            true
        }
    }
}

fn warm_enough(level: &dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    let light = i32::from(if level.dimension().name == "minecraft:the_end" {
        level.block_light(pos)
    } else {
        level.max_local_raw_brightness(pos)
    });
    let threshold = 11 - i32::from(state.age().unwrap_or(0)) - i32::from(state.light_dampening());
    light > threshold
}

fn tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let sheltered = level.random_int(3) != 0
        && frosted_around(level, pos, NEIGHBOURS_TO_AGE) == NEIGHBOURS_TO_AGE;
    if sheltered || !warm_enough(level, pos, state) || !thaw(level, pos, state) {
        schedule(level, pos, 20, 40);
        return;
    }
    for direction in DIRECTIONS {
        let next = step(pos, direction);
        let around = level.state(next);
        if around.block() != Block::FrostedIce {
            continue;
        }
        if !thaw(level, next, around) {
            schedule(level, next, 20, 40);
        }
    }
}

fn neighbour_changed(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    _state: BlockState,
    _direction: Facing,
    neighbour: BlockState,
) {
    if matches!(neighbour.block(), Block::Water | Block::Air)
        && frosted_around(level, pos, NEIGHBOURS_TO_MELT) < NEIGHBOURS_TO_MELT
    {
        melt(level, pos);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{offset, testing::TestLevel};
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn placed_frosted_ice_schedules_a_tick_of_sixty_to_one_twenty() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::FrostedIce.default_state());
        level.run(placed, pos);
        let delay = level.ticks[0].2;
        assert!((60..=120).contains(&delay));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lone_lit_block_ages_then_melts_to_water() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::FrostedIce.default_state());
        for expected in 1..=3 {
            level.run(tick, pos);
            assert_eq!(level.state(pos).age(), Some(expected));
        }
        level.run(tick, pos);
        assert_eq!(level.state(pos).block(), Block::Water);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dark_block_only_reschedules() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::FrostedIce.default_state());
        level.light = (0, 0);
        level.run(tick, pos);
        assert_eq!(level.state(pos).age(), Some(0));
        let delay = level.ticks.last().unwrap().2;
        assert!((20..=40).contains(&delay));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_melting_neighbour_melts_a_block_with_fewer_than_two_frosted_neighbours() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::FrostedIce.default_state());
        level.put(offset(pos, 1, 0, 0), Block::FrostedIce.default_state());
        let ice = Block::FrostedIce.default_state();
        neighbour_changed(&mut level, pos, ice, Facing::East, ice);
        assert_eq!(
            level.state(pos).block(),
            Block::FrostedIce,
            "a neighbour freezing leaves it"
        );
        level.put(offset(pos, 1, 0, 0), Block::Water.default_state());
        neighbour_changed(
            &mut level,
            pos,
            ice,
            Facing::East,
            Block::Water.default_state(),
        );
        assert_eq!(level.state(pos).block(), Block::Water);

        let other = BlockPos::new(10, 64, 0);
        level.put(other, Block::FrostedIce.default_state());
        neighbour_changed(
            &mut level,
            other,
            ice,
            Facing::East,
            Block::Stone.default_state(),
        );
        assert_eq!(level.state(other).block(), Block::FrostedIce);
    }

    fn cluster_fingerprint(rounds: usize) -> u64 {
        let mut level = TestLevel::new();
        let mut cells = Vec::new();
        for x in 0..4 {
            for z in 0..4 {
                for y in 64..66 {
                    let pos = BlockPos::new(x, y, z);
                    let age = ((x * 3 + z + y) % 4) as u8;
                    level.put(pos, aged(Block::FrostedIce.default_state(), age));
                    cells.push(pos);
                }
            }
        }
        for round in 0..rounds {
            level.light = (0, (round % 16) as u8);
            for &pos in &cells {
                let state = level.state(pos);
                if state.block() == Block::FrostedIce {
                    level.run(tick, pos);
                }
            }
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| hash = (hash ^ v).wrapping_mul(0x0100_0000_01b3);
        for &pos in &cells {
            let state = level.state(pos);
            mix(state.block() as u64);
            mix(u64::from(state.age().unwrap_or(9)));
        }
        for (pos, _, delay) in &level.ticks {
            mix(pos.x as u64);
            mix(pos.y as u64);
            mix(pos.z as u64);
            mix(u64::from(*delay));
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_crowded_cluster_thaws_in_a_pinned_order() {
        assert_eq!(cluster_fingerprint(24), 5_654_754_906_930_043_049);
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "release timing"]
    fn cluster_thaw_timing() {
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..2000 {
            sink ^= cluster_fingerprint(24);
        }
        eprintln!("cluster_thaw_timing {:?} {sink}", start.elapsed());
    }
}

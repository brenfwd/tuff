//! Sponges: a dry sponge soaks up the water around it when placed or
//! updated, turning wet; a wet sponge placed where water evaporates dries.

use std::collections::{HashSet, VecDeque};

use tuff_vanilla_data::block::PickupKind;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS};
use tuff_world_model::BlockPos;

use super::{Behaviours, DIRECTIONS, GrowthLevel, GrowthLevelExt, can_survive};

const MAX_DEPTH: u32 = 6;
const MAX_COUNT: u32 = 65;
const WATER_EVAPORATING: i32 = 2009;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_placed(Block::Sponge, |level, pos, _| absorb(level, pos));
    behaviours.register_block_update(Block::Sponge, |level, pos, _, _| absorb(level, pos));
    behaviours.register_placed(Block::WetSponge, dry_out);
}

fn absorb(level: &mut dyn GrowthLevel, pos: BlockPos) {
    if remove_water(level, pos) {
        level.set(pos, Block::WetSponge.default_state(), UPDATE_CLIENTS);
        level.play_block_sound(SoundEvent::BlockSpongeAbsorb, pos, 1.0, 1.0);
    }
}

fn dry_out(level: &mut dyn GrowthLevel, pos: BlockPos, _: BlockState) {
    if !level.water_evaporates() {
        return;
    }
    level.set(pos, Block::Sponge.default_state(), UPDATE_ALL);
    level.level_event(WATER_EVAPORATING, pos, 0);
    let pitch = (1.0 + level.random_float() * 0.2) * 0.7;
    level.play_block_sound(SoundEvent::BlockWetSpongeDries, pos, 1.0, pitch);
}

fn is_water(state: BlockState) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::fluid::key::WATER,
        state.fluid_state().fluid(),
    )
}

/// Removes the water reachable from `start` breadth first, reading each
/// block only when it is reached (so water an earlier removal already
/// changed is seen as it is now). True if any was removed.
fn remove_water(level: &mut dyn GrowthLevel, start: BlockPos) -> bool {
    let mut queue = VecDeque::from([(start, 0)]);
    let mut seen = HashSet::new();
    let mut count = 0;
    while let Some((pos, depth)) = queue.pop_front() {
        if !seen.insert(pos) || (pos != start && !soak(level, pos)) {
            continue;
        }
        count += 1;
        if count >= MAX_COUNT {
            break;
        }
        if depth < MAX_DEPTH {
            for direction in DIRECTIONS {
                let (dx, dy, dz) = direction.step();
                queue.push_back((BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz), depth + 1));
            }
        }
    }
    count > 1
}

/// Takes the water out of the block at `pos`; false if it holds none a
/// sponge can take.
fn soak(level: &mut dyn GrowthLevel, pos: BlockPos) -> bool {
    let Some(state) = level.block(pos) else {
        return false;
    };
    if !is_water(state) {
        return false;
    }
    let block = state.block();
    let air = Block::Air.default_state();
    if let Some(pickup) = block.bucket_pickup()
        && block != Block::Barrier
    {
        match pickup.kind {
            PickupKind::Liquid if state.level() == Some(0) => {
                level.set(pos, air, UPDATE_ALL);
                return true;
            }
            PickupKind::Waterlogged if state.waterlogged() == Some(true) => {
                if let Some(dry) = state.with_waterlogged(false) {
                    level.set(pos, dry, UPDATE_ALL);
                    if !can_survive(level, dry, pos) {
                        level.destroy(pos, true);
                    }
                    return true;
                }
            }
            PickupKind::Whole => {
                level.set(pos, air, UPDATE_ALL);
                return true;
            }
            _ => {}
        }
    }
    if state.is_liquid_block() {
        level.set(pos, air, UPDATE_ALL);
    } else if matches!(
        block,
        Block::Kelp | Block::KelpPlant | Block::Seagrass | Block::TallSeagrass
    ) {
        level.drop_resources(pos);
        level.set(pos, air, UPDATE_ALL);
    } else {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::growth::testing::TestLevel;

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    fn place_sponge(level: &mut TestLevel, pos: BlockPos) {
        let state = Block::Sponge.default_state();
        level.put(pos, state);
        crate::growth::Behaviours::builtin().placed(level, pos, state);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_placed_sponge_soaks_up_at_most_64_water_blocks() {
        let mut level = TestLevel::new();
        let water = Block::Water.default_state();
        level.fill(at(-5, 60, -5), at(5, 64, 5), water);
        let total = level.count(Block::Water);
        place_sponge(&mut level, at(0, 62, 0));
        assert_eq!(level.state_at(at(0, 62, 0)).block(), Block::WetSponge);
        assert_eq!(total - 1 - level.count(Block::Water), 64);
        assert_eq!(
            level.sounds,
            vec![(SoundEvent::BlockSpongeAbsorb, at(0, 62, 0))]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sponge_reaches_six_blocks_and_stays_dry_without_water() {
        let mut level = TestLevel::new();
        let water = Block::Water.default_state();
        level.fill(at(1, 64, 0), at(10, 64, 0), water);
        place_sponge(&mut level, at(0, 64, 0));
        assert_eq!(level.state_at(at(0, 64, 0)).block(), Block::WetSponge);
        assert_eq!(level.count(Block::Water), 4);
        assert_eq!(level.state_at(at(7, 64, 0)), water);
        assert!(is_air_at(&level, at(6, 64, 0)));

        let mut dry = TestLevel::new();
        dry.fill(at(-1, 63, -1), at(1, 63, 1), Block::Stone.default_state());
        place_sponge(&mut dry, at(0, 64, 0));
        assert_eq!(dry.state_at(at(0, 64, 0)).block(), Block::Sponge);
        assert!(dry.sounds.is_empty());
    }

    fn is_air_at(level: &TestLevel, pos: BlockPos) -> bool {
        tuff_world_model::chunk::is_air(level.state_at(pos))
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sponge_drains_waterlogged_blocks_and_plants_but_not_barriers() {
        let mut level = TestLevel::new();
        let stairs = Block::OakStairs
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        let barrier = Block::Barrier
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        let flowing = Block::Water.default_state().with("level", "3").unwrap();
        level.put(at(1, 64, 0), stairs);
        level.put(at(-1, 64, 0), Block::Seagrass.default_state());
        level.put(at(0, 64, 1), barrier);
        level.put(at(0, 64, -1), flowing);
        level.put(at(0, 65, 0), Block::BubbleColumn.default_state());
        place_sponge(&mut level, at(0, 64, 0));
        assert_eq!(level.state_at(at(0, 64, 0)).block(), Block::WetSponge);
        assert_eq!(level.state_at(at(1, 64, 0)).waterlogged(), Some(false));
        assert!(is_air_at(&level, at(-1, 64, 0)));
        assert_eq!(
            level.resources_dropped,
            vec![(at(-1, 64, 0), Block::Seagrass.default_state())]
        );
        assert_eq!(level.state_at(at(0, 64, 1)), barrier);
        assert!(is_air_at(&level, at(0, 64, -1)));
        assert!(is_air_at(&level, at(0, 65, 0)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wet_sponge_dries_where_water_evaporates() {
        let behaviours = crate::growth::Behaviours::builtin();
        let wet = Block::WetSponge.default_state();
        let mut overworld = TestLevel::new();
        overworld.put(at(0, 64, 0), wet);
        behaviours.placed(&mut overworld, at(0, 64, 0), wet);
        assert_eq!(overworld.state_at(at(0, 64, 0)), wet);

        let mut nether = TestLevel::new();
        nether.evaporates = true;
        nether.put(at(0, 64, 0), wet);
        behaviours.placed(&mut nether, at(0, 64, 0), wet);
        assert_eq!(nether.state_at(at(0, 64, 0)), Block::Sponge.default_state());
        assert_eq!(nether.events, vec![(WATER_EVAPORATING, at(0, 64, 0), 0)]);
        assert_eq!(
            nether.sounds,
            vec![(SoundEvent::BlockWetSpongeDries, at(0, 64, 0))]
        );
    }
}

//! Grass and mycelium spreading onto dirt and dying under cover, nylium
//! dying under cover, and leaves decaying away from logs.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, above, light_dampening_into, offset};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_world_model::BlockPos;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("spreading", spreading_tick);
    behaviours.register_random_kind("nylium", nylium_tick);
    behaviours.register_random_kind("leaves", leaves_tick);
    for block in tuff_vanilla_data::Block::ALL {
        if block.random_tick_kind() == Some("leaves") {
            behaviours.register_scheduled(block, leaves_distance_tick);
            behaviours.register_neighbour(block, leaves_neighbour);
        }
    }
}

fn leaves_neighbour(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
    _direction: Facing,
    neighbour: BlockState,
) {
    let distance = tuff_vanilla_common::placement::leaf_distance_at(neighbour) + 1;
    if distance != 1 || state.distance() != Some(distance) {
        level.schedule_tick(pos, state.block(), 1);
    }
}

/// Leaves recount their distance from the nearest log.
fn leaves_distance_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let distance = tuff_vanilla_common::placement::leaf_distance(pos, &|p| level.state(p));
    if let Some(updated) = state.with_distance(distance) {
        level.set(pos, updated, UPDATE_ALL);
    }
}

/// The block grass and mycelium die back into and spread onto.
const BASE: Block = Block::Dirt;

fn can_stay_alive(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    let above_state = level.state(above(pos));
    if above_state.block() == Block::Snow && above_state.layers() == Some(1) {
        return true;
    }
    let fluid = above_state.fluid_state();
    if !fluid.is_empty() && fluid.amount() == 8 {
        return false;
    }
    light_dampening_into(state, above_state, Facing::Up) < 15
}

fn can_propagate(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    let fluid = level.state(above(pos)).fluid_state().fluid();
    can_stay_alive(level, state, pos)
        && !tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, fluid)
}

fn snowy(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::block::key::SNOW,
        level.state(pos).block(),
    )
}

fn spreading_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if !can_stay_alive(level, state, pos) {
        level.set(pos, BASE.default_state(), UPDATE_ALL);
        return;
    }
    if level.max_local_raw_brightness(above(pos)) < 9 {
        return;
    }
    let spread = state.block().default_state();
    for _ in 0..4 {
        let dx = level.random_int(3) - 1;
        let dy = level.random_int(5) - 3;
        let dz = level.random_int(3) - 1;
        let target = offset(pos, dx, dy, dz);
        if level.state(target).block() == BASE && can_propagate(level, spread, target) {
            let grown = spread
                .with_snowy(snowy(level, above(target)))
                .unwrap_or(spread);
            level.set(target, grown, UPDATE_ALL);
        }
    }
}

fn nylium_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let above_state = level.state(above(pos));
    if light_dampening_into(state, above_state, Facing::Up) >= 15 {
        level.set(pos, Block::Netherrack.default_state(), UPDATE_ALL);
    }
}

fn leaves_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.persistent() == Some(false) && state.distance() == Some(7) {
        level.destroy(pos, true);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn grass_spreads_onto_lit_dirt_and_dies_under_stone() {
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-1, 63, -1),
            BlockPos::new(1, 63, 1),
            Block::Dirt.default_state(),
        );
        let pos = BlockPos::new(0, 63, 0);
        level.put(pos, Block::GrassBlock.default_state());
        for _ in 0..200 {
            level.run(spreading_tick, pos);
        }
        assert_eq!(level.count(Block::GrassBlock), 9);
        level.put(above(pos), Block::Stone.default_state());
        level.run(spreading_tick, pos);
        assert_eq!(level.state(pos).block(), Block::Dirt);
    }

    #[tuff_core::watchdog]
    #[test]
    fn grass_does_not_spread_in_the_dark() {
        let mut level = TestLevel::new();
        level.light = (0, 4);
        level.put(BlockPos::new(1, 63, 0), Block::Dirt.default_state());
        let pos = BlockPos::new(0, 63, 0);
        level.put(pos, Block::GrassBlock.default_state());
        for _ in 0..200 {
            level.run(spreading_tick, pos);
        }
        assert_eq!(level.count(Block::GrassBlock), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_distant_natural_leaves_decay() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 70, 0);
        let leaves = Block::OakLeaves.default_state();
        level.put(pos, leaves.with_distance(6).unwrap());
        level.run(leaves_tick, pos);
        assert_eq!(level.state(pos).block(), Block::OakLeaves);
        level.put(
            pos,
            leaves
                .with_distance(7)
                .unwrap()
                .with_persistent(true)
                .unwrap(),
        );
        level.run(leaves_tick, pos);
        assert_eq!(level.state(pos).block(), Block::OakLeaves);
        level.put(
            pos,
            leaves
                .with_distance(7)
                .unwrap()
                .with_persistent(false)
                .unwrap(),
        );
        level.run(leaves_tick, pos);
        assert!(tuff_world_model::chunk::is_air(level.state(pos)));
        assert_eq!(level.destroyed.len(), 1);
    }
}

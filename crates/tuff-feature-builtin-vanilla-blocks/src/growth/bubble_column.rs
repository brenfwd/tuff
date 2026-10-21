//! Bubble columns: a full water source over soul sand or magma turns
//! into a column a second later, the column climbs through the water
//! sources above it, and it turns back into water once whatever held it
//! up goes.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::tag::block::{
    ENABLES_BUBBLE_COLUMN_DRAG_DOWN, ENABLES_BUBBLE_COLUMN_PUSH_UP,
};
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::redstone::UPDATE_CLIENTS;
use tuff_world_model::BlockPos;

use super::{Behaviours, GrowthLevel, GrowthLevelExt, below};

/// Ticks between water over a column base changing and the column forming.
const FORM_DELAY: u32 = 20;
/// Ticks between a column's surroundings changing and it re-checking.
const CHECK_DELAY: u32 = 5;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_scheduled(Block::Water, water_tick);
    behaviours.register_placed(Block::Water, water_placed);
    behaviours.register_neighbour(Block::Water, water_neighbour);
    behaviours.register_scheduled(Block::BubbleColumn, column_tick);
    behaviours.register_neighbour(Block::BubbleColumn, column_neighbour);
}

fn is_base(state: BlockState) -> bool {
    ENABLES_BUBBLE_COLUMN_PUSH_UP.contains(&state.block())
        || ENABLES_BUBBLE_COLUMN_DRAG_DOWN.contains(&state.block())
}

fn is_full_water_source(state: BlockState) -> bool {
    state.block() == Block::Water && state.get("level") == Some("0")
}

/// Whether a column may stand in `state`: a column already, or a full
/// water source block.
pub fn can_occupy(state: BlockState) -> bool {
    state.block() == Block::BubbleColumn || is_full_water_source(state)
}

fn column(drag: bool) -> BlockState {
    let drag = if drag { "true" } else { "false" };
    Block::BubbleColumn
        .default_state()
        .with("drag", drag)
        .expect("bubble columns have a drag property")
}

/// What stands at a position holding `occupied` over `below`.
fn column_state(below: BlockState, occupied: BlockState) -> BlockState {
    if below.block() == Block::BubbleColumn {
        below
    } else if ENABLES_BUBBLE_COLUMN_PUSH_UP.contains(&below.block()) {
        column(false)
    } else if ENABLES_BUBBLE_COLUMN_DRAG_DOWN.contains(&below.block()) {
        column(true)
    } else if occupied.block() == Block::BubbleColumn {
        Block::Water.default_state()
    } else {
        occupied
    }
}

/// Makes `pos` and the column-holding blocks above it what the block
/// below `pos` gives them, stopping at the first one already right.
pub fn update_column(level: &mut dyn GrowthLevel, pos: BlockPos) {
    let Some(occupied) = level.block(pos) else {
        return;
    };
    if !can_occupy(occupied) {
        return;
    }
    let state = column_state(level.state(below(pos)), occupied);
    level.set(pos, state, UPDATE_CLIENTS);
    let mut at = BlockPos::new(pos.x, pos.y + 1, pos.z);
    while let Some(here) = level.block(at)
        && can_occupy(here)
        && here != state
    {
        level.set(at, state, UPDATE_CLIENTS);
        at.y += 1;
    }
}

fn schedule_form(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState, below: BlockState) {
    if is_full_water_source(state) && is_base(below) {
        level.schedule_tick(pos, Block::Water, FORM_DELAY);
    }
}

fn water_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if is_full_water_source(state) {
        update_column(level, pos);
    }
}

fn water_placed(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let under = level.state(below(pos));
    schedule_form(level, pos, state, under);
}

fn water_neighbour(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
    direction: Facing,
    neighbour: BlockState,
) {
    if direction == Facing::Down {
        schedule_form(level, pos, state, neighbour);
    }
}

fn column_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _: BlockState) {
    update_column(level, pos);
}

fn column_neighbour(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    _: BlockState,
    direction: Facing,
    neighbour: BlockState,
) {
    let under = level.state(below(pos));
    let survives = under.block() == Block::BubbleColumn || is_base(under);
    if !survives
        || direction == Facing::Down
        || (direction == Facing::Up
            && neighbour.block() != Block::BubbleColumn
            && can_occupy(neighbour))
    {
        level.schedule_tick(pos, Block::BubbleColumn, CHECK_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::growth::testing::TestLevel;

    fn pool(base: Block) -> TestLevel {
        let mut level = TestLevel::new();
        level.put(BlockPos::new(0, 60, 0), base.default_state());
        level.fill(
            BlockPos::new(0, 61, 0),
            BlockPos::new(0, 64, 0),
            Block::Water.default_state(),
        );
        level
    }

    fn column_at(level: &TestLevel, y: i32) -> Option<bool> {
        let state = level.state(BlockPos::new(0, y, 0));
        (state.block() == Block::BubbleColumn).then(|| state.get("drag") == Some("true"))
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_over_a_base_schedules_and_grows_a_column_to_the_surface() {
        for (base, drag) in [(Block::SoulSand, false), (Block::MagmaBlock, true)] {
            let mut level = pool(base);
            let water = BlockPos::new(0, 61, 0);
            water_placed(&mut level, water, Block::Water.default_state());
            assert_eq!(level.ticks, vec![(water, Block::Water, FORM_DELAY)]);
            level.run(water_tick, water);
            for y in 61..=64 {
                assert_eq!(column_at(&level, y), Some(drag), "y {y} over {base:?}");
            }
            assert!(
                level
                    .block(BlockPos::new(0, 65, 0))
                    .is_some_and(|s| s.block() == Block::Air)
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn flowing_water_and_other_bases_do_not_form_columns() {
        let mut level = pool(Block::Stone);
        let water = BlockPos::new(0, 61, 0);
        water_placed(&mut level, water, Block::Water.default_state());
        assert!(level.ticks.is_empty());
        let flowing = Block::Water.default_state().with("level", "3").unwrap();
        let mut level = pool(Block::SoulSand);
        level.put(water, flowing);
        water_placed(&mut level, water, flowing);
        level.run(water_tick, water);
        assert!(level.ticks.is_empty());
        assert_eq!(level.count(Block::BubbleColumn), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_column_turns_back_into_water_when_its_base_goes() {
        let mut level = pool(Block::SoulSand);
        level.run(water_tick, BlockPos::new(0, 61, 0));
        assert_eq!(level.count(Block::BubbleColumn), 4);
        let bottom = BlockPos::new(0, 61, 0);
        level.put(BlockPos::new(0, 60, 0), Block::Stone.default_state());
        column_neighbour(
            &mut level,
            bottom,
            column(false),
            Facing::Down,
            Block::Stone.default_state(),
        );
        assert_eq!(
            level.ticks,
            vec![(bottom, Block::BubbleColumn, CHECK_DELAY)]
        );
        level.run(column_tick, bottom);
        assert_eq!(level.count(Block::BubbleColumn), 0);
        assert_eq!(level.count(Block::Water), 4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_new_base_flips_the_drag_of_the_whole_column() {
        let mut level = pool(Block::SoulSand);
        level.run(water_tick, BlockPos::new(0, 61, 0));
        level.put(BlockPos::new(0, 60, 0), Block::MagmaBlock.default_state());
        level.run(column_tick, BlockPos::new(0, 61, 0));
        for y in 61..=64 {
            assert_eq!(column_at(&level, y), Some(true));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_column_rechecks_when_water_forms_above_it_but_not_air() {
        let mut level = pool(Block::SoulSand);
        level.run(water_tick, BlockPos::new(0, 61, 0));
        let top = BlockPos::new(0, 64, 0);
        column_neighbour(
            &mut level,
            top,
            column(false),
            Facing::Up,
            Block::Air.default_state(),
        );
        assert!(level.ticks.is_empty());
        column_neighbour(
            &mut level,
            top,
            column(false),
            Facing::Up,
            Block::Water.default_state(),
        );
        assert_eq!(level.ticks, vec![(top, Block::BubbleColumn, CHECK_DELAY)]);
    }
}

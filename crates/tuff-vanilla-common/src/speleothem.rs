//! Speleothems (pointed dripstone, sulfur spikes): stalagmites point up
//! from a floor, stalactites hang from a ceiling. A stalagmite that loses
//! its floor breaks; a stalactite that loses its ceiling falls, the whole
//! column below it too, and its tip hurts whatever it lands on.

use tuff_vanilla_data::block::{Facing, Thickness, VerticalDirection};
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::placement::{Level, step};
use tuff_world_model::BlockPos;

/// Ticks a stalactite waits after losing its ceiling before it falls.
pub const STALACTITE_FALL_DELAY: u32 = 2;
/// Ticks a stalagmite waits after losing its floor before it breaks.
pub const STALAGMITE_BREAK_DELAY: u32 = 1;
/// A falling tip's damage per block fallen is this times the column's
/// length (at least [`MIN_DAMAGE_SIZE`]), capped at [`MAX_DAMAGE`].
const DAMAGE_PER_FALL_DISTANCE_AND_SIZE: f32 = 1.0;
const MIN_DAMAGE_SIZE: i32 = 6;
pub const MAX_DAMAGE: i32 = 40;

const LEVEL_EVENT_DRIPSTONE_LAND: i32 = 1045;
const LEVEL_EVENT_SULFUR_SPIKE_LAND: i32 = 1052;

pub fn is_speleothem(block: Block) -> bool {
    tuff_datapack::tags::is(tags::key::SPELEOTHEMS, block)
}

fn facing(direction: VerticalDirection) -> Facing {
    match direction {
        VerticalDirection::Up => Facing::Up,
        VerticalDirection::Down => Facing::Down,
    }
}

fn vertical(facing: Facing) -> VerticalDirection {
    if facing == Facing::Up {
        VerticalDirection::Up
    } else {
        VerticalDirection::Down
    }
}

/// The direction a speleothem's tip points, `None` for other blocks.
pub fn tip(state: BlockState) -> Option<Facing> {
    if !is_speleothem(state.block()) {
        return None;
    }
    state.vertical_direction().map(facing)
}

fn pointing(state: BlockState, direction: Facing) -> bool {
    tip(state) == Some(direction)
}

pub fn is_stalactite(state: BlockState) -> bool {
    pointing(state, Facing::Down)
}

fn is_tip(state: BlockState) -> bool {
    matches!(
        state.thickness(),
        Some(Thickness::Tip | Thickness::TipMerge)
    )
}

/// Whether `block` pointing `tip` at `pos` is held: a sturdy face behind
/// it, or more of the same speleothem pointing the same way.
pub fn valid_placement(block: Block, pos: BlockPos, tip: Facing, level: Level) -> bool {
    let behind = level(step(pos, tip.opposite()));
    behind.is_face_sturdy(tip, Support::Full) || (behind.block() == block && pointing(behind, tip))
}

pub fn can_survive(state: BlockState, pos: BlockPos, level: Level) -> bool {
    tip(state).is_none_or(|tip| valid_placement(state.block(), pos, tip, level))
}

fn thickness(block: Block, pos: BlockPos, tip: Facing, merge: bool, level: Level) -> Thickness {
    let base = tip.opposite();
    let in_front = level(step(pos, tip));
    if pointing(in_front, base) && in_front.block() == block {
        return if merge || in_front.thickness() == Some(Thickness::TipMerge) {
            Thickness::TipMerge
        } else {
            Thickness::Tip
        };
    }
    if !pointing(in_front, tip) {
        return Thickness::Tip;
    }
    if is_tip(in_front) {
        return Thickness::Frustum;
    }
    if !pointing(level(step(pos, base)), tip) {
        Thickness::Base
    } else {
        Thickness::Middle
    }
}

/// The state `block` places at `pos` for a player looking up (`pitch`
/// below zero) or down, or `None` when neither direction is held.
pub fn placement(
    block: Block,
    pos: BlockPos,
    pitch: f32,
    sneaking: bool,
    waterlogged: bool,
    level: Level,
) -> Option<BlockState> {
    let preferred = if pitch < 0.0 {
        Facing::Down
    } else {
        Facing::Up
    };
    let tip = [preferred, preferred.opposite()]
        .into_iter()
        .find(|tip| valid_placement(block, pos, *tip, level))?;
    let thickness = thickness(block, pos, tip, !sneaking, level);
    block
        .default_state()
        .with_vertical_direction(vertical(tip))?
        .with_thickness(thickness)?
        .with_waterlogged(waterlogged)
}

/// The shape `state` takes when its neighbour in `direction` changes. A
/// speleothem never breaks here: losing support schedules a tick instead
/// (see [`shape_tick`]).
pub fn update_shape(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    level: Level,
) -> BlockState {
    let Some(tip) = tip(state) else {
        return state;
    };
    if direction.is_horizontal() {
        return state;
    }
    if direction == tip.opposite() && !can_survive(state, pos, level) {
        return state;
    }
    let merge = state.thickness() == Some(Thickness::TipMerge);
    state
        .with_thickness(thickness(state.block(), pos, tip, merge, level))
        .unwrap_or(state)
}

/// The tick delay a change of the neighbour in `direction` schedules:
/// losing the block it hangs from or stands on.
pub fn shape_tick(
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    level: Level,
) -> Option<u32> {
    let tip = tip(state)?;
    (direction == tip.opposite() && !can_survive(state, pos, level)).then_some(
        if tip == Facing::Down {
            STALACTITE_FALL_DELAY
        } else {
            STALAGMITE_BREAK_DELAY
        },
    )
}

/// What a speleothem's scheduled tick does.
#[derive(Debug, Clone, PartialEq)]
pub enum TickOutcome {
    /// An unsupported stalagmite breaks and drops.
    Break,
    /// The stalactite column starting here falls, top first: each block
    /// with the damage per block fallen it deals on landing (tips only).
    Fall(Vec<(BlockPos, BlockState, Option<f32>)>),
}

pub fn tick(state: BlockState, pos: BlockPos, level: Level) -> TickOutcome {
    if pointing(state, Facing::Up) && !can_survive(state, pos, level) {
        return TickOutcome::Break;
    }
    let mut column = Vec::new();
    let mut at = pos;
    let mut current = state;
    while is_stalactite(current) {
        if is_tip(current) {
            let size = (1 + pos.y - at.y).max(MIN_DAMAGE_SIZE);
            let damage = DAMAGE_PER_FALL_DISTANCE_AND_SIZE * size as f32;
            column.push((at, current, Some(damage)));
            break;
        }
        column.push((at, current, None));
        at = step(at, Facing::Down);
        current = level(at);
    }
    TickOutcome::Fall(column)
}

/// The level event a falling speleothem plays when it breaks on landing.
pub fn landing_event(block: Block) -> Option<i32> {
    match block {
        Block::PointedDripstone => Some(LEVEL_EVENT_DRIPSTONE_LAND),
        Block::SulfurSpike => Some(LEVEL_EVENT_SULFUR_SPIKE_LAND),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const AIR: BlockState = Block::Air.default_state();

    fn world(blocks: &[(BlockPos, BlockState)]) -> impl Fn(BlockPos) -> BlockState + use<> {
        let map: HashMap<BlockPos, BlockState> = blocks.iter().copied().collect();
        move |pos| map.get(&pos).copied().unwrap_or(AIR)
    }

    fn drip(tip: VerticalDirection, thickness: Thickness) -> BlockState {
        Block::PointedDripstone
            .default_state()
            .with_vertical_direction(tip)
            .unwrap()
            .with_thickness(thickness)
            .unwrap()
    }

    const CEILING: i32 = 10;

    /// A stalactite of `thickness` states hanging from stone at `CEILING`,
    /// top first.
    fn stalactite(thickness: &[Thickness]) -> Vec<(BlockPos, BlockState)> {
        let mut blocks = vec![(BlockPos::new(0, CEILING, 0), Block::Stone.default_state())];
        for (i, t) in thickness.iter().enumerate() {
            blocks.push((
                BlockPos::new(0, CEILING - 1 - i as i32, 0),
                drip(VerticalDirection::Down, *t),
            ));
        }
        blocks
    }

    #[tuff_core::watchdog]
    #[test]
    fn looking_up_at_a_ceiling_places_a_stalactite() {
        let pos = BlockPos::new(0, CEILING - 1, 0);
        let level = world(&stalactite(&[]));
        let state = placement(Block::PointedDripstone, pos, -45.0, false, false, &level).unwrap();
        assert_eq!(state.vertical_direction(), Some(VerticalDirection::Down));
        assert_eq!(state.thickness(), Some(Thickness::Tip));
        // Looking down there is no floor, so it falls back to hanging.
        let state = placement(Block::PointedDripstone, pos, 45.0, false, false, &level).unwrap();
        assert_eq!(state.vertical_direction(), Some(VerticalDirection::Down));
        assert!(
            placement(
                Block::PointedDripstone,
                BlockPos::new(5, 5, 5),
                0.0,
                false,
                false,
                &level
            )
            .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn thickness_follows_the_column() {
        let blocks = stalactite(&[Thickness::Tip]);
        let level = world(&blocks);
        let below = BlockPos::new(0, CEILING - 2, 0);
        let state = placement(Block::PointedDripstone, below, -45.0, false, false, &level).unwrap();
        assert_eq!(state.thickness(), Some(Thickness::Tip));
        let top = BlockPos::new(0, CEILING - 1, 0);
        let mut blocks = blocks;
        blocks.push((below, state));
        let level = world(&blocks);
        let reshaped = update_shape(level(top), top, Facing::Down, &level);
        assert_eq!(reshaped.thickness(), Some(Thickness::Frustum));
    }

    #[tuff_core::watchdog]
    #[test]
    fn losing_the_ceiling_schedules_a_fall_instead_of_breaking() {
        let mut blocks = stalactite(&[Thickness::Frustum, Thickness::Tip]);
        blocks.remove(0);
        let level = world(&blocks);
        let top = BlockPos::new(0, CEILING - 1, 0);
        let state = level(top);
        assert!(!can_survive(state, top, &level));
        assert_eq!(update_shape(state, top, Facing::Up, &level), state);
        assert_eq!(
            shape_tick(state, top, Facing::Up, &level),
            Some(STALACTITE_FALL_DELAY)
        );
        assert_eq!(shape_tick(state, top, Facing::Down, &level), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_whole_column_falls_and_only_its_tip_hurts() {
        let mut blocks = stalactite(&[
            Thickness::Base,
            Thickness::Middle,
            Thickness::Frustum,
            Thickness::Tip,
        ]);
        blocks.remove(0);
        let level = world(&blocks);
        let top = BlockPos::new(0, CEILING - 1, 0);
        let TickOutcome::Fall(column) = tick(level(top), top, &level) else {
            panic!("expected a fall");
        };
        assert_eq!(column.len(), 4);
        assert!(column[..3].iter().all(|(_, _, hurt)| hurt.is_none()));
        assert_eq!(column[3].0, BlockPos::new(0, CEILING - 4, 0));
        assert_eq!(column[3].2, Some(6.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_long_column_hurts_more() {
        let mut thickness = vec![Thickness::Middle; 7];
        thickness.push(Thickness::Tip);
        let mut blocks = stalactite(&thickness);
        blocks.remove(0);
        let level = world(&blocks);
        let top = BlockPos::new(0, CEILING - 1, 0);
        let TickOutcome::Fall(column) = tick(level(top), top, &level) else {
            panic!("expected a fall");
        };
        assert_eq!(column.last().unwrap().2, Some(8.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unsupported_stalagmite_breaks() {
        let pos = BlockPos::new(0, 1, 0);
        let state = drip(VerticalDirection::Up, Thickness::Tip);
        let level = world(&[(pos, state)]);
        assert_eq!(tick(state, pos, &level), TickOutcome::Break);
        assert_eq!(
            shape_tick(state, pos, Facing::Down, &level),
            Some(STALAGMITE_BREAK_DELAY)
        );
    }
}

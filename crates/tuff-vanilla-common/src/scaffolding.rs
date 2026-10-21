//! Scaffolding stability: each block's `distance` counts horizontal steps
//! from a column standing on sturdy ground. A block at the maximum distance
//! has no support and falls (or breaks, if it only just lost support).

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::placement::{Level, step};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::ChunkShape;

pub const MAX_DISTANCE: u8 = 7;
/// Ticks between a change around scaffolding and its stability check.
pub const TICK_DELAY: u32 = 1;

pub fn is_scaffolding(block: Block) -> bool {
    block == Block::Scaffolding
}

/// Scaffolding used on scaffolding at `clicked` extends the structure:
/// up the column, or the way the player faces when the top was clicked;
/// sneaking goes out of the clicked face instead. It passes through up to
/// six more scaffolding blocks sideways (any number upward) and lands on
/// the first other block, which must be replaceable. `None` places
/// nothing.
pub fn extension(
    clicked: BlockPos,
    face: Facing,
    horizontal: Facing,
    sneaking: bool,
    shape: ChunkShape,
    level: Level,
) -> Option<(BlockPos, Facing)> {
    let direction = match (sneaking, face) {
        (true, face) => face,
        (false, Facing::Up) => horizontal,
        (false, _) => Facing::Up,
    };
    let mut pos = step(clicked, direction);
    let mut sideways = 0;
    while sideways < MAX_DISTANCE {
        if !shape.contains_y(pos.y) {
            return None;
        }
        let state = level(pos);
        if !is_scaffolding(state.block()) {
            return state.is_replaceable().then_some((pos, direction));
        }
        pos = step(pos, direction);
        if direction.is_horizontal() {
            sideways += 1;
        }
    }
    None
}

const STABLE: [[f32; 6]; 5] = [
    [0.0, 0.875, 0.0, 1.0, 1.0, 1.0],
    [0.0, 0.0, 0.0, 0.125, 1.0, 0.125],
    [0.875, 0.0, 0.0, 1.0, 1.0, 0.125],
    [0.0, 0.0, 0.875, 0.125, 1.0, 1.0],
    [0.875, 0.0, 0.875, 1.0, 1.0, 1.0],
];
const UNSTABLE_BOTTOM: [[f32; 6]; 1] = [[0.0, 0.0, 0.0, 1.0, 0.125, 1.0]];
const ABOVE_EPSILON: f64 = 1.0e-5;

/// The collision boxes scaffolding at `pos` has for a thing whose box
/// bottom is at `bottom`: its top and legs while it stands above and is
/// not climbing down, the bottom slab of a hanging bottom block while it
/// is at least level with it, else nothing (so it climbs or falls
/// through).
pub fn collision_boxes(
    state: BlockState,
    pos: BlockPos,
    bottom: f64,
    descending: bool,
) -> &'static [[f32; 6]] {
    let y = f64::from(pos.y);
    if bottom > y + 1.0 - ABOVE_EPSILON && !descending {
        &STABLE
    } else if state.distance().is_some_and(|d| d != 0)
        && state.bottom() == Some(true)
        && bottom > y - ABOVE_EPSILON
    {
        &UNSTABLE_BOTTOM
    } else {
        &[]
    }
}

/// How far `pos` is from supported scaffolding: 0 on a sturdy top face,
/// the value of the scaffolding below, or one more than the nearest
/// horizontal neighbour's (below-block distance wins when smaller).
pub fn distance(pos: BlockPos, level: Level) -> u8 {
    let below_pos = step(pos, Facing::Down);
    let below = level(below_pos);
    let mut distance = MAX_DISTANCE;
    if is_scaffolding(below.block()) {
        distance = below.distance().unwrap_or(MAX_DISTANCE);
    } else if below.is_face_sturdy(Facing::Up, Support::Full) {
        return 0;
    }
    for side in [Facing::North, Facing::East, Facing::South, Facing::West] {
        let neighbour = level(step(pos, side));
        if is_scaffolding(neighbour.block()) {
            let through = neighbour
                .distance()
                .unwrap_or(MAX_DISTANCE)
                .saturating_add(1);
            distance = distance.min(through);
            if distance == 1 {
                break;
            }
        }
    }
    distance
}

fn is_bottom(pos: BlockPos, level: Level, distance: u8) -> bool {
    distance > 0 && !is_scaffolding(level(step(pos, Facing::Down)).block())
}

/// `state` with the distance and bottom flag its surroundings give it.
pub fn with_support(state: BlockState, pos: BlockPos, level: Level) -> BlockState {
    let distance = distance(pos, level);
    state
        .with_distance(distance)
        .and_then(|s| s.with_bottom(is_bottom(pos, level, distance)))
        .unwrap_or(state)
}

pub fn can_survive(pos: BlockPos, level: Level) -> bool {
    distance(pos, level) < MAX_DISTANCE
}

/// What a scaffolding block's scheduled tick does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Keep,
    Set(BlockState),
    /// Already unsupported before this tick: it falls as this state.
    Fall(BlockState),
    /// Only just lost support: it breaks and drops.
    Destroy,
}

pub fn tick(state: BlockState, pos: BlockPos, level: Level) -> TickOutcome {
    let new = with_support(state, pos, level);
    if new.distance() == Some(MAX_DISTANCE) {
        return if state.distance() == Some(MAX_DISTANCE) {
            TickOutcome::Fall(new)
        } else {
            TickOutcome::Destroy
        };
    }
    if new == state {
        TickOutcome::Keep
    } else {
        TickOutcome::Set(new)
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

    fn scaffold(distance: u8) -> BlockState {
        Block::Scaffolding
            .default_state()
            .with_distance(distance)
            .unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn distance_is_zero_on_sturdy_ground_and_grows_sideways() {
        let origin = BlockPos::new(0, 1, 0);
        let level = world(&[
            (BlockPos::new(0, 0, 0), Block::Stone.default_state()),
            (origin, scaffold(0)),
        ]);
        assert_eq!(distance(origin, &level), 0);
        assert_eq!(distance(BlockPos::new(1, 1, 0), &level), 1);
        assert_eq!(distance(BlockPos::new(0, 2, 0), &level), 0);
        assert_eq!(distance(BlockPos::new(5, 5, 5), &level), MAX_DISTANCE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_non_sturdy_floor_gives_no_support() {
        let pos = BlockPos::new(0, 1, 0);
        let level = world(&[(BlockPos::new(0, 0, 0), Block::OakFence.default_state())]);
        assert_eq!(distance(pos, &level), MAX_DISTANCE);
        assert!(!can_survive(pos, &level));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_seventh_block_out_is_unsupported() {
        let mut blocks = vec![(BlockPos::new(0, 0, 0), Block::Stone.default_state())];
        for x in 0..7 {
            blocks.push((BlockPos::new(x, 1, 0), scaffold(x as u8)));
        }
        let level = world(&blocks);
        assert_eq!(distance(BlockPos::new(6, 1, 0), &level), 6);
        assert_eq!(distance(BlockPos::new(7, 1, 0), &level), MAX_DISTANCE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn losing_support_breaks_first_then_falls_once_already_unsupported() {
        let pos = BlockPos::new(0, 1, 0);
        let level = world(&[]);
        assert_eq!(tick(scaffold(0), pos, &level), TickOutcome::Destroy);
        let unsupported = scaffold(MAX_DISTANCE);
        match tick(unsupported, pos, &level) {
            TickOutcome::Fall(state) => assert_eq!(state.distance(), Some(MAX_DISTANCE)),
            other => panic!("expected a fall, got {other:?}"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn standing_above_collides_with_the_top_and_inside_falls_through() {
        let pos = BlockPos::new(0, 4, 0);
        let rooted = scaffold(0);
        assert_eq!(collision_boxes(rooted, pos, 5.0, false).len(), 5);
        assert!(collision_boxes(rooted, pos, 5.0, true).is_empty());
        assert!(collision_boxes(rooted, pos, 4.5, false).is_empty());
        let hanging = scaffold(2).with_bottom(true).unwrap();
        assert_eq!(collision_boxes(hanging, pos, 4.0, false), &UNSTABLE_BOTTOM);
        assert_eq!(collision_boxes(hanging, pos, 4.5, true), &UNSTABLE_BOTTOM);
        assert!(collision_boxes(hanging, pos, 3.5, false).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn extension_climbs_the_column_or_runs_along_the_top() {
        let shape = ChunkShape::new(0, 16);
        let level = world(&[
            (BlockPos::new(0, 1, 0), scaffold(0)),
            (BlockPos::new(0, 2, 0), scaffold(0)),
        ]);
        let up = extension(
            BlockPos::new(0, 1, 0),
            Facing::North,
            Facing::South,
            false,
            shape,
            &level,
        );
        assert_eq!(up, Some((BlockPos::new(0, 3, 0), Facing::Up)));
        let along = extension(
            BlockPos::new(0, 2, 0),
            Facing::Up,
            Facing::East,
            false,
            shape,
            &level,
        );
        assert_eq!(along, Some((BlockPos::new(1, 2, 0), Facing::East)));
        let tall = world(&[(BlockPos::new(0, 15, 0), scaffold(0))]);
        let top = extension(
            BlockPos::new(0, 15, 0),
            Facing::North,
            Facing::South,
            false,
            shape,
            &tall,
        );
        assert_eq!(top, None, "above the build limit");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tick_updates_distance_and_bottom() {
        let pos = BlockPos::new(1, 1, 0);
        let level = world(&[
            (BlockPos::new(0, 0, 0), Block::Stone.default_state()),
            (BlockPos::new(0, 1, 0), scaffold(0)),
        ]);
        match tick(scaffold(3), pos, &level) {
            TickOutcome::Set(state) => {
                assert_eq!(state.distance(), Some(1));
                assert_eq!(state.bottom(), Some(true));
            }
            other => panic!("expected a new state, got {other:?}"),
        }
    }
}

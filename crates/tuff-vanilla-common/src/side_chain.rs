//! Powered shelves side by side link into a chain of up to three, so one
//! click swaps the hotbar with all of them. Each shelf keeps its place in
//! the chain in `side_chain`.

use std::collections::VecDeque;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{BlockState, tag};

use tuff_core::world::redstone::{RedstoneLevel, UPDATE_ALL};
use tuff_world_model::BlockPos;

pub const MAX_CHAIN_LENGTH: i32 = 3;
const PROPERTY: &str = "side_chain";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Unconnected,
    Right,
    Center,
    Left,
}

impl Part {
    pub fn of(state: BlockState) -> Option<Self> {
        match state.get(PROPERTY)? {
            "unconnected" => Some(Self::Unconnected),
            "right" => Some(Self::Right),
            "center" => Some(Self::Center),
            "left" => Some(Self::Left),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Unconnected => "unconnected",
            Self::Right => "right",
            Self::Center => "center",
            Self::Left => "left",
        }
    }

    pub const fn is_connected(self) -> bool {
        !matches!(self, Self::Unconnected)
    }

    const fn connects_towards(self, end: Self) -> bool {
        matches!(self, Self::Center) || self as u8 == end as u8
    }

    const fn is_chain_end(self) -> bool {
        !matches!(self, Self::Center)
    }

    pub const fn when_connected_to_right(self) -> Self {
        match self {
            Self::Unconnected | Self::Left => Self::Left,
            Self::Right | Self::Center => Self::Center,
        }
    }

    pub const fn when_connected_to_left(self) -> Self {
        match self {
            Self::Unconnected | Self::Right => Self::Right,
            Self::Center | Self::Left => Self::Center,
        }
    }

    pub const fn when_disconnected_from_right(self) -> Self {
        match self {
            Self::Unconnected | Self::Left => Self::Unconnected,
            Self::Right | Self::Center => Self::Right,
        }
    }

    pub const fn when_disconnected_from_left(self) -> Self {
        match self {
            Self::Unconnected | Self::Right => Self::Unconnected,
            Self::Center | Self::Left => Self::Left,
        }
    }
}

/// Whether `state` is a powered wooden shelf, which links with its
/// neighbours.
pub fn connectable(state: BlockState) -> bool {
    tag::block::WOODEN_SHELVES.contains(&state.block()) && state.get("powered") == Some("true")
}

fn step(pos: BlockPos, direction: Facing, steps: i32) -> BlockPos {
    let (dx, dy, dz) = direction.step();
    BlockPos::new(pos.x + dx * steps, pos.y + dy * steps, pos.z + dz * steps)
}

fn left(pos: BlockPos, facing: Facing, steps: i32) -> BlockPos {
    step(pos, facing.clockwise(), steps)
}

fn right(pos: BlockPos, facing: Facing, steps: i32) -> BlockPos {
    step(pos, facing.counter_clockwise(), steps)
}

fn neighbor_part(
    read: &dyn Fn(BlockPos) -> Option<BlockState>,
    pos: BlockPos,
    facing: Facing,
) -> Option<Part> {
    let state = read(pos)?;
    (connectable(state) && state.facing() == Some(facing))
        .then(|| Part::of(state))
        .flatten()
}

/// The shelves in the chain around `pos`, left to right; empty when `pos`
/// is not a powered shelf.
pub fn connected(read: &dyn Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> Vec<BlockPos> {
    let Some(state) = read(pos) else {
        return Vec::new();
    };
    if !connectable(state) {
        return Vec::new();
    }
    let Some(facing) = state.facing() else {
        return Vec::new();
    };
    let mut results = VecDeque::from([pos]);
    let walk =
        |side: fn(BlockPos, Facing, i32) -> BlockPos, end: Part, add: &mut dyn FnMut(BlockPos)| {
            for steps in 1..MAX_CHAIN_LENGTH {
                let at = side(pos, facing, steps);
                let part = neighbor_part(read, at, facing);
                if part.is_some_and(|p| p.connects_towards(end)) {
                    add(at);
                }
                if part.is_none_or(Part::is_chain_end) {
                    return;
                }
            }
        };
    walk(left, Part::Left, &mut |p| results.push_front(p));
    walk(right, Part::Right, &mut |p| results.push_back(p));
    results.into()
}

fn set_part(level: &mut dyn RedstoneLevel, pos: BlockPos, part: Part) {
    let state = level.block(pos);
    if Part::of(state) != Some(part)
        && let Some(next) = state.with(PROPERTY, part.name())
    {
        level.set_block(pos, next, UPDATE_ALL);
    }
}

fn change_neighbor(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    facing: Facing,
    change: fn(Part) -> Part,
) {
    let part = {
        let read = |p| Some(level.block(p));
        neighbor_part(&read, pos, facing)
    };
    if let Some(part) = part {
        set_part(level, pos, change(part));
    }
}

/// A shelf stopped being powered or left: its neighbours drop their link
/// to it.
pub fn powered_down(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let Some(facing) = state.facing() else {
        return;
    };
    change_neighbor(
        level,
        left(pos, facing, 1),
        facing,
        Part::when_disconnected_from_right,
    );
    change_neighbor(
        level,
        right(pos, facing, 1),
        facing,
        Part::when_disconnected_from_left,
    );
}

/// A shelf became powered: it joins the chains on either side if they fit.
pub fn powered_up(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    state: BlockState,
    old: BlockState,
) {
    if !connectable(state) {
        return;
    }
    let was_connected = connectable(old) && Part::of(old).is_some_and(Part::is_connected);
    if Part::of(state).is_some_and(Part::is_connected) || was_connected {
        return;
    }
    let Some(facing) = state.facing() else {
        return;
    };
    let (left_pos, right_pos) = (left(pos, facing, 1), right(pos, facing, 1));
    let (chain_left, chain_right) = {
        let read = |p| Some(level.block(p));
        let side = |at: BlockPos| {
            if neighbor_part(&read, at, facing).is_some() {
                connected(&read, at).len() as i32
            } else {
                0
            }
        };
        (side(left_pos), side(right_pos))
    };
    let fits = |extra: i32, current: i32| extra > 0 && current + extra <= MAX_CHAIN_LENGTH;
    let mut part = Part::Unconnected;
    let mut length = 1;
    if fits(chain_left, 1) {
        part = part.when_connected_to_left();
        change_neighbor(level, left_pos, facing, Part::when_connected_to_right);
        length = 1 + chain_left;
    }
    if fits(chain_right, length) {
        part = part.when_connected_to_right();
        change_neighbor(level, right_pos, facing, Part::when_connected_to_left);
    }
    set_part(level, pos, part);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn parts_follow_the_connection_table() {
        assert_eq!(Part::Unconnected.when_connected_to_right(), Part::Left);
        assert_eq!(Part::Left.when_connected_to_left(), Part::Center);
        assert_eq!(Part::Center.when_disconnected_from_right(), Part::Right);
        assert_eq!(Part::Right.when_disconnected_from_left(), Part::Unconnected);
        assert!(Part::Center.connects_towards(Part::Left));
        assert!(Part::Left.connects_towards(Part::Left));
        assert!(!Part::Right.connects_towards(Part::Left));
    }
}

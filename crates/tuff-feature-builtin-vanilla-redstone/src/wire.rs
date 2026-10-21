//! Redstone wire: its connection shape and power, updated the way the
//! game's default (non-experimental) wire does, including the order its
//! neighbour updates go out in.

use tuff_vanilla_data::block::{Facing, Side};
use tuff_vanilla_data::{Block, BlockKind, BlockState, Support};

use super::{
    BlockBehavior, DIRECTIONS, Reader, RedstoneLevel, RedstoneLevelExt, SignalBehavior, Signals,
    UPDATE_CLIENTS, behaviour,
};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

mod experimental;

pub struct Wire;

fn side(state: BlockState, direction: Facing) -> Side {
    match direction {
        Facing::North => state.north_side(),
        Facing::East => state.east_side(),
        Facing::South => state.south_side(),
        Facing::West => state.west_side(),
        _ => None,
    }
    .unwrap_or(Side::None)
}

fn with_side(state: BlockState, direction: Facing, side: Side) -> BlockState {
    match direction {
        Facing::North => state.with_north_side(side),
        Facing::East => state.with_east_side(side),
        Facing::South => state.with_south_side(side),
        Facing::West => state.with_west_side(side),
        _ => None,
    }
    .unwrap_or(state)
}

fn connected(state: BlockState, direction: Facing) -> bool {
    side(state, direction) != Side::None
}

fn is_cross(state: BlockState) -> bool {
    Facing::HORIZONTAL.into_iter().all(|d| connected(state, d))
}

fn is_dot(state: BlockState) -> bool {
    !Facing::HORIZONTAL.into_iter().any(|d| connected(state, d))
}

fn power(state: BlockState) -> u8 {
    state.power().unwrap_or(0)
}

fn with_power(state: BlockState, power: u8) -> BlockState {
    state.with_power(power).unwrap_or(state)
}

fn cross(power: u8) -> BlockState {
    Facing::HORIZONTAL.into_iter().fold(
        with_power(Block::RedstoneWire.default_state(), power),
        |state, d| with_side(state, d, Side::Side),
    )
}

fn can_survive_on(below: BlockState) -> bool {
    below.is_face_sturdy(Facing::Up, Support::Full) || below.block() == Block::Hopper
}

/// Whether wire connects to the block at `pos`, seen from a wire in
/// `direction` (or above or below for `None`).
fn connects_to(level: &dyn Reader, pos: BlockPos, direction: Option<Facing>) -> bool {
    let state = level.block(pos);
    behaviour(state.block()).wire_connects(state, direction)
}

fn connecting_side(
    level: &dyn Reader,
    pos: BlockPos,
    direction: Facing,
    can_connect_up: bool,
) -> Side {
    let beside = step(pos, direction);
    let state = level.block(beside);
    if can_connect_up {
        let placeable_above = state.block().kind() == BlockKind::Trapdoor || can_survive_on(state);
        if placeable_above && connects_to(level, step(beside, Facing::Up), None) {
            return if state.is_face_sturdy(direction.opposite(), Support::Full) {
                Side::Up
            } else {
                Side::Side
            };
        }
    }
    if behaviour(state.block()).wire_connects(state, Some(direction))
        || (!state.is_redstone_conductor() && connects_to(level, step(beside, Facing::Down), None))
    {
        Side::Side
    } else {
        Side::None
    }
}

fn can_connect_up(level: &dyn Reader, pos: BlockPos) -> bool {
    !level.block(step(pos, Facing::Up)).is_redstone_conductor()
}

fn missing_connections(level: &dyn Reader, mut state: BlockState, pos: BlockPos) -> BlockState {
    let up = can_connect_up(level, pos);
    for direction in Facing::HORIZONTAL {
        if !connected(state, direction) {
            state = with_side(state, direction, connecting_side(level, pos, direction, up));
        }
    }
    state
}

/// The connections wire at `pos` takes, starting from `state`: a dot
/// stays a dot, and wire that connects on one axis only extends across
/// the other.
pub fn connection_state(level: &dyn Reader, state: BlockState, pos: BlockPos) -> BlockState {
    let was_dot = is_dot(state);
    let mut state = missing_connections(
        level,
        with_power(Block::RedstoneWire.default_state(), power(state)),
        pos,
    );
    if was_dot && is_dot(state) {
        return state;
    }
    let [north, south, east, west] =
        [Facing::North, Facing::South, Facing::East, Facing::West].map(|d| connected(state, d));
    let north_south_empty = !north && !south;
    let east_west_empty = !east && !west;
    if !west && north_south_empty {
        state = with_side(state, Facing::West, Side::Side);
    }
    if !east && north_south_empty {
        state = with_side(state, Facing::East, Side::Side);
    }
    if !north && east_west_empty {
        state = with_side(state, Facing::North, Side::Side);
    }
    if !south && east_west_empty {
        state = with_side(state, Facing::South, Side::Side);
    }
    state
}

/// The order the game's hash set of positions yields them in: by bucket
/// of a 16-bucket table, then insertion order.
fn hash_order(positions: &mut [BlockPos]) {
    let bucket = |pos: &BlockPos| {
        let hash = pos
            .y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x);
        (hash ^ ((hash as u32) >> 16) as i32) & 15
    };
    positions.sort_by_key(bucket);
}

fn target_strength(level: &dyn Reader, pos: BlockPos) -> u8 {
    let silent = Signals { level, wire: false };
    let block_signal = silent.best_neighbor_signal(pos);
    if block_signal == 15 {
        return 15;
    }
    block_signal.max(incoming_wire_signal(level, pos))
}

fn wire_signal(state: BlockState) -> u8 {
    if state.block() == Block::RedstoneWire {
        power(state)
    } else {
        0
    }
}

fn incoming_wire_signal(level: &dyn Reader, pos: BlockPos) -> u8 {
    incoming_signal_with(level, pos, |_, state| wire_signal(state))
}

/// The strongest signal wire beside `pos` brings it, reading each wire's
/// power through `wire_signal`.
fn incoming_signal_with(
    level: &dyn Reader,
    pos: BlockPos,
    wire_signal: impl Fn(BlockPos, BlockState) -> u8,
) -> u8 {
    let mut signal = 0;
    let above_conducts = level.block(step(pos, Facing::Up)).is_redstone_conductor();
    for direction in Facing::HORIZONTAL {
        let beside = step(pos, direction);
        let state = level.block(beside);
        signal = signal.max(wire_signal(beside, state));
        if state.is_redstone_conductor() && !above_conducts {
            let above = step(beside, Facing::Up);
            signal = signal.max(wire_signal(above, level.block(above)));
        } else if !state.is_redstone_conductor() {
            let below = step(beside, Facing::Down);
            signal = signal.max(wire_signal(below, level.block(below)));
        }
    }
    signal.saturating_sub(1)
}

fn update_power_strength(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let target = target_strength(level, pos);
    if power(state) == target {
        return;
    }
    if level.block(pos) == state {
        level.set_block(pos, with_power(state, target), UPDATE_CLIENTS);
    }
    let mut positions = [pos; 7];
    for (slot, direction) in positions[1..].iter_mut().zip(DIRECTIONS) {
        *slot = step(pos, direction);
    }
    hash_order(&mut positions);
    for at in positions {
        level.update_neighbors_at(at, Block::RedstoneWire);
    }
}

fn check_corner_change_at(level: &mut dyn RedstoneLevel, pos: BlockPos) {
    if level.block(pos).block() != Block::RedstoneWire {
        return;
    }
    level.update_neighbors_at(pos, Block::RedstoneWire);
    for direction in DIRECTIONS {
        level.update_neighbors_at(step(pos, direction), Block::RedstoneWire);
    }
}

fn update_neighbors_of_neighboring_wires(level: &mut dyn RedstoneLevel, pos: BlockPos) {
    for direction in Facing::HORIZONTAL {
        check_corner_change_at(level, step(pos, direction));
    }
    for direction in Facing::HORIZONTAL {
        let beside = step(pos, direction);
        if level.block(beside).is_redstone_conductor() {
            check_corner_change_at(level, step(beside, Facing::Up));
        } else {
            check_corner_change_at(level, step(beside, Facing::Down));
        }
    }
}

impl BlockBehavior for Wire {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        source: Block,
    ) {
        let experimental = level.redstone_experiments();
        if experimental && source == Block::RedstoneWire {
            return;
        }
        if can_survive_on(level.block(step(pos, Facing::Down))) {
            if experimental {
                let orientation = level.incoming_orientation();
                experimental::update_power_strength(level, pos, orientation, false);
            } else {
                update_power_strength(level, pos, state);
            }
        } else {
            level.effect(super::Effect::DropResources(pos, state));
            level.remove_block(pos);
        }
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() == Block::RedstoneWire {
            return;
        }
        if level.redstone_experiments() {
            experimental::update_power_strength(level, pos, None, true);
        } else {
            update_power_strength(level, pos, state);
        }
        for direction in [Facing::Up, Facing::Down] {
            level.update_neighbors_at(step(pos, direction), Block::RedstoneWire);
        }
        update_neighbors_of_neighboring_wires(level, pos);
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if moved {
            return;
        }
        for direction in DIRECTIONS {
            level.update_neighbors_at(step(pos, direction), Block::RedstoneWire);
        }
        if level.redstone_experiments() {
            experimental::update_power_strength(level, pos, None, false);
        } else {
            update_power_strength(level, pos, state);
        }
        update_neighbors_of_neighboring_wires(level, pos);
    }

    fn update_shape(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        let level: &dyn Reader = level;
        Some(match direction {
            Facing::Down if !can_survive_on(neighbour) => Block::Air.default_state(),
            Facing::Down => state,
            Facing::Up => connection_state(level, state, pos),
            _ => {
                let side = connecting_side(level, pos, direction, can_connect_up(level, pos));
                if (side != Side::None) == connected(state, direction) && !is_cross(state) {
                    with_side(state, direction, side)
                } else {
                    connection_state(level, with_side(cross(power(state)), direction, side), pos)
                }
            }
        })
    }

    fn update_indirect_shapes(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        limit: u32,
    ) {
        for direction in Facing::HORIZONTAL {
            let beside = step(pos, direction);
            if side(state, direction) == Side::None
                || level.block(beside).block() == Block::RedstoneWire
            {
                continue;
            }
            for vertical in [Facing::Down, Facing::Up] {
                let target = step(beside, vertical);
                if level.block(target).block() == Block::RedstoneWire {
                    let from = step(target, direction.opposite());
                    let from_state = level.block(from);
                    level.shape_update(
                        direction.opposite(),
                        target,
                        from,
                        from_state,
                        flags,
                        limit,
                    );
                }
            }
        }
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        connection_state(level, cross(power(state)), pos)
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        if !(is_cross(state) || is_dot(state)) {
            return false;
        }
        let start = if is_cross(state) {
            with_power(Block::RedstoneWire.default_state(), power(state))
        } else {
            cross(power(state))
        };
        let next = connection_state(level, start, pos);
        if next == state {
            return false;
        }
        level.set_block(pos, next, super::UPDATE_ALL);
        let orientation = level.initial_orientation(None, Some(Facing::Up));
        for direction in Facing::HORIZONTAL {
            let beside = step(pos, direction);
            if connected(state, direction) != connected(next, direction)
                && level.block(beside).is_redstone_conductor()
            {
                level.update_neighbors_oriented(
                    beside,
                    Block::RedstoneWire,
                    Some(direction.opposite()),
                    orientation.map(|o| o.with_front(direction)),
                );
            }
        }
        true
    }
}

impl SignalBehavior for Wire {
    fn signal(&self, s: &Signals, pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        if !s.wire || direction == Facing::Down {
            return 0;
        }
        let power = power(state);
        if power == 0 {
            return 0;
        }
        if direction == Facing::Up
            || connected(connection_state(s.level, state, pos), direction.opposite())
        {
            power
        } else {
            0
        }
    }

    fn direct_signal(
        &self,
        s: &Signals,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
    ) -> u8 {
        if s.wire {
            self.signal(s, pos, state, direction)
        } else {
            0
        }
    }

    fn wire_connects(&self, _state: BlockState, _direction: Option<Facing>) -> bool {
        true
    }
}

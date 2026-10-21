//! The experimental wire evaluator: it works out every wire a change
//! reaches first, turning wire off before turning it back on, then sets
//! their power and updates their neighbours in the order each wire's
//! orientation gives.

use std::collections::{HashMap, HashSet, VecDeque};

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{connected, incoming_signal_with, power, wire_signal, with_power};
use crate::{RedstoneLevel, Signals, UPDATE_CLIENTS, UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE};
use tuff_core::world::redstone::{Orientation, SideBias};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

/// Wires changed so far, in the order first reached, with the orientation
/// they were reached in and their new power.
#[derive(Default)]
struct Changes {
    order: Vec<BlockPos>,
    wires: HashMap<BlockPos, (Orientation, u8)>,
}

impl Changes {
    fn set_power(&mut self, pos: BlockPos, power: u8, orientation: Orientation) {
        match self.wires.get_mut(&pos) {
            Some(entry) => entry.1 = power,
            None => {
                self.order.push(pos);
                self.wires.insert(pos, (orientation, power));
            }
        }
    }

    fn get(&self, pos: BlockPos) -> (Orientation, u8) {
        self.wires[&pos]
    }
}

/// A queue of positions holding each at most once.
#[derive(Default)]
struct Queue {
    items: VecDeque<BlockPos>,
    members: HashSet<BlockPos>,
}

impl Queue {
    fn contains(&self, pos: BlockPos) -> bool {
        self.members.contains(&pos)
    }

    fn push(&mut self, pos: BlockPos) {
        if self.members.insert(pos) {
            self.items.push_back(pos);
        }
    }

    fn pop(&mut self) -> Option<BlockPos> {
        let pos = self.items.pop_front()?;
        self.members.remove(&pos);
        Some(pos)
    }
}

#[derive(Default)]
struct Evaluator {
    changes: Changes,
    turn_off: Queue,
    turn_on: Queue,
}

/// Re-evaluates the wire network around `initial` after a change that
/// reached it carrying `orientation` (a random one when `None`).
/// `shape_update_initial` lets the first wire set reshape the wires around
/// it, as on placement.
pub(super) fn update_power_strength(
    level: &mut dyn RedstoneLevel,
    initial: BlockPos,
    orientation: Option<Orientation>,
    shape_update_initial: bool,
) {
    let orientation = orientation
        .unwrap_or_else(|| level.random_orientation())
        .with_up(Facing::Up)
        .with_side_bias(SideBias::Left);
    let mut evaluator = Evaluator::default();
    evaluator.calculate(level, initial, orientation);
    let mut changed = Vec::with_capacity(evaluator.changes.order.len());
    for (index, pos) in evaluator.changes.order.iter().copied().enumerate() {
        let (orientation, new_power) = evaluator.changes.get(pos);
        let state = level.block(pos);
        if state.block() == Block::RedstoneWire && power(state) != new_power {
            let mut flags = UPDATE_CLIENTS;
            if !shape_update_initial || index != 0 {
                flags |= UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE;
            }
            level.set_block(pos, with_power(state, new_power), flags);
            changed.push((pos, orientation));
        }
    }
    cause_neighbor_updates(level, &changed);
}

fn is_connected(state: BlockState, direction: Facing) -> bool {
    match direction {
        Facing::Down => true,
        Facing::Up => false,
        _ => connected(state, direction),
    }
}

fn cause_neighbor_updates(level: &mut dyn RedstoneLevel, changed: &[(BlockPos, Orientation)]) {
    for &(pos, orientation) in changed {
        let state = level.block(pos);
        for direction in orientation.directions() {
            if !is_connected(state, direction) {
                continue;
            }
            let neighbor = step(pos, direction);
            let neighbor_state = level.block(neighbor);
            let turned = orientation.with_front_preserve_up(direction);
            level.neighbor_changed_oriented(neighbor, Block::RedstoneWire, Some(turned));
            if neighbor_state.is_redstone_conductor() {
                for onward in turned.directions() {
                    if onward != direction.opposite() {
                        level.neighbor_changed_oriented(
                            step(neighbor, onward),
                            Block::RedstoneWire,
                            Some(turned.with_front_preserve_up(onward)),
                        );
                    }
                }
            }
        }
    }
    for &(pos, orientation) in changed {
        level.wire_orientation_debug(pos, orientation);
    }
}

impl Evaluator {
    fn wire_signal(&self, pos: BlockPos, state: BlockState) -> u8 {
        self.changes
            .wires
            .get(&pos)
            .map_or_else(|| wire_signal(state), |entry| entry.1)
    }

    fn target(&self, level: &dyn RedstoneLevel, pos: BlockPos) -> (u8, u8) {
        let block_signal = Signals { level, wire: false }.best_neighbor_signal(pos);
        let wire = incoming_signal_with(level, pos, |at, state| self.wire_signal(at, state));
        (block_signal, block_signal.max(wire))
    }

    fn calculate(
        &mut self,
        level: &mut dyn RedstoneLevel,
        initial: BlockPos,
        orientation: Orientation,
    ) {
        let state = level.block(initial);
        if state.block() == Block::RedstoneWire {
            self.changes.set_power(initial, power(state), orientation);
            self.turn_off.push(initial);
        } else {
            self.propagate(level, initial, 0, orientation, true);
        }
        while let Some(pos) = self.turn_off.pop() {
            let (orientation, old) = self.changes.get(pos);
            let (block_signal, new) = self.target(level, pos);
            let to_set = if new < old {
                if block_signal > 0 && !self.turn_on.contains(pos) {
                    self.turn_on.push(pos);
                }
                0
            } else {
                new
            };
            if to_set != old {
                self.changes.set_power(pos, to_set, orientation);
            }
            self.propagate(level, pos, to_set, orientation, old > new);
        }
        while let Some(pos) = self.turn_on.pop() {
            let (orientation, old) = self.changes.get(pos);
            let (_, new) = self.target(level, pos);
            if new > old {
                self.changes.set_power(pos, new, orientation);
            }
            self.propagate(level, pos, new.max(old), orientation, false);
        }
    }

    fn propagate(
        &mut self,
        level: &dyn RedstoneLevel,
        pos: BlockPos,
        power: u8,
        orientation: Orientation,
        allow_off: bool,
    ) {
        let horizontal = orientation.horizontal_directions();
        for direction in horizontal {
            self.enqueue(
                level,
                step(pos, direction),
                power,
                orientation.with_front(direction),
                allow_off,
            );
        }
        for vertical in orientation.vertical_directions() {
            let offset = step(pos, vertical);
            let solid = level.block(offset).is_redstone_conductor();
            for direction in horizontal {
                let reaches = match vertical {
                    Facing::Up => !solid,
                    _ => !level.block(step(pos, direction)).is_redstone_conductor(),
                };
                if reaches {
                    self.enqueue(
                        level,
                        step(offset, direction),
                        power,
                        orientation.with_front(direction),
                        allow_off,
                    );
                }
            }
        }
    }

    fn enqueue(
        &mut self,
        level: &dyn RedstoneLevel,
        pos: BlockPos,
        from_power: u8,
        orientation: Orientation,
        allow_off: bool,
    ) {
        let state = level.block(pos);
        if state.block() != Block::RedstoneWire {
            return;
        }
        let to_power = self.wire_signal(pos, state);
        if i16::from(to_power) < i16::from(from_power) - 1 && !self.turn_on.contains(pos) {
            self.turn_on.push(pos);
            self.changes.set_power(pos, to_power, orientation);
        }
        if allow_off && to_power > from_power && !self.turn_off.contains(pos) {
            self.turn_off.push(pos);
            self.changes.set_power(pos, to_power, orientation);
        }
    }
}

//! Repeaters and comparators.

use tuff_vanilla_data::block::{Facing, Mode};
use tuff_vanilla_data::redstone::Component;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState, Support};

use super::{
    BlockBehavior, DIRECTIONS, Effect, Reader, RedstoneLevel, RedstoneLevelExt, SignalBehavior,
    Signals, TickPriority, UPDATE_ALL, UPDATE_CLIENTS,
};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Repeater;
pub struct Comparator;

const COMPARATOR_DELAY: u32 = 2;

pub fn is_diode(state: BlockState) -> bool {
    matches!(
        state.block().component(),
        Some(Component::Repeater | Component::Comparator)
    )
}

fn facing(state: BlockState) -> Facing {
    state.facing().unwrap_or(Facing::North)
}

fn powered(state: BlockState) -> bool {
    state.powered() == Some(true)
}

fn with_powered(state: BlockState, powered: bool) -> BlockState {
    state.with_powered(powered).unwrap_or(state)
}

fn can_survive_on(below: BlockState) -> bool {
    below.is_face_sturdy(Facing::Up, Support::Rigid)
}

fn is_subtract(state: BlockState) -> bool {
    state.mode() == Some(Mode::Subtract)
}

/// The signal a diode reads from behind it.
fn diode_input(level: &dyn Reader, pos: BlockPos, state: BlockState) -> u8 {
    let direction = facing(state);
    let behind = step(pos, direction);
    let input = Signals::new(level).signal(behind, direction);
    if input >= 15 {
        return input;
    }
    let behind_state = level.block(behind);
    if behind_state.block() == Block::RedstoneWire {
        input.max(behind_state.power().unwrap_or(0))
    } else {
        input
    }
}

/// The stronger of the diode's two side inputs.
fn alternate_signal(level: &dyn Reader, pos: BlockPos, state: BlockState, only_diodes: bool) -> u8 {
    let direction = facing(state);
    let signals = Signals::new(level);
    [direction.clockwise(), direction.counter_clockwise()]
        .into_iter()
        .map(|side| signals.control_input(step(pos, side), side, only_diodes))
        .max()
        .unwrap_or(0)
}

/// Whether the diode behind this one points into it from another side,
/// which makes this one tick first.
fn should_prioritize(level: &dyn Reader, pos: BlockPos, state: BlockState) -> bool {
    let direction = facing(state).opposite();
    let other = level.block(step(pos, direction));
    is_diode(other) && other.facing() != Some(direction)
}

fn update_neighbors_in_front(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let direction = facing(state);
    let front = step(pos, direction.opposite());
    let block = state.block();
    level.neighbor_changed(front, block);
    level.update_neighbors_except(front, block, Some(direction));
}

/// A diode that lost its support drops and tells its neighbours; true
/// when it survived.
fn check_support(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
    if level.block(pos).block() != state.block() {
        return false;
    }
    if can_survive_on(level.block(step(pos, Facing::Down))) {
        return true;
    }
    level.effect(Effect::DropResources(pos, state));
    level.remove_block(pos);
    for direction in DIRECTIONS {
        level.update_neighbors_at(step(pos, direction), state.block());
    }
    false
}

fn repeater_locked(level: &dyn Reader, pos: BlockPos, state: BlockState) -> bool {
    alternate_signal(level, pos, state, true) > 0
}

fn repeater_delay(state: BlockState) -> u32 {
    u32::from(state.delay().unwrap_or(1)) * 2
}

impl BlockBehavior for Repeater {
    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if repeater_locked(level, pos, state) {
            return;
        }
        let on = powered(state);
        let should = diode_input(level, pos, state) > 0;
        if on && !should {
            level.set_block(pos, with_powered(state, false), UPDATE_CLIENTS);
        } else if !on {
            level.set_block(pos, with_powered(state, true), UPDATE_CLIENTS);
            if !should {
                level.schedule_tick(
                    pos,
                    state.block(),
                    repeater_delay(state),
                    TickPriority::VeryHigh,
                );
            }
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        if !check_support(level, pos, state) || repeater_locked(level, pos, state) {
            return;
        }
        let on = powered(state);
        let should = diode_input(level, pos, state) > 0;
        if on != should && !level.will_tick_this_tick(pos, state.block()) {
            let priority = if should_prioritize(level, pos, state) {
                TickPriority::ExtremelyHigh
            } else if on {
                TickPriority::VeryHigh
            } else {
                TickPriority::High
            };
            level.schedule_tick(pos, state.block(), repeater_delay(state), priority);
        }
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _old: BlockState,
        _moved: bool,
    ) {
        update_neighbors_in_front(level, pos, state);
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved {
            update_neighbors_in_front(level, pos, state);
        }
    }

    fn update_shape(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        if direction == Facing::Down && !can_survive_on(neighbour) {
            return Some(Block::Air.default_state());
        }
        if direction.axis() != facing(state).axis() {
            let locked = repeater_locked(level, pos, state);
            return Some(state.with_locked(locked).unwrap_or(state));
        }
        Some(state)
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        state
            .with_locked(repeater_locked(level, pos, state))
            .unwrap_or(state)
    }

    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if diode_input(level, pos, state) > 0 {
            level.schedule_tick(pos, state.block(), 1, TickPriority::Normal);
        }
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        let delay = state.delay().unwrap_or(1) % 4 + 1;
        if let Some(next) = state.with_delay(delay) {
            level.set_block(pos, next, UPDATE_ALL);
        }
        true
    }
}

impl SignalBehavior for Repeater {
    fn wire_connects(&self, state: BlockState, direction: Option<Facing>) -> bool {
        direction.is_some_and(|d| d == facing(state) || d == facing(state).opposite())
    }
}

fn comparator_input(level: &dyn Reader, pos: BlockPos, state: BlockState) -> u8 {
    let mut input = diode_input(level, pos, state);
    let direction = facing(state);
    let behind = step(pos, direction);
    let behind_state = level.block(behind);
    if behind_state.has_analog_output() {
        input = level.analog_output(behind, behind_state, direction.opposite());
    } else if input < 15 && behind_state.is_redstone_conductor() {
        let further = step(behind, direction);
        let further_state = level.block(further);
        let frame = level.item_frame_output(further, direction);
        let block = further_state
            .has_analog_output()
            .then(|| level.analog_output(further, further_state, direction.opposite()));
        if let Some(signal) = frame.max(block) {
            input = signal;
        }
    }
    input
}

fn comparator_output(level: &dyn Reader, pos: BlockPos, state: BlockState) -> u8 {
    let input = comparator_input(level, pos, state);
    if input == 0 {
        return 0;
    }
    let side = alternate_signal(level, pos, state, false);
    if side > input {
        0
    } else if is_subtract(state) {
        input - side
    } else {
        input
    }
}

fn comparator_turns_on(level: &dyn Reader, pos: BlockPos, state: BlockState) -> bool {
    let input = comparator_input(level, pos, state);
    if input == 0 {
        return false;
    }
    let side = alternate_signal(level, pos, state, false);
    input > side || (input == side && !is_subtract(state))
}

fn refresh_output(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let output = comparator_output(level, pos, state);
    let old = level.comparator_output(pos);
    level.set_comparator_output(pos, output);
    if old != output || !is_subtract(state) {
        let on = comparator_turns_on(level, pos, state);
        let was_on = powered(state);
        if was_on && !on {
            level.set_block(pos, with_powered(state, false), UPDATE_CLIENTS);
        } else if !was_on && on {
            level.set_block(pos, with_powered(state, true), UPDATE_CLIENTS);
        }
        update_neighbors_in_front(level, pos, state);
    }
}

impl BlockBehavior for Comparator {
    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        refresh_output(level, pos, state);
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        if !check_support(level, pos, state) || level.will_tick_this_tick(pos, state.block()) {
            return;
        }
        let output = comparator_output(level, pos, state);
        if output != level.comparator_output(pos)
            || powered(state) != comparator_turns_on(level, pos, state)
        {
            let priority = if should_prioritize(level, pos, state) {
                TickPriority::High
            } else {
                TickPriority::Normal
            };
            level.schedule_tick(pos, state.block(), COMPARATOR_DELAY, priority);
        }
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _old: BlockState,
        _moved: bool,
    ) {
        update_neighbors_in_front(level, pos, state);
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved {
            update_neighbors_in_front(level, pos, state);
        }
    }

    fn update_shape(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        if direction == Facing::Down && !can_survive_on(neighbour) {
            Some(Block::Air.default_state())
        } else {
            Some(state)
        }
    }

    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if comparator_turns_on(level, pos, state) {
            level.schedule_tick(pos, state.block(), 1, TickPriority::Normal);
        }
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        let mode = if is_subtract(state) {
            Mode::Compare
        } else {
            Mode::Subtract
        };
        let Some(next) = state.with_mode(mode) else {
            return true;
        };
        let pitch = if mode == Mode::Subtract { 0.55 } else { 0.5 };
        level.effect(Effect::Sound {
            sound: SoundEvent::BlockComparatorClick,
            pos,
            volume: 0.3,
            pitch,
        });
        level.set_block(pos, next, UPDATE_CLIENTS);
        if level.block(pos).block() == state.block() {
            refresh_output(level, pos, next);
        }
        true
    }
}

impl SignalBehavior for Comparator {
    fn signal(&self, s: &Signals, pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        if facing(state) == direction && powered(state) {
            s.level.comparator_output(pos)
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
        self.signal(s, pos, state, direction)
    }
}

//! Blocks that react to power: lamps, doors, trapdoors, fence gates,
//! note blocks, TNT, hoppers, bells, dispensers and droppers.

use tuff_vanilla_data::block::{Facing, Half};
use tuff_vanilla_data::{Block, BlockState};

use super::{
    BlockBehavior, Effect, Reader, RedstoneLevel, SignalBehavior, Signals, TickPriority,
    UPDATE_ALL, UPDATE_CLIENTS,
};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Lamp;
pub struct Door;
pub struct Trapdoor;
pub struct FenceGate;
pub struct NoteBlock;
pub struct Tnt;
pub struct Hopper;
pub struct Bell;
pub struct Dispenser;

const LAMP_OFF_DELAY: u32 = 4;
const DISPENSE_DELAY: u32 = 4;

fn powered_at(level: &dyn Reader, pos: BlockPos) -> bool {
    Signals::new(level).has_neighbor_signal(pos)
}

fn toggle_sound(level: &mut dyn RedstoneLevel, pos: BlockPos, block: Block, open: bool) {
    if let Some(toggle) = block.toggle() {
        let pitch = level.random_float() * 0.1 + 0.9;
        level.effect(Effect::Sound {
            sound: if open {
                toggle.open_sound
            } else {
                toggle.close_sound
            },
            pos,
            volume: 1.0,
            pitch,
        });
    }
}

impl BlockBehavior for Lamp {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let lit = state.lit() == Some(true);
        if lit == powered_at(level, pos) {
            return;
        }
        if lit {
            level.schedule_tick(pos, state.block(), LAMP_OFF_DELAY, TickPriority::Normal);
        } else {
            level.set_block(pos, state.with_lit(true).unwrap_or(state), UPDATE_CLIENTS);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if state.lit() == Some(true) && !powered_at(level, pos) {
            level.set_block(pos, state.with_lit(false).unwrap_or(state), UPDATE_CLIENTS);
        }
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        state.with_lit(powered_at(level, pos)).unwrap_or(state)
    }
}

impl SignalBehavior for Lamp {}

fn door_powered(level: &dyn Reader, pos: BlockPos, state: BlockState) -> bool {
    let other = if state.half() == Some(Half::Lower) {
        Facing::Up
    } else {
        Facing::Down
    };
    powered_at(level, pos) || powered_at(level, step(pos, other))
}

impl BlockBehavior for Door {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        source: Block,
    ) {
        let signal = door_powered(level, pos, state);
        if source == state.block() || Some(signal) == state.powered() {
            return;
        }
        if Some(signal) != state.open() {
            toggle_sound(level, pos, state.block(), signal);
        }
        let next = state
            .with_powered(signal)
            .and_then(|s| s.with_open(signal))
            .unwrap_or(state);
        level.set_block(pos, next, UPDATE_CLIENTS);
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        let signal = powered_at(level, pos) || powered_at(level, step(pos, Facing::Up));
        state
            .with_powered(signal)
            .and_then(|s| s.with_open(signal))
            .unwrap_or(state)
    }
}

impl SignalBehavior for Door {}

impl BlockBehavior for Trapdoor {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        mut state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos);
        if Some(signal) == state.powered() {
            return;
        }
        if Some(signal) != state.open() {
            state = state.with_open(signal).unwrap_or(state);
            toggle_sound(level, pos, state.block(), signal);
        }
        level.set_block(
            pos,
            state.with_powered(signal).unwrap_or(state),
            UPDATE_CLIENTS,
        );
        if state.waterlogged() == Some(true) {
            level.effect(Effect::WaterTick(pos));
        }
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        if powered_at(level, pos) {
            state
                .with_open(true)
                .and_then(|s| s.with_powered(true))
                .unwrap_or(state)
        } else {
            state
        }
    }
}

impl SignalBehavior for Trapdoor {}

impl BlockBehavior for FenceGate {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos);
        if Some(signal) == state.powered() {
            return;
        }
        let next = state
            .with_powered(signal)
            .and_then(|s| s.with_open(signal))
            .unwrap_or(state);
        level.set_block(pos, next, UPDATE_CLIENTS);
        if Some(signal) != state.open() {
            toggle_sound(level, pos, state.block(), signal);
        }
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        let signal = powered_at(level, pos);
        state
            .with_open(signal)
            .and_then(|s| s.with_powered(signal))
            .unwrap_or(state)
    }
}

impl SignalBehavior for FenceGate {}

impl BlockBehavior for NoteBlock {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos);
        if Some(signal) == state.powered() {
            return;
        }
        if signal {
            level.effect(Effect::PlayNote(pos));
        }
        level.set_block(pos, state.with_powered(signal).unwrap_or(state), UPDATE_ALL);
    }
}

impl SignalBehavior for NoteBlock {}

impl BlockBehavior for Tnt {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() && powered_at(level, pos) {
            level.effect(Effect::PrimeTnt(pos));
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
        if powered_at(level, pos) {
            level.effect(Effect::PrimeTnt(pos));
        }
    }
}

impl SignalBehavior for Tnt {}

fn check_hopper(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let enabled = !powered_at(level, pos);
    if Some(enabled) != state.enabled() {
        level.set_block(
            pos,
            state.with_enabled(enabled).unwrap_or(state),
            UPDATE_CLIENTS,
        );
    }
}

impl BlockBehavior for Hopper {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            check_hopper(level, pos, state);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        check_hopper(level, pos, state);
    }
}

impl SignalBehavior for Hopper {}

impl BlockBehavior for Bell {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos);
        if Some(signal) == state.powered() {
            return;
        }
        if signal {
            level.effect(Effect::RingBell(pos));
        }
        level.set_block(pos, state.with_powered(signal).unwrap_or(state), UPDATE_ALL);
    }
}

impl SignalBehavior for Bell {}

impl BlockBehavior for Dispenser {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos) || powered_at(level, step(pos, Facing::Up));
        let triggered = state.triggered() == Some(true);
        if signal && !triggered {
            level.schedule_tick(pos, state.block(), DISPENSE_DELAY, TickPriority::Normal);
            level.set_block(
                pos,
                state.with_triggered(true).unwrap_or(state),
                UPDATE_CLIENTS,
            );
        } else if !signal && triggered {
            level.set_block(
                pos,
                state.with_triggered(false).unwrap_or(state),
                UPDATE_CLIENTS,
            );
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        level.effect(Effect::Dispense(pos));
    }
}

impl SignalBehavior for Dispenser {}

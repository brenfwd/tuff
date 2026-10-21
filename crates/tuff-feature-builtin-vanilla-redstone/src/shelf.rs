//! Shelves: power toggles them with a click and links powered neighbours
//! into a chain.

use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::tag::block::WOODEN_SHELVES;
use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, Effect, RedstoneLevel, SignalBehavior, Signals, UPDATE_ALL};
use tuff_core::world::game_event::GameEvent;
use tuff_vanilla_common::side_chain;
use tuff_world_model::BlockPos;

pub struct Shelf;

pub fn blocks() -> &'static [Block] {
    WOODEN_SHELVES
}

fn sound(level: &mut dyn RedstoneLevel, pos: BlockPos, sound: SoundEvent) {
    level.effect(Effect::Sound {
        sound,
        pos,
        volume: 1.0,
        pitch: 1.0,
    });
}

impl BlockBehavior for Shelf {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            let signal = Signals::new(level).has_neighbor_signal(pos);
            if signal != (state.powered() == Some(true))
                && let Some(next) = state.with_powered(signal)
            {
                level.set_block(pos, next, UPDATE_ALL);
                return;
            }
        }
        if state.powered() == Some(true) {
            side_chain::powered_up(level, pos, state, old);
        } else {
            side_chain::powered_down(level, pos, state);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        if signal == (state.powered() == Some(true)) {
            return;
        }
        let Some(mut next) = state.with_powered(signal) else {
            return;
        };
        if !signal {
            next = next.with("side_chain", "unconnected").unwrap_or(next);
        }
        level.set_block(pos, next, UPDATE_ALL);
        if signal {
            sound(level, pos, SoundEvent::BlockShelfActivate);
            level.effect(Effect::GameEvent(GameEvent::BlockActivate, pos));
        } else {
            sound(level, pos, SoundEvent::BlockShelfDeactivate);
            level.effect(Effect::GameEvent(GameEvent::BlockDeactivate, pos));
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _moved: bool,
    ) {
        side_chain::powered_down(level, pos, state);
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Shelf {}

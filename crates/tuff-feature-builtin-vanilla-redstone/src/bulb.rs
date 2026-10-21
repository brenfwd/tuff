//! Copper bulbs: each rising edge of power toggles the light.

use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, Effect, RedstoneLevel, SignalBehavior, Signals, UPDATE_ALL};
use tuff_world_model::BlockPos;

pub struct CopperBulb;

pub const BLOCKS: [Block; 8] = [
    Block::CopperBulb,
    Block::ExposedCopperBulb,
    Block::WeatheredCopperBulb,
    Block::OxidizedCopperBulb,
    Block::WaxedCopperBulb,
    Block::WaxedExposedCopperBulb,
    Block::WaxedWeatheredCopperBulb,
    Block::WaxedOxidizedCopperBulb,
];

fn check_and_flip(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let signal = Signals::new(level).has_neighbor_signal(pos);
    let powered = state.powered() == Some(true);
    if signal == powered {
        return;
    }
    let mut next = state;
    if !powered {
        let lit = state.lit() != Some(true);
        next = next.with_lit(lit).unwrap_or(next);
        level.effect(Effect::Sound {
            sound: if lit {
                SoundEvent::BlockCopperBulbTurnOn
            } else {
                SoundEvent::BlockCopperBulbTurnOff
            },
            pos,
            volume: 1.0,
            pitch: 1.0,
        });
    }
    level.set_block(pos, next.with_powered(signal).unwrap_or(next), UPDATE_ALL);
}

impl BlockBehavior for CopperBulb {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            check_and_flip(level, pos, state);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        check_and_flip(level, pos, state);
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for CopperBulb {}

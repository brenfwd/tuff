//! Test blocks: a signal beside them triggers them, and a triggered start
//! block powers every side at full strength.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, Effect, RedstoneLevel, SignalBehavior, Signals};
use tuff_world_model::BlockPos;

pub struct TestBlock;

const START_SIGNAL: u8 = 15;

impl BlockBehavior for TestBlock {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        level.effect(Effect::TestBlockPower(pos, signal));
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        level.effect(Effect::TestBlockReset(pos));
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for TestBlock {
    fn signal(&self, s: &Signals, pos: BlockPos, state: BlockState, _direction: Facing) -> u8 {
        let powered = s
            .level
            .saved_nbt_at(pos)
            .and_then(|nbt| nbt.get_bool("powered"))
            .unwrap_or(false);
        if state.get("mode") == Some("start") && powered {
            START_SIGNAL
        } else {
            0
        }
    }

    fn direct_signal(&self, _: &Signals, _: BlockPos, _: BlockState, _: Facing) -> u8 {
        0
    }
}

/// Structure blocks: a rising signal saves, loads or forgets by mode.
pub struct StructureBlock;

impl BlockBehavior for StructureBlock {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        level.effect(Effect::StructureBlockPower(pos, signal));
    }
}

impl SignalBehavior for StructureBlock {
    fn signal(&self, _: &Signals, _: BlockPos, _: BlockState, _: Facing) -> u8 {
        0
    }
}

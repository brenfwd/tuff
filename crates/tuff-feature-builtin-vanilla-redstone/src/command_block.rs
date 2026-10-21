//! Command blocks: the redstone side only reports power and ticks; the
//! region runs the command.

use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, Effect, RedstoneLevel, SignalBehavior, Signals};
use tuff_world_model::BlockPos;

pub struct CommandBlock;

impl BlockBehavior for CommandBlock {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
        let signal = Signals::new(level).has_neighbor_signal(pos);
        level.effect(Effect::CommandBlockPowered(pos, signal));
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        level.effect(Effect::CommandBlockTick(pos));
    }

    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        level.effect(Effect::CommandBlockPlaced(pos));
        let signal = Signals::new(level).has_neighbor_signal(pos);
        level.effect(Effect::CommandBlockPowered(pos, signal));
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for CommandBlock {}

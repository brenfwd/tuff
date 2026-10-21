use super::{BlockBehavior, Reader, RedstoneLevel, SignalBehavior, Signals, UPDATE_CLIENTS};
use tuff_vanilla_data::{Block, BlockKind, BlockState};
use tuff_world_model::BlockPos;

pub struct Skull;

fn powered_at(level: &dyn Reader, pos: BlockPos) -> bool {
    Signals::new(level).has_neighbor_signal(pos)
}

pub fn is_head(block: Block) -> bool {
    matches!(block.kind(), BlockKind::Skull | BlockKind::WallSkull)
}

impl BlockBehavior for Skull {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let signal = powered_at(level, pos);
        if Some(signal) != state.powered() {
            level.set_block(
                pos,
                state.with_powered(signal).unwrap_or(state),
                UPDATE_CLIENTS,
            );
        }
    }

    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        state.with_powered(powered_at(level, pos)).unwrap_or(state)
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Skull {}

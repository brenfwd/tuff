//! Trapped chests: a signal as strong as the number of players looking
//! inside, strong only downward.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use super::{BlockBehavior, SignalBehavior, Signals};
use tuff_world_model::BlockPos;

pub struct TrappedChest;

impl BlockBehavior for TrappedChest {}

impl SignalBehavior for TrappedChest {
    fn signal(&self, s: &Signals, pos: BlockPos, _state: BlockState, _direction: Facing) -> u8 {
        s.level.open_count(pos).min(15) as u8
    }

    fn direct_signal(
        &self,
        s: &Signals,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
    ) -> u8 {
        if direction == Facing::Up {
            self.signal(s, pos, state, direction)
        } else {
            0
        }
    }
}

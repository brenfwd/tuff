//! Jukeboxes: a playing record powers every side weakly at full strength.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use super::{BlockBehavior, SignalBehavior, Signals};
use tuff_vanilla_common::jukebox;
use tuff_world_model::BlockPos;

pub struct Jukebox;

impl BlockBehavior for Jukebox {}

impl SignalBehavior for Jukebox {
    fn signal(&self, s: &Signals, pos: BlockPos, _state: BlockState, _direction: Facing) -> u8 {
        if s.level.saved_nbt_at(pos).is_some_and(jukebox::is_playing) {
            jukebox::PLAYING_SIGNAL
        } else {
            0
        }
    }

    fn direct_signal(&self, _: &Signals, _: BlockPos, _: BlockState, _: Facing) -> u8 {
        0
    }

    fn wire_connects(&self, _state: BlockState, direction: Option<Facing>) -> bool {
        direction.is_some()
    }
}

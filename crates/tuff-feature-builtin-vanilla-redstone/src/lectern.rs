//! Lecterns: the pulse a page turn gives ends two ticks later.

use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, RedstoneLevel, SignalBehavior, UPDATE_ALL};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Lectern;

impl BlockBehavior for Lectern {
    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if state.powered() != Some(true) {
            return;
        }
        level.set_block(pos, state.with_powered(false).unwrap_or(state), UPDATE_ALL);
        level.update_neighbors_at(
            step(pos, tuff_vanilla_data::block::Facing::Down),
            Block::Lectern,
        );
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Lectern {}

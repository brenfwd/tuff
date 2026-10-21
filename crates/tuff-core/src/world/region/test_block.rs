//! Test blocks: a redstone signal triggers them, and a start block then
//! powers its neighbours until it is reset.

use tuff_vanilla_data::{Block, BlockState};

use super::RegionActor;
use crate::world::redstone::RedstoneLevel as _;
use tuff_world_model::BlockPos;

const POWERED: &str = "powered";

/// The test blocks triggered since their last reset, which (like the
/// game) is never saved.
#[derive(Default)]
struct Triggered(std::collections::HashSet<BlockPos>);

impl RegionActor {
    fn test_block_state(&self, pos: BlockPos) -> Option<BlockState> {
        self.block(pos).filter(|s| s.block() == Block::TestBlock)
    }

    fn test_block_powered(&self, pos: BlockPos) -> bool {
        self.raw_block_entity(pos)
            .and_then(|nbt| nbt.get_bool(POWERED))
            .unwrap_or(false)
    }

    fn set_test_block_powered(&mut self, pos: BlockPos, powered: bool) {
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert(POWERED, powered);
        }
    }

    /// The signal beside a test block changed: a rising edge triggers it,
    /// unless it is a start block, which only powers others.
    pub(super) fn test_block_power(&mut self, pos: BlockPos, signal: bool) {
        let Some(state) = self.test_block_state(pos) else {
            return;
        };
        if state.get("mode") == Some("start") {
            return;
        }
        let powered = self.test_block_powered(pos);
        if signal && !powered {
            self.set_test_block_powered(pos, true);
            self.trigger_test_block(pos, state);
        } else if !signal && powered {
            self.set_test_block_powered(pos, false);
        }
    }

    /// A start block drops its signal; any block forgets it was triggered.
    pub(super) fn test_block_reset(&mut self, pos: BlockPos) {
        let Some(state) = self.test_block_state(pos) else {
            return;
        };
        if state.get("mode") == Some("start") {
            self.set_test_block_powered(pos, false);
            self.update_neighbors_at(pos, Block::TestBlock);
        }
        self.ents.feature_state.get::<Triggered>().0.remove(&pos);
    }

    /// See [`crate::world::messages::BlockOp::TestBlocks`].
    pub(super) fn test_blocks_op(
        &mut self,
        positions: &[BlockPos],
        trigger: bool,
        reset: bool,
    ) -> Vec<Option<bool>> {
        positions
            .iter()
            .map(|&pos| {
                if !self.owns(pos) {
                    return None;
                }
                let state = self.test_block_state(pos)?;
                if trigger {
                    self.trigger_test_block(pos, state);
                    return Some(true);
                }
                let triggered = self.ents.feature_state.get::<Triggered>().0.contains(&pos);
                if triggered && reset {
                    self.test_block_reset(pos);
                }
                Some(triggered)
            })
            .collect()
    }

    fn trigger_test_block(&mut self, pos: BlockPos, state: BlockState) {
        match state.get("mode") {
            Some("start") => {
                self.set_test_block_powered(pos, true);
                self.update_neighbors_at(pos, Block::TestBlock);
                self.log_test_block(pos, "start");
                return;
            }
            Some("log") => self.log_test_block(pos, "log"),
            _ => {}
        }
        if state.get("mode") != Some("start") {
            self.ents.feature_state.get::<Triggered>().0.insert(pos);
        }
    }

    fn log_test_block(&self, pos: BlockPos, mode: &str) {
        let message = self
            .raw_block_entity(pos)
            .and_then(|nbt| nbt.get_str("message"))
            .unwrap_or_default();
        if !message.trim().is_empty() {
            log::info!("Test {mode} (at {pos:?}): {message}");
        }
    }
}

impl RegionActor {
    /// The signal beside a structure block changed; a load it triggers
    /// places across regions off the actor.
    pub(super) fn structure_block_power(&mut self, pos: BlockPos, signal: bool) {
        let triggered = crate::world::session::structure_block::redstone_power(self, pos, signal);
        if let Some((request, extent)) = triggered {
            let shared = self.shared.clone();
            let dimension = self.dimension;
            tokio::spawn(crate::world::session::structure_block::place(
                shared, dimension, request, extent,
            ));
        }
    }
}

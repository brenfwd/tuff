//! What game masters' block editors save: the jigsaw block's fields and the
//! test block's mode and message.

use tuff_vanilla_data::Block;

use crate::protocol::play::JigsawEdit;
use crate::world::redstone::UPDATE_CLIENTS;
use crate::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;

pub const TEST_BLOCK_MODES: [&str; 4] = ["start", "log", "fail", "accept"];

/// Stores the jigsaw editor's fields in the jigsaw block at the edit's
/// position; false if there is none.
pub fn apply_jigsaw_edit(world: &mut dyn RegionWorld, edit: &JigsawEdit) -> bool {
    if world.block(edit.position).map(|s| s.block()) != Some(Block::Jigsaw) {
        return false;
    }
    let Some(nbt) = world.raw_block_entity_mut(edit.position) else {
        return false;
    };
    nbt.insert("name", edit.name.as_str());
    nbt.insert("target", edit.target.as_str());
    nbt.insert("pool", edit.pool.as_str());
    nbt.insert("final_state", edit.final_state.as_str());
    nbt.insert("joint", edit.joint);
    nbt.insert("selection_priority", edit.selection_priority);
    nbt.insert("placement_priority", edit.placement_priority);
    world.block_entity_changed(edit.position);
    true
}

/// Sets the mode (a start, log, fail or accept ordinal, start when out of
/// range) and message of the test block at `pos`; false if there is none.
pub fn apply_test_block_edit(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    mode: i32,
    message: &str,
) -> bool {
    let Some(state) = world.block(pos).filter(|s| s.block() == Block::TestBlock) else {
        return false;
    };
    let mode = usize::try_from(mode)
        .ok()
        .and_then(|mode| TEST_BLOCK_MODES.get(mode))
        .copied()
        .unwrap_or(TEST_BLOCK_MODES[0]);
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return false;
    };
    nbt.insert("mode", mode);
    nbt.insert("message", message);
    if let Some(next) = state.with("mode", mode) {
        world.set_block_flags(pos, next, UPDATE_CLIENTS);
    }
    world.block_entity_changed(pos);
    true
}

//! Composter block states shared by the plugins that work composters.

use tuff_vanilla_data::{Block, BlockState};

use tuff_item::item::ItemStack;

/// Level Event: a composter took an item; data 1 if a layer was added.
pub const COMPOSTER_FILL: i32 = 1500;
pub const READY: u8 = 8;
pub const FULL: u8 = 7;

/// The fill level of a composter state.
pub fn level(state: BlockState) -> Option<u8> {
    (state.block() == Block::Composter)
        .then(|| state.get("level")?.parse().ok())
        .flatten()
}

pub fn with_level(state: BlockState, level: u8) -> BlockState {
    state.with("level", &level.to_string()).unwrap_or(state)
}

/// The layer chance of a stack, if it is compostable.
pub fn compostable(stack: &ItemStack) -> Option<&'static tuff_vanilla_data::provider::IntProvider> {
    if stack.components.is_removed("minecraft:compostable") {
        return None;
    }
    stack.item.compostable()
}

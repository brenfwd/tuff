//! Jukebox saved-NBT vocabulary: a jukebox plays while its block entity
//! holds `ticks_since_song_started` (the game saves it only then), so
//! readers need no runtime state. The behaviour lives in vanilla-blocks.

use tuff_core::world::block_entity::analog::song_id;
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;

pub const RECORD: &str = "RecordItem";
pub const TICKS: &str = "ticks_since_song_started";
/// The signal a playing jukebox gives every side.
pub const PLAYING_SIGNAL: u8 = 15;

pub fn is_playing(nbt: &Compound) -> bool {
    nbt.contains_key(TICKS)
}

pub fn record(nbt: &Compound) -> Option<ItemStack> {
    nbt.get_compound(RECORD)
        .and_then(ItemStack::picture_from_nbt)
}

/// Whether `stack` plays a song (a hopper or dropper may load it).
pub fn plays(stack: &ItemStack) -> bool {
    song_id(stack).is_some()
}

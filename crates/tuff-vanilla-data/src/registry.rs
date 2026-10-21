//! Built-in registries, whose entries and protocol IDs are fixed by the game
//! version.

/// Implemented by every generated registry enum.
pub trait Registry: Copy + 'static {
    /// The registry's resource location, e.g. `minecraft:block`.
    const KEY: &'static str;
    /// The registry's ID in the root registry.
    const PROTOCOL_ID: u32;
    /// Every entry, indexed by protocol ID.
    const ALL: &'static [Self];

    fn id(self) -> u32;
    fn from_id(id: u32) -> Option<Self>;
    fn name(self) -> &'static str;
    fn from_name(name: &str) -> Option<Self>;
}

include!(concat!(env!("OUT_DIR"), "/registries.rs"));

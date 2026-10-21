//! Identifies the game version the data was generated from.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackVersion {
    pub major: i32,
    pub minor: i32,
}

/// A built-in pack that turns on experimental feature flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeaturePack {
    pub id: &'static str,
    pub features: &'static [&'static str],
    /// The translation key of its description.
    pub description: &'static str,
    /// Its JSON files by path below the pack's root
    /// (`data/minecraft/loot_table/...`), compacted.
    pub files: &'static [(&'static str, &'static str)],
}

include!(concat!(env!("OUT_DIR"), "/version.rs"));

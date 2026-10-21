//! Debug and test builds only: one shared library holding core, the feature
//! crates and their dependencies, so the root binaries link them dynamically
//! instead of each carrying its own static copy.

pub use anyhow;
pub use rayon;
pub use tokio;
pub use tuff;
pub use tuff_bot;
pub use tuff_core;
pub use tuff_datapack;
pub use tuff_feature_builtin_audit_log;
pub use tuff_feature_builtin_core_commands;
pub use tuff_feature_builtin_example;
pub use tuff_feature_builtin_example_chunk_progress;
pub use tuff_feature_builtin_moving_block_entities;
pub use tuff_feature_builtin_vanilla_anticheat;
pub use tuff_feature_builtin_vanilla_blocks;
pub use tuff_feature_builtin_vanilla_combat;
pub use tuff_feature_builtin_vanilla_commands;
pub use tuff_feature_builtin_vanilla_dimensions;
pub use tuff_feature_builtin_vanilla_entities;
pub use tuff_feature_builtin_vanilla_items;
pub use tuff_feature_builtin_vanilla_loot;
pub use tuff_feature_builtin_vanilla_maps;
pub use tuff_feature_builtin_vanilla_mobs;
pub use tuff_feature_builtin_vanilla_players;
pub use tuff_feature_builtin_vanilla_recipes;
pub use tuff_feature_builtin_vanilla_redstone;
pub use tuff_feature_builtin_vanilla_survival;
pub use tuff_feature_builtin_vanilla_worldgen;
pub use tuff_feature_builtin_world_map;
pub use tuff_http;
pub use tuff_nbt;
pub use tuff_plugin_wasm;
pub use tuff_replay;
pub use tuff_storage;
pub use tuff_test_support;
pub use tuff_timings;
pub use tuff_trace;
pub use tuff_vanilla_data;
pub use tuff_wire;
pub use tuff_world_model;
pub use tuff_zip;

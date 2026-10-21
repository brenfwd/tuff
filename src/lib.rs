//! The tuff server: core with the vanilla features. Core's modules are
//! re-exported so `tuff::` paths reach them.

pub mod allocator;
pub mod backup_cli;
pub mod console;
pub mod datapack_cli;
pub mod features;
pub mod gametest_cli;
pub mod plugin_cli;
pub mod plugin_new;
pub mod replay_cli;
pub mod scripts;
pub mod trace_cli;
pub mod vanilla_features;

pub use tuff_core::*;

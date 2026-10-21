#![recursion_limit = "512"]
//! Scenario tests of this crate's behaviour on the world actors with the
//! vanilla features loaded, driven through the channels sessions use. The files
//! were written as submodules of core's `world`, so core, its `world`
//! module and the test-support helpers are re-exported here.

#![allow(unused_imports, hidden_glob_reexports)]

pub use tuff_core::world::*;
pub use tuff_core::*;
pub use tuff_test_support::*;

pub use std::collections::HashSet;
pub use std::path::Path;
pub use std::sync::Arc;
pub use std::time::Duration;
pub use tokio::sync::{mpsc, oneshot};

mod burning;
mod combat;
mod explosions;
mod falling;
mod freezing;
mod lightning;
mod mob_damage;
mod projectiles;
mod ranged;

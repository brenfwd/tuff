#![recursion_limit = "512"]
//! Scenario tests of this crate's behaviour on the world actors with the
//! vanilla features loaded, driven through the channels sessions use. Core, its
//! `world` module and the test-support helpers are re-exported here so the
//! files can use them unqualified.

#![expect(hidden_glob_reexports, reason = "test modules share one glob prelude")]
#![expect(
    clippy::disallowed_methods,
    reason = "test code is exempt from the door gate"
)]

pub use tuff_core::world::*;
pub use tuff_core::*;
pub use tuff_test_support::*;

pub use std::collections::HashSet;
pub use std::path::Path;
pub use std::sync::Arc;
pub use std::time::Duration;
pub use tokio::sync::{mpsc, oneshot};

mod redstone;
mod sulfur_cubes;

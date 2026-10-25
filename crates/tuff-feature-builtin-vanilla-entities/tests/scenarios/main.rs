#![recursion_limit = "512"]
//! Vehicle scenario tests of this crate's behaviour on the world actors with the
//! vanilla features loaded, driven through the channels sessions use. The
//! submodules use core's `world` paths unqualified, so core, its `world`
//! module and the test-support helpers are re-exported here.

#![expect(unused_imports, reason = "test modules share one glob prelude")]
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

mod vehicle_parity;
mod vehicles;

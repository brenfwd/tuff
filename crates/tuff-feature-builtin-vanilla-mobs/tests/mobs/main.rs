#![recursion_limit = "512"]
//! Scenario tests of this crate's behaviour on the world actors with the
//! vanilla features loaded, driven through the channels sessions use. The files
//! use paths relative to core's `world` module, so core, its `world` module
//! and the test-support helpers are re-exported here.

#![allow(unused_imports, hidden_glob_reexports)]

pub use tuff_core::world::*;
pub use tuff_core::*;
pub use tuff_test_support::*;

pub use std::collections::HashSet;
pub use std::path::Path;
pub use std::sync::Arc;
pub use std::time::Duration;
pub use tokio::sync::{mpsc, oneshot};

mod aquatic;
mod cramming;
mod creaking_heart;
mod creepers;
mod crowd;
mod death_effects;
mod drowned;
mod golems;
mod guardian;
mod horses;
mod hostile;
mod illagers;
mod leads;
mod mob_riding;
mod mobs;
mod phantom;
mod piglins;
mod raids;
mod shoulders;
mod shulkers;
mod spawn_caps;
mod swimming;
mod tadpoles;
mod trial_chambers;
mod village_life;
mod village_pois;
mod village_wander;
mod villagers;
mod wandering_traders;
mod warden;
mod water_spawns;
mod wither;
mod zombies;

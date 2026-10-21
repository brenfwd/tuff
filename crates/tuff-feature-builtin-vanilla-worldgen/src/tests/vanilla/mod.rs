//! Chunks the vanilla server generated (`scripts/worldgen-fixture.sh`),
//! per stage, and the comparisons against this server's generator.
//!
//! Split by stage: [`fixtures`] holds the shared fixture-loading and
//! chunk-generation infrastructure every other file here builds on;
//! [`stages`] compares the noise/surface/carver stage fixtures;
//! [`decoration`] compares fully decorated chunks and their features;
//! [`structures`] compares structure starts and references, plus the
//! smaller structure- and `/locate`-related checks.

mod decoration;
mod feature_pins;
mod fixtures;
mod seed_aquifer;
mod stages;
mod structures;
mod surface_exact;
mod temperature_condition;

pub use fixtures::*;

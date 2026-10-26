//! Stateless gameplay helpers shared by the vanilla feature crates.

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod block_support;
pub mod brushable;
pub mod combat;
pub mod composter;
pub mod dismount;
pub mod effect_cloud;
pub mod enchant_effects;
pub mod end_spikes;
pub mod jukebox;
pub mod placement;
pub mod ranged;
pub mod scaffolding;
pub mod shadow;
pub mod side_chain;
pub mod spawn_equipment;
pub mod spawn_search;
pub mod speleothem;

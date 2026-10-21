#![recursion_limit = "512"]
//! The vanilla commands: executors for every command vanilla ships except
//! the administration set in `core-commands` (stop, save-all, op, deop,
//! whitelist and the ban lists), registered into core's command registry.

pub mod admin;
pub mod advancement;
pub mod attribute;
pub mod bossbar;
pub mod camera;
pub mod chase;
pub mod compute;
pub mod damage;
pub mod data;
pub mod datapack;
pub mod debugconfig;
pub mod debugpath;
pub mod execute;
pub mod function;
pub mod gamemode;
pub mod gametest;
pub mod item;
pub mod jfr;
pub mod locate;
pub mod loot;
pub mod orient;
pub mod particle;
pub mod place;
pub mod players;
pub mod recipe;
pub mod registry;
pub mod resolve;
pub mod ride;
pub mod schedule;
pub mod scoreboard;
pub mod sequences;
pub mod serverpack;
pub mod spreadplayers;
pub mod tick;
pub mod time;
pub mod warden_spawn_tracker;
pub mod waypoint;
pub mod world;
pub mod worldborder;

pub use tuff_core::world::commands::*;

/// Registers the vanilla commands into core.
pub struct VanillaCommandsFeature;

impl tuff_core::feature::Feature for VanillaCommandsFeature {
    fn id(&self) -> &'static str {
        "vanilla-commands"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            registry::register(&mut cx.registries().commands);
            cx.registries()
                .services
                .push(std::sync::Arc::new(function::FunctionTickerFactory));
            cx.registries()
                .services
                .push(std::sync::Arc::new(bossbar::BossBarFactory));
            cx.registries()
                .services
                .push(std::sync::Arc::new(gametest::GameTestsFactory));
            cx.registries()
                .session_hooks
                .push(std::sync::Arc::new(bossbar::BossBarHooks));
        }
        Ok(())
    }
}

#![recursion_limit = "256"]
//! Runtime plugins as WebAssembly modules (`wasm-plugins`):
//! loaded, unloaded and reloaded while the server runs, from `.wasm`
//! files in the plugin directory, with `/tuff plugin`.
//!
//! The built-in plugin registers fixed bridge hooks once at startup;
//! everything a runtime plugin registers (commands, event subscriptions,
//! timers, per-player state, permission nodes) lives in the host actor's
//! record of that plugin, so unloading it removes every trace; only its
//! persistent store outlives it, by design, until `/tuff plugin purge`. Modules run in a wasmi (or wasmtime)
//! sandbox (pure Rust) with metered fuel and capped memory. The ABI is in
//! [`abi`], and the manifest format in [`manifest`].

pub mod abi;
pub mod background;
pub mod bridge;
pub mod caps;
pub mod combat;
pub mod command;
pub mod content;
pub mod dimensions;
pub mod display;
pub mod entities;
pub mod fake;
pub mod files;
pub mod grants_edit;
pub mod guard;
pub mod host;
pub mod inventory;
pub mod items;
pub mod line_filters;
pub mod manifest;
pub mod menus;
pub mod net;
pub mod panel;
pub mod parity;
pub mod particles;
pub mod payload;
pub mod player_state;
pub mod portal;
pub mod registry;
pub mod routes;
pub mod sandbox;
pub mod stash;
pub mod state;
pub mod storage;
pub mod wit;
pub mod world;
pub mod worldgen;

use std::path::PathBuf;
use std::sync::Arc;

use tuff_core::config::{Key, Value};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::permission::Node;
use tuff_core::world::commands::registry::TuffSubcommand;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_core::world::session::hooks::Interceptor;
use tuff_item::load::{LoadError, Phase};

pub use host::WasmHost;
pub use sandbox::Limits;

pub const ID: &str = "wasm-plugins";

/// Where the shipped server looks for modules by default, relative to its
/// working directory.
pub const DEFAULT_DIRECTORY: &str = "plugins";

pub const ENABLED_KEY: &str = "tuff.plugins.wasm.enabled";
pub const DIRECTORY_KEY: &str = "tuff.plugins.wasm.directory";
pub const LOGIN_TIMEOUT_KEY: &str = "tuff.plugins.wasm.loginTimeoutMillis";
pub const LOGIN_FAIL_CLOSED_KEY: &str = "tuff.plugins.wasm.loginFailClosed";

fn config_keys() -> [Key; 4] {
    [
        Key::new(
            ENABLED_KEY,
            Value::Bool(true),
            "Whether runtime WebAssembly plugins load, from the plugin directory at startup and with /tuff plugin; turning it off unloads them all",
        ),
        Key::new(
            DIRECTORY_KEY,
            Value::String(DEFAULT_DIRECTORY.to_owned()),
            "The directory runtime WebAssembly plugins load from, relative to the working directory",
        ),
        Key::new(
            LOGIN_TIMEOUT_KEY,
            Value::Int(bridge::LOGIN_TIMEOUT_MS),
            "Milliseconds the server waits for plugins to decide a login before the fail-closed setting applies",
        )
        .validated(|value| match value {
            Value::Int(ms) if (1..=10_000).contains(ms) => Ok(()),
            _ => Err("must be between 1 and 10000".to_owned()),
        }),
        Key::new(
            LOGIN_FAIL_CLOSED_KEY,
            Value::Bool(false),
            "Whether a login that plugins did not decide in time is refused instead of admitted",
        ),
    ]
}

/// The plugin. `directory` is where modules are loaded from; `None` uses
/// the [`DIRECTORY_KEY`] setting for a world on disk and no directory
/// otherwise.
#[derive(Default)]
pub struct WasmPluginsFeature {
    pub directory: Option<PathBuf>,
    pub limits: Limits,
}

impl WasmPluginsFeature {
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: Some(directory.into()),
            limits: Limits::default(),
        }
    }
}

struct Factory {
    directory: Option<PathBuf>,
    limits: Limits,
}

impl ServiceFactory for Factory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(WasmHost::new(self.directory.clone(), self.limits))
    }
}

impl Feature for WasmPluginsFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != Phase::Content {
            return Ok(());
        }
        let registries = cx.registries();
        registries.config_keys.extend(config_keys());
        registries.permissions.register(
            ID,
            Node::new(
                command::NODE,
                "Load, unload, reload, list and purge the data of runtime plugins with /tuff plugin",
            ),
        )?;
        registries
            .dimension_providers
            .push(Arc::new(worldgen::WorldgenProvider::new(
                self.directory.clone(),
                self.limits,
            )));
        registries
            .content_providers
            .push(Arc::new(content::WasmContent::new(
                self.directory.clone(),
                self.limits,
            )));
        registries.services.push(Arc::new(Factory {
            directory: self.directory.clone(),
            limits: self.limits,
        }));
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(bridge::Commands),
        });
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(bridge::Clicks),
        });
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(menus::MenuClicks),
        });
        registries.session_hooks.push(Arc::new(menus::MenuSessions));
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(combat::Attacks),
        });
        registries.session_hooks.push(Arc::new(combat::Damage));
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(guard::Uses),
        });
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(items::Actions),
        });
        registries.session_hooks.push(Arc::new(items::Pickups));
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(payload::Payloads),
        });
        registries.packet_interceptors.push(Interceptor {
            order: bridge::ORDER,
            hook: Arc::new(inventory::Clicks),
        });
        registries.session_hooks.push(Arc::new(inventory::Windows));
        registries.session_hooks.push(Arc::new(state::States));
        registries.region_hooks.push(Arc::new(guard::Blocks));
        registries.session_hooks.push(Arc::new(guard::Sessions));
        registries.login_gates.push(Arc::new(bridge::LoginBridge));
        registries.session_hooks.push(Arc::new(bridge::Sessions));
        registries.region_hooks.push(Arc::new(bridge::Blocks));
        registries.region_hooks.push(Arc::new(bridge::Entities));
        registries.region_hooks.push(Arc::new(combat::Damage));
        registries.region_hooks.push(Arc::new(portal::InsideHooks));
        registries.session_hooks.push(Arc::new(portal::Contacts));
        registries.commands.add_line_claim(Arc::new(bridge::Lines));
        registries.commands.add_tuff_subcommand(TuffSubcommand {
            node: &command::PLUGIN,
            providers: &[
                ("id", tuff_core::command::ASK_SERVER),
                ("cap", tuff_core::command::ASK_SERVER),
                ("entry", tuff_core::command::ASK_SERVER),
            ],
            executor: Arc::new(command::PluginCommand),
        });
        Ok(())
    }
}

#[cfg(all(feature = "runtime-wasmi", feature = "runtime-wasmtime"))]
compile_error!("features `runtime-wasmi` and `runtime-wasmtime` are mutually exclusive");
#[cfg(not(any(feature = "runtime-wasmi", feature = "runtime-wasmtime")))]
compile_error!("enable exactly one of the features `runtime-wasmi` and `runtime-wasmtime`");

//! The plugin ABI: plugins built against `wit/` (the `plugin` world) with
//! a canonical-ABI binding generator (wit-bindgen for Rust and C, or
//! anything else that emits a core module) run on wasmi or wasmtime as plain core
//! modules, without componentization. The host lifts and lowers the
//! synchronous canonical ABI itself from layouts [`schema`] computes with
//! wit-parser; [`canon`] bounds everything read from the guest. The typed
//! Rust side of the world, [`bindings`], is generated from the same files
//! by the build script.

use std::sync::{Arc, OnceLock};

#[cfg(feature = "runtime-wasmi")]
#[path = "backend_wasmi.rs"]
mod backend;
#[cfg(feature = "runtime-wasmtime")]
#[path = "backend_wasmtime.rs"]
mod backend;

pub mod canon;
pub mod runtime;
pub mod schema;
pub mod testing;
pub mod tree;
pub mod value;

#[expect(
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    reason = "generated bindings carry their own allow attributes"
)]
#[allow(clippy::all, dead_code)]
pub mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use schema::Schema;

/// The WIT package, as `(file name, text)`, in the order the build script
/// reads it.
pub const FILES: [(&str, &str); 2] = [
    (
        "capabilities.wit",
        include_str!("../../wit/capabilities.wit"),
    ),
    ("plugin.wit", include_str!("../../wit/plugin.wit")),
];
pub const WORLD: &str = "plugin-worldgen";

/// The parsed world every plugin is built against, laid out in the order
/// of [`bindings::IMPORTS`] and [`bindings::EXPORTS`].
pub fn schema() -> Arc<Schema> {
    static SCHEMA: OnceLock<Arc<Schema>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let schema = Schema::parse_files(&FILES, WORLD).expect("the plugin WIT is valid");
            let imports: Vec<(&str, &str)> = schema
                .imports
                .iter()
                .map(|sig| (sig.module.as_str(), sig.field.as_str()))
                .collect();
            assert_eq!(imports, bindings::IMPORTS, "bindings are stale");
            let exports: Vec<&str> = schema.exports.iter().map(|s| s.field.as_str()).collect();
            assert_eq!(exports, bindings::EXPORTS, "bindings are stale");
            Arc::new(schema)
        })
        .clone()
}

#[cfg(test)]
mod tests;

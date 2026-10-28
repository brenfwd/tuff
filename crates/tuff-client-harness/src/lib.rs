//! Fetches and caches the vanilla client (jar, libraries, assets) for the
//! opt-in real-client test harness. Launching is supervised and lock-guarded; nothing here runs under `cargo t`.

#[path = "../../tuff-vanilla-data/build/cache.rs"]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the cache helpers are used by the build script, tests use a subset"
    )
)]
mod cache;
pub mod control;
pub mod fetch;
pub mod latency;
pub mod launch;
pub mod lock;
pub mod log;
pub mod meta;
pub mod protocol;
pub mod supervisor;

pub use fetch::{
    AssetProfile, ClientFiles, HttpSource, Source, cache_dir, ensure_client, resolve_java,
};

mod cache;
mod codegen;
mod fetch;
mod input;
mod names;

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const VERSION: &str = "26.3";

fn main() -> Result<()> {
    println!("cargo::rerun-if-changed=build");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").context("OUT_DIR is not set")?);
    let cache = match env::var_os(fetch::CACHE_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => target_dir(&out_dir).join("vanilla"),
    };
    let vanilla = fetch::prepare(VERSION, &cache)?;
    let input = input::Input::load(VERSION, &vanilla)?;
    codegen::generate(&input, &out_dir)
}

/// `OUT_DIR` is `<target>/<profile>/build/<package>...`, so the download cache
/// can live in `<target>` and survive across profiles and build-script hashes.
/// Falls back to `OUT_DIR` for unrecognized layouts.
fn target_dir(out_dir: &Path) -> &Path {
    out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(|build| build.parent()?.parent())
        .unwrap_or(out_dir)
}

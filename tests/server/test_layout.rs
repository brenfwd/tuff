//! The root package lists its test binaries explicitly, so a new
//! `tests/*.rs` would otherwise never build: each one must be a `[[test]]`
//! of the root or the tuff-tests manifest.

use std::fs;
use std::path::Path;

#[tuff_core::watchdog]
#[test]
fn every_top_level_test_file_is_a_test_target() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let manifests = fs::read_to_string(root.join("Cargo.toml")).unwrap()
        + &fs::read_to_string(root.join("crates/tuff-tests/Cargo.toml")).unwrap();
    let mut missing = Vec::new();
    for entry in fs::read_dir(root.join("tests")).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".rs")
            && !manifests.contains(&format!("name = \"{stem}\""))
        {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "not built by any manifest (move them into tests/server/ as modules): {missing:?}"
    );
}

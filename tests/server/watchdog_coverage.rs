//! Every test in the workspace carries `#[tuff_core::watchdog]`, so a
//! hang anywhere aborts the run naming the test instead of stalling it.
//! `tuff-vanilla-data` and `tuff-macros` are exempt: they do not depend on core.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_test_attribute(line: &str) -> bool {
    line == "#[test]" || line.starts_with("#[tokio::test")
}

#[tuff_core::watchdog]
#[test]
fn every_test_arms_the_watchdog() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut files = Vec::new();
    rust_files(&root.join("tests"), &mut files);
    for entry in fs::read_dir(root.join("crates")).unwrap().flatten() {
        let name = entry.file_name();
        if name == "tuff-vanilla-data" || name == "tuff-macros" {
            continue;
        }
        rust_files(&entry.path().join("src"), &mut files);
        rust_files(&entry.path().join("tests"), &mut files);
    }
    let mut missing = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (index, line) in lines.iter().enumerate() {
            if !is_test_attribute(line) {
                continue;
            }
            let armed = lines[..index]
                .iter()
                .rev()
                .take_while(|above| above.starts_with("#["))
                .any(|above| *above == "#[tuff_core::watchdog]");
            if !armed {
                missing.push(format!("{}:{}", file.display(), index + 1));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "tests without #[tuff_core::watchdog]. Fix: add `#[tuff_core::watchdog]` as an attribute \
         directly above each `#[test]` / `#[tokio::test]` (in the same attribute block, e.g. \
         `#[tuff_core::watchdog]` then `#[test]`). Offenders:\n{}",
        missing.join("\n")
    );
}

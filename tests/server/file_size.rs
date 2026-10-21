//! Source files stay small enough to review. A non-test file over
//! `MAX_LINES` must be listed in `ALLOWED` with a ceiling it may not grow
//! past; shrink or remove the entry when the file is split.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 1500;

/// Files over the limit still waiting for their split, with their line ceiling.
const ALLOWED: [(&str, usize); 0] = [];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name != "tests" && name != "target")
            {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name != "tests.rs" && !name.ends_with("_tests.rs")
            })
        {
            out.push(path);
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn source_files_stay_under_the_size_limit() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("src"), &mut files);
    let mut problems = Vec::new();
    for file in files {
        let lines = fs::read_to_string(&file).map_or(0, |text| text.lines().count());
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let ceiling = ALLOWED
            .iter()
            .find(|(path, _)| *path == rel)
            .map_or(MAX_LINES, |(_, ceiling)| *ceiling);
        if lines > ceiling {
            problems.push(format!("{rel}: {lines} lines, limit {ceiling}"));
        }
    }
    for (path, _) in ALLOWED {
        let lines = fs::read_to_string(root.join(path)).map_or(0, |text| text.lines().count());
        if lines <= MAX_LINES {
            problems.push(format!("{path}: {lines} lines, remove it from ALLOWED"));
        }
    }
    assert!(
        problems.is_empty(),
        "file size guard:\n{}",
        problems.join("\n")
    );
}

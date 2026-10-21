//! Server code logs through the `log` macros so every line reaches the log
//! hub (terminal, `logs/latest.log`, console subscribers). Direct terminal
//! printing is only allowed for test code and the files listed below.

use std::fs;
use std::path::{Path, PathBuf};

/// Files allowed to print directly, with how many such lines they may have.
const ALLOWED: [(&str, usize); 4] = [
    ("crates/tuff-core/src/logging.rs", usize::MAX),
    ("crates/tuff-core/src/watchdog.rs", usize::MAX),
    ("src/gametest_cli.rs", 1),
    ("src/main.rs", 11),
];
/// Crates that are not server code or do not depend on core
const EXEMPT_CRATES: [&str; 5] = [
    "tuff-bot",
    "tuff-macros",
    "tuff-plugin-sdk-gen",
    "tuff-test-support",
    "tuff-vanilla-data",
];
const PRINTS: [&str; 4] = ["println!(", "eprintln!(", "print!(", "eprint!("];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "tests") {
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

fn prints(line: &str) -> bool {
    PRINTS.iter().any(|print| {
        line.match_indices(print).any(|(at, _)| {
            !line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
        })
    })
}

/// Lines that print outside comments and `#[cfg(test)] mod` blocks.
fn printing_lines(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if trimmed == "#[cfg(test)]"
            && let Some(next) = lines.get(index + 1)
            && next.trim_start().starts_with("mod ")
            && next.trim_end().ends_with('{')
        {
            let indent = &next[..next.len() - next.trim_start().len()];
            let close = format!("{indent}}}");
            index += 2;
            while index < lines.len() && lines[index] != close {
                index += 1;
            }
        } else if !trimmed.starts_with("//") && prints(line) {
            found.push(index + 1);
        }
        index += 1;
    }
    found
}

#[tuff_core::watchdog]
#[test]
fn server_code_logs_instead_of_printing() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    for entry in fs::read_dir(root.join("crates")).unwrap().flatten() {
        if EXEMPT_CRATES.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        rust_files(&entry.path().join("src"), &mut files);
    }
    let mut offending = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let found = printing_lines(&fs::read_to_string(file).unwrap());
        let allowed = ALLOWED
            .iter()
            .find(|(path, _)| *path == relative)
            .map_or(0, |(_, count)| *count);
        if found.len() > allowed {
            offending.extend(found.iter().map(|line| format!("{relative}:{line}")));
        }
    }
    assert!(
        offending.is_empty(),
        "use the log macros (log::info!/warn!/error!) instead of printing:\n{}",
        offending.join("\n")
    );
}

#[tuff_core::watchdog]
#[test]
fn the_scan_skips_test_modules_and_comments() {
    let text = "fn a() {\n    eprintln!(\"x\");\n    log::info!(\"y\");\n    // println!(\"z\")\n}\n\
                #[cfg(test)]\nfn helper() { println!(\"q\"); }\n\
                #[cfg(test)]\nmod tests {\n    fn b() { println!(\"t\"); }\n}\n\
                fn c() { print!(\"w\"); }\n";
    assert_eq!(printing_lines(text), [2, 7, 12]);
}

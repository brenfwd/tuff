//! A block exists exactly once. The tokens (`BrokenBlock`, `HeldBlock`) make
//! that a matter of types; this scan keeps the few doors that mint one out
//! of thin air confined to the places that need them.

use std::fs;
use std::path::{Path, PathBuf};

struct Door {
    needle: &'static str,
    /// Files (relative to the repo root) allowed to use it anywhere.
    allowed: &'static [&'static str],
    /// Whether code after a `#[cfg(test)]` line of any file may use it.
    in_tests: bool,
}

const DOORS: &[Door] = &[
    Door {
        needle: "BrokenBlock::simulated",
        allowed: &["crates/tuff-feature-builtin-vanilla-commands/src/loot.rs"],
        in_tests: false,
    },
    Door {
        needle: "HeldBlock::from_storage",
        allowed: &[
            "crates/tuff-core/src/world/region/pistons.rs",
            "crates/tuff-feature-builtin-vanilla-blocks/src/falling.rs",
            "crates/tuff-feature-builtin-vanilla-mobs/src/enderman.rs",
        ],
        in_tests: false,
    },
    Door {
        needle: "BrokenBlock::test(",
        allowed: &[],
        in_tests: true,
    },
    Door {
        needle: "HeldBlock::test(",
        allowed: &[],
        in_tests: true,
    },
    Door {
        needle: "TntOrigin::Command",
        allowed: &[],
        in_tests: false,
    },
    Door {
        needle: "TntOrigin::Duplicated",
        allowed: &[],
        in_tests: false,
    },
    Door {
        needle: "block_ledger::duplicate(",
        allowed: &["crates/tuff-core/src/world/region/ownership.rs"],
        in_tests: false,
    },
    Door {
        needle: "block_ledger::duplicate_tnt(",
        allowed: &["crates/tuff-core/src/world/region/explosions.rs"],
        in_tests: false,
    },
    Door {
        needle: "block_ledger::duplicate_held(",
        allowed: &["crates/tuff-feature-builtin-vanilla-blocks/src/falling.rs"],
        in_tests: false,
    },
    Door {
        needle: "MintReason::BlockDrop",
        allowed: &[],
        in_tests: false,
    },
];

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

#[tuff_core::watchdog]
#[test]
fn block_minting_doors_stay_at_their_sites() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .canonicalize()
        .unwrap();
    let mut files = Vec::new();
    for entry in fs::read_dir(root.join("crates")).unwrap().flatten() {
        rust_files(&entry.path().join("src"), &mut files);
    }
    let ledger = "crates/tuff-core/src/world/block_ledger.rs";
    let mut offenders = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ledger {
            continue;
        }
        let text = fs::read_to_string(file).unwrap();
        let tests_from = text
            .lines()
            .position(|line| line.trim() == "#[cfg(test)]")
            .unwrap_or(usize::MAX);
        for (index, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for door in DOORS {
                if !line.contains(door.needle) || door.allowed.contains(&relative.as_str()) {
                    continue;
                }
                if door.in_tests && index > tests_from {
                    continue;
                }
                offenders.push(format!("{relative}:{}: {}", index + 1, door.needle));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "block doors used outside their allowlist. Fix: take the block through a claim or \
         take_up, or add the site to the allowlist in tests/server/block_doors.rs with a \
         reason. Offenders:\n{}",
        offenders.join("\n")
    );
}

//! The root test binaries link core, the feature crates and their native
//! libraries through `libtuff_dylib.so`. A crate or C library that also ends
//! up statically in the test binary would run with two copies of its
//! statics (two rayon pools, two log hubs, two SQLite lock tables), so the
//! binary must define no static of a crate the dylib carries and no symbol
//! the dylib defines outside Rust.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(not(target_os = "macos"))]
fn dylib_path() -> Option<PathBuf> {
    std::fs::read_to_string("/proc/self/maps")
        .ok()?
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| path.ends_with("/libtuff_dylib.so"))
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn dylib_path() -> Option<PathBuf> {
    unsafe extern "C" {
        fn _dyld_image_count() -> u32;
        fn _dyld_get_image_name(index: u32) -> *const std::ffi::c_char;
    }
    (0..unsafe { _dyld_image_count() })
        .map(|i| unsafe { std::ffi::CStr::from_ptr(_dyld_get_image_name(i)) })
        .map(|name| name.to_string_lossy().into_owned())
        .find(|path| path.ends_with("/libtuff_dylib.dylib"))
        .map(PathBuf::from)
}

/// Mach-O symbol names carry a leading underscore that ELF names lack.
fn unprefixed(name: &str) -> &str {
    if cfg!(target_os = "macos") {
        name.strip_prefix('_').unwrap_or(name)
    } else {
        name
    }
}

/// `(kind, name)` of every symbol `path` defines, or `None` without `nm`.
fn defined_symbols(path: &Path) -> Option<Vec<(char, String)>> {
    let out = Command::new("nm")
        .arg("--defined-only")
        .arg(path)
        .output()
        .ok()?;
    assert!(out.status.success(), "nm failed on {}", path.display());
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let _address = parts.next()?;
                let kind = parts.next()?.chars().next()?;
                Some((kind, unprefixed(parts.next()?).to_owned()))
            })
            .collect(),
    )
}

/// The crate a v0-mangled plain item path (`_RNvNtC...` of statics and
/// functions) belongs to; `None` for impls, instantiations and other names.
fn plain_item_crate(symbol: &str) -> Option<&str> {
    let bytes = symbol.as_bytes();
    let mut at = 2;
    (symbol.starts_with("_R") && symbol.len() > 2).then_some(())?;
    path_crate(symbol, bytes, &mut at)
}

fn path_crate<'a>(symbol: &'a str, bytes: &[u8], at: &mut usize) -> Option<&'a str> {
    match *bytes.get(*at)? {
        b'N' => {
            *at += 2;
            let krate = path_crate(symbol, bytes, at)?;
            identifier(symbol, bytes, at)?;
            Some(krate)
        }
        b'C' => {
            *at += 1;
            identifier(symbol, bytes, at)
        }
        _ => None,
    }
}

fn identifier<'a>(symbol: &'a str, bytes: &[u8], at: &mut usize) -> Option<&'a str> {
    if bytes.get(*at) == Some(&b's') {
        *at += bytes[*at..].iter().position(|&b| b == b'_')? + 1;
    }
    if bytes.get(*at) == Some(&b'u') {
        *at += 1;
    }
    let digits = bytes[*at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let len: usize = symbol.get(*at..*at + digits)?.parse().ok()?;
    *at += digits;
    if bytes.get(*at) == Some(&b'_') {
        *at += 1;
    }
    let name = symbol.get(*at..*at + len)?;
    *at += len;
    Some(name)
}

fn is_native(name: &str) -> bool {
    !name.starts_with("_R")
        && !name.starts_with("_ZN")
        && !name.starts_with('.')
        && !name.starts_with("__")
        && !name.starts_with("GCC_except_table")
        && !name.starts_with("DW.ref.")
        && !name.starts_with("anon.")
        && !matches!(
            name,
            "_init"
                | "_fini"
                | "_etext"
                | "etext"
                | "edata"
                | "end"
                | "_DYNAMIC"
                | "_GLOBAL_OFFSET_TABLE_"
                | "_edata"
                | "_end"
                | "data_start"
                | "_start"
                | "_IO_stdin_used"
                | "main"
                | "rust_eh_personality"
        )
}

#[tuff_core::watchdog]
#[test]
fn no_crate_is_linked_both_statically_and_through_the_dylib() {
    let Some(dylib) = dylib_path() else {
        panic!("this binary does not map libtuff_dylib.so");
    };
    let exe = std::env::current_exe().unwrap();
    let (Some(exe_symbols), Some(dylib_symbols)) = (defined_symbols(&exe), defined_symbols(&dylib))
    else {
        eprintln!("nm not found; skipping the duplicate link check");
        return;
    };
    let dylib_crates: HashSet<&str> = dylib_symbols
        .iter()
        .filter_map(|(_, name)| plain_item_crate(name))
        .collect();
    let dylib_native: HashSet<&str> = dylib_symbols
        .iter()
        .filter(|(_, name)| is_native(name))
        .map(|(_, name)| name.as_str())
        .collect();
    assert!(dylib_crates.contains("tuff_core"), "{dylib_crates:?}");

    let mut twice = BTreeSet::new();
    for (kind, name) in &exe_symbols {
        let is_static = matches!(kind.to_ascii_lowercase(), 'b' | 'd');
        if is_static
            && let Some(krate) = plain_item_crate(name)
            && dylib_crates.contains(krate)
        {
            twice.insert(format!("static of {krate}: {name}"));
        }
        if kind.is_ascii_uppercase() && is_native(name) && dylib_native.contains(name.as_str()) {
            twice.insert(format!("native symbol {name}"));
        }
    }
    assert!(
        twice.is_empty(),
        "linked both into {} and {}:\n{}",
        exe.display(),
        dylib.display(),
        twice.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[tuff_core::watchdog]
#[test]
fn plain_item_paths_name_their_crate() {
    assert_eq!(
        plain_item_crate("_RNvNtCs1a2B3_9tuff_core7logging3HUB"),
        Some("tuff_core")
    );
    assert_eq!(plain_item_crate("_RNvCsx_5rayon8REGISTRY"), Some("rayon"));
    assert_eq!(
        plain_item_crate("_RNvMNtCs1_5tokio4tasksNtB2_4Task3new"),
        None
    );
    assert_eq!(plain_item_crate("sqlite3_open"), None);
}

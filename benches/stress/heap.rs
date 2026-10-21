//! Where the sampled live heap sits (`--heap_sample <bytes>`): call stacks
//! from the bench allocator, symbolized with `atos` on macOS, as the
//! biggest stacks and the functions holding the most bytes, either now or
//! grown since an earlier snapshot.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::alloc::{self, Site, Stack};

const MIB: f64 = 1024.0 * 1024.0;
const TOP_STACKS: usize = 40;
const TOP_FUNCTIONS: usize = 80;
const FRAMES_SHOWN: usize = 14;
const SYMBOLIZE_MIN_BYTES: f64 = 32.0 * 1024.0;
#[cfg(target_os = "macos")]
const ATOS_BATCH: usize = 2000;

/// Frames that only say an allocation happened, skipped at a stack's top.
const PLUMBING: &[&str] = &[
    "stress::alloc::",
    "<stress::alloc::",
    "__rustc::",
    "__rust",
    "__rdl",
    "__rg_",
    "alloc::",
    "<alloc::",
    "core::",
    "<core::",
    "hashbrown::",
    "<hashbrown::",
    "std::collections::",
    "<std::collections::",
    "bytes::",
    "<bytes::",
    "smallvec::",
    "<smallvec::",
];

pub type Snapshot = HashMap<Stack, Site>;

pub fn snapshot() -> Option<Snapshot> {
    (alloc::sampling() > 0).then(|| {
        alloc::live_sites()
            .into_iter()
            .map(|site| (site.stack, site))
            .collect()
    })
}

/// The heap now, or its growth since `since`, as JSON.
pub async fn report(now: &Snapshot, since: Option<&Snapshot>) -> Value {
    let mut sites: Vec<(Stack, f64, i64, u64)> = now
        .values()
        .map(|site| {
            let before = since.and_then(|s| s.get(&site.stack));
            (
                site.stack,
                site.bytes - before.map_or(0.0, |b| b.bytes),
                site.samples as i64 - before.map_or(0, |b| b.samples as i64),
                site.largest,
            )
        })
        .collect();
    if let Some(since) = since {
        sites.extend(
            since
                .values()
                .filter(|site| !now.contains_key(&site.stack))
                .map(|site| {
                    (
                        site.stack,
                        -site.bytes,
                        -(site.samples as i64),
                        site.largest,
                    )
                }),
        );
    }
    let total: f64 = sites.iter().map(|s| s.1).sum();
    let addresses: Vec<usize> = sites
        .iter()
        .filter(|s| s.1.abs() >= SYMBOLIZE_MIN_BYTES)
        .flat_map(|s| s.0.iter().copied().filter(|&ip| ip != 0))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let names = tokio::task::spawn_blocking(move || symbolize(&addresses))
        .await
        .unwrap_or_default();
    let name = |ip: usize| {
        names
            .get(&ip)
            .cloned()
            .unwrap_or_else(|| format!("{ip:#x}"))
    };
    let frames = |stack: &Stack| -> Vec<String> {
        stack
            .iter()
            .filter(|&&ip| ip != 0)
            .map(|&ip| name(ip))
            .skip_while(|frame| PLUMBING.iter().any(|p| frame.starts_with(p)))
            .collect()
    };
    let mut functions: HashMap<String, f64> = HashMap::new();
    for (stack, bytes, _, _) in &sites {
        if bytes.abs() < SYMBOLIZE_MIN_BYTES {
            continue;
        }
        let unique: HashSet<String> = frames(stack).into_iter().map(|f| function(&f)).collect();
        for f in unique {
            *functions.entry(f).or_default() += bytes;
        }
    }
    let mut functions: Vec<(String, f64)> = functions.into_iter().collect();
    functions.sort_by(|a, b| b.1.total_cmp(&a.1));
    sites.sort_by(|a, b| b.1.total_cmp(&a.1));
    json!({
        "sample_every": alloc::sampling(),
        "total_mib": total / MIB,
        "stacks": sites.iter().take(TOP_STACKS).map(|(stack, bytes, samples, largest)| json!({
            "mib": bytes / MIB,
            "samples": samples,
            "largest": largest,
            "frames": frames(stack).into_iter().take(FRAMES_SHOWN).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "large_objects": large_objects(&sites, &frames),
        "functions": functions.iter().take(TOP_FUNCTIONS).map(|(f, bytes)| json!([bytes / MIB, f])).collect::<Vec<_>>(),
    })
}

/// Sizes of medium and large pages' objects (84 KiB to 512 KiB) pin a
/// whole 4 MiB allocator page each, so these are listed apart.
const LARGE_OBJECT: u64 = 84 << 10;

fn large_objects(
    sites: &[(Stack, f64, i64, u64)],
    frames: &dyn Fn(&Stack) -> Vec<String>,
) -> Value {
    sites
        .iter()
        .filter(|(_, bytes, samples, largest)| {
            *largest >= LARGE_OBJECT && *bytes > 0.0 && *samples > 0
        })
        .take(TOP_STACKS)
        .map(|(stack, bytes, samples, largest)| {
            json!({
                "mib": bytes / MIB,
                "samples": samples,
                "largest": largest,
                "frames": frames(stack).into_iter().take(FRAMES_SHOWN).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// A frame's function, without the `(in binary)` and `(file:line)` parts.
fn function(frame: &str) -> String {
    frame.split(" (").next().unwrap_or(frame).trim().to_owned()
}

/// An `atos` line with a v0-mangled Rust symbol turned into its path
/// segments (`crate::module::Type::method`, generic arguments flattened
/// in), keeping the `(file:line)` when there is one.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only the macOS atos path and the bench_memory test call it"
    )
)]
pub fn readable(line: &str) -> String {
    let (symbol, rest) = line.split_once(' ').unwrap_or((line, ""));
    let Some(mangled) = symbol.strip_prefix("_R") else {
        return line.to_owned();
    };
    let bytes = mangled.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let disambiguator = c == b's' && i > 0 && b"MNCXYI".contains(&bytes[i - 1]);
        if mangled[i..].starts_with("Cs") || c == b'B' || disambiguator {
            match mangled[i..].find('_') {
                Some(end) => i += end + 1,
                None => break,
            }
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let len: usize = mangled[start..i].parse().unwrap_or(0);
            if bytes.get(i) == Some(&b'_') {
                i += 1;
            }
            if let Some(ident) = mangled.get(i..i + len)
                && len > 0
                && ident
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                parts.push(ident);
                i += len;
            }
            continue;
        }
        i += 1;
    }
    let place = rest
        .rsplit_once('(')
        .and_then(|(_, place)| place.strip_suffix(')'))
        .filter(|place| place.contains(':'));
    match place {
        Some(place) => format!("{} ({place})", parts.join("::")),
        None => parts.join("::"),
    }
}

#[cfg(target_os = "macos")]
fn symbolize(addresses: &[usize]) -> HashMap<usize, String> {
    unsafe extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const u8;
    }
    let load = unsafe { _dyld_get_image_header(0) } as usize;
    let Ok(exe) = std::env::current_exe() else {
        return HashMap::new();
    };
    let mut names = HashMap::new();
    for batch in addresses.chunks(ATOS_BATCH) {
        let mut command = std::process::Command::new("atos");
        command
            .arg("-o")
            .arg(&exe)
            .arg("-l")
            .arg(format!("{load:#x}"));
        command.args(batch.iter().map(|ip| format!("{:#x}", ip - 1)));
        let Ok(output) = command.output() else {
            break;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        for (ip, line) in batch.iter().zip(text.lines()) {
            names.insert(*ip, readable(line));
        }
    }
    names
}

#[cfg(not(target_os = "macos"))]
fn symbolize(_addresses: &[usize]) -> HashMap<usize, String> {
    HashMap::new()
}

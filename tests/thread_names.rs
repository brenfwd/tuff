//! Every thread the server starts carries a role-based name: the tokio
//! runtime, the rayon pools and each std thread (checked by scanning the
//! non-test sources).

extern crate tuff_dylib;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tuff_core::threads;

fn current_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned()
}

/// Waits until `parties` callers arrived (or a timeout passes), so each
/// caller holds its thread while the others pick theirs.
#[derive(Clone)]
struct Rendezvous(Arc<(Mutex<usize>, Condvar)>, usize);

impl Rendezvous {
    fn new(parties: usize) -> Self {
        Self(Arc::new((Mutex::new(0), Condvar::new())), parties)
    }

    fn arrive(&self) -> String {
        let (count, ready) = &*self.0;
        let mut count = count.lock().unwrap();
        *count += 1;
        ready.notify_all();
        let _ = ready
            .wait_timeout_while(count, Duration::from_secs(20), |count| *count < self.1)
            .unwrap();
        current_name()
    }
}

#[tuff_core::watchdog]
#[test]
fn runtime_and_pools_name_their_threads() {
    assert!(threads::init_rayon());
    let mut global = rayon::broadcast(|ctx| (ctx.index(), current_name()));
    global.sort();
    for (index, name) in &global {
        assert_eq!(*name, format!("rayon-{index}"));
    }

    let pool = threads::rayon_pool("map").num_threads(2).build().unwrap();
    assert_eq!(
        pool.broadcast(|_| current_name()),
        ["rayon-map-0", "rayon-map-1"]
    );

    let runtime = threads::name_tokio_workers(&mut tokio::runtime::Builder::new_multi_thread(), 2)
        .enable_all()
        .build()
        .map(threads::started)
        .unwrap();
    let (mut workers, mut blocking) = runtime.block_on(async {
        let meet = Rendezvous::new(2);
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let meet = meet.clone();
                tokio::spawn(async move { meet.arrive() })
            })
            .collect();
        let mut workers = Vec::new();
        for task in tasks {
            workers.push(task.await.unwrap());
        }
        let meet = Rendezvous::new(2);
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let meet = meet.clone();
                tokio::task::spawn_blocking(move || meet.arrive())
            })
            .collect();
        let mut blocking = Vec::new();
        for task in tasks {
            blocking.push(task.await.unwrap());
        }
        (workers, blocking)
    });
    workers.sort();
    blocking.sort();
    assert_eq!(workers, ["tokio-worker-0", "tokio-worker-1"]);
    assert_eq!(blocking, ["tokio-blocking-0", "tokio-blocking-1"]);

    for _ in 0..5 {
        let server = tuff::allocator::runtime().unwrap();
        assert_eq!(
            server.metrics().num_workers(),
            threads::default_tokio_workers()
        );
        let (blocking, worker) = server.block_on(async {
            let blocking = tokio::task::spawn_blocking(current_name).await.unwrap();
            let worker = tokio::spawn(async { current_name() }).await.unwrap();
            (blocking, worker)
        });
        assert_eq!(blocking, "tokio-blocking-0");
        assert!(worker.starts_with("tokio-worker-"), "{worker}");
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "tests") {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_stem().is_some_and(|stem| stem != "tests")
        {
            out.push(path);
        }
    }
}

/// The source with every `#[cfg(test)]` item blanked, line numbers kept.
fn without_tests(text: &str) -> String {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[cfg(test)]" {
            out.push(line);
            continue;
        }
        out.push("");
        let mut depth = 0i32;
        let mut opened = false;
        for line in lines.by_ref() {
            out.push("");
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            opened |= line.contains('{');
            let attribute = line.trim().starts_with("#[");
            if !attribute && ((opened && depth <= 0) || (!opened && line.trim_end().ends_with(';')))
            {
                break;
            }
        }
    }
    out.join("\n")
}

/// Where `needle` starts a path, not the tail of a longer identifier.
fn occurrences<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(needle)
        .map(|(at, _)| at)
        .filter(move |&at| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Each `needle` must be followed by `required` before the next `until`.
fn unnamed_chains(text: &str, needle: &str, until: &str, required: &str) -> Vec<usize> {
    occurrences(text, needle)
        .filter(|at| {
            let rest = &text[at + needle.len()..];
            let chain = rest.find(until).map_or(rest, |end| &rest[..end]);
            !chain.contains(required)
        })
        .map(|at| line_of(text, at))
        .collect()
}

/// Lines starting a thread without a name.
fn violations(source: &str) -> Vec<usize> {
    let text = without_tests(source);
    let mut lines: Vec<usize> = Vec::new();
    for needle in ["thread::spawn(", "Runtime::new()", "#[tokio::main"] {
        lines.extend(occurrences(&text, needle).map(|at| line_of(&text, at)));
    }
    lines.extend(unnamed_chains(
        &text,
        "thread::Builder::new()",
        ".spawn",
        ".name(",
    ));
    lines.extend(unnamed_chains(
        &text,
        "ThreadPoolBuilder::new()",
        ".build",
        ".thread_name(",
    ));
    for (at, _) in text.match_indices("new_multi_thread()") {
        let start = text[..at].rfind('\n').map_or(0, |n| n + 1);
        let rest = &text[at..];
        let chain = rest.find(".build()").map_or(rest, |end| &rest[..end]);
        if !text[start..at].contains("name_tokio") && !chain.contains("thread_name") {
            lines.push(line_of(&text, at));
        }
    }
    for (at, _) in text.match_indices("thread::scope(|") {
        let rest = &text[at + "thread::scope(|".len()..];
        let Some(param) = rest.split('|').next() else {
            continue;
        };
        let spawn = format!("{}.spawn(", param.trim());
        lines.extend(occurrences(&text, &spawn).map(|at| line_of(&text, at)));
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

#[tuff_core::watchdog]
#[test]
fn the_scan_finds_unnamed_threads() {
    let source = r#"
fn a() {
    std::thread::spawn(|| {});
    std::thread::Builder::new().stack_size(1).spawn(|| {});
    ConsoleRuntime::new();
    std::thread::Builder::new().name("ok".into()).spawn(|| {});
    std::thread::scope(|s| {
        s.spawn(|| {});
    });
    rayon::ThreadPoolBuilder::new().num_threads(2).build();
    tuff_core::threads::rayon_pool("ok").build();
    tokio::runtime::Builder::new_multi_thread().build();
    threads::name_tokio(&mut tokio::runtime::Builder::new_multi_thread()).build();
}

#[cfg(test)]
mod tests {
    fn b() {
        std::thread::spawn(|| {});
    }
}

#[cfg(test)]
fn c() {
    std::thread::spawn(|| {});
}
"#;
    assert_eq!(violations(source), [3, 4, 8, 10, 12]);
}

#[tuff_core::watchdog]
#[test]
fn every_std_thread_and_pool_is_named() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("benches"), &mut files);
    for entry in fs::read_dir(root.join("crates")).unwrap().flatten() {
        rust_files(&entry.path().join("src"), &mut files);
        rust_files(&entry.path().join("benches"), &mut files);
    }
    assert!(files.len() > 100, "found only {} files", files.len());
    let mut unnamed = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for line in violations(&text) {
            unnamed.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        unnamed.is_empty(),
        "threads started without a name (use std::thread::Builder::name, \
         tuff_core::threads::rayon_pool or tuff_core::threads::name_tokio):\n{}",
        unnamed.join("\n")
    );
}

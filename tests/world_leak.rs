#[path = "../benches/common/alloc.rs"]
mod alloc;

use std::time::Duration;

use tuff_test_support::{TestDir, TestWorld};

const WARMUP: usize = 2;
const WORLDS: usize = 8;
const BOUND_PER_WORLD: u64 = 128 << 10;

async fn start_and_drop() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let shared = std::sync::Arc::downgrade(&world.shared);
    drop(world);
    for _ in 0..100 {
        if shared.strong_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("a dropped world's shared state is still held");
}

async fn settled_live_bytes() -> u64 {
    let mut last = alloc::live_bytes();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = alloc::live_bytes();
        if now.abs_diff(last) < 4096 {
            return now;
        }
        last = now;
    }
    last
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, Iterator::count)
}

async fn settled_threads(limit: Option<usize>) -> usize {
    let mut last = threads();
    for _ in 0..200 {
        if limit.is_some_and(|l| last <= l) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = threads();
        if limit.is_none() && now == last {
            return now;
        }
        last = now;
    }
    last
}

fn dump_sites(path: &str) {
    use std::fmt::Write as _;
    let mut out = String::new();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let exe = std::env::current_exe().unwrap();
    let base = maps
        .lines()
        .find(|l| l.ends_with(exe.to_str().unwrap()))
        .and_then(|l| usize::from_str_radix(l.split('-').next()?, 16).ok())
        .unwrap_or(0);
    let _ = writeln!(out, "exe {} base {base:#x}", exe.display());
    let mut sites = alloc::live_sites();
    sites.sort_by(|a, b| b.bytes.total_cmp(&a.bytes));
    for site in sites.iter().take(60) {
        let frames: Vec<String> = site
            .stack
            .iter()
            .take_while(|&&a| a != 0)
            .map(|&a| format!("{:#x}", a.wrapping_sub(base)))
            .collect();
        let _ = writeln!(
            out,
            "{:.0} {} {}",
            site.bytes,
            site.samples,
            frames.join(" ")
        );
    }
    std::fs::write(path, out).unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn world_start_drop_keeps_heap_bounded() {
    let profile = std::env::var("WORLD_LEAK_PROFILE").ok();
    let worlds = std::env::var("WORLD_LEAK_N")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(WORLDS);
    for _ in 0..WARMUP {
        start_and_drop().await;
    }
    let before = settled_live_bytes().await;
    let threads_before = settled_threads(None).await;
    if profile.is_some() {
        alloc::sample_every(64 << 10);
    }
    for _ in 0..worlds {
        start_and_drop().await;
    }
    let after = settled_live_bytes().await;
    if let Some(path) = &profile {
        dump_sites(path);
    }
    let grown = after.saturating_sub(before);
    eprintln!("world_leak: {worlds} worlds grew live heap by {grown} bytes");
    assert!(
        grown < BOUND_PER_WORLD * worlds as u64,
        "{worlds} world starts kept {grown} bytes live"
    );
    let threads_after = settled_threads(Some(threads_before + 2)).await;
    assert!(
        threads_after <= threads_before + 2,
        "threads grew from {threads_before} to {threads_after}"
    );
}

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(target_os = "macos")]
use tuff::allocator::collected;
use tuff::allocator::{Trigger, idle, purge_delay, runtime, set_idle, trim};
use tuff_core::metrics::{ManagerReport, Snapshot};

/// Tests that trim, change the purge delay or measure the footprint run one
/// at a time so none sees another's collections or blocks.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Footprint after the OS has had up to ten seconds to settle below `limit`.
#[cfg(target_os = "macos")]
async fn settled_footprint_mib(limit: f64) -> f64 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let now = footprint_mib();
        if now < limit || std::time::Instant::now() > deadline {
            return now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn fires(counts: &[u64]) -> Vec<bool> {
    let mut trigger = Trigger::default();
    counts
        .iter()
        .map(|&chunks| trigger.observe(chunks))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn growing_and_steady_chunk_counts_never_trim() {
    assert!(
        fires(&[0, 100, 900, 2000, 1900, 1500, 2100, 1200])
            .iter()
            .all(|fired| !fired)
    );
}

#[tuff_core::watchdog]
#[test]
fn halving_the_peak_trims_once_and_resets_the_peak() {
    assert_eq!(
        fires(&[2000, 1200, 1000, 900, 450, 449, 0]),
        [false, false, true, false, true, false, true]
    );
}

#[tuff_core::watchdog]
#[test]
fn small_drops_do_not_trim() {
    assert_eq!(fires(&[100, 36, 0]), [false, true, false]);
    assert_eq!(fires(&[63, 0]), [false, false]);
}

#[tuff_core::watchdog]
#[test]
fn idle_needs_no_players_and_no_region_up_or_closing() {
    let snapshot = |players, regions, closing| {
        let mut snapshot = Snapshot::default();
        snapshot.world.players = players;
        snapshot.manager = Some(ManagerReport {
            regions,
            closing,
            ..ManagerReport::default()
        });
        snapshot
    };
    assert!(idle(&snapshot(0, 0, 0)));
    assert!(!idle(&snapshot(1, 0, 0)));
    assert!(!idle(&snapshot(0, 2, 0)));
    assert!(!idle(&snapshot(0, 0, 1)));
    assert!(!idle(&Snapshot::default()), "no manager report yet");
}

#[tuff_core::watchdog]
#[test]
fn trimming_keeps_the_runtime_and_rayon_working() {
    let _alone = serial();
    let runtime = runtime().unwrap();
    let total = runtime.block_on(async {
        let mut tasks = Vec::new();
        for round in 0..4u64 {
            trim().await;
            for task in 0..16u64 {
                tasks.push(tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    let data: Vec<u64> = (0..1024).map(|i| i * round + task).collect();
                    data.iter().sum::<u64>()
                }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let mut total = 0;
        for task in tasks {
            total += task.await.unwrap();
        }
        total
    });
    let expected: u64 = (0..4u64)
        .flat_map(|round| {
            (0..16u64).map(move |task| (0..1024).map(|i| i * round + task).sum::<u64>())
        })
        .sum();
    assert_eq!(total, expected);
    let sum: u64 = rayon::broadcast(|ctx| ctx.index() as u64).iter().sum();
    let threads = rayon::current_num_threads() as u64;
    assert_eq!(sum, threads * (threads - 1) / 2);
}

#[tuff_core::watchdog]
#[test]
fn idle_purges_soon_and_busy_restores_the_delay() {
    let _alone = serial();
    let busy = purge_delay();
    assert_eq!(busy, 1000, "the option index names mimalloc's purge delay");
    set_idle(true);
    assert_eq!(purge_delay(), 1);
    set_idle(false);
    assert_eq!(purge_delay(), busy);
}

#[cfg(target_os = "macos")]
fn footprint_mib() -> f64 {
    #[repr(C)]
    struct RusageInfoV0 {
        uuid: [u8; 16],
        fields: [u64; 10],
    }
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
    }
    let mut info = RusageInfoV0 {
        uuid: [0; 16],
        fields: [0; 10],
    };
    let ok = unsafe { proc_pid_rusage(std::process::id() as i32, 0, &mut info) };
    assert_eq!(ok, 0);
    info.fields[7] as f64 / 1048576.0
}

/// Memory freed on another thread than the one that allocated it stays
/// with mimalloc until the owning threads collect; a trim gives it back.
#[cfg(target_os = "macos")]
#[tuff_core::watchdog]
#[test]
fn trim_returns_memory_freed_across_threads() {
    use rayon::prelude::*;
    let _alone = serial();
    const TOTAL: usize = 256 << 20;
    let runtime = runtime().unwrap();
    let blocks: Vec<Vec<u8>> = (0..64usize)
        .into_par_iter()
        .flat_map_iter(|t| {
            (0..TOTAL / 64 / 4096).map(move |i| vec![1u8; 16 + (i * 7919 + t) % 8192])
        })
        .collect();
    let held = footprint_mib();
    drop(blocks);
    runtime.block_on(async {
        trim().await;
    });
    let trimmed = runtime.block_on(settled_footprint_mib(held - 0.6 * (TOTAL >> 20) as f64));
    assert!(
        held - trimmed > 0.6 * (TOTAL >> 20) as f64,
        "held {held:.0} MiB, after trim {trimmed:.0} MiB"
    );
}

/// About 2 MiB in small blocks.
#[cfg(target_os = "macos")]
fn small_blocks(seed: usize) -> Vec<Vec<u8>> {
    (0..512)
        .map(|i| vec![1u8; 16 + (i * 7919 + seed) % 8192])
        .collect()
}

/// A parked worker and an idle blocking thread run nothing that would
/// collect their heaps, so pages they allocated and another thread freed
/// stay with them unless a trim hands each one a task.
#[cfg(target_os = "macos")]
#[tuff_core::watchdog]
#[test]
fn trim_reaches_parked_workers_and_idle_blocking_threads() {
    let _alone = serial();
    let runtime = runtime().unwrap();
    let workers = runtime.metrics().num_workers();
    runtime.block_on(async {
        let start = footprint_mib();
        let mut tasks = Vec::new();
        for seed in 0..64 {
            tasks.push(tokio::spawn(async move { small_blocks(seed) }));
            tasks.push(tokio::task::spawn_blocking(move || small_blocks(seed + 64)));
        }
        let mut held = Vec::new();
        for task in tasks {
            held.push(task.await.unwrap());
        }
        let peak = footprint_mib();
        std::thread::spawn(move || drop(held)).join().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        trim().await;
        let (workers_collected, blocking_collected) = collected();
        assert!(
            workers_collected >= workers,
            "{workers_collected} of {workers} workers"
        );
        assert!(blocking_collected >= 1);
        let trimmed = settled_footprint_mib(start + 0.1 * (peak - start)).await;
        assert!(
            trimmed - start < 0.1 * (peak - start),
            "start {start:.0} MiB, peak {peak:.0} MiB, after trim {trimmed:.0} MiB"
        );
    });
}

//! Role-based thread names, so profilers and the timings panel can tell
//! the server's threads apart. A pool thread is `<pool>-<index>`.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

pub const RAYON: &str = "rayon";
pub const TOKIO_WORKER: &str = "tokio-worker";
pub const TOKIO_BLOCKING: &str = "tokio-blocking";

/// How much lower than the tick's threads the global pool (world
/// generation, lighting) runs, as a nice increment.
const BACKGROUND_NICE: i32 = 5;

/// Builds rayon's global pool with `rayon-<index>` threads, running below
/// the tick's priority so a busy tick wins the cores. False when the pool
/// already exists (someone used rayon first).
pub fn init_rayon() -> bool {
    rayon::ThreadPoolBuilder::new()
        .thread_name(|index| format!("{RAYON}-{index}"))
        .start_handler(|_| {
            lower_this_thread(BACKGROUND_NICE);
            tuff_datapack::tags::require_scope_on_this_thread()
        })
        .build_global()
        .is_ok()
}

/// Raises this thread's nice value by `by`; a no-op where threads cannot
/// be reprioritised one by one.
pub fn lower_this_thread(by: i32) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid has no preconditions, and setpriority on this
        // thread's own id only changes its scheduling.
        unsafe {
            let tid = libc::gettid();
            let current = libc::getpriority(libc::PRIO_PROCESS, tid as libc::id_t);
            libc::setpriority(
                libc::PRIO_PROCESS,
                tid as libc::id_t,
                (current + by).min(19),
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = by;
}

/// How long a thread that sleeps on a queue waits before checking for a trim.
pub const TRIM_POLL: Duration = Duration::from_secs(5);

static TRIM_EPOCH: AtomicU64 = AtomicU64::new(0);
static COLLECTOR: OnceLock<fn()> = OnceLock::new();
static POOLS: Mutex<Vec<Weak<rayon::ThreadPool>>> = Mutex::new(Vec::new());

/// Installs the heap collection that [`collect_on_trim`] runs.
pub fn set_collector(collector: fn()) {
    let _ = COLLECTOR.set(collector);
}

/// Starts a trim round; returns its epoch.
pub fn begin_trim() -> u64 {
    TRIM_EPOCH.fetch_add(1, Ordering::AcqRel) + 1
}

/// Collects this thread's heap once per trim round. Threads that sleep on a
/// queue call it when they wake, and on a [`TRIM_POLL`] timeout.
pub fn collect_on_trim() {
    thread_local!(static SEEN: Cell<u64> = const { Cell::new(0) });
    let epoch = TRIM_EPOCH.load(Ordering::Acquire);
    if SEEN.get() != epoch {
        SEEN.set(epoch);
        if let Some(collector) = COLLECTOR.get() {
            collector();
        }
    }
}

/// Adds a dedicated pool to those [`collect_pools`] reaches.
pub fn register_pool(pool: &Arc<rayon::ThreadPool>) {
    let mut pools = POOLS.lock().unwrap_or_else(|e| e.into_inner());
    pools.retain(|pool| pool.strong_count() > 0);
    pools.push(Arc::downgrade(pool));
}

/// Runs [`collect_on_trim`] on every thread of every registered pool.
pub fn collect_pools() {
    let pools: Vec<_> = POOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for pool in pools {
        pool.broadcast(|_| collect_on_trim());
    }
}

/// A dedicated rayon pool whose threads are `rayon-<pool>-<index>`.
pub fn rayon_pool(pool: &str) -> rayon::ThreadPoolBuilder {
    let prefix = format!("{RAYON}-{pool}");
    rayon::ThreadPoolBuilder::new()
        .thread_name(move |index| format!("{prefix}-{index}"))
        .start_handler(|_| tuff_datapack::tags::require_scope_on_this_thread())
}

/// The async worker count tokio would pick on its own.
pub fn default_tokio_workers() -> usize {
    std::env::var("TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&workers| workers > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, NonZeroUsize::get))
}

/// Names a multi-thread runtime's async workers `tokio-worker-<index>` and
/// its blocking pool `tokio-blocking-<index>`, with the default worker count.
pub fn name_tokio(builder: &mut tokio::runtime::Builder) -> &mut tokio::runtime::Builder {
    name_tokio_workers(builder, default_tokio_workers())
}

/// [`name_tokio`] with `workers` async workers. Tokio starts every worker
/// through its blocking pool while building the runtime, so the first
/// `workers` threads it names are the workers; pass the built runtime
/// through [`started`] so no blocking task can take a worker's thread.
pub fn name_tokio_workers(
    builder: &mut tokio::runtime::Builder,
    workers: usize,
) -> &mut tokio::runtime::Builder {
    let names = Arc::new(TokioNames::new(workers));
    let stopped = names.clone();
    builder
        .worker_threads(workers)
        .thread_name_fn(move || names.next())
        .on_thread_start(tuff_datapack::tags::require_scope_on_this_thread)
        .on_thread_stop(move || stopped.release(std::thread::current().name()))
}

/// Waits (briefly) until every async worker runs. Until then the blocking
/// pool's queue still holds worker start-ups, and a blocking task queued
/// behind them could land on a thread named for a worker.
pub fn started(runtime: tokio::runtime::Runtime) -> tokio::runtime::Runtime {
    let metrics = runtime.metrics();
    let deadline = Instant::now() + Duration::from_secs(2);
    while (0..metrics.num_workers()).any(|worker| metrics.worker_park_count(worker) == 0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_micros(100));
    }
    runtime
}

/// Hands out tokio thread names; a stopped blocking thread's index is
/// reused, so idle churn does not grow the numbers forever.
#[derive(Debug)]
struct TokioNames {
    workers: usize,
    state: Mutex<NameState>,
}

#[derive(Debug, Default)]
struct NameState {
    workers_named: usize,
    next_blocking: usize,
    free: BTreeSet<usize>,
}

impl TokioNames {
    fn new(workers: usize) -> Self {
        Self {
            workers,
            state: Mutex::default(),
        }
    }

    fn next(&self) -> String {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.workers_named < self.workers {
            state.workers_named += 1;
            return format!("{TOKIO_WORKER}-{}", state.workers_named - 1);
        }
        let index = state.free.pop_first().unwrap_or_else(|| {
            state.next_blocking += 1;
            state.next_blocking - 1
        });
        format!("{TOKIO_BLOCKING}-{index}")
    }

    fn release(&self, name: Option<&str>) {
        let Some(index) = name
            .and_then(|name| name.strip_prefix(TOKIO_BLOCKING))
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|index| index.parse().ok())
        else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if index < state.next_blocking {
            state.free.insert(index);
        }
    }
}

/// The pool the region tick spreads its own work over (entity ticks,
/// random tick planning), at the tick's priority rather than the global
/// pool's background one, so world generation cannot stall a tick.
pub fn tick_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon_pool("tick")
            .num_threads(default_tokio_workers())
            .build()
            .expect("the tick pool starts")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn workers_are_named_first_then_blocking_threads_reuse_freed_indices() {
        let names = TokioNames::new(2);
        let first: Vec<String> = (0..4).map(|_| names.next()).collect();
        assert_eq!(
            first,
            [
                "tokio-worker-0",
                "tokio-worker-1",
                "tokio-blocking-0",
                "tokio-blocking-1"
            ]
        );
        names.release(Some("tokio-blocking-0"));
        names.release(Some("tokio-worker-1"));
        names.release(Some("tokio-blocking-9"));
        names.release(None);
        assert_eq!(names.next(), "tokio-blocking-0");
        assert_eq!(names.next(), "tokio-blocking-2");
    }
}

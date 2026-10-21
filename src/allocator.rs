//! mimalloc hands pages freed from another thread back only when the owning
//! thread allocates or collects again, so threads that go idle after a burst
//! of chunk work keep that memory. Once the loaded chunk count falls to half
//! its peak, and again a while after the world goes idle (no players, no
//! region up or closing) and has dropped its caches, every rayon thread,
//! tokio worker and tokio blocking thread collects its heap. A parked worker
//! or an idle blocking thread runs nothing on its own, so a trim hands each
//! one a task that collects.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tuff_core::ServerHandle;
use tuff_core::metrics::Snapshot;

/// Freed chunk memory reaches the allocator a little after the count drops.
const SETTLE: Duration = Duration::from_secs(5);
/// Smaller drops are not worth a collection.
const MIN_DROP: u64 = 64;
/// How long after the world goes idle to collect, each counted from the
/// previous one: the world drops its caches and closes files meanwhile.
const IDLE_TRIMS: [Duration; 3] = [
    Duration::from_secs(3),
    Duration::from_secs(7),
    Duration::from_secs(20),
];
/// Rounds of worker tasks before a trim gives up on a worker that never
/// picks one up.
const WORKER_ROUNDS: usize = 8;
/// How long a worker task keeps its worker busy waiting for the others, so
/// the next task goes to another worker.
const WORKER_HOLD: Duration = Duration::from_secs(1);
/// How long a blocking task waits for the others to take every idle
/// blocking thread; a thread busy with a long task is skipped.
const BLOCKING_WAIT: Duration = Duration::from_secs(1);

static EPOCH: AtomicU64 = AtomicU64::new(0);
/// Live threads the tokio runtime started, workers included.
static TOKIO_THREADS: AtomicUsize = AtomicUsize::new(0);
static WORKERS_COLLECTED: Tally = Tally::new();
static BLOCKING_COLLECTED: Tally = Tally::new();

extern crate mimalloc;

/// mimalloc's `mi_option_purge_delay`.
const PURGE_DELAY: std::ffi::c_int = 15;

unsafe extern "C" {
    fn mi_collect(force: bool);
    fn mi_option_get(option: std::ffi::c_int) -> std::ffi::c_long;
    fn mi_option_set(option: std::ffi::c_int, value: std::ffi::c_long);
}

/// How long mimalloc waits before giving freed memory back to the OS, in
/// milliseconds.
pub fn purge_delay() -> i64 {
    unsafe { mi_option_get(PURGE_DELAY) as i64 }
}

/// While idle, freed memory goes back to the OS after a millisecond (0
/// would purge on free but stop collections purging what is already
/// scheduled); busy, it waits the usual delay so memory freed and reused
/// soon is not returned and faulted in again.
pub fn set_idle(idle: bool) {
    static BUSY_DELAY: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let busy = *BUSY_DELAY.get_or_init(purge_delay);
    let delay = if idle { 1 } else { busy };
    unsafe { mi_option_set(PURGE_DELAY, delay as std::ffi::c_long) };
}

/// Threads of one kind that collected during the current epoch: the epoch
/// in the high half, the count in the low half, so a collect that read an
/// older epoch does not count towards the new one.
struct Tally(AtomicU64);

impl Tally {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn reset(&self, epoch: u64) {
        self.0.store(epoch << 32, Ordering::Release);
    }

    fn add(&self, epoch: u64) {
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value >> 32 == epoch & 0xffff_ffff).then_some(value + 1)
            });
    }

    fn count(&self) -> usize {
        (self.0.load(Ordering::Acquire) & 0xffff_ffff) as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Other,
    Blocking,
    Worker,
}

/// Counts a tokio thread out when it exits.
struct TokioThread;

impl Drop for TokioThread {
    fn drop(&mut self) {
        TOKIO_THREADS.fetch_sub(1, Ordering::Relaxed);
    }
}

thread_local! {
    static KIND: Cell<Kind> = const { Cell::new(Kind::Other) };
    static TOKIO_THREAD: TokioThread = const { TokioThread };
}

fn collect() {
    thread_local!(static SEEN: Cell<u64> = const { Cell::new(0) });
    let epoch = EPOCH.load(Ordering::Acquire);
    if SEEN.get() != epoch {
        SEEN.set(epoch);
        unsafe { mi_collect(true) };
        match KIND.get() {
            Kind::Worker => WORKERS_COLLECTED.add(epoch),
            Kind::Blocking => BLOCKING_COLLECTED.add(epoch),
            Kind::Other => {}
        }
    }
}

/// For `tokio::runtime::Builder::on_thread_start`: counts the thread as a
/// blocking thread until it first parks as a worker.
pub fn on_thread_start() {
    KIND.set(Kind::Blocking);
    TOKIO_THREADS.fetch_add(1, Ordering::Relaxed);
    TOKIO_THREAD.with(|_| {});
}

/// For `tokio::runtime::Builder::on_thread_park`: collects once per trim.
pub fn on_thread_park() {
    KIND.set(Kind::Worker);
    collect();
}

/// Tokio runtime with [`on_thread_start`] and [`on_thread_park`] installed.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tuff_core::threads::name_tokio(&mut tokio::runtime::Builder::new_multi_thread())
        .enable_all()
        .on_thread_start(on_thread_start)
        .on_thread_park(on_thread_park)
        .build()
        .map(tuff_core::threads::started)
}

/// Collects on every thread of the rayon pool and of the current tokio
/// runtime (built by [`runtime`]), then purges what they freed.
pub async fn trim() {
    let epoch = EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    tuff_core::threads::set_collector(|| unsafe { mi_collect(true) });
    tuff_core::threads::begin_trim();
    WORKERS_COLLECTED.reset(epoch);
    BLOCKING_COLLECTED.reset(epoch);
    let _ = tokio::task::spawn_blocking(|| rayon::broadcast(|_| collect())).await;
    collect();
    let _ = tokio::task::spawn_blocking(tuff_core::threads::collect_pools).await;
    collect_workers().await;
    collect_blocking().await;
    // Each thread purges after collecting, but only one at a time while the
    // others skip, so pages freed meanwhile wait for this last purge.
    unsafe { mi_collect(true) };
}

/// Tokio workers, then blocking threads, that collected during the latest
/// trim.
pub fn collected() -> (usize, usize) {
    (WORKERS_COLLECTED.count(), BLOCKING_COLLECTED.count())
}

/// Hands the workers tasks that collect and hold their worker briefly, so
/// queued ones wake parked workers, until every worker has collected.
async fn collect_workers() {
    let workers = tokio::runtime::Handle::current().metrics().num_workers();
    for _ in 0..WORKER_ROUNDS {
        if WORKERS_COLLECTED.count() >= workers {
            return;
        }
        let tasks: Vec<_> = (0..workers)
            .map(|_| {
                tokio::spawn(async move {
                    collect();
                    let deadline = Instant::now() + WORKER_HOLD;
                    while WORKERS_COLLECTED.count() < workers && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                })
            })
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// One task per blocking thread; each collects and then waits for the
/// others, so no two land on one thread.
async fn collect_blocking() {
    let workers = tokio::runtime::Handle::current().metrics().num_workers();
    let threads = TOKIO_THREADS
        .load(Ordering::Relaxed)
        .saturating_sub(workers);
    let deadline = Instant::now() + BLOCKING_WAIT;
    let tasks: Vec<_> = (0..threads)
        .map(|_| {
            tokio::task::spawn_blocking(move || {
                collect();
                while BLOCKING_COLLECTED.count() < threads && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        })
        .collect();
    for task in tasks {
        let _ = task.await;
    }
}

/// Fires once the loaded chunk count has fallen to half its peak since the
/// last trim (by at least [`MIN_DROP`]).
#[derive(Debug, Default)]
pub struct Trigger {
    peak: u64,
}

impl Trigger {
    pub fn observe(&mut self, chunks: u64) -> bool {
        self.peak = self.peak.max(chunks);
        let fire = chunks * 2 <= self.peak && self.peak - chunks >= MIN_DROP;
        if fire {
            self.peak = chunks;
        }
        fire
    }
}

/// Watches the server's metrics and trims after large chunk unloads, until
/// the server stops.
pub fn spawn_trimmer(handle: ServerHandle) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(metrics) = handle.metrics().await else {
            return;
        };
        let snapshots = metrics.subscribe();
        drop((metrics, handle));
        run(snapshots).await;
    })
}

/// No player is on and no region is up or closing.
pub fn idle(snapshot: &Snapshot) -> bool {
    snapshot.world.players == 0
        && snapshot
            .manager
            .as_ref()
            .is_some_and(|manager| manager.regions == 0 && manager.closing == 0)
}

async fn run(mut snapshots: watch::Receiver<Arc<Snapshot>>) {
    let mut trigger = Trigger::default();
    let mut was_idle = false;
    while snapshots.changed().await.is_ok() {
        let snapshot = snapshots.borrow_and_update().clone();
        let now_idle = idle(&snapshot);
        if now_idle != was_idle {
            set_idle(now_idle);
        }
        if now_idle && !was_idle {
            for wait in IDLE_TRIMS {
                tokio::time::sleep(wait).await;
                if !idle(&snapshots.borrow_and_update()) {
                    set_idle(false);
                    break;
                }
                trim().await;
            }
        } else if trigger.observe(snapshot.world.chunks) {
            tokio::time::sleep(SETTLE).await;
            trim().await;
        }
        was_idle = now_idle;
    }
}

//! The process-wide recorder behind the span macros.
//!
//! Each thread that records gets a [`ThreadBuf`] the first time it opens a
//! span in a recording: a segmented arena of call-tree nodes keyed by
//! (parent, site, tag). Only the owning thread writes it, with plain
//! relaxed loads and stores on atomics, so the stopper can read it at any
//! time without a lock. The thread's own lookup table and span stack live
//! in a thread local. Registering a thread takes the state lock once per
//! recording; nothing on the span path locks, allocates (once a call path
//! has been seen) or sends a message.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use super::clock;
use super::histogram::{self, BUCKETS};
use super::{Site, Tag};

pub(super) static ACTIVE: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static STATE: Mutex<Option<Recording>> = Mutex::new(None);

/// Most nodes one thread keeps; later new call paths are counted as
/// dropped.
pub const MAX_NODES: usize = 16 * 1024;
/// Deepest span nesting recorded; deeper spans are counted as dropped.
pub const MAX_DEPTH: usize = 64;

const SEGMENT: usize = 64;
const SEGMENTS: usize = MAX_NODES / SEGMENT;
const TABLE: usize = MAX_NODES * 2;
const ROOT: u32 = u32::MAX;
const UNTRACKED: u32 = u32::MAX - 1;

struct Recording {
    generation: u64,
    base: Instant,
    started: SystemTime,
    threads: Vec<Arc<ThreadBuf>>,
}

/// Why a recording could not start or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderError {
    AlreadyRecording,
    NotRecording,
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AlreadyRecording => "a timings recording is already running",
            Self::NotRecording => "no timings recording is running",
        })
    }
}

impl std::error::Error for RecorderError {}

fn state() -> std::sync::MutexGuard<'static, Option<Recording>> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts recording every span site in the process.
pub fn start() -> Result<(), RecorderError> {
    let mut state = state();
    if state.is_some() {
        return Err(RecorderError::AlreadyRecording);
    }
    clock::init();
    let generation = GENERATION.load(Ordering::Relaxed) + 1;
    *state = Some(Recording {
        generation,
        base: Instant::now(),
        started: SystemTime::now(),
        threads: Vec::new(),
    });
    GENERATION.store(generation, Ordering::Release);
    ACTIVE.store(true, Ordering::Release);
    Ok(())
}

/// Stops the recording and takes what every thread recorded.
pub fn stop() -> Result<Snapshot, RecorderError> {
    let recording = {
        let mut state = state();
        let recording = state.take().ok_or(RecorderError::NotRecording)?;
        ACTIVE.store(false, Ordering::Release);
        GENERATION.store(recording.generation + 1, Ordering::Release);
        recording
    };
    let duration = recording.base.elapsed();
    let threads = recording.threads.iter().map(|buf| buf.snapshot()).collect();
    Ok(Snapshot {
        started: recording.started,
        duration,
        threads,
    })
}

/// When the running recording started and how many threads joined it.
pub fn running() -> Option<(SystemTime, Duration, usize)> {
    state()
        .as_ref()
        .map(|r| (r.started, r.base.elapsed(), r.threads.len()))
}

pub fn is_recording() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Everything a recording collected, per thread, in arena order.
#[derive(Debug)]
pub struct Snapshot {
    pub started: SystemTime,
    pub duration: Duration,
    pub threads: Vec<ThreadSnapshot>,
}

#[derive(Debug)]
pub struct ThreadSnapshot {
    pub index: usize,
    pub name: String,
    /// Spans and call paths lost to the depth or node limits.
    pub dropped: u64,
    pub nodes: Vec<RawNode>,
}

/// One call-tree node as recorded; `parent` indexes the same thread's
/// `nodes` and always precedes the node.
#[derive(Debug)]
pub struct RawNode {
    pub site: &'static Site,
    pub tag: Tag,
    pub parent: Option<usize>,
    pub stats: Stats,
}

/// Aggregated durations of one node.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub count: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub buckets: Box<[u32]>,
}

impl Stats {
    pub fn empty() -> Self {
        Self {
            count: 0,
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            buckets: vec![0; BUCKETS].into_boxed_slice(),
        }
    }

    pub fn merge(&mut self, other: &Stats) {
        self.count += other.count;
        self.total_ns = self.total_ns.saturating_add(other.total_ns);
        self.min_ns = self.min_ns.min(other.min_ns);
        self.max_ns = self.max_ns.max(other.max_ns);
        for (mine, theirs) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *mine = mine.saturating_add(*theirs);
        }
    }

    pub fn quantile(&self, q: f64) -> u64 {
        histogram::quantile(&self.buckets, q, self.min_ns, self.max_ns)
    }
}

pub(super) struct ThreadBuf {
    index: usize,
    name: String,
    len: AtomicUsize,
    dropped: AtomicU64,
    segments: Box<[OnceLock<Box<[Slot]>>]>,
}

struct Slot {
    key: OnceLock<SlotKey>,
    count: AtomicU64,
    total: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
    buckets: [AtomicU32; BUCKETS],
}

struct SlotKey {
    site: &'static Site,
    tag: Tag,
    parent: u32,
}

impl Slot {
    fn new() -> Self {
        Self {
            key: OnceLock::new(),
            count: AtomicU64::new(0),
            total: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    #[inline]
    fn add(&self, ns: u64) {
        let r = Ordering::Relaxed;
        self.count.store(self.count.load(r) + 1, r);
        self.total.store(self.total.load(r).saturating_add(ns), r);
        if ns < self.min.load(r) {
            self.min.store(ns, r);
        }
        if ns > self.max.load(r) {
            self.max.store(ns, r);
        }
        let bucket = &self.buckets[histogram::bucket(ns)];
        let seen = bucket.load(r);
        if seen != u32::MAX {
            bucket.store(seen + 1, r);
        }
    }

    fn stats(&self) -> Stats {
        let r = Ordering::Relaxed;
        Stats {
            count: self.count.load(r),
            total_ns: self.total.load(r),
            min_ns: self.min.load(r),
            max_ns: self.max.load(r),
            buckets: self.buckets.iter().map(|b| b.load(r)).collect(),
        }
    }
}

impl ThreadBuf {
    fn new(index: usize) -> Self {
        Self {
            index,
            name: std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned(),
            len: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            segments: (0..SEGMENTS).map(|_| OnceLock::new()).collect(),
        }
    }

    #[inline]
    fn slot(&self, node: u32) -> Option<&Slot> {
        let node = node as usize;
        self.segments
            .get(node / SEGMENT)?
            .get()
            .map(|segment| &segment[node % SEGMENT])
    }

    fn push(&self, key: SlotKey) -> u32 {
        let index = self.len.load(Ordering::Relaxed);
        let segment = self.segments[index / SEGMENT]
            .get_or_init(|| (0..SEGMENT).map(|_| Slot::new()).collect());
        let _ = segment[index % SEGMENT].key.set(key);
        self.len.store(index + 1, Ordering::Release);
        index as u32
    }

    fn drop_one(&self) {
        let r = Ordering::Relaxed;
        self.dropped.store(self.dropped.load(r) + 1, r);
    }

    fn snapshot(&self) -> ThreadSnapshot {
        let len = self.len.load(Ordering::Acquire);
        let nodes = (0..len as u32)
            .filter_map(|index| {
                let slot = self.slot(index)?;
                let key = slot.key.get()?;
                Some(RawNode {
                    site: key.site,
                    tag: key.tag,
                    parent: (key.parent != ROOT).then_some(key.parent as usize),
                    stats: slot.stats(),
                })
            })
            .collect();
        ThreadSnapshot {
            index: self.index,
            name: self.name.clone(),
            dropped: self.dropped.load(Ordering::Relaxed),
            nodes,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    parent: u32,
    site: usize,
    tag: TagKey,
}

/// A tag compared by identity, so lookups never compare string contents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TagKey {
    None,
    Name(usize, usize),
    Region(u16, i32, i32),
}

impl Key {
    fn new(parent: u32, site: &'static Site, tag: Tag) -> Self {
        Self {
            parent,
            site: site as *const Site as usize,
            tag: match tag {
                Tag::None => TagKey::None,
                Tag::Name(name) => TagKey::Name(name.as_ptr() as usize, name.len()),
                Tag::Region { dimension, x, z } => TagKey::Region(dimension, x, z),
            },
        }
    }

    #[inline]
    fn hash(&self) -> u64 {
        let tag = match self.tag {
            TagKey::None => 0,
            TagKey::Name(ptr, len) => (ptr as u64) ^ ((len as u64) << 48),
            TagKey::Region(d, x, z) => {
                (u64::from(d) << 48) ^ ((x as u32 as u64) << 24) ^ (z as u32 as u64)
            }
        };
        let mut h = (self.site as u64) ^ (u64::from(self.parent) << 32) ^ tag;
        h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        h ^ (h >> 31)
    }
}

struct Local {
    generation: u64,
    buf: Arc<ThreadBuf>,
    keys: Vec<Key>,
    table: Box<[u32]>,
    stack: [(u32, u64); MAX_DEPTH],
    depth: usize,
}

impl Local {
    /// Where a new span on this thread goes: under its innermost open
    /// span, else under the running task's open timers.
    #[inline]
    fn parent(&mut self) -> u32 {
        if self.depth == 0 {
            task_parent(self)
        } else {
            self.stack[self.depth - 1].0
        }
    }

    fn path(&mut self, frames: &[Option<Frame>]) -> u32 {
        frames.iter().flatten().fold(ROOT, |parent, frame| {
            self.node(parent, frame.site, frame.tag)
        })
    }

    fn node(&mut self, parent: u32, site: &'static Site, tag: Tag) -> u32 {
        if parent == UNTRACKED {
            return UNTRACKED;
        }
        let key = Key::new(parent, site, tag);
        let mask = self.table.len() - 1;
        let mut i = key.hash() as usize & mask;
        loop {
            match self.table[i] {
                0 => return self.insert(i, key, site, tag),
                entry if self.keys[(entry - 1) as usize] == key => return entry - 1,
                _ => i = (i + 1) & mask,
            }
        }
    }

    #[cold]
    fn insert(&mut self, slot: usize, key: Key, site: &'static Site, tag: Tag) -> u32 {
        if self.keys.len() >= MAX_NODES {
            self.buf.drop_one();
            return UNTRACKED;
        }
        let index = self.buf.push(SlotKey {
            site,
            tag,
            parent: key.parent,
        });
        self.keys.push(key);
        self.table[slot] = index + 1;
        index
    }

    #[inline]
    fn record(&self, node: u32, ns: u64) {
        if let Some(slot) = self.buf.slot(node) {
            slot.add(ns);
        }
    }
}

thread_local! {
    static LOCAL: RefCell<Option<Box<Local>>> = const { RefCell::new(None) };
}

/// This thread's recorder for the running recording, registering the
/// thread on its first span of the recording.
fn current(slot: &mut Option<Box<Local>>) -> Option<&mut Local> {
    let generation = GENERATION.load(Ordering::Acquire);
    if slot
        .as_ref()
        .is_none_or(|local| local.generation != generation)
    {
        *slot = register(generation, slot.take());
    }
    slot.as_deref_mut()
}

#[cold]
fn register(generation: u64, old: Option<Box<Local>>) -> Option<Box<Local>> {
    let mut state = state();
    let recording = state.as_mut().filter(|r| r.generation == generation)?;
    let buf = Arc::new(ThreadBuf::new(recording.threads.len()));
    recording.threads.push(buf.clone());
    drop(state);
    let local = match old {
        Some(mut local) => {
            local.generation = generation;
            local.buf = buf;
            local.keys.clear();
            local.table.fill(0);
            local.depth = 0;
            local
        }
        None => Box::new(Local {
            generation,
            buf,
            keys: Vec::with_capacity(1024),
            table: vec![0; TABLE].into_boxed_slice(),
            stack: [(ROOT, 0); MAX_DEPTH],
            depth: 0,
        }),
    };
    Some(local)
}

/// An open span; it closes when dropped. Not `Send`: a span must close on
/// the thread that opened it, so it cannot be held across an `.await` in a
/// spawned task (use [`super::Timer`] there).
#[must_use = "a span closes when dropped; bind it to a named variable"]
pub struct Span {
    entered: Option<Entered>,
    _thread_bound: PhantomData<*const ()>,
}

#[derive(Clone, Copy)]
struct Entered {
    generation: u64,
    depth: usize,
}

impl Span {
    #[inline(always)]
    pub(super) fn inactive() -> Self {
        Self {
            entered: None,
            _thread_bound: PhantomData,
        }
    }

    #[inline(always)]
    pub(super) fn open(site: &'static Site, tag: Tag) -> Self {
        Self {
            entered: enter(site, tag),
            _thread_bound: PhantomData,
        }
    }

    /// Whether this span is being recorded.
    pub fn is_recording(&self) -> bool {
        self.entered.is_some()
    }
}

impl Drop for Span {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(entered) = self.entered {
            exit(entered);
        }
    }
}

#[inline(never)]
fn enter(site: &'static Site, tag: Tag) -> Option<Entered> {
    LOCAL
        .try_with(|cell| {
            let mut slot = cell.try_borrow_mut().ok()?;
            let local = current(&mut slot)?;
            if local.depth >= MAX_DEPTH {
                local.buf.drop_one();
                return None;
            }
            let parent = local.parent();
            let node = local.node(parent, site, tag);
            let now = clock::now();
            local.stack[local.depth] = (node, now);
            local.depth += 1;
            Some(Entered {
                generation: local.generation,
                depth: local.depth,
            })
        })
        .ok()
        .flatten()
}

#[inline(never)]
fn exit(entered: Entered) {
    let _ = LOCAL.try_with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let Some(local) = slot.as_deref_mut() else {
            return;
        };
        let now = clock::now();
        if local.generation != entered.generation || local.depth < entered.depth {
            return;
        }
        local.depth = entered.depth - 1;
        let (node, start) = local.stack[local.depth];
        if node != UNTRACKED {
            local.record(node, clock::ns(now.saturating_sub(start)));
        }
    });
}

/// Adds an already measured duration under this thread's open span.
#[inline(never)]
pub(super) fn record(site: &'static Site, tag: Tag, ns: u64) {
    let _ = LOCAL.try_with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let Some(local) = current(&mut slot) else {
            return;
        };
        let parent = local.parent();
        let node = local.node(parent, site, tag);
        if node != UNTRACKED {
            local.record(node, ns);
        }
    });
}

/// Most timers one task keeps open for nesting; deeper ones record as if
/// outside a task scope.
pub const MAX_TASK_DEPTH: usize = 16;

#[derive(Clone, Copy)]
struct Frame {
    site: &'static Site,
    tag: Tag,
    serial: u64,
}

/// A scoped task's open timers, innermost last.
pub(super) struct TaskFrames {
    frames: [Option<Frame>; MAX_TASK_DEPTH],
    len: usize,
    /// Bumped on every push and pop: identifies frames and versions
    /// `resolved`.
    serial: u64,
    /// The node `frames` last resolved to: (serial, generation, thread
    /// buffer, node).
    resolved: Option<(u64, u64, usize, u32)>,
}

/// A timer's frame on its task's stack.
#[derive(Clone, Copy, Debug)]
pub(super) struct Pushed {
    depth: usize,
    serial: u64,
}

impl TaskFrames {
    fn holds(&self, pushed: Pushed) -> bool {
        pushed.depth <= self.len
            && self.frames[pushed.depth - 1].is_some_and(|f| f.serial == pushed.serial)
    }
}

tokio::task_local! {
    static TASK: Box<RefCell<TaskFrames>>;
}

/// Runs `future` with its own timer stack.
pub(super) fn scope<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    TASK.scope(
        Box::new(RefCell::new(TaskFrames {
            frames: [None; MAX_TASK_DEPTH],
            len: 0,
            serial: 0,
            resolved: None,
        })),
        future,
    )
}

#[inline(never)]
fn task_parent(local: &mut Local) -> u32 {
    TASK.try_with(|task| {
        let Ok(mut frames) = task.try_borrow_mut() else {
            return ROOT;
        };
        if frames.len == 0 {
            return ROOT;
        }
        let key = (
            frames.serial,
            local.generation,
            Arc::as_ptr(&local.buf) as usize,
        );
        if let Some((serial, generation, buf, node)) = frames.resolved
            && (serial, generation, buf) == key
        {
            return node;
        }
        let node = local.path(&frames.frames[..frames.len]);
        frames.resolved = Some((key.0, key.1, key.2, node));
        node
    })
    .unwrap_or(ROOT)
}

/// Opens a timer's frame on the running task's stack, if it is scoped.
#[inline(never)]
pub(super) fn push_frame(site: &'static Site, tag: Tag) -> Option<Pushed> {
    TASK.try_with(|task| {
        let mut frames = task.try_borrow_mut().ok()?;
        if frames.len >= MAX_TASK_DEPTH {
            return None;
        }
        frames.serial += 1;
        let (len, serial) = (frames.len, frames.serial);
        frames.frames[len] = Some(Frame { site, tag, serial });
        frames.len += 1;
        Some(Pushed {
            depth: len + 1,
            serial,
        })
    })
    .ok()
    .flatten()
}

/// Records a finished timer (`ns`, when it still belongs to the running
/// recording) under its task's open timers, or like [`record`] when its
/// frame is gone (it finished in another task, or after its parent), and
/// closes its frame.
#[inline(never)]
pub(super) fn finish_timer(site: &'static Site, tag: Tag, ns: Option<u64>, pushed: Option<Pushed>) {
    if let Some(ns) = ns {
        let _ = LOCAL.try_with(|cell| {
            let Ok(mut slot) = cell.try_borrow_mut() else {
                return;
            };
            let Some(local) = current(&mut slot) else {
                return;
            };
            let nested = pushed.and_then(|pushed| {
                TASK.try_with(|task| {
                    let frames = task.try_borrow().ok()?;
                    frames
                        .holds(pushed)
                        .then(|| local.path(&frames.frames[..pushed.depth]))
                })
                .ok()
                .flatten()
            });
            let node = match nested {
                Some(node) => node,
                None => {
                    let parent = local.parent();
                    local.node(parent, site, tag)
                }
            };
            if node != UNTRACKED {
                local.record(node, ns);
            }
        });
    }
    if let Some(pushed) = pushed {
        let _ = TASK.try_with(|task| {
            if let Ok(mut frames) = task.try_borrow_mut()
                && frames.holds(pushed)
            {
                frames.len = pushed.depth - 1;
                frames.serial += 1;
            }
        });
    }
}

pub(super) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

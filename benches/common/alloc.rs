//! The bench process's global allocator, wrapped to count live heap bytes
//! so a report can set what the server holds against what stays resident.
//! Counts are spread over cache-line shards picked by stack address, so
//! threads rarely contend on one counter.
//!
//! With [`sample_every`] set it also samples allocations (about one per
//! that many bytes allocated) with their frame-pointer call stacks, so a
//! report can say where the live heap is held.

#![expect(dead_code, reason = "helpers used by only some targets")]

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicU64, AtomicUsize, Ordering};

const INNER: mimalloc::MiMalloc = mimalloc::MiMalloc;

const SHARDS: usize = 64;

#[repr(align(128))]
struct Shard(AtomicIsize);

static LIVE: [Shard; SHARDS] = [const { Shard(AtomicIsize::new(0)) }; SHARDS];

pub struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn shard() -> &'static AtomicIsize {
    let probe = 0u8;
    let at = std::ptr::addr_of!(probe) as usize as u64;
    let index = ((at >> 12).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize;
    &LIVE[index % SHARDS].0
}

fn add(bytes: usize) {
    shard().fetch_add(bytes as isize, Ordering::Relaxed);
}

fn sub(bytes: usize) {
    shard().fetch_sub(bytes as isize, Ordering::Relaxed);
}

/// Bytes allocated and not yet freed, as requested (not as rounded up by
/// the allocator).
pub fn live_bytes() -> u64 {
    let sum: isize = LIVE.iter().map(|s| s.0.load(Ordering::Relaxed)).sum();
    sum.max(0) as u64
}

/// Frames kept per sampled stack.
pub const DEPTH: usize = 32;
pub type Stack = [usize; DEPTH];

const SLOT_BITS: u32 = 20;
const SLOTS_LEN: usize = 1 << SLOT_BITS;
const SIZE_BITS: u32 = 40;

static SAMPLE_EVERY: AtomicUsize = AtomicUsize::new(0);
static SLOTS: [AtomicUsize; SLOTS_LEN] = [const { AtomicUsize::new(0) }; SLOTS_LEN];
static META: [AtomicU64; SLOTS_LEN] = [const { AtomicU64::new(0) }; SLOTS_LEN];

type Ids = HashMap<Stack, u32, BuildHasherDefault<DefaultHasher>>;
static STACKS: Mutex<(Ids, Vec<Stack>)> =
    Mutex::new((HashMap::with_hasher(BuildHasherDefault::new()), Vec::new()));

thread_local! {
    static COUNTDOWN: Cell<isize> = const { Cell::new(0) };
    static RNG: Cell<u64> = const { Cell::new(0) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// Starts sampling about one allocation per `bytes` allocated (0 stops).
pub fn sample_every(bytes: usize) {
    SAMPLE_EVERY.store(bytes, Ordering::Relaxed);
}

pub fn sampling() -> usize {
    SAMPLE_EVERY.load(Ordering::Relaxed)
}

fn slot(ptr: usize) -> usize {
    ((ptr as u64 >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SLOT_BITS)) as usize
}

fn next_interval(every: usize) -> isize {
    RNG.with(|rng| {
        let mut x = rng.get();
        if x == 0 {
            x = (std::ptr::addr_of!(*rng) as u64) | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        rng.set(x);
        (x % (2 * every as u64)) as isize
    })
}

#[inline(always)]
fn frame_pointer() -> usize {
    let fp: usize;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        fp = 0;
    }
    fp
}

/// Walks the frame-pointer chain, which Apple targets always keep; it
/// stops at the first link that does not move up the same stack.
#[inline(never)]
fn capture() -> Stack {
    let mut stack = [0; DEPTH];
    let mut fp = frame_pointer();
    let top = fp;
    for frame in stack.iter_mut() {
        if fp == 0 || !fp.is_multiple_of(8) {
            break;
        }
        let (next, ret) = unsafe { (*(fp as *const usize), *((fp + 8) as *const usize)) };
        if ret == 0 {
            break;
        }
        *frame = ret;
        if next <= fp || next - top > 16 << 20 {
            break;
        }
        fp = next;
    }
    stack
}

fn intern(stack: Stack) -> Option<u32> {
    let mut guard = STACKS.lock().ok()?;
    let (ids, stacks) = &mut *guard;
    if let Some(&id) = ids.get(&stack) {
        return Some(id);
    }
    let id = u32::try_from(stacks.len()).ok()?;
    if id >= 1 << (64 - SIZE_BITS) {
        return None;
    }
    stacks.push(stack);
    ids.insert(stack, id);
    Some(id)
}

fn maybe_sample(ptr: *mut u8, size: usize) {
    let every = SAMPLE_EVERY.load(Ordering::Relaxed);
    if every == 0 {
        return;
    }
    let left = COUNTDOWN.get() - size as isize;
    if left > 0 {
        COUNTDOWN.set(left);
        return;
    }
    if BUSY.replace(true) {
        return;
    }
    COUNTDOWN.set(next_interval(every));
    let at = slot(ptr as usize);
    if SLOTS[at]
        .compare_exchange(0, ptr as usize, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let size = (size as u64).min((1 << SIZE_BITS) - 1);
        match intern(capture()) {
            Some(id) => META[at].store(u64::from(id) << SIZE_BITS | size, Ordering::Release),
            None => SLOTS[at].store(0, Ordering::Relaxed),
        }
    }
    BUSY.set(false);
}

fn forget(ptr: *mut u8) {
    if SAMPLE_EVERY.load(Ordering::Relaxed) == 0 {
        return;
    }
    let at = slot(ptr as usize);
    if SLOTS[at].load(Ordering::Relaxed) == ptr as usize {
        let _ = SLOTS[at].compare_exchange(ptr as usize, 0, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// One sampled call stack (return addresses, innermost first) and the
/// live bytes it stands for.
#[derive(Debug, Clone)]
pub struct Site {
    pub stack: Stack,
    pub bytes: f64,
    pub samples: u64,
    /// The largest sampled allocation's size.
    pub largest: u64,
}

/// The live sampled allocations grouped by call stack, each weighted by
/// the inverse of its chance of being sampled.
pub fn live_sites() -> Vec<Site> {
    let every = SAMPLE_EVERY.load(Ordering::Relaxed);
    if every == 0 {
        return Vec::new();
    }
    let mut by_id: HashMap<u32, (f64, u64, u64)> = HashMap::new();
    for (at, slot) in SLOTS.iter().enumerate() {
        if slot.load(Ordering::Relaxed) == 0 {
            continue;
        }
        let meta = META[at].load(Ordering::Acquire);
        let bytes = meta & ((1 << SIZE_BITS) - 1);
        let size = bytes as f64;
        let chance = 1.0 - (-size / every as f64).exp();
        let entry = by_id.entry((meta >> SIZE_BITS) as u32).or_default();
        entry.0 += size / chance.max(f64::MIN_POSITIVE);
        entry.1 += 1;
        entry.2 = entry.2.max(bytes);
    }
    let was_busy = BUSY.replace(true);
    let stacks = STACKS
        .lock()
        .map(|guard| guard.1.clone())
        .unwrap_or_default();
    BUSY.set(was_busy);
    by_id
        .into_iter()
        .filter_map(|(id, (bytes, samples, largest))| {
            Some(Site {
                stack: *stacks.get(id as usize)?,
                bytes,
                samples,
                largest,
            })
        })
        .collect()
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { INNER.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
            maybe_sample(ptr, layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { INNER.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
            maybe_sample(ptr, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        forget(ptr);
        unsafe { INNER.dealloc(ptr, layout) };
        sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        forget(ptr);
        let moved = unsafe { INNER.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            add(new_size);
            sub(layout.size());
            maybe_sample(moved, new_size);
        }
        moved
    }
}

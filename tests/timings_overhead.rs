//! What a timings span costs, off and on, and that recording allocates
//! nothing once a call path has been seen. Its own binary so the counting
//! allocator sees only this test's threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::{Duration, Instant};

use tuff_timings::{self as timings, Profile, Tag, span, timed, timer};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

const KINDS: [&str; 4] = [
    "minecraft:pig",
    "minecraft:cow",
    "minecraft:zombie",
    "minecraft:item",
];

fn workload(i: u64) {
    let _tick = span!("overhead.tick");
    for kind in KINDS {
        let _entity = span!("overhead.entity", Tag::Name(kind));
        timed!("overhead.leaf", black_box(i));
    }
    let _timer = timer!("overhead.timer");
    timings::record!("overhead.measured", Duration::from_nanos(i % 1000));
}

#[tuff_core::watchdog]
#[test]
fn recording_allocates_nothing_once_paths_are_known() {
    let _serial = timings::serial();
    timings::start().unwrap();
    workload(0);
    let before = allocations();
    for i in 0..10_000 {
        workload(i);
    }
    let allocated = allocations() - before;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let scoped = runtime.block_on(timings::scope(async {
        let _task = timer!("overhead.task");
        workload(0);
        let before = allocations();
        for i in 0..10_000 {
            workload(i);
        }
        allocations() - before
    }));
    let profile = Profile::from_snapshot(timings::stop().unwrap());
    assert_eq!(allocated, 0, "allocations while recording");
    assert_eq!(scoped, 0, "allocations while recording in a scoped task");
    assert_eq!(
        profile
            .find(&["overhead.task", "overhead.tick", "overhead.entity"])
            .unwrap()
            .stats
            .count,
        10_001
    );
    let entity = profile
        .nodes()
        .into_iter()
        .find(|n| n.name == "overhead.entity" && n.tag == Tag::Name("minecraft:zombie"))
        .unwrap();
    assert_eq!(entity.stats.count, 10_001);
}

fn per_iteration(iterations: u64, body: impl Fn(u64)) -> f64 {
    let started = Instant::now();
    for i in 0..iterations {
        body(black_box(i));
    }
    started.elapsed().as_nanos() as f64 / iterations as f64
}

fn best_of(runs: usize, iterations: u64, body: impl Fn(u64)) -> f64 {
    (0..runs)
        .map(|_| per_iteration(iterations, &body))
        .fold(f64::INFINITY, f64::min)
}

fn interleaved_best(runs: usize, iterations: u64) -> (f64, f64) {
    let mut bare = f64::INFINITY;
    let mut off = f64::INFINITY;
    for _ in 0..runs {
        bare = bare.min(per_iteration(iterations, |i| {
            black_box(i);
        }));
        off = off.min(per_iteration(iterations, |i| {
            let _span = span!("overhead.off");
            black_box(i);
        }));
    }
    (bare, off)
}

#[tuff_core::watchdog]
#[test]
fn a_span_costs_one_load_off_and_tens_of_nanoseconds_on() {
    let _serial = timings::serial();
    let iterations = 2_000_000;
    let (bare, off) = interleaved_best(200, iterations / 40);
    let off_tagged = best_of(5, iterations, |i| {
        let _span = span!("overhead.off", Tag::Name(KINDS[(i & 3) as usize]));
        black_box(i);
    });
    let clock = best_of(5, iterations, |_| {
        black_box(Instant::now());
    });
    timings::start().unwrap();
    let on = best_of(5, iterations, |i| {
        let _span = span!("overhead.on");
        black_box(i);
    });
    let on_nested = best_of(5, iterations / 4, |i| {
        let _outer = span!("overhead.outer");
        let _inner = span!("overhead.inner", Tag::Name(KINDS[(i & 3) as usize]));
        black_box(i);
    }) / 2.0;
    let on_timer = best_of(5, iterations, |i| {
        let _timer = timer!("overhead.timer");
        black_box(i);
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (scoped_timer, scoped_span) = runtime.block_on(timings::scope(async {
        let _task = timer!("overhead.task");
        let timer = best_of(5, iterations, |i| {
            let _timer = timer!("overhead.scoped_timer");
            black_box(i);
        });
        let span = best_of(5, iterations, |i| {
            let _span = span!("overhead.scoped_span");
            black_box(i);
        });
        (timer, span)
    }));
    let profile = Profile::from_snapshot(timings::stop().unwrap());
    let empty = best_of(5, iterations, |i| {
        let _span = span!("overhead.on");
        black_box(i);
    });
    eprintln!(
        "timings cost per span: off {:.2} ns, off tagged {:.2} ns, off after a recording \
         {:.2} ns (bare loop {bare:.2} ns); on {:.1} ns, nested tagged {:.1} ns, timer {:.1} ns, \
         in a scoped task: timer {:.1} ns, span {:.1} ns (one Instant read {:.1} ns)",
        off - bare,
        off_tagged - bare,
        empty - bare,
        on - bare,
        on_nested - bare / 2.0,
        on_timer - bare,
        scoped_timer - bare,
        scoped_span - bare,
        clock - bare,
    );
    assert_eq!(
        profile.find(&["overhead.on"]).unwrap().stats.count,
        5 * iterations
    );
    assert!(off - bare < 8.0, "a disabled span costs {} ns", off - bare);
    assert!(
        on - bare < 1_000.0,
        "an enabled span costs {} ns",
        on - bare
    );
}

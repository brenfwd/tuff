//! Built-in timings: hierarchical span timings recorded on a live server
//! and reported as a call tree (`/tuff timings`, the admin panel).
//!
//! Instrumenting code is one line:
//!
//! ```ignore
//! let _span = tuff_timings::span!("myfeature.pathfinding");
//! let _span = tuff_timings::span!("entity", Tag::Name(kind.name()));
//! let value = tuff_timings::timed!("myfeature.lookup", expensive());
//! ```
//!
//! A span records from the macro to the end of the enclosing scope and
//! nests under whatever span the same thread has open, so a feature's spans
//! appear under the core span that called into it (region hooks run under
//! `feature [<feature id>]`). While no recording runs a span site costs one
//! relaxed atomic load. While recording a span reads the [`clock`] (the
//! CPU's counter where it can be read directly) twice and updates a
//! thread-local call-tree node keyed by its static site and tag: no lock,
//! no channel message, and no allocation once that call path has been
//! seen. Threads are merged when the recording stops, keeping each
//! thread's own tree.
//!
//! [`span!`] guards are not `Send`. Code that awaits uses [`timer!`],
//! which measures wall time across awaits, marked asynchronous in
//! reports. In a [`scope`]d task timers nest under the task's open
//! timers; elsewhere a timer adds its time under whatever span is open
//! where it finishes (normally the thread's root). [`record!`] adds a duration measured
//! elsewhere (the server tick time).
//!
//! The recorder is process-wide: one recording at a time, covering every
//! server in the process. [`Timings`] is a server's controller (start,
//! stop, status, list and fetch of saved reports); see [`report`] for the
//! JSON schema.

pub mod clock;
mod histogram;
mod profile;
mod recorder;
pub mod report;
mod service;

pub use histogram::BUCKETS;
pub use profile::{Labels, Node, Profile, ThreadProfile, short_type_name};
pub use recorder::{
    MAX_DEPTH, MAX_NODES, RawNode, RecorderError, Snapshot, Span, Stats, ThreadSnapshot,
    is_recording, running,
};
pub use service::{
    Export, Info, Recording, ReportFiles, Runner, StartOptions, Status, Summary, Timings,
    TimingsError, file_stamp, report_file,
};

/// Starts the process-wide recording directly; servers go through
/// [`Timings::start`] instead.
pub fn start() -> Result<(), RecorderError> {
    recorder::start()
}

/// Stops the process-wide recording and returns what it collected.
pub fn stop() -> Result<Snapshot, RecorderError> {
    recorder::stop()
}

/// A span site: one per macro call, identified by its address.
#[derive(Debug)]
pub struct Site {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
    /// Measured with [`Timer`] (wall time across awaits).
    pub asynchronous: bool,
}

impl Site {
    pub const fn new(
        name: &'static str,
        file: &'static str,
        line: u32,
        asynchronous: bool,
    ) -> Self {
        Self {
            name,
            file,
            line,
            asynchronous,
        }
    }
}

/// What distinguishes calls of one site: the entity type ticked, the
/// packet encoded, the feature a hook belongs to, the region ticking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    None,
    Name(&'static str),
    Region { dimension: u16, x: i32, z: i32 },
}

/// The tag every feature hook span carries: the feature id.
pub const FEATURE: &str = "feature";
/// The site name of the per-tick server time, used for the TPS and mspt
/// summary.
pub const SERVER_TICK: &str = "server.tick";

#[doc(hidden)]
#[inline(always)]
pub fn enter(site: &'static Site, tag: impl FnOnce() -> Tag) -> Span {
    if recorder::ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        Span::open(site, tag())
    } else {
        Span::inactive()
    }
}

#[doc(hidden)]
#[inline(always)]
pub fn record_at(site: &'static Site, tag: impl FnOnce() -> Tag, elapsed: std::time::Duration) {
    if recorder::ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        recorder::record(site, tag(), elapsed.as_nanos() as u64);
    }
}

#[doc(hidden)]
#[inline(always)]
pub fn timer_at(site: &'static Site, tag: impl FnOnce() -> Tag) -> Timer {
    if recorder::ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let tag = tag();
        let frame = recorder::push_frame(site, tag);
        Timer {
            started: Some(Started {
                ticks: clock::now(),
                generation: recorder::generation(),
                site,
                tag,
                frame,
            }),
        }
    } else {
        Timer { started: None }
    }
}

/// A `Send` measurement for code that awaits; it records when dropped.
#[must_use = "a timer records when dropped; bind it to a named variable"]
pub struct Timer {
    started: Option<Started>,
}

struct Started {
    ticks: u64,
    generation: u64,
    site: &'static Site,
    tag: Tag,
    frame: Option<recorder::Pushed>,
}

impl Drop for Timer {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(started) = self.started.take() {
            let ns = (recorder::ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
                && recorder::generation() == started.generation)
                .then(|| clock::ns(clock::now().saturating_sub(started.ticks)));
            recorder::finish_timer(started.site, started.tag, ns, started.frame);
        }
    }
}

/// Runs `future` with its own timer stack: [`timer!`]s in it nest under
/// the ones it has open, and spans it opens outside any other span nest
/// under them too, whichever thread the task runs on. Player sessions
/// run scoped. A timer finishing in another task records at that
/// task's root, and its frame stays open here until an enclosing timer
/// of this task finishes.
pub fn scope<F: std::future::Future>(future: F) -> impl std::future::Future<Output = F::Output> {
    recorder::scope(future)
}

#[doc(hidden)]
#[macro_export]
macro_rules! __timings_span {
    ($name:expr) => {
        $crate::span!($name, $crate::Tag::None)
    };
    ($name:expr, $tag:expr) => {{
        static SITE: $crate::Site = $crate::Site::new($name, file!(), line!(), false);
        $crate::enter(&SITE, || $tag)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __timings_timed {
    ($name:expr, $body:expr) => {{
        let _span = $crate::span!($name);
        $body
    }};
    ($name:expr, $tag:expr, $body:expr) => {{
        let _span = $crate::span!($name, $tag);
        $body
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __timings_timer {
    ($name:expr) => {
        $crate::timer!($name, $crate::Tag::None)
    };
    ($name:expr, $tag:expr) => {{
        static SITE: $crate::Site = $crate::Site::new($name, file!(), line!(), true);
        $crate::timer_at(&SITE, || $tag)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __timings_record {
    ($name:expr, $elapsed:expr) => {
        $crate::record!($name, $crate::Tag::None, $elapsed)
    };
    ($name:expr, $tag:expr, $elapsed:expr) => {{
        static SITE: $crate::Site = $crate::Site::new($name, file!(), line!(), false);
        $crate::record_at(&SITE, || $tag, $elapsed)
    }};
}

/// Adds a measured `Duration`: `record!(name, elapsed)` or
/// `record!(name, tag, elapsed)`.
pub use crate::__timings_record as record;
/// Opens a span until the end of the scope: `span!(name)` or
/// `span!(name, tag)`. The tag expression is only evaluated while
/// recording.
pub use crate::__timings_span as span;
/// Runs an expression inside a span: `timed!(name, expr)` or
/// `timed!(name, tag, expr)`.
pub use crate::__timings_timed as timed;
/// A `Send` timer for async code: `timer!(name)` or `timer!(name, tag)`.
pub use crate::__timings_timer as timer;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serializes tests that start the process-wide recorder.
#[doc(hidden)]
pub fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.blocking_lock()
}

/// Async form of [`serial`] for tests running inside a runtime.
#[doc(hidden)]
pub async fn serial_async() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

#[cfg(test)]
mod tests;

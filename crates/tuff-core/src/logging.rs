//! The process-wide log. A `log` facade backend turns every record (ours and
//! our dependencies') into a [`LogLine`] and hands it to the [`LogHub`] actor
//! without waiting. The hub writes the terminal and `logs/latest.log`, keeps
//! the last [`RING_LINES`] lines and broadcasts new ones to subscribers.
//!
//! Subscribing (for a console or any other viewer):
//!
//! ```ignore
//! let hub = tuff_core::logging::installed().expect("no log hub");
//! let mut sub = hub.subscribe().await.expect("hub stopped");
//! for line in &sub.backfill { show(line) }
//! loop {
//!     match sub.live.recv().await {
//!         Ok(line) => show(&line),
//!         Err(RecvError::Lagged(skipped)) => note_gap(skipped),
//!         Err(RecvError::Closed) => break,
//!     }
//! }
//! ```
//!
//! `backfill` and `live` are taken atomically: the first live line follows
//! the last backfilled one (`seq` is consecutive across both). A subscriber
//! more than [`LIVE_LINES`] behind gets `Lagged`; game actors never wait.
//! With no hub installed (tests, tools), records fall back to stderr.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, oneshot};

use tuff_storage::admin::civil_from_days;

/// Lines kept for a new subscriber's backfill.
pub const RING_LINES: usize = 2000;
/// Lines queued for the hub before new ones are dropped (and counted).
pub const QUEUE_LINES: usize = 4096;
/// Lines a live subscriber may fall behind before it sees `Lagged`.
pub const LIVE_LINES: usize = 1024;
/// The file the hub appends; archived as `<yyyy-MM-dd>-<n>.log.gz`.
pub const LATEST: &str = "latest.log";
/// Requests to the hub are rare and awaited, so a small queue suffices.
const CONTROL_QUEUE: usize = 16;
const DAY_MILLIS: i64 = 86_400_000;
/// Dependencies whose info-level chatter stays out of the log.
const QUIET_TARGETS: [&str; 9] = [
    "h2",
    "hyper",
    "hyper_util",
    "jni",
    "mio",
    "reqwest",
    "rustls",
    "rustls_platform_verifier",
    "want",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Position in the hub's output, from 0, assigned by the hub.
    pub seq: u64,
    pub unix_millis: i64,
    pub level: Level,
    /// The emitting module path (`tuff_core::world::coordinator`), or an
    /// explicit `target:` given to the log macro.
    pub target: String,
    pub text: String,
}

impl LogLine {
    pub fn new(level: Level, target: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            seq: 0,
            unix_millis: now_millis(),
            level,
            target: target.into(),
            text: text.into(),
        }
    }

    /// `[HH:MM:SS] [target/LEVEL]: text` in UTC, as `latest.log` stores it.
    pub fn file_line(&self) -> String {
        let secs = self.unix_millis.div_euclid(1000).rem_euclid(86_400);
        format!(
            "[{:02}:{:02}:{:02}] [{}/{}]: {}",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60,
            self.target,
            self.level,
            self.text
        )
    }

    fn day(&self) -> i64 {
        self.unix_millis.div_euclid(DAY_MILLIS)
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HubStats {
    /// Lines published, drop notices included.
    pub written: u64,
    /// Lines lost to a full queue.
    pub dropped: u64,
}

pub struct Subscription {
    /// Up to [`RING_LINES`] most recent lines, oldest first.
    pub backfill: Vec<Arc<LogLine>>,
    /// Every line published after the backfill.
    pub live: broadcast::Receiver<Arc<LogLine>>,
}

#[derive(Debug, Clone)]
pub struct HubOptions {
    /// Directory for `latest.log` and its archives; `None` keeps no file.
    pub dir: Option<PathBuf>,
    /// Echo lines to stdout (info and below) or stderr (warn, error).
    pub terminal: bool,
    pub ring: usize,
    pub queue: usize,
    pub live: usize,
}

impl HubOptions {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
            ..Self::default()
        }
    }
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            dir: None,
            terminal: true,
            ring: RING_LINES,
            queue: QUEUE_LINES,
            live: LIVE_LINES,
        }
    }
}

enum Control {
    Subscribe(oneshot::Sender<Subscription>),
    Flush(oneshot::Sender<HubStats>),
    Shutdown(oneshot::Sender<HubStats>),
}

/// A handle to the hub actor. Cheap to clone.
#[derive(Clone)]
pub struct LogHub {
    lines: mpsc::Sender<LogLine>,
    control: mpsc::Sender<Control>,
    dropped: Arc<AtomicU64>,
}

impl LogHub {
    /// Starts the hub on its own thread, so terminal and file writes never
    /// occupy a runtime worker. Works inside or outside a tokio runtime.
    pub fn spawn(options: HubOptions) -> LogHub {
        let (hub, actor) = Self::new(options);
        std::thread::Builder::new()
            .name("log-hub".into())
            .spawn(move || actor.run_blocking())
            .expect("cannot start the log hub thread");
        hub
    }

    fn new(options: HubOptions) -> (LogHub, Hub) {
        let (lines, line_rx) = mpsc::channel(options.queue.max(1));
        let (control, control_rx) = mpsc::channel(CONTROL_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let hub = LogHub {
            lines,
            control,
            dropped: dropped.clone(),
        };
        let actor = Hub {
            lines: line_rx,
            control: control_rx,
            dropped,
            ring: VecDeque::with_capacity(options.ring.min(RING_LINES)),
            ring_capacity: options.ring,
            live: broadcast::channel(options.live.max(1)).0,
            dir: options.dir,
            file: None,
            terminal: options.terminal,
            next_seq: 0,
            stats: HubStats::default(),
        };
        (hub, actor)
    }

    /// Queues `line` without waiting. A full queue drops it and counts the
    /// drop; a stopped hub hands it back.
    pub fn offer(&self, line: LogLine) -> Result<(), LogLine> {
        match self.lines.try_send(line) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Closed(line)) => Err(line),
        }
    }

    /// The ring as backfill plus a receiver for everything after it, or
    /// `None` once the hub has stopped.
    pub async fn subscribe(&self) -> Option<Subscription> {
        self.request(Control::Subscribe).await
    }

    /// Waits until every line queued before this call is written.
    pub async fn flush(&self) -> Option<HubStats> {
        self.request(Control::Flush).await
    }

    /// Writes what is queued, closes the file and stops the hub. Later
    /// records fall back to stderr.
    pub async fn shutdown(&self) -> Option<HubStats> {
        self.request(Control::Shutdown).await
    }

    async fn request<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Control) -> Option<T> {
        let (reply, response) = oneshot::channel();
        self.control.send(make(reply)).await.ok()?;
        response.await.ok()
    }
}

static INSTALLED: RwLock<Option<LogHub>> = RwLock::new(None);
static LOGGER: Logger = Logger;

/// Routes the `log` facade to this module (idempotent). Until a hub is
/// installed, records go to stderr.
pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// Logs every uncaught panic with its thread name and location, ahead of
/// the previous hook.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info.location().map(|l| (l.file(), l.line()));
        let name = thread.name().unwrap_or("unnamed");
        log::error!("{}", describe_panic(name, location, info.payload()));
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        if let Some(path) = crate::crash_report::record_panic(name, location, message) {
            log::error!("This crash report has been saved to: {}", path.display());
        }
        previous(info);
    }));
}

fn describe_panic(
    thread: &str,
    location: Option<(&str, u32)>,
    payload: &(dyn std::any::Any + Send),
) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    match location {
        Some((file, line)) => {
            format!("Uncaught panic in thread {thread}: {message} at {file}:{line}")
        }
        None => format!("Uncaught panic in thread {thread}: {message}"),
    }
}

/// Makes `hub` the destination of every log record in the process.
pub fn install(hub: &LogHub) {
    init();
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = Some(hub.clone());
}

/// The hub records currently go to, if any.
pub fn installed() -> Option<LogHub> {
    INSTALLED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Flushes the installed hub, if any.
pub async fn flush() {
    if let Some(hub) = installed() {
        hub.flush().await;
    }
}

fn enabled(level: Level, target: &str) -> bool {
    let root = target.split("::").next().unwrap_or(target);
    let quiet = QUIET_TARGETS.contains(&root);
    level <= if quiet { Level::Warn } else { Level::Info }
}

/// Where a record went.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Hub,
    Stderr,
}

fn route(hub: Option<&LogHub>, line: LogLine) -> Route {
    let line = match hub {
        Some(hub) => match hub.offer(line) {
            Ok(()) => return Route::Hub,
            Err(line) => line,
        },
        None => line,
    };
    eprintln!("{}", line.text);
    Route::Stderr
}

struct Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        enabled(metadata.level(), metadata.target())
    }

    fn log(&self, record: &Record) {
        if !enabled(record.level(), record.target()) {
            return;
        }
        let line = LogLine::new(record.level(), record.target(), record.args().to_string());
        let hub = INSTALLED.read().unwrap_or_else(PoisonError::into_inner);
        route(hub.as_ref(), line);
    }

    fn flush(&self) {}
}

struct Hub {
    lines: mpsc::Receiver<LogLine>,
    control: mpsc::Receiver<Control>,
    dropped: Arc<AtomicU64>,
    ring: VecDeque<Arc<LogLine>>,
    ring_capacity: usize,
    live: broadcast::Sender<Arc<LogLine>>,
    dir: Option<PathBuf>,
    file: Option<LogFile>,
    terminal: bool,
    next_seq: u64,
    stats: HubStats,
}

impl Hub {
    fn run_blocking(self) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("cannot build the log hub runtime");
        runtime.block_on(self.run());
    }

    async fn run(mut self) {
        if let Some(dir) = self.dir.take() {
            match LogFile::open(&dir, now_millis().div_euclid(DAY_MILLIS)) {
                Ok(file) => self.file = Some(file),
                Err(err) => eprintln!("cannot open {}: {err}", dir.join(LATEST).display()),
            }
        }
        let mut poll = tokio::time::interval(crate::threads::TRIM_POLL);
        loop {
            tokio::select! {
                _ = poll.tick() => crate::threads::collect_on_trim(),
                line = self.lines.recv() => match line {
                    Some(line) => {
                        self.publish(line);
                        self.drain();
                    }
                    None => break,
                },
                control = self.control.recv() => {
                    self.drain();
                    match control {
                        Some(Control::Subscribe(reply)) => {
                            let _ = reply.send(Subscription {
                                backfill: self.ring.iter().cloned().collect(),
                                live: self.live.subscribe(),
                            });
                        }
                        Some(Control::Flush(reply)) => {
                            let _ = reply.send(self.stats);
                        }
                        Some(Control::Shutdown(reply)) => {
                            self.lines.close();
                            self.drain();
                            self.file = None;
                            let _ = reply.send(self.stats);
                            return;
                        }
                        None => break,
                    }
                }
            }
        }
        self.drain();
    }

    /// Publishes what is queued now (not what arrives meanwhile, so steady
    /// logging cannot starve requests), then reports drops and flushes.
    fn drain(&mut self) {
        for _ in 0..self.lines.len() {
            let Ok(line) = self.lines.try_recv() else {
                break;
            };
            self.publish(line);
        }
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.stats.dropped += dropped;
            self.publish(LogLine::new(
                Level::Warn,
                module_path!(),
                format!("{dropped} log line(s) dropped: the log queue was full"),
            ));
        }
        if let Some(file) = &mut self.file
            && let Err(err) = file.flush()
        {
            eprintln!("cannot write the log file: {err}");
            self.file = None;
        }
    }

    fn publish(&mut self, mut line: LogLine) {
        line.seq = self.next_seq;
        self.next_seq += 1;
        self.stats.written += 1;
        if self.terminal && !echo_through_sink(&line) {
            let _ = if line.level <= Level::Warn {
                writeln!(io::stderr(), "{}", line.text)
            } else {
                writeln!(io::stdout(), "{}", line.text)
            };
        }
        if let Some(file) = &mut self.file
            && let Err(err) = file.write(&line)
        {
            eprintln!("cannot write the log file: {err}");
            self.file = None;
        }
        let line = Arc::new(line);
        if self.ring_capacity > 0 {
            if self.ring.len() >= self.ring_capacity {
                self.ring.pop_front();
            }
            self.ring.push_back(line.clone());
        }
        let _ = self.live.send(line);
    }
}

/// `latest.log`, archived when the hub starts and when the UTC day changes.
struct LogFile {
    dir: PathBuf,
    out: BufWriter<File>,
    day: i64,
}

impl LogFile {
    fn open(dir: &Path, today: i64) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let latest = dir.join(LATEST);
        if let Ok(meta) = fs::metadata(&latest)
            && meta.len() > 0
        {
            let day = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(today, |d| d.as_secs() as i64 / 86_400);
            archive(dir, day)?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            out: BufWriter::new(File::create(&latest)?),
            day: today,
        })
    }

    fn write(&mut self, line: &LogLine) -> io::Result<()> {
        if line.day() > self.day {
            self.out.flush()?;
            if self.out.get_ref().metadata()?.len() > 0 {
                archive(&self.dir, self.day)?;
            }
            self.out = BufWriter::new(File::create(self.dir.join(LATEST))?);
            self.day = line.day();
        }
        writeln!(self.out, "{}", line.file_line())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Gzips `latest.log` to the first free `<yyyy-MM-dd>-<n>.log.gz` of `day`
/// and removes it.
fn archive(dir: &Path, day: i64) -> io::Result<PathBuf> {
    let (y, m, d) = civil_from_days(day);
    let latest = dir.join(LATEST);
    let target = (1..)
        .map(|n| dir.join(format!("{y:04}-{m:02}-{d:02}-{n}.log.gz")))
        .find(|path| !path.exists())
        .expect("an unused archive name");
    let partial = target.with_extension("gz.partial");
    let mut encoder =
        flate2::write::GzEncoder::new(File::create(&partial)?, flate2::Compression::default());
    io::copy(&mut File::open(&latest)?, &mut encoder)?;
    encoder.finish()?.sync_all()?;
    fs::rename(&partial, &target)?;
    fs::remove_file(&latest)?;
    Ok(target)
}

#[cfg(test)]
mod tests;

/// Receives each terminal echo line (without newline) and whether it
/// belongs on stderr, in place of the hub writing it itself.
pub type TerminalSink = Arc<dyn Fn(&str, bool) + Send + Sync>;

static TERMINAL_SINK: std::sync::Mutex<Option<TerminalSink>> = std::sync::Mutex::new(None);

/// Routes the hub's terminal echo through `sink` (`None` restores direct
/// writes), so an input line editor can keep its pending line below the
/// output.
pub fn set_terminal_sink(sink: Option<TerminalSink>) {
    *TERMINAL_SINK.lock().unwrap_or_else(|e| e.into_inner()) = sink;
}

fn echo_through_sink(line: &LogLine) -> bool {
    let sink = TERMINAL_SINK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match sink {
        Some(sink) => {
            sink(&line.text, line.level <= Level::Warn);
            true
        }
        None => false,
    }
}

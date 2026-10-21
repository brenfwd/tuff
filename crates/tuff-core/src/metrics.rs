//! Live server metrics: one actor owns the latest summary from every
//! producer, the registered series and their history, and publishes a
//! [`Snapshot`] once per interval.
//!
//! Producers never block. Ticking actors summarize locally and `try_send`
//! one [`Report`] per pace window (a full queue drops it and counts the
//! drop in `metrics.dropped`). Hot counters (network, HTTP, feature series)
//! are [`Series`] cells bumped with relaxed atomics and sampled by the
//! actor once per interval, so no message is ever sent per event.

mod actor;
mod history;
mod reporters;
mod system;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};

pub use history::{HISTORY_MINUTES, HISTORY_SECONDS, Point};
pub use reporters::{
    NetMeter, Pacer, StateGauge, TickReporter, Window, WindowStats, millis, tick_report,
};
pub use system::{
    CpuSample, CpuStats, MIN_CPU_WINDOW, MemoryStats, RuntimeStats, cores_between, host_cores,
    parse_proc_status,
};

/// Reports queued for the actor before producers start dropping them.
/// Bounded so a stalled actor cannot grow memory; producers only ever
/// `try_send`, so the bound never makes a region or session wait.
pub const REPORT_QUEUE: usize = 4096;
/// Panel queries (history, collect) waiting for the actor; callers await
/// a slot, which pushes back on HTTP handlers, never on the game.
pub const QUERY_QUEUE: usize = 64;
/// How many of the slowest regions a snapshot lists.
pub const TOP_REGIONS: usize = 10;
/// A region that has not reported for this many ticks of the newest
/// report seen is gone (closed or unloaded).
pub const STALE_REGION_TICKS: u64 = 200;
/// A tick report older than this is marked stale (frozen or stalled).
pub const STALE_TICK_REPORT: Duration = Duration::from_secs(5);

pub const TICK_TPS: &str = "tick.tps";
pub const TICK_MSPT: &str = "tick.mspt";
pub const TICK_MSPT_MAX: &str = "tick.mspt.max";
pub const TICK_MSPT_P95: &str = "tick.mspt.p95";
pub const PLAYERS: &str = "players";
pub const WORLD_REGIONS: &str = "world.regions";
pub const WORLD_CHUNKS: &str = "world.chunks";
pub const WORLD_ENTITIES: &str = "world.entities";
pub const CHUNK_IO_QUEUED: &str = "chunk_io.queued";
pub const MEMORY_RSS: &str = "memory.rss";
pub const PROCESS_CPU: &str = "process.cpu";

/// How often producers report and the actor samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MetricsConfig {
    /// How often the actor samples series and publishes a snapshot.
    pub interval: Duration,
    /// Ticking producers report once both have passed since their last
    /// report: at least `every` of wall time and `ticks` ticks.
    pub pace: Pace,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    pub every: Duration,
    pub ticks: u64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            pace: Pace {
                every: Duration::from_secs(1),
                ticks: 20,
            },
        }
    }
}

impl MetricsConfig {
    /// Reports every 20 ticks regardless of wall time, so tests that run
    /// ticks back to back see reports deterministically.
    pub fn by_ticks() -> Self {
        Self {
            pace: Pace {
                every: Duration::ZERO,
                ticks: 20,
            },
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Only grows; history shows its rate per second.
    Counter,
    /// A level that goes up and down; history shows its value.
    Gauge,
}

/// One named number, bumped lock-free by its owner and read by the
/// metrics actor once per interval. Clones share the cell. Handles from
/// several [`Metrics::series`] calls with one name are summed.
#[derive(Clone, Debug)]
pub struct Series {
    kind: Kind,
    cell: Arc<AtomicI64>,
}

impl Series {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            cell: Arc::default(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn add(&self, amount: i64) {
        self.cell.fetch_add(amount, Ordering::Relaxed);
    }

    pub fn inc(&self) {
        self.add(1);
    }

    /// Gauges only: a counter must never go down.
    pub fn dec(&self) {
        debug_assert_eq!(self.kind, Kind::Gauge);
        self.add(-1);
    }

    /// Gauges only.
    pub fn set(&self, value: i64) {
        debug_assert_eq!(self.kind, Kind::Gauge);
        self.cell.store(value, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.cell.load(Ordering::Relaxed)
    }
}

/// The series core itself feeds.
#[derive(Clone, Debug)]
pub struct CoreSeries {
    pub bytes_in: Series,
    pub bytes_out: Series,
    pub packets_in: Series,
    pub packets_out: Series,
    /// Open Minecraft connections by protocol state, indexed like
    /// [`tuff_trace::ConnState`].
    pub connections: [Series; 5],
    pub http_requests: Series,
    pub http_bytes_out: Series,
    pub pack_downloads: Series,
    pub anticheat_alerts: Series,
    /// Reports dropped because the actor's queue was full.
    pub dropped: Series,
    /// Region actor loops that panicked and were restarted.
    pub region_panics: Series,
    /// Player sessions that panicked and were made to leave.
    pub session_panics: Series,
    /// Panics of every other long-lived actor (manager, coordinator,
    /// storage, services). Process-wide, like the actors it counts.
    pub actor_panics: Series,
}

static ACTOR_PANICS: std::sync::LazyLock<Series> = std::sync::LazyLock::new(|| Series {
    kind: Kind::Counter,
    cell: tuff_supervise::actor_panics().clone(),
});

/// The process-wide counter behind [`CoreSeries::actor_panics`], for
/// actors that have no metrics handle.
pub fn actor_panics() -> &'static Series {
    &ACTOR_PANICS
}

impl CoreSeries {
    fn named(&self) -> Vec<(String, Series)> {
        let mut named = vec![
            ("net.bytes_in".to_owned(), self.bytes_in.clone()),
            ("net.bytes_out".to_owned(), self.bytes_out.clone()),
            ("net.packets_in".to_owned(), self.packets_in.clone()),
            ("net.packets_out".to_owned(), self.packets_out.clone()),
            ("http.requests".to_owned(), self.http_requests.clone()),
            ("http.bytes_out".to_owned(), self.http_bytes_out.clone()),
            (
                "http.pack_downloads".to_owned(),
                self.pack_downloads.clone(),
            ),
            ("anticheat.alerts".to_owned(), self.anticheat_alerts.clone()),
            ("metrics.dropped".to_owned(), self.dropped.clone()),
            (
                "actors.region_panics".to_owned(),
                self.region_panics.clone(),
            ),
            (
                "actors.session_panics".to_owned(),
                self.session_panics.clone(),
            ),
            ("actors.panics".to_owned(), self.actor_panics.clone()),
        ];
        for state in CONN_STATES {
            named.push((
                format!("net.connections.{}", state.name()),
                self.connection(state).clone(),
            ));
        }
        named
    }

    pub fn connection(&self, state: tuff_trace::ConnState) -> &Series {
        &self.connections[conn_index(state)]
    }
}

const CONN_STATES: [tuff_trace::ConnState; 5] = [
    tuff_trace::ConnState::Handshake,
    tuff_trace::ConnState::Status,
    tuff_trace::ConnState::Login,
    tuff_trace::ConnState::Configuration,
    tuff_trace::ConnState::Play,
];

fn conn_index(state: tuff_trace::ConnState) -> usize {
    use tuff_trace::ConnState;
    match state {
        ConnState::Handshake => 0,
        ConnState::Status => 1,
        ConnState::Login => 2,
        ConnState::Configuration => 3,
        ConnState::Play => 4,
    }
}

impl Default for CoreSeries {
    fn default() -> Self {
        let counter = || Series::new(Kind::Counter);
        Self {
            bytes_in: counter(),
            bytes_out: counter(),
            packets_in: counter(),
            packets_out: counter(),
            connections: std::array::from_fn(|_| Series::new(Kind::Gauge)),
            http_requests: counter(),
            http_bytes_out: counter(),
            pack_downloads: counter(),
            anticheat_alerts: counter(),
            dropped: counter(),
            region_panics: counter(),
            session_panics: counter(),
            actor_panics: actor_panics().clone(),
        }
    }
}

/// A producer's summary, sent at most once per pace window.
#[derive(Debug)]
pub enum Report {
    Tick(TickReport),
    Region(Box<RegionReport>),
    Manager(ManagerReport),
    ChunkIo(ChunkIoReport),
}

/// The coordinator's tick loop over one window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TickReport {
    pub tick: u64,
    pub window_ms: f64,
    pub ticks: u64,
    pub tps: f64,
    /// Time per tick; under tick lockstep this includes waiting for the
    /// slowest region or player to finish.
    pub mspt_mean: f64,
    pub mspt_max: f64,
    pub mspt_p50: f64,
    pub mspt_p95: f64,
    pub mspt_p99: f64,
    pub target_rate: f32,
    pub frozen: bool,
    pub sprinting: bool,
    /// Ticks the window fell short of the target rate (0 while frozen or
    /// sprinting).
    pub behind: f64,
    pub players: Vec<PlayerPing>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlayerPing {
    pub name: String,
    pub uuid: String,
    pub ping_ms: i32,
}

/// One region actor over one window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RegionReport {
    pub dimension: String,
    pub x: i32,
    pub z: i32,
    pub tick: u64,
    pub ticks: u64,
    pub mspt_mean: f64,
    pub mspt_max: f64,
    pub chunks: u64,
    pub loading: u64,
    pub generating: u64,
    pub entities: u64,
    /// Entities by mob category id ([`tuff_vanilla_data::entity::MobCategory`]).
    #[serde(skip)]
    pub entities_by_category: Vec<u64>,
    pub block_entities: u64,
    pub scheduled_ticks: u64,
    /// Players whose chunk lies in this region.
    pub players: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ManagerReport {
    pub regions: u64,
    pub closing: u64,
    /// Tickets held on regions (viewing players and held chunks).
    pub region_tickets: u64,
    /// Chunks held loaded by tickets of any kind.
    pub held_chunks: u64,
    pub forced_chunks: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ChunkIoReport {
    pub window_ms: f64,
    /// Ops waiting for their file, not counting ones being run.
    pub queued: u64,
    pub running: u64,
    pub open_files: u64,
    pub reads: u64,
    pub writes: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    /// Longest time a batch with writes took from submission to its
    /// answer within the window.
    pub save_latency_max_ms: f64,
}

/// Region figures summed over every live region.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorldTotals {
    pub regions: u64,
    pub chunks: u64,
    pub loading: u64,
    pub generating: u64,
    pub entities: u64,
    pub entities_by_category: std::collections::BTreeMap<&'static str, u64>,
    pub block_entities: u64,
    pub scheduled_ticks: u64,
    pub players: u64,
}

/// Region figures summed over one dimension's live regions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DimensionTotals {
    pub regions: u64,
    pub chunks: u64,
    pub entities: u64,
    pub players: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeriesValue {
    pub kind: Kind,
    pub value: i64,
    /// Change per second since the previous sample (counters).
    pub rate: f64,
}

/// Everything the actor knows as of one sample.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub unix_ms: u64,
    pub uptime_s: f64,
    pub tick: Option<TickReport>,
    pub tick_stale: bool,
    pub world: WorldTotals,
    /// [`DimensionTotals`] by dimension key; dimensions with no live
    /// region are absent.
    pub dimensions: std::collections::BTreeMap<String, DimensionTotals>,
    pub slowest_regions: Vec<RegionReport>,
    pub manager: Option<ManagerReport>,
    pub chunk_io: Option<ChunkIoReport>,
    pub runtime: Option<RuntimeStats>,
    /// `None` where the platform offers no way to read it without native
    /// dependencies (everything but Linux and macOS).
    pub memory: Option<MemoryStats>,
    /// `None` before the first long enough window and where the platform
    /// has no process CPU time (everything but unix).
    pub cpu: Option<CpuStats>,
    pub series: std::collections::BTreeMap<String, SeriesValue>,
    pub dropped_reports: i64,
}

impl Snapshot {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

enum Query {
    #[cfg(test)]
    Panic,
    Collect {
        reply: oneshot::Sender<Arc<Snapshot>>,
    },
    History {
        series: String,
        range: Duration,
        reply: oneshot::Sender<Option<Vec<Point>>>,
    },
}

struct Register {
    name: String,
    series: Series,
}

struct Inner {
    reports: mpsc::Sender<Report>,
    queries: mpsc::Sender<Query>,
    // Unbounded: registration happens from sync code that must not lose
    // it, at most once per series.
    registrations: mpsc::UnboundedSender<Register>,
    latest: watch::Receiver<Arc<Snapshot>>,
    config: MetricsConfig,
    core: CoreSeries,
}

/// A handle to the metrics actor; cheap to clone.
#[derive(Clone)]
pub struct Metrics(Arc<Inner>);

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics")
            .field("config", &self.0.config)
            .finish_non_exhaustive()
    }
}

impl Metrics {
    /// Starts the actor; needs a tokio runtime.
    pub fn spawn(config: MetricsConfig) -> Self {
        let (metrics, actor) = Self::build(config);
        tuff_supervise::spawn("the metrics actor", actor.restarting());
        metrics
    }

    /// A handle with no actor behind it: reports and series go nowhere and
    /// the snapshot stays empty.
    pub fn detached() -> Self {
        Self::build(MetricsConfig::default()).0
    }

    fn build(config: MetricsConfig) -> (Self, actor::Actor) {
        let (reports, report_rx) = mpsc::channel(REPORT_QUEUE);
        let (queries, query_rx) = mpsc::channel(QUERY_QUEUE);
        let (registrations, registration_rx) = mpsc::unbounded_channel();
        let (publish, latest) = watch::channel(Arc::new(Snapshot::default()));
        let core = CoreSeries::default();
        let actor = actor::Actor::new(
            config,
            report_rx,
            query_rx,
            registration_rx,
            publish,
            core.named(),
        );
        let metrics = Self(Arc::new(Inner {
            reports,
            queries,
            registrations,
            latest,
            config,
            core,
        }));
        (metrics, actor)
    }

    pub fn config(&self) -> MetricsConfig {
        self.0.config
    }

    pub fn pace(&self) -> Pace {
        self.0.config.pace
    }

    pub fn core(&self) -> &CoreSeries {
        &self.0.core
    }

    /// Queues `report` without waiting; false if it was dropped.
    pub fn report(&self, report: Report) -> bool {
        match self.0.reports.try_send(report) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.0.core.dropped.inc();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Reports dropped so far because the queue was full.
    pub fn dropped(&self) -> i64 {
        self.0.core.dropped.get()
    }

    /// The most recently published snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.0.latest.borrow().clone()
    }

    /// Every published snapshot from now on.
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.0.latest.clone()
    }

    /// Takes a sample now, after every report already queued, and
    /// publishes and returns it.
    pub async fn collect(&self) -> Arc<Snapshot> {
        let (reply, answer) = oneshot::channel();
        if self.0.queries.send(Query::Collect { reply }).await.is_err() {
            return self.snapshot();
        }
        answer.await.unwrap_or_else(|_| self.snapshot())
    }

    /// The history of `series` over the last `range`: one point per
    /// sample up to [`HISTORY_SECONDS`] seconds, one per minute beyond
    /// (up to [`HISTORY_MINUTES`]). `None` for a series never recorded.
    pub async fn history(&self, series: &str, range: Duration) -> Option<Vec<Point>> {
        let (reply, answer) = oneshot::channel();
        self.0
            .queries
            .send(Query::History {
                series: series.to_owned(),
                range,
                reply,
            })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// A series named `name` (dotted, e.g. `map.tiles_rendered`) that this
    /// caller bumps; it appears in every snapshot and has history. Call
    /// once and keep the handle. A name already registered with another
    /// kind is refused: the handle works but is never sampled.
    pub fn series(&self, name: &str, kind: Kind) -> Series {
        let series = Series::new(kind);
        let _ = self.0.registrations.send(Register {
            name: name.to_owned(),
            series: series.clone(),
        });
        series
    }
}

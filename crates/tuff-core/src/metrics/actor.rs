use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch};
use tuff_vanilla_data::entity::MobCategory;

use super::history::History;
use super::system::{CpuSample, CpuStats, MemoryStats, RuntimeStats};
use super::*;

struct Registered {
    kind: Kind,
    cells: Vec<Series>,
    /// The total at the previous sample, for counter rates.
    last: i64,
}

struct Latest<T> {
    report: T,
    at: Instant,
}

pub(super) struct Actor {
    config: MetricsConfig,
    reports: mpsc::Receiver<Report>,
    queries: mpsc::Receiver<Query>,
    registrations: mpsc::UnboundedReceiver<Register>,
    publish: watch::Sender<Arc<Snapshot>>,
    started: Instant,
    last_sample: Option<Instant>,
    series: BTreeMap<String, Registered>,
    history: History,
    tick: Option<Latest<TickReport>>,
    regions: HashMap<(String, i32, i32), RegionReport>,
    /// The newest tick any report carried, which ages out closed regions.
    newest_tick: u64,
    manager: Option<ManagerReport>,
    chunk_io: Option<ChunkIoReport>,
    /// The start of the current CPU window.
    cpu_since: Option<CpuSample>,
    cpu: Option<CpuStats>,
    host_cores: Option<u64>,
}

impl Actor {
    pub(super) fn new(
        config: MetricsConfig,
        reports: mpsc::Receiver<Report>,
        queries: mpsc::Receiver<Query>,
        registrations: mpsc::UnboundedReceiver<Register>,
        publish: watch::Sender<Arc<Snapshot>>,
        core: Vec<(String, Series)>,
    ) -> Self {
        let mut actor = Self {
            config,
            reports,
            queries,
            registrations,
            publish,
            started: Instant::now(),
            last_sample: None,
            series: BTreeMap::new(),
            history: History::default(),
            tick: None,
            regions: HashMap::new(),
            newest_tick: 0,
            manager: None,
            chunk_io: None,
            cpu_since: CpuSample::now(),
            cpu: None,
            host_cores: host_cores(),
        };
        for (name, series) in core {
            actor.register(Register { name, series });
        }
        actor
    }

    pub(super) async fn restarting(mut self) {
        use tuff_supervise::{CatchUnwind, count_panic, panic_message};
        while let Err(panic) = CatchUnwind::new(Box::pin(self.run())).await {
            count_panic();
            log::error!(
                "the metrics actor panicked: {}; restarting it with its series and history",
                panic_message(&*panic)
            );
        }
    }

    async fn run(&mut self) {
        let mut interval = tokio::time::interval(self.config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                Some(register) = self.registrations.recv() => self.register(register),
                report = self.reports.recv() => match report {
                    Some(report) => self.apply(report),
                    None => return,
                },
                Some(query) = self.queries.recv() => self.answer(query),
                _ = interval.tick() => {
                    self.sample(Instant::now(), SystemTime::now());
                }
            }
        }
    }

    fn register(&mut self, Register { name, series }: Register) {
        match self.series.get_mut(&name) {
            Some(known) if known.kind == series.kind => {
                known.last += series.get();
                known.cells.push(series);
            }
            Some(known) => log::warn!(
                "metrics series {name} is a {:?}; a {:?} of that name is ignored",
                known.kind,
                series.kind
            ),
            None => {
                let last = series.get();
                self.series.insert(
                    name,
                    Registered {
                        kind: series.kind,
                        cells: vec![series],
                        last,
                    },
                );
            }
        }
    }

    pub(super) fn apply(&mut self, report: Report) {
        match report {
            Report::Tick(report) => {
                self.newest_tick = self.newest_tick.max(report.tick);
                self.tick = Some(Latest {
                    report,
                    at: Instant::now(),
                });
            }
            Report::Region(report) => {
                self.newest_tick = self.newest_tick.max(report.tick);
                let key = (report.dimension.clone(), report.x, report.z);
                self.regions.insert(key, *report);
            }
            Report::Manager(report) => self.manager = Some(report),
            Report::ChunkIo(report) => self.chunk_io = Some(report),
        }
    }

    fn answer(&mut self, query: Query) {
        match query {
            #[cfg(test)]
            Query::Panic => panic!("test panic"),
            Query::Collect { reply } => {
                while let Ok(register) = self.registrations.try_recv() {
                    self.register(register);
                }
                while let Ok(report) = self.reports.try_recv() {
                    self.apply(report);
                }
                let _ = reply.send(self.sample(Instant::now(), SystemTime::now()));
            }
            Query::History {
                series,
                range,
                reply,
            } => {
                let now = unix_ms(SystemTime::now());
                let _ = reply.send(self.history.range(&series, now, range));
            }
        }
    }

    pub(super) fn sample(&mut self, now: Instant, wall: SystemTime) -> Arc<Snapshot> {
        let unix_ms = unix_ms(wall);
        let elapsed = self
            .last_sample
            .map(|last| now.saturating_duration_since(last).as_secs_f64());
        self.last_sample = Some(now);
        let newest = self.newest_tick;
        self.regions
            .retain(|_, region| newest.saturating_sub(region.tick) <= STALE_REGION_TICKS);
        let world = self.world_totals();
        let slowest = self.slowest_regions();
        let mut series = BTreeMap::new();
        for (name, registered) in &mut self.series {
            let value: i64 = registered.cells.iter().map(Series::get).sum();
            let rate = match (registered.kind, elapsed) {
                (Kind::Counter, Some(seconds)) if seconds > 0.0 => {
                    (value - registered.last) as f64 / seconds
                }
                _ => 0.0,
            };
            registered.last = value;
            let shown = match registered.kind {
                Kind::Counter => rate,
                Kind::Gauge => value as f64,
            };
            self.history.push(name, unix_ms, shown);
            series.insert(
                name.clone(),
                SeriesValue {
                    kind: registered.kind,
                    value,
                    rate,
                },
            );
        }
        let tick = self.tick.as_ref().map(|latest| latest.report.clone());
        let tick_stale = self
            .tick
            .as_ref()
            .is_some_and(|latest| now.saturating_duration_since(latest.at) > STALE_TICK_REPORT);
        let memory = MemoryStats::sample();
        self.sample_cpu();
        let players = tick
            .as_ref()
            .map_or(world.players, |tick| tick.players.len() as u64);
        if let Some(tick) = &tick
            && !tick_stale
        {
            self.history.push(TICK_TPS, unix_ms, tick.tps);
            self.history.push(TICK_MSPT, unix_ms, tick.mspt_mean);
            self.history.push(TICK_MSPT_MAX, unix_ms, tick.mspt_max);
            self.history.push(TICK_MSPT_P95, unix_ms, tick.mspt_p95);
        }
        self.history.push(PLAYERS, unix_ms, players as f64);
        self.history
            .push(WORLD_REGIONS, unix_ms, world.regions as f64);
        self.history
            .push(WORLD_CHUNKS, unix_ms, world.chunks as f64);
        self.history
            .push(WORLD_ENTITIES, unix_ms, world.entities as f64);
        if let Some(io) = &self.chunk_io {
            self.history
                .push(CHUNK_IO_QUEUED, unix_ms, io.queued as f64);
        }
        if let Some(memory) = &memory {
            self.history
                .push(MEMORY_RSS, unix_ms, memory.rss_bytes as f64);
        }
        if let Some(cpu) = &self.cpu {
            self.history.push(PROCESS_CPU, unix_ms, cpu.cores);
        }
        let dropped_reports = series.get("metrics.dropped").map_or(0, |s| s.value);
        let snapshot = Arc::new(Snapshot {
            unix_ms,
            uptime_s: now.saturating_duration_since(self.started).as_secs_f64(),
            tick,
            tick_stale,
            world,
            dimensions: self.dimension_totals(),
            slowest_regions: slowest,
            manager: self.manager.clone(),
            chunk_io: self.chunk_io.clone(),
            runtime: RuntimeStats::sample(),
            memory,
            cpu: self.cpu,
            series,
            dropped_reports,
        });
        self.publish.send_replace(snapshot.clone());
        snapshot
    }

    /// Starts a new window only once the current one is long enough, so
    /// back to back samples (a collect right after a tick) keep the last
    /// figure instead of a noisy one.
    fn sample_cpu(&mut self) {
        let Some(now) = CpuSample::now() else {
            return;
        };
        let Some(since) = self.cpu_since else {
            self.cpu_since = Some(now);
            return;
        };
        if let Some(cores) = cores_between(since, now) {
            self.cpu_since = Some(now);
            self.cpu = Some(CpuStats {
                cores,
                host_cores: self.host_cores,
            });
        }
    }

    fn world_totals(&self) -> WorldTotals {
        let mut totals = WorldTotals {
            regions: self.regions.len() as u64,
            ..WorldTotals::default()
        };
        let mut by_category = [0u64; MobCategory::COUNT];
        for region in self.regions.values() {
            totals.chunks += region.chunks;
            totals.loading += region.loading;
            totals.generating += region.generating;
            totals.entities += region.entities;
            totals.block_entities += region.block_entities;
            totals.scheduled_ticks += region.scheduled_ticks;
            totals.players += region.players;
            for (total, count) in by_category.iter_mut().zip(&region.entities_by_category) {
                *total += count;
            }
        }
        totals.entities_by_category = MobCategory::ALL
            .iter()
            .zip(by_category)
            .map(|(category, count)| (category.name(), count))
            .collect();
        totals
    }

    fn dimension_totals(&self) -> BTreeMap<String, DimensionTotals> {
        let mut out = BTreeMap::<String, DimensionTotals>::new();
        for region in self.regions.values() {
            let totals = match out.get_mut(&region.dimension) {
                Some(totals) => totals,
                None => out.entry(region.dimension.clone()).or_default(),
            };
            totals.regions += 1;
            totals.chunks += region.chunks;
            totals.entities += region.entities;
            totals.players += region.players;
        }
        out
    }

    fn slowest_regions(&self) -> Vec<RegionReport> {
        slowest(self.regions.values(), TOP_REGIONS)
    }
}

/// The `count` regions with the highest mean tick time, then max, then
/// position for a stable order.
pub(super) fn slowest<'a>(
    regions: impl Iterator<Item = &'a RegionReport>,
    count: usize,
) -> Vec<RegionReport> {
    let mut all: Vec<&RegionReport> = regions.collect();
    all.sort_by(|a, b| {
        b.mspt_mean
            .total_cmp(&a.mspt_mean)
            .then(b.mspt_max.total_cmp(&a.mspt_max))
            .then_with(|| (&a.dimension, a.x, a.z).cmp(&(&b.dimension, b.x, b.z)))
    });
    all.into_iter().take(count).cloned().collect()
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

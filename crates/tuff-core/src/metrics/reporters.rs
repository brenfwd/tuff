//! Producer-side helpers: they summarize locally and decide when a report
//! is due, so the producing actor only makes one call per loop.

use std::time::{Duration, Instant};

use super::{Metrics, Pace, PlayerPing, Report, Series, TickReport};
use crate::world::tick_rate::TickState;
use tuff_trace::ConnState;

/// Most per-tick durations a window keeps for percentiles (a sprint can
/// run thousands of ticks per second).
const MAX_SAMPLES: usize = 10_000;
/// How often a connection adds its local counts to the shared series.
const NET_FLUSH: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowStats {
    pub elapsed: Duration,
    pub ticks: u64,
    pub mean: Duration,
    pub max: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

/// Tick durations since the last report.
pub struct Window {
    started: Instant,
    ticks: u64,
    sum: Duration,
    max: Duration,
    samples: Option<Vec<Duration>>,
}

impl Window {
    /// Percentiles are only computed when `percentiles` is set.
    pub fn new(percentiles: bool) -> Self {
        Self {
            started: Instant::now(),
            ticks: 0,
            sum: Duration::ZERO,
            max: Duration::ZERO,
            samples: percentiles.then(Vec::new),
        }
    }

    /// Adds one tick; returns the window's figures and starts a new one
    /// once `pace` says a report is due.
    pub fn record(&mut self, pace: Pace, busy: Duration, now: Instant) -> Option<WindowStats> {
        self.ticks += 1;
        self.sum += busy;
        self.max = self.max.max(busy);
        if let Some(samples) = &mut self.samples
            && samples.len() < MAX_SAMPLES
        {
            samples.push(busy);
        }
        let elapsed = now.saturating_duration_since(self.started);
        if self.ticks < pace.ticks || elapsed < pace.every {
            return None;
        }
        let mut stats = WindowStats {
            elapsed,
            ticks: self.ticks,
            mean: self.sum / self.ticks.max(1) as u32,
            max: self.max,
            ..WindowStats::default()
        };
        if let Some(samples) = &mut self.samples {
            samples.sort_unstable();
            let at = |fraction: f64| {
                let index = ((samples.len() as f64 * fraction) as usize).min(samples.len() - 1);
                samples[index]
            };
            (stats.p50, stats.p95, stats.p99) = (at(0.5), at(0.95), at(0.99));
            samples.clear();
        }
        self.started = now;
        self.ticks = 0;
        self.sum = Duration::ZERO;
        self.max = Duration::ZERO;
        Some(stats)
    }
}

pub fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The coordinator's tick reporter.
pub struct TickReporter {
    window: Window,
}

impl Default for TickReporter {
    fn default() -> Self {
        Self {
            window: Window::new(true),
        }
    }
}

impl TickReporter {
    /// Records one finished tick and reports the window when due;
    /// `players` is only called then.
    pub fn record(
        &mut self,
        metrics: &Metrics,
        tick: u64,
        busy: Duration,
        state: TickState,
        sprinting: bool,
        players: impl FnOnce() -> Vec<PlayerPing>,
    ) {
        let Some(stats) = self.window.record(metrics.pace(), busy, Instant::now()) else {
            return;
        };
        metrics.report(Report::Tick(tick_report(
            tick,
            stats,
            state,
            sprinting,
            players(),
        )));
    }
}

pub fn tick_report(
    tick: u64,
    stats: WindowStats,
    state: TickState,
    sprinting: bool,
    players: Vec<PlayerPing>,
) -> TickReport {
    let seconds = stats.elapsed.as_secs_f64();
    let tps = if seconds > 0.0 {
        stats.ticks as f64 / seconds
    } else {
        0.0
    };
    let behind = if state.frozen || sprinting {
        0.0
    } else {
        (seconds * f64::from(state.rate) - stats.ticks as f64)
            .floor()
            .max(0.0)
    };
    TickReport {
        tick,
        window_ms: millis(stats.elapsed),
        ticks: stats.ticks,
        tps,
        mspt_mean: millis(stats.mean),
        mspt_max: millis(stats.max),
        mspt_p50: millis(stats.p50),
        mspt_p95: millis(stats.p95),
        mspt_p99: millis(stats.p99),
        target_rate: state.rate,
        frozen: state.frozen,
        sprinting,
        behind,
        players,
    }
}

/// Paces a producer that reports counts rather than tick durations.
pub struct Pacer {
    started: Instant,
    ticks: u64,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            ticks: 0,
        }
    }
}

impl Pacer {
    /// Counts one tick; the window's length once a report is due.
    pub fn tick(&mut self, pace: Pace, now: Instant) -> Option<Duration> {
        self.ticks += 1;
        if self.ticks < pace.ticks {
            return None;
        }
        self.time(pace, now)
    }

    /// For producers without ticks: due once `pace.every` has passed.
    pub fn time(&mut self, pace: Pace, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < pace.every {
            return None;
        }
        self.started = now;
        self.ticks = 0;
        Some(elapsed)
    }
}

/// One direction of a connection's traffic, counted locally and added to
/// the shared series at most once per second and when dropped.
pub struct NetMeter {
    bytes: Series,
    packets: Series,
    pending_bytes: i64,
    pending_packets: i64,
    flushed: Instant,
}

impl NetMeter {
    fn new(bytes: &Series, packets: &Series) -> Self {
        Self {
            bytes: bytes.clone(),
            packets: packets.clone(),
            pending_bytes: 0,
            pending_packets: 0,
            flushed: Instant::now(),
        }
    }

    pub fn inbound(metrics: &Metrics) -> Self {
        let core = metrics.core();
        Self::new(&core.bytes_in, &core.packets_in)
    }

    pub fn outbound(metrics: &Metrics) -> Self {
        let core = metrics.core();
        Self::new(&core.bytes_out, &core.packets_out)
    }

    pub fn bytes(&mut self, count: usize) {
        self.pending_bytes += count as i64;
    }

    pub fn packets(&mut self, count: usize) {
        self.pending_packets += count as i64;
        if self.flushed.elapsed() >= NET_FLUSH {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        self.flushed = Instant::now();
        if self.pending_bytes != 0 {
            self.bytes.add(std::mem::take(&mut self.pending_bytes));
        }
        if self.pending_packets != 0 {
            self.packets.add(std::mem::take(&mut self.pending_packets));
        }
    }
}

impl Drop for NetMeter {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Counts a connection in the gauge of its protocol state while it lives.
pub struct StateGauge {
    states: [Series; 5],
    current: usize,
}

impl StateGauge {
    pub fn new(metrics: &Metrics, state: ConnState) -> Self {
        let states = metrics.core().connections.clone();
        let current = super::conn_index(state);
        states[current].inc();
        Self { states, current }
    }

    pub fn set(&mut self, state: ConnState) {
        let next = super::conn_index(state);
        if next != self.current {
            self.states[self.current].dec();
            self.states[next].inc();
            self.current = next;
        }
    }
}

impl Drop for StateGauge {
    fn drop(&mut self) {
        self.states[self.current].dec();
    }
}

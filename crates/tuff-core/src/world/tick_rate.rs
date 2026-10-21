//! `/tick` state: the target simulation rate, freeze/step/sprint, and the
//! sample window `/tick query` reports on. The coordinator is the only
//! actor that ticks the game clock (`Shared.clock`); every region, session
//! and the world clocks simply wait on that clock's `watch`, so pausing it
//! here is what makes freeze stop the whole simulation while commands and
//! new connections keep being handled.

use std::collections::VecDeque;
use std::time::Duration;

pub const DEFAULT_RATE: f32 = 20.0;
/// The game's fixed sample window for `/tick query`'s average and
/// percentiles.
const SAMPLE_WINDOW: usize = 100;

/// What clients are told: the target rate and whether ticking is paused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickState {
    pub rate: f32,
    pub frozen: bool,
}

impl Default for TickState {
    fn default() -> Self {
        Self {
            rate: DEFAULT_RATE,
            frozen: false,
        }
    }
}

impl TickState {
    pub fn period(self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.rate))
    }
}

/// How long one finished tick took, as published to
/// [`crate::server::ServerHandle::tick_times`] subscribers. Under
/// lockstep this spans every region and session finishing the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickTime {
    pub tick: u64,
    pub busy: Duration,
}

/// A ring buffer of how long each tick's own processing took, for
/// `/tick query`'s average and percentiles.
#[derive(Debug, Default, Clone)]
pub struct Samples(VecDeque<Duration>);

impl Samples {
    pub fn push(&mut self, duration: Duration) {
        if self.0.len() == SAMPLE_WINDOW {
            self.0.pop_front();
        }
        self.0.push_back(duration);
    }

    pub fn average(&self) -> Duration {
        if self.0.is_empty() {
            return Duration::ZERO;
        }
        self.0.iter().sum::<Duration>() / self.0.len() as u32
    }

    /// p50, p95, p99 and the sample count, the way the game computes them:
    /// sorted ascending, indexed by a truncated fraction of the count.
    pub fn percentiles(&self) -> (Duration, Duration, Duration, usize) {
        let mut sorted: Vec<Duration> = self.0.iter().copied().collect();
        sorted.sort_unstable();
        let at = |fraction: f64| {
            let index = (sorted.len() as f64 * fraction) as usize;
            sorted.get(index).copied().unwrap_or_default()
        };
        (at(0.5), at(0.95), at(0.99), sorted.len())
    }
}

/// A running `/tick sprint`: ticks as fast as possible until `target`
/// ticks have run or it is stopped.
#[derive(Debug, Clone, Copy)]
pub struct Sprint {
    pub target: u64,
    pub completed: u64,
    /// Time actually spent processing sprint ticks (not wall-clock time
    /// between them), matching what the game measures.
    pub busy: Duration,
    /// Whether to re-freeze once the sprint ends.
    pub resume_frozen: bool,
    /// A tick that began before the sprint is still finishing; its
    /// completion is not one of the sprint's ticks.
    pub skip_in_flight: bool,
}

impl Sprint {
    pub fn new(target: u64, resume_frozen: bool) -> Self {
        Self {
            target,
            completed: 0,
            busy: Duration::ZERO,
            resume_frozen,
            skip_in_flight: false,
        }
    }

    pub fn skipping_in_flight(mut self, in_flight: bool) -> Self {
        self.skip_in_flight = in_flight;
        self
    }

    /// Records one tick's processing time; `true` once the sprint has run
    /// every requested tick.
    pub fn record(&mut self, duration: Duration) -> bool {
        if std::mem::take(&mut self.skip_in_flight) {
            return false;
        }
        self.completed += 1;
        self.busy += duration;
        self.completed >= self.target
    }
}

/// `/tick sprint`'s completion report: achieved ticks per second and
/// milliseconds per tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SprintReport {
    pub ticks_per_second: i32,
    pub ms_per_tick: f64,
}

pub fn sprint_report(sprint: Sprint, target_period: Duration) -> SprintReport {
    let elapsed_ms = (sprint.busy.as_secs_f64() * 1000.0).max(1.0);
    let ticks_per_second = ((1000.0 * sprint.completed as f64) / elapsed_ms) as i32;
    let ms_per_tick = if sprint.completed == 0 {
        target_period.as_secs_f64() * 1000.0
    } else {
        elapsed_ms / sprint.completed as f64
    };
    SprintReport {
        ticks_per_second,
        ms_per_tick,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn samples_average_and_percentiles_match_a_full_window() {
        let mut samples = Samples::default();
        for ms in 1..=100u64 {
            samples.push(Duration::from_millis(ms));
        }
        assert_eq!(
            samples.average(),
            Duration::from_millis(50) + Duration::from_micros(500)
        );
        let (p50, p95, p99, count) = samples.percentiles();
        assert_eq!(
            (p50, p95, p99, count),
            (
                Duration::from_millis(51),
                Duration::from_millis(96),
                Duration::from_millis(100),
                100,
            )
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn samples_window_drops_the_oldest() {
        let mut samples = Samples::default();
        for ms in 0..150u64 {
            samples.push(Duration::from_millis(ms));
        }
        let (_, _, _, count) = samples.percentiles();
        assert_eq!(count, 100);
        // The oldest 50 (0..50) fell off; the window is 50..150.
        assert_eq!(samples.percentiles().0, Duration::from_millis(100));
    }

    #[tuff_core::watchdog]
    #[test]
    fn sprint_report_matches_completed_ticks_over_busy_time() {
        let mut sprint = Sprint::new(100, false);
        for _ in 0..100 {
            sprint.record(Duration::from_millis(10));
        }
        let report = sprint_report(sprint, TickState::default().period());
        assert_eq!(report.ticks_per_second, 100);
        assert!((report.ms_per_tick - 10.0).abs() < 0.01);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_tick_already_in_flight_is_not_a_sprint_tick() {
        let mut sprint = Sprint::new(2, false).skipping_in_flight(true);
        assert!(!sprint.record(Duration::from_millis(7)));
        assert!(!sprint.record(Duration::from_millis(10)));
        assert!(sprint.record(Duration::from_millis(10)));
        assert_eq!(sprint.completed, 2);
        assert_eq!(sprint.busy, Duration::from_millis(20));
    }

    #[tuff_core::watchdog]
    #[test]
    fn sprint_report_with_no_completed_ticks_uses_the_target_period() {
        let sprint = Sprint::new(10, false);
        let report = sprint_report(sprint, Duration::from_millis(50));
        assert_eq!(report.ticks_per_second, 0);
        assert_eq!(report.ms_per_tick, 50.0);
    }
}

/// A `/tick` subcommand, run by the coordinator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TickAction {
    Query,
    Rate(f32),
    Freeze,
    Unfreeze,
    /// Runs while frozen for this many ticks, then re-freezes.
    Step(u32),
    StepStop,
    /// Runs as fast as possible for this many ticks, then reports.
    Sprint(u32),
    SprintStop,
}

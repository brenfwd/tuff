//! Report-only stall monitor. Actors publish a heartbeat with relaxed
//! atomic stores; a plain thread reads them and logs an actor whose current
//! step has run past [`STALL_THRESHOLD`], repeating every [`STALL_REPEAT`]
//! and logging when it finishes. The thread runs only while a beat exists.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

pub const STALL_THRESHOLD: Duration = Duration::from_secs(10);
pub const STALL_REPEAT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Idle,
    Tick,
    FrozenTick,
    Request,
    Internal,
    Job,
    Message,
    StartTick,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Tick => "tick",
            Phase::FrozenTick => "frozen tick",
            Phase::Request => "request",
            Phase::Internal => "internal message",
            Phase::Job => "job result",
            Phase::Message => "message",
            Phase::StartTick => "start tick",
        }
    }

    fn from_u8(value: u8) -> Self {
        const ALL: [Phase; 8] = [
            Phase::Idle,
            Phase::Tick,
            Phase::FrozenTick,
            Phase::Request,
            Phase::Internal,
            Phase::Job,
            Phase::Message,
            Phase::StartTick,
        ];
        ALL.get(value as usize).copied().unwrap_or(Phase::Idle)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Report {
    Stalled {
        label: String,
        phase: Phase,
        running: Duration,
    },
    Recovered {
        label: String,
    },
}

pub struct Beat {
    label: String,
    epoch: Instant,
    started_ns: AtomicU64,
    phase: AtomicU8,
    reported_start: AtomicU64,
    reported_at: AtomicU64,
}

impl Beat {
    /// Registers an actor; the beat deregisters when dropped.
    pub fn register(label: String) -> Arc<Self> {
        let beat = Arc::new(Self::detached(label, Instant::now()));
        registry().add(&beat);
        beat
    }

    fn detached(label: String, epoch: Instant) -> Self {
        Self {
            label,
            epoch,
            started_ns: AtomicU64::new(0),
            phase: AtomicU8::new(Phase::Idle as u8),
            reported_start: AtomicU64::new(0),
            reported_at: AtomicU64::new(0),
        }
    }

    fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64 + 1
    }

    pub fn begin(&self, phase: Phase) {
        self.phase.store(phase as u8, Relaxed);
        self.started_ns.store(self.now_ns(), Relaxed);
    }

    pub fn end(&self) {
        self.started_ns.store(0, Relaxed);
        self.phase.store(Phase::Idle as u8, Relaxed);
    }

    /// Reports for the beat as seen at `now_ns` (nanoseconds since its epoch).
    fn check(&self, now_ns: u64, threshold: Duration, repeat: Duration) -> Option<Report> {
        let started = self.started_ns.load(Relaxed);
        let reported_start = self.reported_start.load(Relaxed);
        if (started == 0 || started != reported_start) && reported_start != 0 {
            self.reported_start.store(0, Relaxed);
            return Some(Report::Recovered {
                label: self.label.clone(),
            });
        }
        if started == 0 {
            return None;
        }
        let running = now_ns.saturating_sub(started);
        if running < threshold.as_nanos() as u64 {
            return None;
        }
        if started == reported_start
            && now_ns.saturating_sub(self.reported_at.load(Relaxed)) < repeat.as_nanos() as u64
        {
            return None;
        }
        self.reported_start.store(started, Relaxed);
        self.reported_at.store(now_ns, Relaxed);
        Some(Report::Stalled {
            label: self.label.clone(),
            phase: Phase::from_u8(self.phase.load(Relaxed)),
            running: Duration::from_nanos(running),
        })
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        registry().prune();
    }
}

/// Collects the reports due for `beats` at `now_ns`.
pub fn due(beats: &[Arc<Beat>], now_ns: u64, threshold: Duration, repeat: Duration) -> Vec<Report> {
    beats
        .iter()
        .filter_map(|beat| beat.check(now_ns, threshold, repeat))
        .collect()
}

#[derive(Default)]
struct Registry {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    beats: Vec<Weak<Beat>>,
    running: bool,
}

fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn add(&'static self, beat: &Arc<Beat>) {
        let mut state = self.lock();
        state.beats.push(Arc::downgrade(beat));
        if !state.running {
            state.running = true;
            std::thread::Builder::new()
                .name("stall-monitor".into())
                .spawn(move || self.run())
                .expect("spawn the stall monitor");
        }
    }

    fn prune(&self) {
        if let Ok(mut state) = self.state.try_lock() {
            state.beats.retain(|beat| beat.strong_count() > 0);
        }
    }

    fn run(&self) {
        loop {
            std::thread::sleep(POLL);
            let beats: Vec<Arc<Beat>> = {
                let mut state = self.lock();
                state.beats.retain(|beat| beat.strong_count() > 0);
                if state.beats.is_empty() {
                    state.running = false;
                    return;
                }
                state.beats.iter().filter_map(Weak::upgrade).collect()
            };
            for beat in &beats {
                let now = beat.now_ns();
                if let Some(report) = beat.check(now, STALL_THRESHOLD, STALL_REPEAT) {
                    log_report(&report);
                }
            }
        }
    }
}

fn log_report(report: &Report) {
    match report {
        Report::Stalled {
            label,
            phase,
            running,
        } => log::warn!(
            "{label} has been stuck in {} for {:.1}s",
            phase.name(),
            running.as_secs_f64()
        ),
        Report::Recovered { label } => log::warn!("{label} finished the step it was stuck in"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(10);
    const R: Duration = Duration::from_secs(30);

    fn at(beat: &Beat, secs: u64) -> u64 {
        beat.started_ns.load(Relaxed) + secs * 1_000_000_000
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stuck_beat_is_reported_once_per_interval_with_label_and_phase() {
        let beat = Beat::detached("region 0,0".into(), Instant::now());
        beat.begin(Phase::Tick);
        assert_eq!(beat.check(at(&beat, 5), T, R), None);
        let first = beat.check(at(&beat, 11), T, R);
        assert!(matches!(
            first,
            Some(Report::Stalled { ref label, phase: Phase::Tick, .. }) if label == "region 0,0"
        ));
        assert_eq!(beat.check(at(&beat, 20), T, R), None);
        assert!(matches!(
            beat.check(at(&beat, 42), T, R),
            Some(Report::Stalled { .. })
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_finished_beat_is_silent_and_a_reported_one_recovers() {
        let beat = Beat::detached("coordinator".into(), Instant::now());
        beat.begin(Phase::Message);
        let late = at(&beat, 12);
        beat.end();
        assert_eq!(beat.check(late, T, R), None);

        beat.begin(Phase::Request);
        assert!(beat.check(at(&beat, 12), T, R).is_some());
        beat.end();
        assert_eq!(
            beat.check(at(&beat, 13), T, R),
            Some(Report::Recovered {
                label: "coordinator".into()
            })
        );
        assert_eq!(beat.check(at(&beat, 14), T, R), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn due_collects_only_stalled_beats() {
        let epoch = Instant::now();
        let stuck = Arc::new(Beat::detached("a".into(), epoch));
        let idle = Arc::new(Beat::detached("b".into(), epoch));
        stuck.begin(Phase::Job);
        let now = at(&stuck, 15);
        let reports = due(&[stuck, idle], now, T, R);
        assert_eq!(reports.len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn registered_beats_deregister_on_drop() {
        let beat = Beat::register("x".into());
        let weak = Arc::downgrade(&beat);
        assert!(registry().lock().beats.iter().any(|b| b.ptr_eq(&weak)));
        drop(beat);
        registry().prune();
        assert!(!registry().lock().beats.iter().any(|b| b.ptr_eq(&weak)));
    }
}

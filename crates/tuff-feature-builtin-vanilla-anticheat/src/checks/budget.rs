use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Limits for a [`PacketBudget`].
#[derive(Clone, Copy, Debug)]
pub struct BudgetLimits {
    pub per_second: f64,
    pub window: Duration,
    pub tolerance: f64,
    pub slack: f64,
    pub burst_window: Duration,
    pub burst_slack: f64,
    pub max_credit: f64,
}

impl BudgetLimits {
    pub const MOVEMENT: Self = Self {
        per_second: 20.0,
        window: Duration::from_secs(6),
        tolerance: 0.03,
        slack: 4.0,
        burst_window: Duration::from_millis(500),
        burst_slack: 4.0,
        max_credit: 60.0,
    };

    /// New block break starts: the game's client starts at most one block
    /// a client tick. Credit covers about two seconds of stalled sweeping.
    pub const BREAKS: Self = Self {
        per_second: 20.0,
        window: Duration::from_secs(6),
        tolerance: 0.05,
        slack: 6.0,
        burst_window: Duration::from_millis(500),
        burst_slack: 6.0,
        max_credit: 40.0,
    };

    fn interval(&self) -> f64 {
        1.0 / self.per_second
    }
}

const TICK: Duration = Duration::from_millis(50);

/// Wall-clock time that never runs behind the session's ticks, so ticks
/// advanced faster than real time (tests, `/tick sprint`) count as time.
#[derive(Clone, Copy, Debug, Default)]
pub struct TickClock {
    anchor: Option<(Instant, u64)>,
}

impl TickClock {
    pub fn now(&mut self, now: Instant, tick: u64) -> Instant {
        let (at, first) = *self.anchor.get_or_insert((now, tick));
        let ticks = u32::try_from(tick.saturating_sub(first)).unwrap_or(u32::MAX);
        now.max(at + TICK * ticks)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overrun {
    Sustained,
    Burst,
}

/// Counts packets that should arrive at a fixed real-time rate over a
/// rolling window and a short burst window. Gaps longer than one
/// interval earn credit that later early packets spend, so a stall and
/// its catch-up burst add up to real time. A packet that overruns is
/// not counted, since the caller rejects it.
pub struct PacketBudget {
    limits: BudgetLimits,
    counted: VecDeque<Instant>,
    free: VecDeque<Instant>,
    first: Option<Instant>,
    last: Option<Instant>,
    credit: f64,
}

impl PacketBudget {
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            counted: VecDeque::new(),
            free: VecDeque::new(),
            first: None,
            last: None,
            credit: limits.max_credit,
        }
    }

    pub fn credit(&self) -> f64 {
        self.credit
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.limits);
    }

    pub fn record(&mut self, now: Instant) -> Option<Overrun> {
        self.record_scaled(now, 1.0)
    }

    /// Like [`Self::record`], with the slack and credit cap multiplied
    /// by `lag` (at least 1) while the server itself runs behind.
    pub fn record_scaled(&mut self, now: Instant, lag: f64) -> Option<Overrun> {
        let lag = lag.max(1.0);
        let l = self.limits;
        let interval = l.interval();
        let first = *self.first.get_or_insert(now);
        let gap = self.last.map_or(interval, |last| {
            now.saturating_duration_since(last).as_secs_f64()
        });
        self.last = Some(now);
        if gap > interval {
            while self
                .free
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) > l.window)
            {
                self.free.pop_front();
            }
            let pending = (self.counted.len() + self.free.len()) as f64 + 1.0;
            let granted = (1.0 + l.tolerance)
                * now
                    .saturating_duration_since(first)
                    .min(l.window)
                    .as_secs_f64()
                * l.per_second;
            if pending <= granted {
                self.credit = (self.credit + gap / interval - 1.0).min(l.max_credit * lag);
            }
        }
        if gap < interval * 0.5 && self.credit >= 1.0 {
            self.credit -= 1.0;
            self.free.push_back(now);
            return None;
        }
        while self
            .counted
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > l.window)
        {
            self.counted.pop_front();
        }
        let span = now
            .saturating_duration_since(first)
            .min(l.window)
            .as_secs_f64();
        let allowed = (span * l.per_second).mul_add(1.0 + l.tolerance, l.slack * lag);
        if self.counted.len() as f64 + 1.0 > allowed {
            return Some(Overrun::Sustained);
        }
        let burst = self
            .counted
            .iter()
            .rev()
            .take_while(|at| now.saturating_duration_since(**at) <= l.burst_window)
            .count();
        let burst_allowed = (l.burst_window.as_secs_f64() * l.per_second)
            .mul_add(1.0 + l.tolerance, l.burst_slack * lag);
        if burst as f64 + 1.0 > burst_allowed {
            return Some(Overrun::Burst);
        }
        self.counted.push_back(now);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock {
        start: Instant,
        at: Duration,
        budget: PacketBudget,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                at: Duration::ZERO,
                budget: PacketBudget::new(BudgetLimits::MOVEMENT),
            }
        }

        fn send(&mut self, count: u32, every: Duration) -> Vec<(Duration, Overrun)> {
            let mut out = Vec::new();
            for _ in 0..count {
                self.at += every;
                if let Some(o) = self.budget.record(self.start + self.at) {
                    out.push((self.at, o));
                }
            }
            out
        }

        fn stall(&mut self, d: Duration) {
            self.at += d;
        }
    }

    const TICK: Duration = Duration::from_millis(50);

    #[tuff_core::watchdog]
    #[test]
    fn bunched_triplets_above_rate_overrun() {
        let mut c = Clock::new();
        let mut over = false;
        for _ in 0..600 {
            over |= !c.send(1, Duration::from_millis(100)).is_empty();
            over |= !c.send(2, Duration::ZERO).is_empty();
        }
        assert!(over);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nominal_rate_never_overruns() {
        let mut c = Clock::new();
        assert!(c.send(20 * 120, TICK).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lag_spikes_and_catch_up_bursts_never_overrun() {
        let mut c = Clock::new();
        assert!(c.send(100, TICK).is_empty());
        for stall_ms in [200u64, 400, 700, 1000, 2500] {
            c.stall(Duration::from_millis(stall_ms));
            let behind = (stall_ms / 50) as u32;
            assert!(
                c.send(behind, Duration::from_micros(200)).is_empty(),
                "{stall_ms}"
            );
            assert!(c.send(200, TICK).is_empty(), "{stall_ms}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn jitter_without_net_speedup_never_overruns() {
        let mut c = Clock::new();
        for _ in 0..600 {
            assert!(c.send(1, Duration::from_millis(80)).is_empty());
            assert!(c.send(1, Duration::from_millis(20)).is_empty());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reconnect_burst_is_absorbed() {
        let mut c = Clock::new();
        assert!(c.send(40, Duration::from_micros(100)).is_empty());
        assert!(c.send(400, TICK).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn ten_percent_speedup_overruns_within_seconds() {
        let mut c = Clock::new();
        let hits = c.send(22 * 20, Duration::from_micros(1_000_000 / 22));
        let first = hits.first().expect("flagged").0;
        assert!(first < Duration::from_secs(8), "{first:?}");
        assert!(hits.iter().all(|(_, o)| *o == Overrun::Sustained));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flood_trips_the_burst_detector() {
        let mut c = Clock::new();
        c.send(200, TICK);
        c.budget.credit = 0.0;
        let hits = c.send(30, Duration::from_millis(5));
        assert!(hits.iter().any(|(_, o)| *o == Overrun::Burst));
    }

    #[tuff_core::watchdog]
    #[test]
    fn banked_credit_cannot_beat_real_time() {
        let mut c = Clock::new();
        c.send(200, TICK);
        let mut hits = 0;
        for _ in 0..20 {
            c.stall(Duration::from_secs(1));
            hits += c.send(40, Duration::from_millis(1)).len();
            hits += c.send(20, TICK).len();
        }
        assert!(hits > 100, "{hits}");
    }
}

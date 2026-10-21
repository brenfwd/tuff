//! How fast the server really ticks, so a bot can keep its moves within
//! what the server accepts per tick when it falls behind: roughly from
//! the game time its periodic time syncs carry, and exactly from per-tick
//! reports where the server sends them.

use std::time::{Duration, Instant};

/// The tick rate a server aims for.
pub const NORMAL_TPS: f64 = 20.0;

/// The farthest a bot moves per server tick. A server checks each tick's
/// moves against the position it started the tick with, allowing 10
/// blocks per move packet it got that tick, but only 10 in all once more
/// than five packets arrive in one tick. Staying under 10 per server tick
/// leaves margin for ticks that take in more packets than average.
pub const MOVE_PER_SERVER_TICK: f64 = 6.0;

/// The farthest a bot strays from where it was one tick report back. The
/// moves a server checks against one tick's start were all sent after
/// the report before it, and 10 blocks pass however many packets came.
pub const MOVE_PER_TICK_WINDOW: f64 = 9.0;

/// Tick reports older than this no longer limit moves, in case they stop.
const REPORTS_STALE: Duration = Duration::from_secs(5);

/// Syncs closer than this in game time are too short to time the server.
const MIN_SPAN_TICKS: i64 = 10;

/// A sync later than this many intervals means the server has stalled.
const OVERDUE_INTERVALS: f64 = 1.5;
const OVERDUE_SLACK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ServerClock {
    last: Option<(i64, Instant)>,
    rate: Option<f64>,
    interval: Option<Duration>,
}

impl ServerClock {
    /// Records a time sync saying the game time is `age` at `at`.
    pub fn sync(&mut self, age: i64, at: Instant) {
        match self.last {
            Some((last_age, last_at)) => {
                let ticks = age - last_age;
                let elapsed = at.saturating_duration_since(last_at);
                if ticks < MIN_SPAN_TICKS || elapsed.is_zero() {
                    if ticks < 0 {
                        self.last = Some((age, at));
                    }
                    return;
                }
                self.rate = Some(ticks as f64 / elapsed.as_secs_f64());
                self.interval = Some(elapsed);
                self.last = Some((age, at));
            }
            None => self.last = Some((age, at)),
        }
    }

    /// Ticks per second over the last sync interval, once two syncs came.
    pub fn rate(&self) -> Option<f64> {
        self.rate
    }

    /// The share of the normal tick rate the server keeps up: 1 until it
    /// has been timed, 0 while its next sync is overdue.
    pub fn pace(&self, now: Instant) -> f64 {
        let (Some(rate), Some(interval), Some((_, last_at))) =
            (self.rate, self.interval, self.last)
        else {
            return 1.0;
        };
        let overdue = interval.mul_f64(OVERDUE_INTERVALS) + OVERDUE_SLACK;
        if now.saturating_duration_since(last_at) > overdue {
            return 0.0;
        }
        (rate / NORMAL_TPS).clamp(0.0, 1.0)
    }

    /// The farthest one client tick's move may go now.
    pub fn move_allowance(&self, now: Instant) -> f64 {
        MOVE_PER_SERVER_TICK * self.pace(now)
    }
}

/// Where the bot was when each of the server's last two tick reports
/// arrived.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TickWindow {
    anchors: [Option<[f64; 3]>; 2],
    last: Option<Instant>,
}

impl TickWindow {
    /// Records a tick report arriving at `at` with the bot at `position`.
    pub fn report(&mut self, position: [f64; 3], at: Instant) {
        self.anchors = [self.anchors[1], Some(position)];
        self.last = Some(at);
    }

    /// The farthest the bot may move from `position` now: unbounded with
    /// no recent reports.
    pub fn allowance(&self, position: [f64; 3], now: Instant) -> f64 {
        let recent = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) <= REPORTS_STALE);
        let Some(anchor) = self.anchors[0].or(self.anchors[1]).filter(|_| recent) else {
            return f64::INFINITY;
        };
        let strayed = (0..3)
            .map(|i| (position[i] - anchor[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        (MOVE_PER_TICK_WINDOW - strayed).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tick_reports_bound_the_distance_from_two_reports_back() {
        let start = Instant::now();
        let mut window = TickWindow::default();
        assert_eq!(window.allowance([0.0; 3], start), f64::INFINITY);
        window.report([0.0; 3], start);
        assert_eq!(window.allowance([3.0, 0.0, 4.0], start), 4.0);
        window.report([2.0, 0.0, 0.0], start);
        assert_eq!(window.allowance([6.0, 0.0, 0.0], start), 3.0);
        window.report([6.0, 0.0, 0.0], start);
        assert_eq!(window.allowance([6.0, 0.0, 0.0], start), 5.0);
        assert_eq!(window.allowance([20.0, 0.0, 0.0], start), 0.0);
        assert_eq!(
            window.allowance([20.0, 0.0, 0.0], start + Duration::from_secs(6)),
            f64::INFINITY
        );
    }

    fn secs(start: Instant, s: f64) -> Instant {
        start + Duration::from_secs_f64(s)
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_untimed_server_gets_the_full_allowance() {
        let start = Instant::now();
        let mut clock = ServerClock::default();
        assert_eq!(clock.pace(start), 1.0);
        clock.sync(100, start);
        assert_eq!(clock.pace(secs(start, 30.0)), 1.0);
        assert_eq!(clock.move_allowance(start), MOVE_PER_SERVER_TICK);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lagging_server_scales_the_allowance_down() {
        let start = Instant::now();
        let mut clock = ServerClock::default();
        clock.sync(0, start);
        clock.sync(20, secs(start, 1.0));
        assert_eq!(clock.rate(), Some(20.0));
        assert_eq!(clock.pace(secs(start, 1.5)), 1.0);
        clock.sync(40, secs(start, 5.0));
        assert_eq!(clock.rate(), Some(5.0));
        assert!((clock.pace(secs(start, 6.0)) - 0.25).abs() < 1e-9);
        assert!((clock.move_allowance(secs(start, 6.0)) - 1.5).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_overdue_sync_holds_the_bot_still() {
        let start = Instant::now();
        let mut clock = ServerClock::default();
        clock.sync(0, start);
        clock.sync(20, secs(start, 1.0));
        assert_eq!(clock.pace(secs(start, 2.5)), 1.0);
        assert_eq!(clock.pace(secs(start, 2.7)), 0.0);
        clock.sync(40, secs(start, 4.0));
        assert!((clock.pace(secs(start, 4.1)) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn close_or_backward_syncs_do_not_time_the_server() {
        let start = Instant::now();
        let mut clock = ServerClock::default();
        clock.sync(0, start);
        clock.sync(20, secs(start, 1.0));
        clock.sync(23, secs(start, 1.01));
        assert_eq!(clock.rate(), Some(20.0));
        clock.sync(5, secs(start, 1.2));
        clock.sync(25, secs(start, 2.2));
        assert_eq!(clock.rate(), Some(20.0));
        assert_eq!(clock.pace(secs(start, 2.3)), 1.0);
    }
}

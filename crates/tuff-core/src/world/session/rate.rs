use std::time::Duration;
use tokio::time::Instant;

const SMOOTHING: f32 = 0.75;
const WINDOW: Duration = Duration::from_secs(1);

pub(super) struct RateMeter {
    window_start: Instant,
    count: u32,
    average: f32,
}

impl RateMeter {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            count: 0,
            average: 0.0,
        }
    }

    pub(super) fn record(&mut self, now: Instant, limit: u32) -> bool {
        let mut exceeded = false;
        while now.duration_since(self.window_start) >= WINDOW {
            self.window_start += WINDOW;
            self.average = self.count as f32 + SMOOTHING * (self.average - self.count as f32);
            self.count = 0;
            exceeded |= self.average > limit as f32;
        }
        self.count += 1;
        exceeded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_single_burst_is_smoothed_like_vanilla() {
        let start = Instant::now();
        let mut meter = RateMeter::new(start);
        for _ in 0..8 {
            assert!(!meter.record(start, 3));
        }
        assert!(meter.record(start + Duration::from_millis(1100), 1));
        let mut meter = RateMeter::new(start);
        for _ in 0..8 {
            meter.record(start, 3);
        }
        assert!(!meter.record(start + Duration::from_millis(1100), 3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sustained_rate_converges_to_the_limit() {
        let start = Instant::now();
        let mut meter = RateMeter::new(start);
        let mut kicked_at = None;
        for second in 0..40u64 {
            for _ in 0..6 {
                if meter.record(start + Duration::from_secs(second), 5) && kicked_at.is_none() {
                    kicked_at = Some(second);
                }
            }
        }
        assert!(kicked_at.is_some_and(|s| s > 1));
        let mut meter = RateMeter::new(start);
        for second in 0..40u64 {
            for _ in 0..5 {
                assert!(!meter.record(start + Duration::from_secs(second), 5));
            }
        }
    }
}

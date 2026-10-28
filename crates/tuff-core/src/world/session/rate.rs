use std::time::Duration;
use tokio::time::Instant;

use crate::protocol::play::Serverbound;

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

/// Token bucket: `capacity` tokens, refilled at `per_second`.
pub(super) struct Budget {
    capacity: f32,
    per_second: f32,
    tokens: f32,
    last: Instant,
}

impl Budget {
    pub(super) fn new(capacity: u32, per_second: u32, now: Instant) -> Self {
        Self {
            capacity: capacity as f32,
            per_second: per_second as f32,
            tokens: capacity as f32,
            last: now,
        }
    }

    pub(super) fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f32();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Per-type budgets for packets that are expensive to handle; the excess
/// is dropped without a kick.
pub(super) struct Budgets {
    recipes: Budget,
    suggestions: Budget,
    books: Budget,
    signs: Budget,
}

impl Budgets {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            recipes: Budget::new(40, 20, now),
            suggestions: Budget::new(40, 20, now),
            books: Budget::new(4, 2, now),
            signs: Budget::new(8, 4, now),
        }
    }

    pub(super) fn admit(&mut self, packet: &Serverbound, now: Instant) -> bool {
        let budget = match packet {
            Serverbound::PlaceRecipe { .. } => &mut self.recipes,
            Serverbound::CommandSuggestion { .. } => &mut self.suggestions,
            Serverbound::EditBook { .. } => &mut self.books,
            Serverbound::SignUpdate { .. } => &mut self.signs,
            _ => return true,
        };
        budget.take(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_budget_allows_a_burst_refills_and_then_drops() {
        let start = Instant::now();
        let mut budget = Budget::new(4, 2, start);
        for _ in 0..4 {
            assert!(budget.take(start));
        }
        assert!(!budget.take(start));
        assert!(budget.take(start + Duration::from_millis(600)));
        assert!(!budget.take(start + Duration::from_millis(600)));
        for _ in 0..4 {
            assert!(budget.take(start + Duration::from_secs(60)));
        }
        assert!(!budget.take(start + Duration::from_secs(60)));
    }

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

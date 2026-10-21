//! The improbable score: flags from several lenient checks add up in one
//! per-player score kept in short time buckets, so a client that stays
//! just under each limit but trips many of them now and then still
//! escalates. Only the score's own `improbable` check climbs the ladder
//! for a feed-only check.

use std::time::Instant;

pub const CHECK: &str = "improbable";

const BUCKET_TICKS: u64 = 100;
const BUCKETS: usize = 12;
/// Score in the current bucket above which the player is flagged.
pub const SHORT_LIMIT: f64 = 6.0;
/// Score over all buckets above which the player is flagged.
pub const TOTAL_LIMIT: f64 = 24.0;
/// Per-tick factor the improbable ladder level shrinks by when quiet.
pub const RELAX: f64 = 0.995;
const AFTER_FLAG: f64 = 0.5;
const LAG_WINDOW: u64 = 20;
const MAX_LAG: f64 = 4.0;

/// The checks that feed the score, with the weight of one of their flags.
pub const FEEDERS: &[(&str, f64)] = &[
    ("no_slow", 1.0),
    ("sprint", 0.5),
    ("place_rate", 0.5),
    ("scaffold", 1.0),
    ("click_rate", 0.5),
    ("attack_rate", 1.0),
    ("reach", 1.0),
    ("wrong_block", 1.0),
    ("fast_bow", 1.0),
    ("velocity", 1.0),
    ("aim", 1.0),
    ("multi_aura", 1.0),
    ("fast_break", 1.0),
    ("rotation", 0.5),
];

/// Feeders that only feed the score unless configured otherwise.
const FEED_ONLY: &[&str] = &["rotation"];

pub fn feeds_only_by_default(check: &str) -> bool {
    FEED_ONLY.contains(&check)
}

pub fn weight_of(check: &str) -> Option<f64> {
    FEEDERS
        .iter()
        .find(|(name, _)| *name == check)
        .map(|(_, weight)| *weight)
}

/// Fraction of a check's limit where a measurement starts to look odd.
pub const WEAK_FROM: f64 = 0.8;
/// Rotation change per tick (degrees) beyond which a turn is a weak signal.
pub const ROTATION_RATE_LIMIT: f64 = 180.0;

pub fn weak_weight(value: f64, limit: f64, max_weight: f64) -> f64 {
    if limit <= 0.0 || !value.is_finite() {
        return 0.0;
    }
    let ratio = value / limit;
    if !(WEAK_FROM..1.0).contains(&ratio) {
        return 0.0;
    }
    max_weight * (ratio - WEAK_FROM) / (1.0 - WEAK_FROM)
}

/// Weak weight of one tick's yaw and pitch change; snaps above the limit
/// are left to the aim check.
pub fn rotation_rate_weight(delta_yaw: f64, delta_pitch: f64) -> f64 {
    let yaw = delta_yaw.abs() % 360.0;
    let yaw = yaw.min(360.0 - yaw);
    weak_weight(yaw.hypot(delta_pitch.abs()), ROTATION_RATE_LIMIT, 0.5)
}

#[derive(Debug)]
pub struct Improbable {
    buckets: [f64; BUCKETS],
    /// The bucket index (in ticks / `BUCKET_TICKS`) of the newest bucket.
    current: u64,
    window: Option<(u64, Instant)>,
    lag: f64,
}

impl Default for Improbable {
    fn default() -> Self {
        Self {
            buckets: [0.0; BUCKETS],
            current: 0,
            window: None,
            lag: 1.0,
        }
    }
}

impl Improbable {
    fn advance(&mut self, tick: u64) {
        let index = tick / BUCKET_TICKS;
        let elapsed = index.saturating_sub(self.current);
        for step in 1..=elapsed.min(BUCKETS as u64) {
            self.buckets[((self.current + step) % BUCKETS as u64) as usize] = 0.0;
        }
        self.current = self.current.max(index);
    }

    /// Tracks how far session ticks fall behind the wall clock, so a
    /// lagging server (which bunches packets) does not inflate the score.
    pub fn observe(&mut self, tick: u64, now: Instant, tick_rate: f32) {
        let Some((start, at)) = self.window else {
            self.window = Some((tick, now));
            return;
        };
        let ticks = tick.saturating_sub(start);
        if ticks < LAG_WINDOW {
            return;
        }
        let expected =
            now.saturating_duration_since(at).as_secs_f64() * f64::from(tick_rate.clamp(0.1, 20.0));
        self.lag = (expected / ticks as f64).clamp(1.0, MAX_LAG);
        self.window = Some((tick, now));
    }

    /// Adds a feeder's flag; true when the score is now improbable.
    pub fn feed(&mut self, tick: u64, weight: f64) -> bool {
        self.advance(tick);
        let slot = (self.current % BUCKETS as u64) as usize;
        self.buckets[slot] += weight;
        let short = self.buckets[slot] / self.lag;
        let total = self.buckets.iter().sum::<f64>() / self.lag;
        if short > SHORT_LIMIT || total > TOTAL_LIMIT {
            for bucket in &mut self.buckets {
                *bucket *= AFTER_FLAG;
            }
            true
        } else {
            false
        }
    }

    /// Adds a sub-threshold signal: `value` is how close a measurement is
    /// to its flag `limit`; only the part above [`WEAK_FROM`] of the limit
    /// counts, scaled up to `max_weight` at the limit.
    pub fn feed_weak(&mut self, tick: u64, value: f64, limit: f64, max_weight: f64) -> bool {
        let weight = weak_weight(value, limit, max_weight);
        if weight <= 0.0 {
            self.advance(tick);
            return false;
        }
        self.feed(tick, weight)
    }

    pub fn total(&mut self, tick: u64) -> f64 {
        self.advance(tick);
        self.buckets.iter().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_burst_of_weak_signals_flags_but_the_same_spread_out_does_not() {
        let mut score = Improbable::default();
        let flagged = (0..10).filter(|n| score.feed(1000 + n, 1.0)).count();
        assert!(flagged >= 1);
        let mut score = Improbable::default();
        let flagged = (0..100).filter(|n| score.feed(n * 300, 1.0)).count();
        assert_eq!(flagged, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_total_over_a_minute_flags_a_steady_drip() {
        let mut score = Improbable::default();
        let flagged = (0..60).filter(|n| score.feed(n * 20, 0.6)).count();
        assert!(flagged >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn old_buckets_drop_out_after_a_minute() {
        let mut score = Improbable::default();
        for n in 0..5 {
            score.feed(n, 1.0);
        }
        assert_eq!(score.total(5), 5.0);
        assert_eq!(score.total(BUCKET_TICKS * BUCKETS as u64 + 10), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn weak_signals_only_count_near_the_limit() {
        assert_eq!(weak_weight(0.5, 1.0, 1.0), 0.0);
        assert_eq!(weak_weight(1.0, 1.0, 1.0), 0.0);
        assert!((weak_weight(0.9, 1.0, 1.0) - 0.5).abs() < 1e-9);
        let mut score = Improbable::default();
        assert!(!score.feed_weak(10, 0.5, 1.0, 1.0));
        assert_eq!(score.total(10), 0.0);
        let flagged = (0..40)
            .filter(|n| score.feed_weak(20 + n, 0.99, 1.0, 1.0))
            .count();
        assert!(flagged >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotation_rate_wraps_yaw() {
        assert_eq!(rotation_rate_weight(10.0, 5.0), 0.0);
        assert!(rotation_rate_weight(359.0, 0.0) == 0.0);
        assert!(rotation_rate_weight(160.0, 0.0) > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn server_lag_divides_the_score() {
        let mut score = Improbable::default();
        let start = Instant::now();
        score.observe(0, start, 20.0);
        score.observe(40, start + std::time::Duration::from_secs(6), 20.0);
        let flagged = (0..10).filter(|n| score.feed(1000 + n, 1.0)).count();
        assert_eq!(flagged, 0);
    }
}

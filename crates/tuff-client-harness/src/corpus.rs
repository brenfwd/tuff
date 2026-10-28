//! Deterministic variation for repeated movement corpus recordings: a seed
//! and a round pick the lane order, tick counts and start offsets, and the
//! fixture file name. Seed 0 round 0 reproduces the original lanes exactly.

/// A splitmix64 stream; small, dependency free and stable across platforms.
pub struct Variation {
    state: u64,
    identity: bool,
}

impl Variation {
    pub fn new(seed: u64, round: u32) -> Self {
        let state = seed ^ u64::from(round).wrapping_mul(0xA24B_AED4_963E_E407);
        Self {
            state,
            identity: state == 0,
        }
    }

    /// Seed 0 round 0 changes nothing.
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[-1, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// A permutation of `0..n`; identity for the identity variation.
    pub fn order(&mut self, n: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..n).collect();
        if self.is_identity() {
            return order;
        }
        for i in (1..n).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        order
    }

    /// `ticks` moved by up to `pct` percent either way, at least 1.
    pub fn ticks(&mut self, ticks: u32, pct: u32) -> u32 {
        if self.is_identity() {
            return ticks;
        }
        let span = i64::from(ticks) * i64::from(pct) / 100;
        let delta = (self.next() % (2 * span as u64 + 1)) as i64 - span;
        (i64::from(ticks) + delta).max(1) as u32
    }

    /// A value in `[-max, max)`; zero for the identity variation.
    pub fn offset(&mut self, max: f64) -> f64 {
        if self.is_identity() {
            return 0.0;
        }
        self.unit() * max
    }
}

/// `movement_corpus[_special][_rtt<ms>][_s<seed>].txt`.
pub fn file_name(stem: &str, rtt_ms: u64, seed: u64) -> String {
    let mut name = stem.to_owned();
    if rtt_ms != 0 {
        name.push_str(&format!("_rtt{rtt_ms}"));
    }
    if seed != 0 {
        name.push_str(&format!("_s{seed}"));
    }
    name.push_str(".txt");
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn corpus_seed_zero_keeps_lane_order() {
        let mut v = Variation::new(0, 0);
        assert_eq!(v.order(17), (0..17).collect::<Vec<_>>());
        assert_eq!(v.ticks(80, 20), 80);
        assert_eq!(v.offset(0.3), 0.0);
        assert_eq!(file_name("movement_corpus", 0, 0), "movement_corpus.txt");
        assert_eq!(
            file_name("movement_corpus_special", 250, 3),
            "movement_corpus_special_rtt250_s3.txt"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn corpus_seed_variation_is_deterministic_and_bounded() {
        let run = |seed, round| {
            let mut v = Variation::new(seed, round);
            let order = v.order(17);
            let ticks: Vec<u32> = (0..50).map(|_| v.ticks(80, 20)).collect();
            let offs: Vec<f64> = (0..50).map(|_| v.offset(0.3)).collect();
            (order, ticks, offs)
        };
        let a = run(7, 2);
        assert_eq!(a, run(7, 2));
        assert_ne!(a.0, run(7, 3).0);
        assert_ne!(run(0, 1).0, (0..17).collect::<Vec<_>>());
        let mut sorted = a.0.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..17).collect::<Vec<_>>());
        assert!(a.1.iter().all(|t| (64..=96).contains(t)));
        assert!(a.1.iter().any(|&t| t != 80));
        assert!(a.2.iter().all(|o| (-0.3..0.3).contains(o)));
    }
}

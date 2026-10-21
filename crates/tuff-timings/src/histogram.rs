//! Log-linear duration buckets: exact below 16 ns, then eight buckets per
//! power of two (at most 12.5% wide), clamped at 2^40 ns.

pub const BUCKETS: usize = LINEAR as usize + OCTAVES * SUB as usize;

const LINEAR: u64 = 16;
const SUB_BITS: u32 = 3;
const SUB: u64 = 1 << SUB_BITS;
const FIRST_OCTAVE: u32 = LINEAR.trailing_zeros();
const OCTAVES: usize = 40 - FIRST_OCTAVE as usize;

#[inline]
pub fn bucket(ns: u64) -> usize {
    if ns < LINEAR {
        return ns as usize;
    }
    let msb = 63 - ns.leading_zeros();
    let sub = (ns >> (msb - SUB_BITS)) & (SUB - 1);
    let index = LINEAR as usize + (msb - FIRST_OCTAVE) as usize * SUB as usize + sub as usize;
    index.min(BUCKETS - 1)
}

/// The smallest duration in bucket `index` and the bucket's width.
pub fn bounds(index: usize) -> (u64, u64) {
    if index < LINEAR as usize {
        return (index as u64, 1);
    }
    let offset = index - LINEAR as usize;
    let msb = FIRST_OCTAVE + (offset / SUB as usize) as u32;
    let sub = (offset % SUB as usize) as u64;
    let width = 1u64 << (msb - SUB_BITS);
    ((SUB + sub) * width, width)
}

/// The `q` quantile (0..=1) of the counted durations, as the midpoint of
/// the bucket holding it, kept within the observed `min..=max`.
pub fn quantile(counts: &[u32], q: f64, min: u64, max: u64) -> u64 {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    if total == 0 {
        return 0;
    }
    let rank = ((q.clamp(0.0, 1.0) * total as f64).ceil() as u64).max(1);
    let mut seen = 0;
    for (index, &count) in counts.iter().enumerate() {
        seen += u64::from(count);
        if seen >= rank {
            let (low, width) = bounds(index);
            return (low + width / 2).clamp(min, max);
        }
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_duration_lands_in_the_bucket_that_bounds_it() {
        for ns in (0..5000).chain([1 << 20, (1 << 30) + 12345, (1 << 39) + 7]) {
            let (low, width) = bounds(bucket(ns));
            assert!(low <= ns && ns < low + width, "{ns} in {low}+{width}");
        }
        assert_eq!(bucket(u64::MAX), BUCKETS - 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn buckets_are_at_most_an_eighth_of_their_start_wide() {
        for index in LINEAR as usize..BUCKETS {
            let (low, width) = bounds(index);
            assert!(width * 8 <= low, "bucket {index}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn quantiles_come_from_the_ranked_bucket() {
        let mut counts = vec![0u32; BUCKETS];
        for ns in 1..=100u64 {
            counts[bucket(ns * 1000)] += 1;
        }
        let p50 = quantile(&counts, 0.5, 1000, 100_000);
        assert!((46_000..=54_000).contains(&p50), "{p50}");
        assert_eq!(quantile(&counts, 1.0, 1000, 100_000), 100_000);
        assert_eq!(quantile(&counts, 0.0, 1000, 100_000), 1000);
    }
}

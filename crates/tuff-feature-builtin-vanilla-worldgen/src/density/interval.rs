//! Float value ranges of density functions. The game uses them to skip
//! work (a `min` whose sides cannot overlap never evaluates one of them), so
//! they are computed exactly as the game computes them.

pub fn java_min(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && b.is_sign_negative() {
        return b;
    }
    if a <= b { a } else { b }
}

pub fn java_max(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && a.is_sign_negative() {
        return b;
    }
    if a >= b { a } else { b }
}

pub fn java_signum(a: f32) -> f32 {
    if a == 0.0 || a.is_nan() {
        a
    } else {
        1f32.copysign(a)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub min: f32,
    pub max: f32,
}

impl Interval {
    pub const NAI: Interval = Interval {
        min: f32::NAN,
        max: f32::NAN,
    };
    pub const INFINITE: Interval = Interval {
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
    };

    pub fn of(min: f32, max: f32) -> Self {
        assert!(
            max >= min && !min.is_nan() && !max.is_nan(),
            "bad interval [{min}; {max}]"
        );
        Self { min, max }
    }

    pub fn symmetric(range: f32) -> Self {
        Self::of(-range, range)
    }

    pub fn exact(value: f32) -> Self {
        Self::of(value, value)
    }

    pub fn is_nai(&self) -> bool {
        self.min.is_nan()
    }

    pub fn contains(&self, value: f32) -> bool {
        value >= self.min && value <= self.max
    }

    pub fn encapsulating(intervals: &[Interval]) -> Self {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for interval in intervals {
            if !interval.is_nai() {
                min = java_min(interval.min, min);
                max = java_max(interval.max, max);
            }
        }
        if max < min {
            return Self::NAI;
        }
        Self::of(min, max)
    }

    pub fn encapsulating_values(first: f32, second: f32) -> Self {
        match (first.is_nan(), second.is_nan()) {
            (true, true) => Self::NAI,
            (true, false) => Self::exact(second),
            (false, true) => Self::exact(first),
            _ => Self::of(java_min(first, second), java_max(first, second)),
        }
    }

    fn encapsulating_value(first: Interval, second: f32) -> Self {
        if second.is_nan() {
            return first;
        }
        if first.is_nai() {
            return Self::exact(second);
        }
        Self::of(java_min(first.min, second), java_max(first.max, second))
    }

    pub fn sum(left: Interval, right: Interval) -> Self {
        let min = left.min + right.min;
        let max = left.max + right.max;
        if min.is_nan() || max.is_nan() {
            return Self::NAI;
        }
        Self::of(min, max)
    }

    pub fn difference(left: Interval, right: Interval) -> Self {
        let min = left.min - right.max;
        let max = left.max - right.min;
        if min.is_nan() || max.is_nan() {
            return Self::NAI;
        }
        Self::of(min, max)
    }

    fn mul_bound(left: f32, right: f32) -> f32 {
        if left == 0.0 || right == 0.0 {
            0.0
        } else {
            left * right
        }
    }

    pub fn product(left: Interval, right: Interval) -> Self {
        if left.is_nai() || right.is_nai() {
            return Self::NAI;
        }
        let a = Self::mul_bound(left.min, right.min);
        let b = Self::mul_bound(left.min, right.max);
        let c = Self::mul_bound(left.max, right.min);
        let d = Self::mul_bound(left.max, right.max);
        Self::of(
            java_min(java_min(a, b), java_min(c, d)),
            java_max(java_max(a, b), java_max(c, d)),
        )
    }

    pub fn reciprocal(input: Interval) -> Self {
        if input.is_nai() || (input.min == 0.0 && input.max == 0.0) {
            return Self::NAI;
        }
        if !input.contains(0.0) {
            return Self::of(1.0 / input.max, 1.0 / input.min);
        }
        if input.max == 0.0 {
            return Self::of(f32::NEG_INFINITY, 1.0 / input.min);
        }
        if input.min == 0.0 {
            return Self::of(1.0 / input.max, f32::INFINITY);
        }
        Self::INFINITE
    }

    pub fn quotient(left: Interval, right: Interval) -> Self {
        Self::product(left, Self::reciprocal(right))
    }

    pub fn min(left: Interval, right: Interval) -> Self {
        if left.is_nai() || right.is_nai() {
            return Self::NAI;
        }
        Self::of(java_min(left.min, right.min), java_min(left.max, right.max))
    }

    pub fn max(left: Interval, right: Interval) -> Self {
        if left.is_nai() || right.is_nai() {
            return Self::NAI;
        }
        Self::of(java_max(left.min, right.min), java_max(left.max, right.max))
    }

    pub fn clamp(input: Interval, min: f32, max: f32) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        if input.min >= max {
            return Self::of(max, max);
        }
        if input.max <= min {
            return Self::of(min, min);
        }
        Self::of(java_max(input.min, min), java_min(input.max, max))
    }

    pub fn abs(input: Interval) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let max = java_max(input.min.abs(), input.max.abs());
        if input.contains(0.0) {
            return Self::of(0.0, max);
        }
        Self::of(java_min(input.min.abs(), input.max.abs()), max)
    }

    pub fn square(input: Interval) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let (a, b) = (input.min * input.min, input.max * input.max);
        let max = java_max(a, b);
        if input.contains(0.0) {
            return Self::of(0.0, max);
        }
        Self::of(java_min(a, b), max)
    }

    pub fn map_monotonic(input: Interval, op: impl Fn(f32) -> f32) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        let (a, b) = (op(input.min), op(input.max));
        assert!(
            !a.is_nan() && !b.is_nan(),
            "monotonic operator produced NaN"
        );
        Self::of(java_min(a, b), java_max(a, b))
    }

    pub fn pow(base: Interval, exponent: Interval) -> Self {
        if base.is_nai() || exponent.is_nai() {
            return Self::NAI;
        }
        let mut hull = Self::raise_point(base.min, exponent);
        if base.max != base.min {
            hull = Self::encapsulating(&[hull, Self::raise_point(base.max, exponent)]);
            let straddles_zero = base.min <= 0.0 && base.max >= 0.0;
            for zero in [0.0f32, -0.0] {
                let reaches_side = if zero.is_sign_positive() {
                    base.max > 0.0
                } else {
                    base.min < 0.0
                };
                if straddles_zero && reaches_side {
                    hull = Self::encapsulating(&[hull, Self::raise_point(zero, exponent)]);
                }
            }
        }
        hull
    }

    fn raise(base: f32, exponent: f32) -> f32 {
        (base as f64).powf(exponent as f64) as f32
    }

    fn raise_point(base: f32, exponent: Interval) -> Self {
        if base.is_nan() {
            return Self::NAI;
        }
        let (lo, hi) = (exponent.min, exponent.max);
        if lo == hi {
            let single = Self::raise(base, lo);
            return if single.is_nan() {
                Self::NAI
            } else {
                Self::exact(single)
            };
        }
        match base {
            b if b == 0.0 => {
                let magnitude = match (lo < 0.0, hi > 0.0, lo == 0.0, hi == 0.0) {
                    _ if hi < 0.0 => Self::exact(f32::INFINITY),
                    _ if lo > 0.0 => Self::exact(0.0),
                    (_, _, _, true) => Self::of(1.0, f32::INFINITY),
                    (_, _, true, _) => Self::of(0.0, 1.0),
                    _ => Self::of(0.0, f32::INFINITY),
                };
                Self::product(magnitude, Self::exact(1f32.copysign(b)))
            }
            1.0 => Self::exact(1.0),
            b if b > 0.0 => Self::raise_positive(b, lo, hi),
            b => Self::raise_negative(b, lo, hi),
        }
    }

    fn raise_positive(base: f32, lo: f32, hi: f32) -> Self {
        let shrinks = base < 1.0;
        match (lo.is_infinite(), hi.is_infinite()) {
            (false, false) => {
                Self::encapsulating_values(Self::raise(base, lo), Self::raise(base, hi))
            }
            (true, true) => Self::of(0.0, f32::INFINITY),
            (true, false) => {
                let edge = Self::raise(base, hi);
                if shrinks {
                    Self::of(edge, f32::INFINITY)
                } else {
                    Self::of(0.0, edge)
                }
            }
            (false, true) => {
                let edge = Self::raise(base, lo);
                if shrinks {
                    Self::of(0.0, edge)
                } else {
                    Self::of(edge, f32::INFINITY)
                }
            }
        }
    }

    fn raise_negative(base: f32, lo: f32, hi: f32) -> Self {
        let first = (lo as f64).ceil() as f32;
        let last = (hi as f64).floor() as f32;
        if first > last {
            return Self::NAI;
        }
        let (at_first, at_last) = (Self::raise(base, first), Self::raise(base, last));
        let neighbour = |end: f32, value: f32, inner: f32, room: bool| {
            if end.is_infinite() {
                -value
            } else if room {
                Self::raise(base, inner)
            } else {
                f32::NAN
            }
        };
        let extras = [
            neighbour(first, at_first, first + 1.0, first + 1.0 < last),
            neighbour(last, at_last, last - 1.0, last - 1.0 > first),
        ];
        extras.into_iter().fold(
            Self::encapsulating_values(at_first, at_last),
            Self::encapsulating_value,
        )
    }

    pub fn log(input: Interval) -> Self {
        if input.max < 0.0 {
            return Self::NAI;
        }
        let clipped = Self::max(input, Self::exact(0.0));
        Self::map_monotonic(clipped, |x| (x as f64).ln() as f32)
    }

    pub fn sign(input: Interval) -> Self {
        if input.is_nai() {
            return Self::NAI;
        }
        if input.min == input.max {
            return Self::exact(java_signum(input.min));
        }
        if !input.contains(0.0) {
            return Self::exact(if input.min > 0.0 { 1.0 } else { -1.0 });
        }
        if input.min == 0.0 {
            return Self::of(0.0, 1.0);
        }
        if input.max == 0.0 {
            return Self::of(-1.0, 0.0);
        }
        Self::of(-1.0, 1.0)
    }

    pub fn lerp(alpha: Interval, first: Interval, second: Interval) -> Self {
        if alpha.is_nai() || first.is_nai() || second.is_nai() {
            return Self::NAI;
        }
        Self::encapsulating(&[
            Self::lerp_values(alpha, first.min, second.min),
            Self::lerp_values(alpha, first.max, second.min),
            Self::lerp_values(alpha, first.min, second.max),
            Self::lerp_values(alpha, first.max, second.max),
        ])
    }

    fn lerp_values(alpha: Interval, first: f32, second: f32) -> Self {
        if alpha.is_nai() || first.is_nan() || second.is_nan() {
            return Self::NAI;
        }
        if first.is_finite() && second.is_finite() {
            let bound = |a: f32| first + Self::mul_bound(a, second - first);
            return Self::encapsulating_values(bound(alpha.min), bound(alpha.max));
        }
        if first == second {
            return Self::exact(first);
        }
        let bound = |a: f32| {
            let first_part = Self::mul_bound(1.0 - a, first);
            let second_part = Self::mul_bound(a, second);
            if !first_part.is_infinite() || !second_part.is_infinite() {
                return first_part + second_part;
            }
            if a <= 0.0 {
                return if second > first {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                };
            }
            if a >= 1.0 {
                return if second > first {
                    f32::INFINITY
                } else {
                    f32::NEG_INFINITY
                };
            }
            f32::NAN
        };
        let (min, max) = (bound(alpha.min), bound(alpha.max));
        if min.is_nan() || max.is_nan() {
            return Self::NAI;
        }
        Self::encapsulating_values(min, max)
    }
}

#[cfg(test)]
mod pow_tests {
    use super::Interval;

    const SAMPLES: [f32; 15] = [
        f32::NEG_INFINITY,
        -3.5,
        -2.0,
        -1.0,
        -0.5,
        -0.0,
        0.0,
        0.25,
        0.5,
        1.0,
        1.5,
        2.0,
        3.0,
        7.25,
        f32::INFINITY,
    ];

    fn intervals() -> Vec<Interval> {
        let mut out = vec![Interval::NAI];
        for (i, &a) in SAMPLES.iter().enumerate() {
            for &b in &SAMPLES[i..] {
                out.push(Interval::of(a, b));
            }
        }
        out
    }

    fn fingerprint() -> u64 {
        let all = intervals();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            hash ^= v;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        };
        for &base in &all {
            for &exponent in &all {
                let outcome = std::panic::catch_unwind(|| Interval::pow(base, exponent));
                match outcome {
                    Ok(r) if r.is_nai() => mix(1),
                    Ok(r) => {
                        mix(r.min.to_bits() as u64);
                        mix(r.max.to_bits() as u64);
                    }
                    Err(_) => mix(2),
                }
            }
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn pow_matches_pinned_fingerprint() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let print = fingerprint();
        std::panic::set_hook(hook);
        assert_eq!(print, PINNED, "{print:#x}");
    }

    const PINNED: u64 = 0x4ee6_03ba_13c2_9151;

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing run, use --release --ignored"]
    fn pow_timing() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let all = intervals();
        let pairs: Vec<(Interval, Interval)> = all
            .iter()
            .flat_map(|&b| all.iter().map(move |&e| (b, e)))
            .filter(|&(b, e)| std::panic::catch_unwind(|| Interval::pow(b, e)).is_ok())
            .collect();
        std::panic::set_hook(hook);
        let start = std::time::Instant::now();
        let mut acc = 0f32;
        for _ in 0..2000 {
            for &(base, exponent) in &pairs {
                let r = Interval::pow(std::hint::black_box(base), exponent);
                if !r.is_nai() {
                    acc += r.min.max(-1.0);
                }
            }
        }
        eprintln!("pow_timing {:?} {acc}", start.elapsed());
    }
}

//! The density function tree as data. Named functions stay behind
//! [`DensityFunction::Reference`] until compilation inlines them, so shared
//! subtrees (and the caches on them) keep their identity like the game's
//! registry holders do.

use std::fmt;
use std::sync::Arc;

use super::Interval;
use super::sampler::Sampler;
use crate::normal_noise::NormalNoise;

pub type Df = Arc<DensityFunction>;

pub const AXIS_X: u8 = 1;
pub const AXIS_Y: u8 = 2;
pub const AXIS_Z: u8 = 4;
pub const ALL_AXES: u8 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn bit(self) -> u8 {
        match self {
            Axis::X => AXIS_X,
            Axis::Y => AXIS_Y,
            Axis::Z => AXIS_Z,
        }
    }

    pub fn choose(self, x: i32, y: i32, z: i32) -> i32 {
        match self {
            Axis::X => x,
            Axis::Y => y,
            Axis::Z => z,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tiling {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

impl DistanceMetric {
    pub fn compute(self, dx: f32, dy: f32, dz: f32) -> f32 {
        match self {
            DistanceMetric::Euclidean => ((dx * dx + dy * dy + dz * dz) as f64).sqrt() as f32,
            DistanceMetric::EuclideanSquared => dx * dx + dy * dy + dz * dz,
            DistanceMetric::Manhattan => dx.abs() + dy.abs() + dz.abs(),
            DistanceMetric::Chebyshev => {
                super::java_max(super::java_max(dx.abs(), dy.abs()), dz.abs())
            }
        }
    }
}

/// A named noise definition, identified by its registry name.
pub struct NoiseHolder {
    pub name: String,
    pub noise: NormalNoise,
}

impl fmt::Debug for NoiseHolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "noise:{}", self.name)
    }
}

/// A named density function (a registry entry).
pub struct NamedFunction {
    pub name: String,
    pub function: Df,
}

impl fmt::Debug for NamedFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ref:{}", self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Abs,
    Square,
    Cube,
    Sqrt,
    HalfNegative,
    QuarterNegative,
    Reciprocal,
    Negate,
    Squeeze,
    Log,
    Sign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Pow,
    Round(RoundMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundMode {
    Floor,
    Round,
    Ceil,
    Truncate,
}

impl RoundMode {
    pub fn apply(self, v: f32) -> f32 {
        match self {
            RoundMode::Floor => v.floor(),
            RoundMode::Round => {
                if v.is_nan() {
                    0.0
                } else {
                    (v + 0.5).floor().clamp(i32::MIN as f32, i32::MAX as f32)
                }
            }
            RoundMode::Ceil => v.ceil(),
            RoundMode::Truncate => {
                if v > 0.0 {
                    v.floor()
                } else {
                    v.ceil()
                }
            }
        }
    }

    pub fn round_to_multiple(self, input: f32, multiple: f32) -> f32 {
        if multiple == 0.0 {
            input
        } else {
            self.apply(input / multiple) * multiple
        }
    }
}

/// Keys into the sampler context a plugin or the generator can bind (the
/// structure beardifier, old-chunk blending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextKey {
    BlendAlpha,
    BlendOffset,
    Beardifier,
}

#[derive(Debug, Clone)]
pub enum Spline {
    Constant(f32),
    Multipoint {
        coordinate: Df,
        locations: Vec<f32>,
        values: Vec<Spline>,
        derivatives: Vec<f32>,
    },
}

/// A density function kind supplied from outside this module (a plugin).
pub trait CustomDensity: Send + Sync + fmt::Debug {
    fn range(&self) -> Interval;
    fn domain_axes(&self) -> u8;
    fn compile(&self) -> Sampler;
}

#[derive(Debug)]
pub enum DensityFunction {
    Constant(f32),
    Reference(Arc<NamedFunction>),
    Noise {
        noise: Arc<NoiseHolder>,
        xz_scale: f64,
        y_scale: f64,
        shift_x: Df,
        shift_y: Df,
        shift_z: Df,
    },
    ShiftA(Arc<NoiseHolder>),
    ShiftB(Arc<NoiseHolder>),
    Shift(Arc<NoiseHolder>),
    EndIslands,
    DistanceToPoint {
        point: [i32; 3],
        metric: DistanceMetric,
    },
    Gradient {
        axis: Axis,
        tiling: Tiling,
        from_coordinate: i32,
        to_coordinate: i32,
        from_value: f32,
        to_value: f32,
    },
    Unary(UnaryOp, Df),
    Binary(BinaryOp, Df, Df),
    Spline(Arc<Spline>),
    Lerp {
        alpha: Df,
        first: Df,
        second: Df,
    },
    Clamp {
        input: Df,
        min: f32,
        max: f32,
    },
    RangeChoice {
        input: Df,
        min_inclusive: f32,
        max_exclusive: f32,
        when_in_range: Df,
        when_out_of_range: Df,
    },
    IntervalSelect {
        input: Df,
        thresholds: Vec<f32>,
        functions: Vec<Df>,
    },
    Cache(Df),
    BlendDensity(Df),
    Interpolated {
        input: Df,
        cell_xz: i32,
        cell_y: i32,
    },
    Slice {
        axis: Axis,
        coordinate: i32,
        input: Df,
    },
    FindTopSurface {
        density: Df,
        upper_bound: Df,
        lower_bound: i32,
        cell_height: i32,
    },
    OldBlendedNoise {
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    },
    ContextBound(ContextKey),
    Custom(Arc<dyn CustomDensity>),
    /// A deduplicated cache, only present in compiled trees.
    Prepared {
        id: usize,
        range: Interval,
        axes: u8,
        sampler: Arc<Sampler>,
    },
}

impl DensityFunction {
    pub fn constant(value: f32) -> Df {
        Arc::new(DensityFunction::Constant(value))
    }

    pub fn is_zero_constant(&self) -> bool {
        matches!(self, DensityFunction::Constant(v) if v.to_bits() == 0)
    }

    pub fn constant_value(&self) -> Option<f32> {
        match self {
            DensityFunction::Constant(v) => Some(*v),
            _ => None,
        }
    }

    /// A string that is equal for structurally equal trees; the game
    /// deduplicates caches and spline coordinates by value equality.
    pub fn identity_key(&self) -> String {
        let mut key = String::new();
        self.write_key(&mut key);
        key
    }

    fn write_key(&self, out: &mut String) {
        use DensityFunction::*;
        use std::fmt::Write;
        let f = |v: f32| v.to_bits();
        let d = |v: f64| v.to_bits();
        match self {
            Constant(v) => {
                let _ = write!(out, "c{}", f(*v));
            }
            Reference(named) => {
                let _ = write!(out, "r{}", named.name);
            }
            Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let _ = write!(out, "n({},{},{},", noise.name, d(*xz_scale), d(*y_scale));
                shift_x.write_key(out);
                out.push(',');
                shift_y.write_key(out);
                out.push(',');
                shift_z.write_key(out);
                out.push(')');
            }
            ShiftA(noise) => {
                let _ = write!(out, "sa{}", noise.name);
            }
            ShiftB(noise) => {
                let _ = write!(out, "sb{}", noise.name);
            }
            Shift(noise) => {
                let _ = write!(out, "sh{}", noise.name);
            }
            EndIslands => out.push_str("ei"),
            DistanceToPoint { point, metric } => {
                let _ = write!(out, "dp{point:?}{metric:?}");
            }
            Gradient {
                axis,
                tiling,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => {
                let _ = write!(
                    out,
                    "g({axis:?},{tiling:?},{from_coordinate},{to_coordinate},{},{})",
                    f(*from_value),
                    f(*to_value)
                );
            }
            Unary(op, input) => {
                let _ = write!(out, "u{op:?}(");
                input.write_key(out);
                out.push(')');
            }
            Binary(op, left, right) => {
                let _ = write!(out, "b{op:?}(");
                left.write_key(out);
                out.push(',');
                right.write_key(out);
                out.push(')');
            }
            Spline(spline) => {
                out.push_str("sp(");
                write_spline_key(spline, out);
                out.push(')');
            }
            Lerp {
                alpha,
                first,
                second,
            } => {
                out.push_str("l(");
                for (i, c) in [alpha, first, second].into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    c.write_key(out);
                }
                out.push(')');
            }
            Clamp { input, min, max } => {
                let _ = write!(out, "cl({},{},", f(*min), f(*max));
                input.write_key(out);
                out.push(')');
            }
            RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let _ = write!(out, "rc({},{},", f(*min_inclusive), f(*max_exclusive));
                input.write_key(out);
                out.push(',');
                when_in_range.write_key(out);
                out.push(',');
                when_out_of_range.write_key(out);
                out.push(')');
            }
            IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                out.push_str("is(");
                for t in thresholds {
                    let _ = write!(out, "{},", f(*t));
                }
                input.write_key(out);
                for function in functions {
                    out.push(',');
                    function.write_key(out);
                }
                out.push(')');
            }
            Cache(input) => {
                out.push_str("ca(");
                input.write_key(out);
                out.push(')');
            }
            BlendDensity(input) => {
                out.push_str("bd(");
                input.write_key(out);
                out.push(')');
            }
            Interpolated {
                input,
                cell_xz,
                cell_y,
            } => {
                let _ = write!(out, "i({cell_xz},{cell_y},");
                input.write_key(out);
                out.push(')');
            }
            Slice {
                axis,
                coordinate,
                input,
            } => {
                let _ = write!(out, "sl({axis:?},{coordinate},");
                input.write_key(out);
                out.push(')');
            }
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let _ = write!(out, "ft({lower_bound},{cell_height},");
                density.write_key(out);
                out.push(',');
                upper_bound.write_key(out);
                out.push(')');
            }
            OldBlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => {
                let _ = write!(
                    out,
                    "ob({},{},{},{},{})",
                    d(*xz_scale),
                    d(*y_scale),
                    d(*xz_factor),
                    d(*y_factor),
                    d(*smear_scale_multiplier)
                );
            }
            ContextBound(key) => {
                let _ = write!(out, "cb{key:?}");
            }
            Custom(custom) => {
                let _ = write!(out, "cu{:p}", Arc::as_ptr(custom));
            }
            Prepared { id, .. } => {
                let _ = write!(out, "p{id}");
            }
        }
    }

    pub fn range(&self) -> Interval {
        use DensityFunction::*;
        match self {
            Constant(v) => Interval::exact(*v),
            Reference(named) => named.function.range(),
            Noise { noise, .. } => noise.noise.range(),
            ShiftA(noise) | ShiftB(noise) | Shift(noise) => {
                Interval::product(noise.noise.range(), Interval::exact(4.0))
            }
            EndIslands => Interval::of(-0.84375, 0.5625),
            DistanceToPoint { .. } => Interval::of(0.0, f32::INFINITY),
            Gradient {
                from_value,
                to_value,
                ..
            } => Interval::encapsulating_values(*from_value, *to_value),
            Unary(op, input) => unary_range(*op, input.range()),
            Binary(op, left, right) => {
                let (l, r) = (left.range(), right.range());
                match op {
                    BinaryOp::Add => Interval::sum(l, r),
                    BinaryOp::Sub => Interval::difference(l, r),
                    BinaryOp::Mul => Interval::product(l, r),
                    BinaryOp::Div => Interval::quotient(l, r),
                    BinaryOp::Min => Interval::min(l, r),
                    BinaryOp::Max => Interval::max(l, r),
                    BinaryOp::Pow => Interval::pow(l, r),
                    BinaryOp::Round(mode) => Interval::product(
                        Interval::map_monotonic(Interval::quotient(l, r), |v| mode.apply(v)),
                        r,
                    ),
                }
            }
            Spline(spline) => spline_range(spline),
            Lerp {
                alpha,
                first,
                second,
            } => Interval::lerp(alpha.range(), first.range(), second.range()),
            Clamp { input, min, max } => Interval::clamp(input.range(), *min, *max),
            RangeChoice {
                when_in_range,
                when_out_of_range,
                ..
            } => Interval::encapsulating(&[when_in_range.range(), when_out_of_range.range()]),
            IntervalSelect { functions, .. } => {
                let ranges: Vec<Interval> = functions.iter().map(|f| f.range()).collect();
                Interval::encapsulating(&ranges)
            }
            Cache(input) | BlendDensity(input) => input.range(),
            Interpolated { input, .. } | Slice { input, .. } => input.range(),
            FindTopSurface {
                upper_bound,
                lower_bound,
                ..
            } => {
                let lower = *lower_bound as f32;
                Interval::of(lower, super::java_max(lower, upper_bound.range().max))
            }
            OldBlendedNoise {
                y_scale,
                smear_scale_multiplier,
                ..
            } => crate::blended::range(*y_scale, *smear_scale_multiplier),
            ContextBound(key) => match key {
                ContextKey::BlendAlpha => Interval::of(0.0, 1.0),
                ContextKey::BlendOffset => Interval::INFINITE,
                ContextKey::Beardifier => crate::beardifier::RANGE,
            },
            Custom(custom) => custom.range(),
            Prepared { range, .. } => *range,
        }
    }

    pub fn domain_axes(&self) -> u8 {
        use DensityFunction::*;
        match self {
            Constant(_) => 0,
            Reference(named) => named.function.domain_axes(),
            Noise {
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
                ..
            } => {
                let mut axes = ALL_AXES;
                if *y_scale == 0.0 {
                    axes &= !AXIS_Y;
                }
                if *xz_scale == 0.0 {
                    axes &= !(AXIS_X | AXIS_Z);
                }
                axes | shift_x.domain_axes() | shift_y.domain_axes() | shift_z.domain_axes()
            }
            ShiftA(_) | ShiftB(_) | EndIslands => AXIS_X | AXIS_Z,
            Shift(_) => ALL_AXES,
            DistanceToPoint { .. } => ALL_AXES,
            Gradient { axis, .. } => axis.bit(),
            Unary(_, input) => input.domain_axes(),
            Binary(_, left, right) => left.domain_axes() | right.domain_axes(),
            Spline(spline) => {
                let mut axes = 0;
                spline_for_each_coordinate(spline, &mut |c| axes |= c.domain_axes());
                axes
            }
            Lerp {
                alpha,
                first,
                second,
            } => alpha.domain_axes() | first.domain_axes() | second.domain_axes(),
            Clamp { input, .. } => input.domain_axes(),
            RangeChoice {
                input,
                when_in_range,
                when_out_of_range,
                ..
            } => {
                input.domain_axes() | when_in_range.domain_axes() | when_out_of_range.domain_axes()
            }
            IntervalSelect {
                input, functions, ..
            } => functions
                .iter()
                .fold(input.domain_axes(), |axes, f| axes | f.domain_axes()),
            Cache(input) | BlendDensity(input) | Interpolated { input, .. } => input.domain_axes(),
            Slice { axis, input, .. } => input.domain_axes() & !axis.bit(),
            FindTopSurface {
                density,
                upper_bound,
                ..
            } => (density.domain_axes() | upper_bound.domain_axes()) & !AXIS_Y,
            OldBlendedNoise { .. } => ALL_AXES,
            ContextBound(key) => match key {
                ContextKey::Beardifier => ALL_AXES,
                _ => AXIS_X | AXIS_Z,
            },
            Custom(custom) => custom.domain_axes(),
            Prepared { axes, .. } => *axes,
        }
    }
}

fn unary_range(op: UnaryOp, input: Interval) -> Interval {
    match op {
        UnaryOp::Abs => Interval::abs(input),
        UnaryOp::Square => Interval::square(input),
        UnaryOp::Cube => Interval::map_monotonic(input, |v| v * v * v),
        UnaryOp::Sqrt => Interval::pow(input, Interval::exact(0.5)),
        UnaryOp::HalfNegative => Interval::map_monotonic(input, |v| leaky_relu(0.5, v)),
        UnaryOp::QuarterNegative => Interval::map_monotonic(input, |v| leaky_relu(0.25, v)),
        UnaryOp::Reciprocal => Interval::reciprocal(input),
        UnaryOp::Negate => Interval::difference(Interval::exact(0.0), input),
        UnaryOp::Squeeze => Interval::map_monotonic(input, squeeze),
        UnaryOp::Log => Interval::log(input),
        UnaryOp::Sign => Interval::sign(input),
    }
}

pub fn leaky_relu(factor: f32, input: f32) -> f32 {
    if input > 0.0 { input } else { input * factor }
}

pub fn squeeze(input: f32) -> f32 {
    let c = input.clamp(-1.0, 1.0);
    c / 2.0 - c * c * c / 24.0
}

fn write_spline_key(spline: &Spline, out: &mut String) {
    use std::fmt::Write;
    match spline {
        Spline::Constant(v) => {
            let _ = write!(out, "k{}", v.to_bits());
        }
        Spline::Multipoint {
            coordinate,
            locations,
            values,
            derivatives,
        } => {
            out.push('[');
            coordinate.write_key(out);
            for ((l, v), d) in locations.iter().zip(values).zip(derivatives) {
                let _ = write!(out, ";{},{},", l.to_bits(), d.to_bits());
                write_spline_key(v, out);
            }
            out.push(']');
        }
    }
}

pub fn spline_for_each_coordinate(spline: &Spline, f: &mut impl FnMut(&Df)) {
    if let Spline::Multipoint {
        coordinate, values, ..
    } = spline
    {
        f(coordinate);
        for value in values {
            spline_for_each_coordinate(value, f);
        }
    }
}

fn linear_extend(
    input: f32,
    locations: &[f32],
    value: f32,
    derivatives: &[f32],
    index: usize,
) -> f32 {
    let derivative = derivatives[index];
    if derivative == 0.0 {
        value
    } else {
        value + derivative * (input - locations[index])
    }
}

pub fn spline_range(spline: &Spline) -> Interval {
    let Spline::Multipoint {
        coordinate,
        locations,
        values,
        derivatives,
    } = spline
    else {
        let Spline::Constant(v) = spline else {
            unreachable!()
        };
        return Interval::exact(*v);
    };
    spline_range_with(
        coordinate.range(),
        locations,
        values,
        derivatives,
        spline_range,
    )
}

pub fn spline_range_with<S>(
    input: Interval,
    locations: &[f32],
    values: &[S],
    derivatives: &[f32],
    range_of: impl Fn(&S) -> Interval,
) -> Interval {
    use super::{java_max as max, java_min as min};
    let last = locations.len() - 1;
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    if input.is_nai() {
        return input;
    }
    if input.min < locations[0] {
        let first = range_of(&values[0]);
        let a = linear_extend(input.min, locations, first.min, derivatives, 0);
        let b = linear_extend(input.min, locations, first.max, derivatives, 0);
        min_value = min(f32::INFINITY, min(a, b));
        max_value = max(f32::NEG_INFINITY, max(a, b));
    }
    if input.max > locations[last] {
        let last_range = range_of(&values[last]);
        let a = linear_extend(input.max, locations, last_range.min, derivatives, last);
        let b = linear_extend(input.max, locations, last_range.max, derivatives, last);
        min_value = min(min_value, min(a, b));
        max_value = max(max_value, max(a, b));
    }
    let ranges: Vec<Interval> = values.iter().map(&range_of).collect();
    for range in &ranges {
        min_value = min(min_value, range.min);
        max_value = max(max_value, range.max);
    }
    for i in 0..last {
        let x_diff = locations[i + 1] - locations[i];
        let (r1, r2) = (ranges[i], ranges[i + 1]);
        let (d1, d2) = (derivatives[i], derivatives[i + 1]);
        if d1 != 0.0 || d2 != 0.0 {
            let p1 = d1 * x_diff;
            let p2 = d2 * x_diff;
            let min_lerp1 = min(r1.min, r2.min);
            let max_lerp1 = max(r1.max, r2.max);
            let min_a = p1 - r2.max + r1.min;
            let max_a = p1 - r2.min + r1.max;
            let min_b = -p2 + r2.min - r1.max;
            let max_b = -p2 + r2.max - r1.min;
            let min_lerp2 = min(min_a, min_b);
            let max_lerp2 = max(max_a, max_b);
            min_value = min(min_value, min_lerp1 + 0.25 * min_lerp2);
            max_value = max(max_value, max_lerp1 + 0.25 * max_lerp2);
        }
    }
    Interval::of(min_value, max_value)
}

/// Spline evaluation, generic over how a coordinate is read so the point
/// and volume samplers share it.
pub fn spline_sample<S>(
    spline: &S,
    shape: &impl Fn(&S) -> SplineShape<'_, S>,
    read: &mut impl FnMut(usize) -> f32,
) -> f32 {
    match shape(spline) {
        SplineShape::Constant(v) => v,
        SplineShape::Multipoint {
            coordinate,
            locations,
            values,
            derivatives,
        } => {
            let input = read(coordinate);
            let start = locations.partition_point(|&l| input >= l || input.is_nan() || l.is_nan())
                as isize
                - 1;
            let last = locations.len() - 1;
            if start < 0 {
                let v = spline_sample(&values[0], shape, read);
                return linear_extend(input, locations, v, derivatives, 0);
            }
            let start = start as usize;
            if start == last {
                let v = spline_sample(&values[last], shape, read);
                return linear_extend(input, locations, v, derivatives, last);
            }
            let (x1, x2) = (locations[start], locations[start + 1]);
            let t = (input - x1) / (x2 - x1);
            let (d1, d2) = (derivatives[start], derivatives[start + 1]);
            let y1 = spline_sample(&values[start], shape, read);
            let y2 = spline_sample(&values[start + 1], shape, read);
            let a = d1 * (x2 - x1) - (y2 - y1);
            let b = -d2 * (x2 - x1) + (y2 - y1);
            crate::noise::lerp(t, y1, y2) + t * (1.0 - t) * crate::noise::lerp(t, a, b)
        }
    }
}

pub enum SplineShape<'a, S> {
    Constant(f32),
    Multipoint {
        coordinate: usize,
        locations: &'a [f32],
        values: &'a [S],
        derivatives: &'a [f32],
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn round_modes_follow_the_floor_round_ceil_and_truncate_rules() {
        assert_eq!(RoundMode::Floor.apply(-1.5), -2.0);
        assert_eq!(RoundMode::Round.apply(-1.5), -1.0);
        assert_eq!(RoundMode::Round.apply(2.5), 3.0);
        assert_eq!(RoundMode::Ceil.apply(-1.5), -1.0);
        assert_eq!(RoundMode::Truncate.apply(-1.5), -1.0);
        assert_eq!(RoundMode::Truncate.apply(1.5), 1.0);
        assert_eq!(RoundMode::Floor.round_to_multiple(7.0, 4.0), 4.0);
        assert_eq!(RoundMode::Round.round_to_multiple(7.0, 0.0), 7.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn pow_and_round_functions_report_their_value_range() {
        let pow = DensityFunction::Binary(
            BinaryOp::Pow,
            DensityFunction::constant(2.0),
            DensityFunction::constant(3.0),
        );
        let range = pow.range();
        assert_eq!((range.min, range.max), (8.0, 8.0));
        let round = DensityFunction::Binary(
            BinaryOp::Round(RoundMode::Floor),
            DensityFunction::constant(7.0),
            DensityFunction::constant(4.0),
        );
        let range = round.range();
        assert_eq!((range.min, range.max), (4.0, 4.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn three_axis_shift_depends_on_every_axis_and_matches_the_shift_range() {
        let holder = |name: &str| {
            Arc::new(NoiseHolder {
                name: name.to_string(),
                noise: crate::normal_noise::NormalNoise::parity(-3, &[1.0, 1.0]),
            })
        };
        let shift = DensityFunction::Shift(holder("offset"));
        let shift_a = DensityFunction::ShiftA(holder("offset"));
        assert_eq!(shift.domain_axes(), ALL_AXES);
        assert_ne!(shift_a.domain_axes(), ALL_AXES);
        let (a, b) = (shift.range(), shift_a.range());
        assert_eq!((a.min, a.max), (b.min, b.max));
    }
}

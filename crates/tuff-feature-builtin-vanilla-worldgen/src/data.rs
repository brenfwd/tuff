//! Builds runtime density function trees from the generated vanilla data.
//! Named functions and noises map to one shared node each, so references
//! keep their identity.

use std::collections::HashMap;
use std::sync::Arc;

use tuff_vanilla_data::worldgen as data;

use crate::density::function::{BinaryOp, ContextKey, NoiseHolder, RoundMode, Spline, UnaryOp};
use crate::density::{Axis, DensityFunction, Df, DistanceMetric, NamedFunction, Tiling};
use crate::normal_noise::{NoiseParameters, NormalNoise, Normalization};

#[derive(Default)]
pub struct Loader {
    functions: HashMap<&'static str, Arc<NamedFunction>>,
    noises: HashMap<&'static str, Arc<NoiseHolder>>,
}

impl Loader {
    pub fn noise(&mut self, noise: &'static data::Noise) -> Arc<NoiseHolder> {
        self.noises
            .entry(noise.name)
            .or_insert_with(|| {
                Arc::new(NoiseHolder {
                    name: noise.name.to_string(),
                    noise: NormalNoise::new(NoiseParameters {
                        base_amplitude: noise.base_amplitude,
                        base_octave: noise.base_octave,
                        octave_count: noise.octave_count,
                        normalize: match noise.normalize {
                            data::Normalize::Disabled => Normalization::Disabled,
                            data::Normalize::Enabled => Normalization::Enabled,
                            data::Normalize::Legacy => Normalization::Legacy,
                        },
                        amplitude_modifiers: noise.amplitude_modifiers.to_vec(),
                    }),
                })
            })
            .clone()
    }

    pub fn named(&mut self, named: &'static data::NamedDensityFunction) -> Arc<NamedFunction> {
        if let Some(existing) = self.functions.get(named.name) {
            return existing.clone();
        }
        let function = self.function(&named.function);
        let named_fn = Arc::new(NamedFunction {
            name: named.name.to_string(),
            function,
        });
        self.functions.insert(named.name, named_fn.clone());
        named_fn
    }

    pub fn function(&mut self, function: &'static data::DensityFunction) -> Df {
        use data::DensityFunction as D;
        Arc::new(match function {
            D::Constant(v) => DensityFunction::Constant(*v),
            D::Reference(named) => DensityFunction::Reference(self.named(named)),
            D::Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => DensityFunction::Noise {
                noise: self.noise(noise),
                xz_scale: *xz_scale,
                y_scale: *y_scale,
                shift_x: self.function(shift_x),
                shift_y: self.function(shift_y),
                shift_z: self.function(shift_z),
            },
            D::ShiftA(noise) => DensityFunction::ShiftA(self.noise(noise)),
            D::ShiftB(noise) => DensityFunction::ShiftB(self.noise(noise)),
            D::Shift(noise) => DensityFunction::Shift(self.noise(noise)),
            D::EndIslands => DensityFunction::EndIslands,
            D::DistanceToPoint { point, metric } => DensityFunction::DistanceToPoint {
                point: *point,
                metric: match metric {
                    data::DistanceMetric::Euclidean => DistanceMetric::Euclidean,
                    data::DistanceMetric::EuclideanSquared => DistanceMetric::EuclideanSquared,
                    data::DistanceMetric::Manhattan => DistanceMetric::Manhattan,
                    data::DistanceMetric::Chebyshev => DistanceMetric::Chebyshev,
                },
            },
            D::Gradient {
                axis,
                tiling,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => DensityFunction::Gradient {
                axis: convert_axis(*axis),
                tiling: match tiling {
                    data::Tiling::ClampToEdge => Tiling::ClampToEdge,
                    data::Tiling::Repeat => Tiling::Repeat,
                    data::Tiling::MirroredRepeat => Tiling::MirroredRepeat,
                },
                from_coordinate: *from_coordinate,
                to_coordinate: *to_coordinate,
                from_value: *from_value,
                to_value: *to_value,
            },
            D::Unary(op, input) => DensityFunction::Unary(
                match op {
                    data::UnaryOp::Abs => UnaryOp::Abs,
                    data::UnaryOp::Square => UnaryOp::Square,
                    data::UnaryOp::Cube => UnaryOp::Cube,
                    data::UnaryOp::HalfNegative => UnaryOp::HalfNegative,
                    data::UnaryOp::QuarterNegative => UnaryOp::QuarterNegative,
                    data::UnaryOp::Negate => UnaryOp::Negate,
                    data::UnaryOp::Squeeze => UnaryOp::Squeeze,
                    data::UnaryOp::Sqrt => UnaryOp::Sqrt,
                    data::UnaryOp::Reciprocal => UnaryOp::Reciprocal,
                    data::UnaryOp::Log => UnaryOp::Log,
                    data::UnaryOp::Sign => UnaryOp::Sign,
                },
                self.function(input),
            ),
            D::Binary(op, left, right) => DensityFunction::Binary(
                match op {
                    data::BinaryOp::Add => BinaryOp::Add,
                    data::BinaryOp::Sub => BinaryOp::Sub,
                    data::BinaryOp::Mul => BinaryOp::Mul,
                    data::BinaryOp::Div => BinaryOp::Div,
                    data::BinaryOp::Min => BinaryOp::Min,
                    data::BinaryOp::Max => BinaryOp::Max,
                    data::BinaryOp::Pow => BinaryOp::Pow,
                    data::BinaryOp::Floor => BinaryOp::Round(RoundMode::Floor),
                    data::BinaryOp::Round => BinaryOp::Round(RoundMode::Round),
                    data::BinaryOp::Ceil => BinaryOp::Round(RoundMode::Ceil),
                    data::BinaryOp::Truncate => BinaryOp::Round(RoundMode::Truncate),
                },
                self.function(left),
                self.function(right),
            ),
            D::Spline(spline) => DensityFunction::Spline(Arc::new(self.spline(spline))),
            D::Lerp {
                alpha,
                first,
                second,
            } => DensityFunction::Lerp {
                alpha: self.function(alpha),
                first: self.function(first),
                second: self.function(second),
            },
            D::Clamp { input, min, max } => DensityFunction::Clamp {
                input: self.function(input),
                min: *min,
                max: *max,
            },
            D::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => DensityFunction::RangeChoice {
                input: self.function(input),
                min_inclusive: *min_inclusive,
                max_exclusive: *max_exclusive,
                when_in_range: self.function(when_in_range),
                when_out_of_range: self.function(when_out_of_range),
            },
            D::IntervalSelect {
                input,
                thresholds,
                functions,
            } => DensityFunction::IntervalSelect {
                input: self.function(input),
                thresholds: thresholds.to_vec(),
                functions: functions.iter().map(|f| self.function(f)).collect(),
            },
            D::Cache(input) => DensityFunction::Cache(self.function(input)),
            D::BlendDensity(input) => DensityFunction::BlendDensity(self.function(input)),
            D::Interpolated {
                input,
                cell_xz,
                cell_y,
            } => DensityFunction::Interpolated {
                input: self.function(input),
                cell_xz: *cell_xz,
                cell_y: *cell_y,
            },
            D::Slice {
                axis,
                coordinate,
                input,
            } => DensityFunction::Slice {
                axis: convert_axis(*axis),
                coordinate: *coordinate,
                input: self.function(input),
            },
            D::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => DensityFunction::FindTopSurface {
                density: self.function(density),
                upper_bound: self.function(upper_bound),
                lower_bound: *lower_bound,
                cell_height: *cell_height,
            },
            D::OldBlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => DensityFunction::OldBlendedNoise {
                xz_scale: *xz_scale,
                y_scale: *y_scale,
                xz_factor: *xz_factor,
                y_factor: *y_factor,
                smear_scale_multiplier: *smear_scale_multiplier,
            },
            D::BlendAlpha => DensityFunction::ContextBound(ContextKey::BlendAlpha),
            D::BlendOffset => DensityFunction::ContextBound(ContextKey::BlendOffset),
            D::Beardifier => DensityFunction::ContextBound(ContextKey::Beardifier),
        })
    }

    fn spline(&mut self, spline: &'static data::Spline) -> Spline {
        match spline {
            data::Spline::Constant(v) => Spline::Constant(*v),
            data::Spline::Multipoint { coordinate, points } => Spline::Multipoint {
                coordinate: self.function(coordinate),
                locations: points.iter().map(|p| p.location).collect(),
                values: points.iter().map(|p| self.spline(&p.value)).collect(),
                derivatives: points.iter().map(|p| p.derivative).collect(),
            },
        }
    }
}

fn convert_axis(axis: data::Axis) -> Axis {
    match axis {
        data::Axis::X => Axis::X,
        data::Axis::Y => Axis::Y,
        data::Axis::Z => Axis::Z,
    }
}

//! Turns density function trees into samplers the way the game does:
//! references are inlined, caches with equal inputs are merged into one
//! numbered cache, axes a subtree does not depend on are sliced away, and a
//! few node shapes compile to specialized samplers.

use std::collections::HashMap;
use std::sync::Arc;

use super::function::{
    ALL_AXES, AXIS_X, AXIS_Y, AXIS_Z, Axis, BinaryOp, ContextKey, DensityFunction, Df, NoiseHolder,
    Spline, Tiling, UnaryOp,
};
use super::program::{Compiled, ProgramMemo};
use super::sampler::{GradientKind, GradientSampler, Sampler, SplineNode, UnaryKind};
use crate::blended;
use crate::noise::{NoiseStack, Simplex};
use tuff_item::random::{LegacyRandom, PositionalRandom, Random, RandomSource};

const LEGACY_NETHER_NOISES: [(&str, i64); 2] = [
    ("minecraft:nether/temperature", 0),
    ("minecraft:nether/vegetation", 1),
];

/// Compiles density functions for one world seed. Noise instances and
/// caches are shared between everything it compiles.
pub struct Compiler {
    seed: i64,
    legacy: bool,
    random: PositionalRandom,
    noises: HashMap<String, Arc<NoiseStack>>,
    caches: HashMap<String, Df>,
    next_cache: usize,
    programs: ProgramMemo,
    interpret: bool,
}

enum Rule {
    Inline,
    Slice(u8),
}

impl Compiler {
    pub fn new(seed: i64, legacy: bool, random: PositionalRandom) -> Self {
        Self {
            seed,
            legacy,
            random,
            noises: HashMap::new(),
            caches: HashMap::new(),
            next_cache: 0,
            programs: ProgramMemo::default(),
            interpret: false,
        }
    }

    /// Makes later compiles tree-walk volumes instead of running op-list
    /// programs.
    pub fn set_interpret(&mut self, interpret: bool) {
        self.interpret = interpret;
    }

    /// How many cache cells have been numbered so far.
    pub fn cache_count(&self) -> usize {
        self.next_cache
    }

    pub fn compile(&mut self, function: &Df) -> Sampler {
        let optimized = self.optimize(function);
        let tree = self.compile_node(&optimized);
        let mut compiled = Compiled::new(tree, &mut self.programs);
        compiled.interpreted = self.interpret;
        Sampler::Compiled(Box::new(compiled))
    }

    /// The noise instance for a named noise, created once per world.
    pub fn noise(&mut self, holder: &NoiseHolder) -> Arc<NoiseStack> {
        if let Some((_, offset)) = LEGACY_NETHER_NOISES.iter().find(|(n, _)| *n == holder.name) {
            let mut random = Random::Legacy(LegacyRandom::new(self.seed.wrapping_add(*offset)));
            return Arc::new(holder.noise.create_legacy_nether(&mut random));
        }
        if let Some(noise) = self.noises.get(&holder.name) {
            return noise.clone();
        }
        let noise = Arc::new(
            holder
                .noise
                .create(&mut self.random.from_hash_of(&holder.name)),
        );
        self.noises.insert(holder.name.clone(), noise.clone());
        noise
    }

    fn optimize(&mut self, function: &Df) -> Df {
        let inlined = self.rewrite(Rule::Inline, function);
        self.rewrite(Rule::Slice(ALL_AXES), &inlined)
    }

    fn rewrite(&mut self, rule: Rule, function: &Df) -> Df {
        match rule {
            Rule::Inline => {
                let function = match &**function {
                    DensityFunction::Reference(named) => named.function.clone(),
                    _ => function.clone(),
                };
                if let DensityFunction::Cache(input) = &*function {
                    return self.prepare_cache(input);
                }
                self.rewrite_children(&function, &mut |c, f| c.rewrite(Rule::Inline, f))
            }
            Rule::Slice(parent) => {
                if matches!(
                    &**function,
                    DensityFunction::Constant(_) | DensityFunction::Gradient { .. }
                ) {
                    return function.clone();
                }
                let axes = function.domain_axes();
                if parent == axes {
                    return self
                        .rewrite_children(function, &mut |c, f| c.rewrite(Rule::Slice(parent), f));
                }
                let rewritten =
                    self.rewrite_children(function, &mut |c, f| c.rewrite(Rule::Slice(axes), f));
                remove_axes(rewritten, parent & !axes)
            }
        }
    }

    fn prepare_cache(&mut self, input: &Df) -> Df {
        let key = input.identity_key();
        if let Some(prepared) = self.caches.get(&key) {
            return prepared.clone();
        }
        let id = self.next_cache;
        self.next_cache += 1;
        let optimized = self.optimize(input);
        let sampler = Arc::new(Sampler::Cache {
            id,
            input: Arc::new(self.compile_node(&optimized)),
        });
        let prepared = Arc::new(DensityFunction::Prepared {
            id,
            range: optimized.range(),
            axes: optimized.domain_axes(),
            sampler,
        });
        self.caches.insert(key, prepared.clone());
        prepared
    }

    /// Rebuilds `function` with each child passed through `rule`, keeping
    /// the original node when no child changed.
    fn rewrite_children(
        &mut self,
        function: &Df,
        rule: &mut dyn FnMut(&mut Self, &Df) -> Df,
    ) -> Df {
        use DensityFunction::*;
        let mut changed = false;
        let mut map = |c: &mut Self, child: &Df| {
            let new = rule(c, child);
            changed |= !Arc::ptr_eq(&new, child);
            new
        };
        let rebuilt = match &**function {
            Reference(named) => {
                let new = rule(self, &named.function);
                if Arc::ptr_eq(&new, &named.function) {
                    return function.clone();
                }
                return new;
            }
            Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => Noise {
                noise: noise.clone(),
                xz_scale: *xz_scale,
                y_scale: *y_scale,
                shift_x: map(self, shift_x),
                shift_y: map(self, shift_y),
                shift_z: map(self, shift_z),
            },
            Unary(op, input) => Unary(*op, map(self, input)),
            Binary(op, left, right) => {
                let left = map(self, left);
                let right = map(self, right);
                Binary(*op, left, right)
            }
            Spline(spline) => Spline(Arc::new(map_spline(spline, &mut |f| map(self, f)))),
            Lerp {
                alpha,
                first,
                second,
            } => Lerp {
                alpha: map(self, alpha),
                first: map(self, first),
                second: map(self, second),
            },
            Clamp { input, min, max } => Clamp {
                input: map(self, input),
                min: *min,
                max: *max,
            },
            RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => RangeChoice {
                input: map(self, input),
                min_inclusive: *min_inclusive,
                max_exclusive: *max_exclusive,
                when_in_range: map(self, when_in_range),
                when_out_of_range: map(self, when_out_of_range),
            },
            IntervalSelect {
                input,
                thresholds,
                functions,
            } => IntervalSelect {
                input: map(self, input),
                thresholds: thresholds.clone(),
                functions: functions.iter().map(|f| map(self, f)).collect(),
            },
            Cache(input) => Cache(map(self, input)),
            BlendDensity(input) => BlendDensity(map(self, input)),
            Interpolated {
                input,
                cell_xz,
                cell_y,
            } => Interpolated {
                input: map(self, input),
                cell_xz: *cell_xz,
                cell_y: *cell_y,
            },
            Slice {
                axis,
                coordinate,
                input,
            } => Slice {
                axis: *axis,
                coordinate: *coordinate,
                input: map(self, input),
            },
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => FindTopSurface {
                density: map(self, density),
                upper_bound: map(self, upper_bound),
                lower_bound: *lower_bound,
                cell_height: *cell_height,
            },
            Constant(_)
            | ShiftA(_)
            | ShiftB(_)
            | Shift(_)
            | EndIslands
            | DistanceToPoint { .. }
            | Gradient { .. }
            | OldBlendedNoise { .. }
            | ContextBound(_)
            | Custom(_)
            | Prepared { .. } => return function.clone(),
        };
        if changed {
            Arc::new(rebuilt)
        } else {
            function.clone()
        }
    }

    fn compile_node(&mut self, function: &DensityFunction) -> Sampler {
        use DensityFunction::*;
        match function {
            Constant(v) => Sampler::Constant(*v),
            Reference(named) => self.compile_node(&named.function),
            Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let stack = self.noise(noise);
                if shift_x.is_zero_constant()
                    && shift_y.is_zero_constant()
                    && shift_z.is_zero_constant()
                {
                    return Sampler::Noise {
                        noise: stack,
                        xz_scale: *xz_scale,
                        y_scale: *y_scale,
                    };
                }
                let shift_x = Box::new(self.compile_node(shift_x));
                let shift_z = Box::new(self.compile_node(shift_z));
                let shift_y = if shift_y.is_zero_constant() {
                    None
                } else {
                    Some(Box::new(self.compile_node(shift_y)))
                };
                Sampler::ShiftedNoise {
                    shift_x,
                    shift_y,
                    shift_z,
                    noise: stack,
                    xz_scale: *xz_scale,
                    y_scale: *y_scale,
                }
            }
            ShiftA(noise) => Sampler::ConstMul(
                Box::new(Sampler::Noise {
                    noise: self.noise(noise),
                    xz_scale: 0.25,
                    y_scale: 0.0,
                }),
                4.0,
            ),
            Shift(noise) => Sampler::ConstMul(
                Box::new(Sampler::Noise {
                    noise: self.noise(noise),
                    xz_scale: 0.25,
                    y_scale: 0.25,
                }),
                4.0,
            ),
            ShiftB(noise) => Sampler::ShiftB(self.noise(noise)),
            EndIslands => {
                let mut random = Random::Legacy(LegacyRandom::new(self.seed));
                random.consume_count(17292);
                Sampler::EndIslands(Arc::new(Simplex::new(&mut random, true)))
            }
            DistanceToPoint { point, metric } => Sampler::Distance {
                point: *point,
                metric: *metric,
            },
            Gradient {
                axis,
                tiling,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => {
                let range = to_coordinate - from_coordinate;
                let factor = (to_value - from_value) / range as f32;
                let kind = match tiling {
                    Tiling::ClampToEdge => GradientKind::Clamped {
                        min: *from_coordinate.min(to_coordinate),
                        max: *from_coordinate.max(to_coordinate),
                    },
                    Tiling::Repeat => GradientKind::Repeat { range },
                    Tiling::MirroredRepeat => GradientKind::Mirrored { range },
                };
                Sampler::Gradient(GradientSampler {
                    axis: *axis,
                    kind,
                    from: *from_coordinate,
                    from_value: *from_value,
                    factor,
                })
            }
            Unary(op, input) => {
                let kind = match op {
                    UnaryOp::Abs => UnaryKind::Abs,
                    UnaryOp::Square => UnaryKind::Square,
                    UnaryOp::Cube => UnaryKind::Cube,
                    UnaryOp::Sqrt => UnaryKind::Sqrt,
                    UnaryOp::HalfNegative => UnaryKind::LeakyRelu(0.5),
                    UnaryOp::QuarterNegative => UnaryKind::LeakyRelu(0.25),
                    UnaryOp::Reciprocal => UnaryKind::Reciprocal,
                    UnaryOp::Negate => UnaryKind::Negate,
                    UnaryOp::Squeeze => UnaryKind::Squeeze,
                    UnaryOp::Log => UnaryKind::Log,
                    UnaryOp::Sign => UnaryKind::Sign,
                };
                Sampler::Unary(kind, Box::new(self.compile_node(input)))
            }
            Binary(op, left, right) => self.compile_binary(*op, left, right),
            Spline(spline) => {
                let mut coordinates: Vec<(String, Sampler)> = Vec::new();
                let root = self.compile_spline(spline, &mut coordinates);
                Sampler::Spline {
                    root,
                    coordinates: coordinates.into_iter().map(|(_, s)| s).collect(),
                }
            }
            Lerp {
                alpha,
                first,
                second,
            } => Sampler::Lerp(
                Box::new(self.compile_node(alpha)),
                Box::new(self.compile_node(first)),
                Box::new(self.compile_node(second)),
            ),
            Clamp { input, min, max } => {
                Sampler::Clamp(Box::new(self.compile_node(input)), *min, *max)
            }
            RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => Sampler::RangeChoice {
                input: Box::new(self.compile_node(input)),
                min: *min_inclusive,
                max: *max_exclusive,
                in_range: Box::new(self.compile_node(when_in_range)),
                out_of_range: Box::new(self.compile_node(when_out_of_range)),
            },
            IntervalSelect {
                input,
                thresholds,
                functions,
            } => Sampler::IntervalSelect {
                input: Box::new(self.compile_node(input)),
                thresholds: thresholds.clone(),
                samplers: functions.iter().map(|f| self.compile_node(f)).collect(),
            },
            Cache(_) => panic!("caches are prepared before compiling"),
            BlendDensity(input) => Sampler::BlendDensity(Box::new(self.compile_node(input))),
            Interpolated {
                input,
                cell_xz,
                cell_y,
            } => Sampler::Interpolated {
                input: Box::new(self.compile_node(input)),
                cell_xz: *cell_xz,
                cell_y: *cell_y,
                inv_xz: 1.0 / *cell_xz as f32,
                inv_y: 1.0 / *cell_y as f32,
            },
            Slice {
                axis,
                coordinate,
                input,
            } => {
                if let Slice {
                    axis: inner_axis,
                    coordinate: inner_coordinate,
                    input: inner_input,
                } = &**input
                {
                    let pair = match (axis, inner_axis) {
                        (Axis::X, Axis::Z) => Some((*coordinate, *inner_coordinate)),
                        (Axis::Z, Axis::X) => Some((*inner_coordinate, *coordinate)),
                        _ => None,
                    };
                    if let Some((x, z)) = pair {
                        return Sampler::SliceXz(Box::new(self.compile_node(inner_input)), x, z);
                    }
                }
                let input = Box::new(self.compile_node(input));
                match axis {
                    Axis::X => Sampler::SliceX(input, *coordinate),
                    Axis::Y => Sampler::SliceY(input, *coordinate),
                    Axis::Z => Sampler::SliceZ(input, *coordinate),
                }
            }
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => Sampler::SliceY(
                Box::new(Sampler::FindTopSurface {
                    density: Box::new(self.compile_node(density)),
                    upper_bound: Box::new(self.compile_node(upper_bound)),
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                }),
                0,
            ),
            OldBlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => {
                let mut random = if self.legacy {
                    Random::Legacy(LegacyRandom::new(self.seed))
                } else {
                    self.random.from_hash_of("minecraft:terrain")
                };
                blended::compile(
                    &mut random,
                    *xz_scale,
                    *y_scale,
                    *xz_factor,
                    *y_factor,
                    *smear_scale_multiplier,
                )
            }
            ContextBound(key) => {
                let fallback = match key {
                    ContextKey::BlendAlpha => 1.0,
                    ContextKey::BlendOffset | ContextKey::Beardifier => 0.0,
                };
                Sampler::ContextBound(*key, Box::new(Sampler::Constant(fallback)))
            }
            Custom(custom) => custom.compile(),
            Prepared { sampler, .. } => Sampler::Shared(sampler.clone()),
        }
    }

    fn compile_binary(&mut self, op: BinaryOp, left: &Df, right: &Df) -> Sampler {
        let l = self.compile_node(left);
        let r = self.compile_node(right);
        let (lc, rc) = (left.constant_value(), right.constant_value());
        match op {
            BinaryOp::Add => match (lc, rc) {
                (Some(v), _) => Sampler::ConstAdd(Box::new(r), v),
                (_, Some(v)) => Sampler::ConstAdd(Box::new(l), v),
                _ => Sampler::Add(Box::new(l), Box::new(r)),
            },
            BinaryOp::Sub => match (lc, rc) {
                (Some(v), _) => Sampler::ConstSub(v, Box::new(r)),
                (_, Some(v)) => Sampler::ConstAdd(Box::new(l), -v),
                _ => Sampler::Sub(Box::new(l), Box::new(r)),
            },
            BinaryOp::Mul => match (lc, rc) {
                (Some(v), _) => Sampler::ConstMul(Box::new(r), v),
                (_, Some(v)) => Sampler::ConstMul(Box::new(l), v),
                _ => Sampler::Mul(Box::new(l), Box::new(r)),
            },
            BinaryOp::Div => match (lc, rc) {
                (Some(v), _) => Sampler::ConstDiv(v, Box::new(r)),
                (_, Some(v)) => Sampler::ConstMul(Box::new(l), 1.0 / v),
                _ => Sampler::Div(Box::new(l), Box::new(r)),
            },
            BinaryOp::Pow => Sampler::Pow(Box::new(l), Box::new(r)),
            BinaryOp::Round(mode) => Sampler::Round(mode, Box::new(l), Box::new(r)),
            BinaryOp::Min => {
                let (lr, rr) = (left.range(), right.range());
                if lr.max < rr.min {
                    return l;
                }
                if rr.max < lr.min {
                    return r;
                }
                match (lc, rc) {
                    (Some(v), _) => Sampler::ConstMin(Box::new(r), v),
                    (_, Some(v)) => Sampler::ConstMin(Box::new(l), v),
                    _ => Sampler::Min(Box::new(l), Box::new(r), rr.min),
                }
            }
            BinaryOp::Max => {
                let (lr, rr) = (left.range(), right.range());
                if lr.min > rr.max {
                    return l;
                }
                if rr.min > lr.max {
                    return r;
                }
                match (lc, rc) {
                    (Some(v), _) => Sampler::ConstMax(Box::new(r), v),
                    (_, Some(v)) => Sampler::ConstMax(Box::new(l), v),
                    _ => Sampler::Max(Box::new(l), Box::new(r), rr.max),
                }
            }
        }
    }

    fn compile_spline(
        &mut self,
        spline: &Spline,
        coordinates: &mut Vec<(String, Sampler)>,
    ) -> SplineNode {
        match spline {
            Spline::Constant(v) => SplineNode::Constant(*v),
            Spline::Multipoint {
                coordinate,
                locations,
                values,
                derivatives,
            } => {
                let key = coordinate.identity_key();
                let index = match coordinates.iter().position(|(k, _)| *k == key) {
                    Some(index) => index,
                    None => {
                        let sampler = self.compile_node(coordinate);
                        coordinates.push((key, sampler));
                        coordinates.len() - 1
                    }
                };
                SplineNode::Multipoint {
                    coordinate: index,
                    locations: locations.clone(),
                    values: values
                        .iter()
                        .map(|v| self.compile_spline(v, coordinates))
                        .collect(),
                    derivatives: derivatives.clone(),
                }
            }
        }
    }
}

fn map_spline(spline: &Spline, f: &mut impl FnMut(&Df) -> Df) -> Spline {
    match spline {
        Spline::Constant(v) => Spline::Constant(*v),
        Spline::Multipoint {
            coordinate,
            locations,
            values,
            derivatives,
        } => Spline::Multipoint {
            coordinate: f(coordinate),
            locations: locations.clone(),
            values: values.iter().map(|v| map_spline(v, f)).collect(),
            derivatives: derivatives.clone(),
        },
    }
}

fn remove_axes(function: Df, axes: u8) -> Df {
    let mut existing = 0;
    let mut current = &function;
    while let DensityFunction::Slice { axis, input, .. } = &**current {
        existing |= axis.bit();
        current = input;
    }
    let filtered = axes & !existing;
    let mut function = function;
    for (bit, axis) in [(AXIS_X, Axis::X), (AXIS_Z, Axis::Z), (AXIS_Y, Axis::Y)] {
        if filtered & bit != 0 {
            function = Arc::new(DensityFunction::Slice {
                axis,
                coordinate: 0,
                input: function,
            });
        }
    }
    function
}

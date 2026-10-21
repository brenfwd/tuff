//! Linear evaluation programs for density volumes. A [`Program`] is a
//! sampler tree flattened into an op list over register buffers: register 0
//! is the output and an operation's operands always sit in the registers
//! directly after its destination. Every op repeats the arithmetic of the
//! matching sampler node in the same order and with the same types.

use std::collections::HashMap;
use std::sync::Arc;

use super::DensityVolume;
use super::function::{Axis, ContextKey, spline_sample};
use super::sampler::{
    Buf, Context, CustomSampler, GradientSampler, Sampler, SplineNode, UnaryKind, clamp,
    find_top_surface, interpolated_volume, select_index, spline_shape,
};
use crate::noise::{NoiseStack, lerp};

#[derive(Debug, Clone)]
pub struct Compiled {
    pub(super) tree: Sampler,
    pub(super) program: Arc<Program>,
    /// Volumes walk `tree` instead of running `program`.
    pub(super) interpreted: bool,
}

impl Compiled {
    pub(super) fn new(tree: Sampler, memo: &mut ProgramMemo) -> Self {
        let program = Arc::new(Program::build(&tree, memo));
        Self {
            tree,
            program,
            interpreted: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn lazy_selects(&self) -> (usize, usize) {
        self.program.lazy_selects()
    }
}

/// Programs already built for shared subtrees, keyed by the subtree's
/// address; the entry keeps the subtree alive so the address stays valid.
#[derive(Default)]
pub struct ProgramMemo {
    map: HashMap<usize, (Arc<Sampler>, Arc<Program>)>,
}

#[derive(Debug, Clone, Copy)]
enum BinKind {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Pow,
    Round(super::function::RoundMode),
}

#[derive(Debug, Clone, Copy)]
enum ConstKind {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

#[derive(Debug, Clone, Copy)]
enum SliceKind {
    X(i32),
    Y(i32),
    Z(i32),
    Xz(i32, i32),
}

#[derive(Debug)]
enum Op {
    Fill {
        dst: usize,
        value: f32,
    },
    Noise {
        dst: usize,
        noise: Arc<NoiseStack>,
        xz_scale: f64,
        y_scale: f64,
    },
    ShiftedNoise {
        dst: usize,
        has_y: bool,
        noise: Arc<NoiseStack>,
        xz_scale: f64,
        y_scale: f64,
    },
    ShiftB {
        dst: usize,
        noise: Arc<NoiseStack>,
    },
    Columns {
        dst: usize,
        node: Sampler,
    },
    Naive {
        dst: usize,
        node: Sampler,
    },
    Gradient {
        dst: usize,
        gradient: GradientSampler,
    },
    Unary {
        dst: usize,
        kind: UnaryKind,
    },
    Binary {
        dst: usize,
        kind: BinKind,
    },
    Const {
        dst: usize,
        kind: ConstKind,
        value: f32,
    },
    Clamp {
        dst: usize,
        min: f32,
        max: f32,
    },
    Lerp {
        dst: usize,
    },
    RangeChoice {
        dst: usize,
        min: f32,
        max: f32,
    },
    IntervalSelect {
        dst: usize,
        thresholds: Vec<f32>,
        count: usize,
    },
    SelectUsed {
        dst: usize,
        thresholds: Vec<f32>,
        samplers: Vec<Program>,
    },
    Cache {
        dst: usize,
        id: usize,
        input: Arc<Program>,
    },
    Call {
        dst: usize,
        program: Arc<Program>,
    },
    Interpolated {
        dst: usize,
        input: Program,
        cell_xz: i32,
        cell_y: i32,
        inv_xz: f32,
        inv_y: f32,
    },
    Slice {
        dst: usize,
        kind: SliceKind,
        input: Program,
    },
    Spline {
        dst: usize,
        root: SplineNode,
        coordinates: Vec<Program>,
    },
    FindTopSurface {
        dst: usize,
        density: Sampler,
        lower_bound: i32,
        cell_height: i32,
    },
    ContextBound {
        dst: usize,
        key: ContextKey,
        fallback: Program,
    },
    Custom {
        dst: usize,
        custom: Arc<dyn CustomSampler>,
    },
}

#[derive(Debug)]
pub struct Program {
    ops: Vec<Op>,
    registers: usize,
}

const MAX_LAZY_VARIANTS: usize = 64;

impl Op {
    /// True when running the op leaves no trace in the context (no caches,
    /// bound samplers or opaque callbacks), so skipping a whole program made of
    /// such ops cannot change any later result.
    fn is_stateless(&self) -> bool {
        match self {
            Op::Fill { .. }
            | Op::Noise { .. }
            | Op::ShiftedNoise { .. }
            | Op::ShiftB { .. }
            | Op::Gradient { .. }
            | Op::Unary { .. }
            | Op::Binary { .. }
            | Op::Const { .. }
            | Op::Clamp { .. }
            | Op::Lerp { .. }
            | Op::RangeChoice { .. }
            | Op::IntervalSelect { .. } => true,
            Op::SelectUsed { samplers, .. } => samplers.iter().all(Program::is_stateless),
            Op::Call { program, .. } => program.is_stateless(),
            Op::Interpolated { input, .. } | Op::Slice { input, .. } => input.is_stateless(),
            Op::Columns { .. }
            | Op::Naive { .. }
            | Op::Cache { .. }
            | Op::Spline { .. }
            | Op::FindTopSurface { .. }
            | Op::ContextBound { .. }
            | Op::Custom { .. } => false,
        }
    }
}

struct Builder<'a> {
    ops: Vec<Op>,
    registers: usize,
    memo: &'a mut ProgramMemo,
}

fn shared_program(shared: &Arc<Sampler>, memo: &mut ProgramMemo) -> Arc<Program> {
    if let Sampler::Compiled(c) = &**shared {
        return c.program.clone();
    }
    let key = Arc::as_ptr(shared) as usize;
    if let Some((_, program)) = memo.map.get(&key) {
        return program.clone();
    }
    let program = Arc::new(Program::build(shared, memo));
    memo.map.insert(key, (shared.clone(), program.clone()));
    program
}

impl Program {
    pub(super) fn build(root: &Sampler, memo: &mut ProgramMemo) -> Program {
        let mut builder = Builder {
            ops: Vec::new(),
            registers: 1,
            memo,
        };
        builder.emit(root, 0);
        Program {
            ops: builder.ops,
            registers: builder.registers,
        }
    }

    pub(super) fn is_stateless(&self) -> bool {
        self.ops.iter().all(Op::is_stateless)
    }

    /// Adds the ids of every cache cell this program touches to `out`;
    /// false when it has a stateful op other than caches.
    pub(super) fn cache_ids(&self, out: &mut Vec<usize>) -> bool {
        self.ops.iter().all(|op| match op {
            Op::Cache { id, input, .. } => {
                out.push(*id);
                input.cache_ids(out)
            }
            Op::SelectUsed { samplers, .. } => samplers.iter().all(|p| p.cache_ids(out)),
            Op::Call { program, .. } => program.cache_ids(out),
            Op::Interpolated { input, .. } | Op::Slice { input, .. } => input.cache_ids(out),
            other => other.is_stateless(),
        })
    }

    #[cfg(test)]
    pub(crate) fn lazy_selects(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        for op in &self.ops {
            let nested: Vec<&Program> = match op {
                Op::SelectUsed { samplers, .. } => {
                    counts.0 += 1;
                    samplers.iter().collect()
                }
                Op::IntervalSelect { .. } => {
                    counts.1 += 1;
                    Vec::new()
                }
                Op::Cache { input, .. } => vec![&**input],
                Op::Call { program, .. } => vec![&**program],
                Op::Interpolated { input, .. } | Op::Slice { input, .. } => vec![input],
                Op::Spline { coordinates, .. } => coordinates.iter().collect(),
                Op::ContextBound { fallback, .. } => vec![fallback],
                _ => Vec::new(),
            };
            for program in nested {
                let (lazy, eager) = program.lazy_selects();
                counts.0 += lazy;
                counts.1 += eager;
            }
        }
        counts
    }

    pub fn run(&self, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the avx2 feature was just detected at runtime.
            return unsafe { self.run_avx2(ctx, out, volume) };
        }
        self.run_lanes(ctx, out, volume)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn run_avx2(&self, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
        self.run_lanes(ctx, out, volume)
    }

    #[inline(always)]
    fn run_lanes(&self, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
        let size = volume.size();
        let key = self as *const Program as usize;
        let mut buffers = ctx.take_registers(key);
        buffers.resize_with(self.registers - 1, Buf::default);
        for buffer in &mut buffers {
            buffer.set_len(size);
        }
        {
            let mut slots: Vec<&mut [f32]> = Vec::with_capacity(self.registers);
            slots.push(out);
            slots.extend(buffers.iter_mut().map(|b| &mut **b));
            for op in &self.ops {
                op.exec(ctx, &mut slots, volume);
            }
        }
        ctx.put_registers(key, buffers);
    }
}

impl Builder<'_> {
    fn reserve(&mut self, register: usize) {
        self.registers = self.registers.max(register + 1);
    }

    fn sub(&mut self, node: &Sampler) -> Program {
        Program::build(node, self.memo)
    }

    fn emit(&mut self, node: &Sampler, dst: usize) {
        use Sampler::*;
        self.reserve(dst);
        match node {
            Constant(value) => self.ops.push(Op::Fill { dst, value: *value }),
            Noise {
                noise,
                xz_scale,
                y_scale,
            } => self.ops.push(Op::Noise {
                dst,
                noise: noise.clone(),
                xz_scale: *xz_scale,
                y_scale: *y_scale,
            }),
            ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                self.emit(shift_x, dst);
                let mut next = dst + 1;
                if let Some(shift_y) = shift_y {
                    self.emit(shift_y, next);
                    next += 1;
                }
                self.emit(shift_z, next);
                self.ops.push(Op::ShiftedNoise {
                    dst,
                    has_y: shift_y.is_some(),
                    noise: noise.clone(),
                    xz_scale: *xz_scale,
                    y_scale: *y_scale,
                });
            }
            ShiftB(noise) => self.ops.push(Op::ShiftB {
                dst,
                noise: noise.clone(),
            }),
            EndIslands(_) => self.ops.push(Op::Columns {
                dst,
                node: node.clone(),
            }),
            Distance { .. } => self.ops.push(Op::Naive {
                dst,
                node: node.clone(),
            }),
            Gradient(gradient) => self.ops.push(Op::Gradient {
                dst,
                gradient: gradient.clone(),
            }),
            Unary(kind, input) => {
                self.emit(input, dst);
                self.ops.push(Op::Unary { dst, kind: *kind });
            }
            Add(l, r) => self.binary(l, r, dst, BinKind::Add),
            Sub(l, r) => self.binary(l, r, dst, BinKind::Sub),
            Mul(l, r) => self.binary(l, r, dst, BinKind::Mul),
            Div(l, r) => self.binary(l, r, dst, BinKind::Div),
            Pow(l, r) => self.binary(l, r, dst, BinKind::Pow),
            Round(mode, l, r) => self.binary(l, r, dst, BinKind::Round(*mode)),
            Min(l, r, _) => self.binary(l, r, dst, BinKind::Min),
            Max(l, r, _) => self.binary(l, r, dst, BinKind::Max),
            ConstAdd(l, c) => self.constant(l, dst, ConstKind::Add, *c),
            ConstSub(c, r) => self.constant(r, dst, ConstKind::Sub, *c),
            ConstMul(l, c) => self.constant(l, dst, ConstKind::Mul, *c),
            ConstDiv(c, r) => self.constant(r, dst, ConstKind::Div, *c),
            ConstMin(l, c) => self.constant(l, dst, ConstKind::Min, *c),
            ConstMax(l, c) => self.constant(l, dst, ConstKind::Max, *c),
            Clamp(input, min, max) => {
                self.emit(input, dst);
                self.ops.push(Op::Clamp {
                    dst,
                    min: *min,
                    max: *max,
                });
            }
            Lerp(alpha, first, second) => {
                self.emit(alpha, dst);
                self.emit(first, dst + 1);
                self.emit(second, dst + 2);
                self.ops.push(Op::Lerp { dst });
            }
            RangeChoice {
                input,
                min,
                max,
                in_range,
                out_of_range,
            } => {
                self.emit(in_range, dst);
                self.emit(input, dst + 1);
                self.emit(out_of_range, dst + 2);
                self.ops.push(Op::RangeChoice {
                    dst,
                    min: *min,
                    max: *max,
                });
            }
            IntervalSelect {
                input,
                thresholds,
                samplers,
            } => {
                self.emit(input, dst);
                if (2..=MAX_LAZY_VARIANTS).contains(&samplers.len()) {
                    let variants: Vec<Program> = samplers.iter().map(|s| self.sub(s)).collect();
                    if variants.iter().all(Program::is_stateless) {
                        self.ops.push(Op::SelectUsed {
                            dst,
                            thresholds: thresholds.clone(),
                            samplers: variants,
                        });
                        return;
                    }
                }
                for (i, sampler) in samplers.iter().enumerate() {
                    self.emit(sampler, dst + 1 + i);
                }
                self.ops.push(Op::IntervalSelect {
                    dst,
                    thresholds: thresholds.clone(),
                    count: samplers.len(),
                });
            }
            Cache { id, input } => {
                let input = shared_program(input, self.memo);
                self.ops.push(Op::Cache {
                    dst,
                    id: *id,
                    input,
                });
            }
            Shared(inner) => {
                let program = shared_program(inner, self.memo);
                self.ops.push(Op::Call { dst, program });
            }
            Interpolated {
                input,
                cell_xz,
                cell_y,
                inv_xz,
                inv_y,
            } => {
                let input = self.sub(input);
                self.ops.push(Op::Interpolated {
                    dst,
                    input,
                    cell_xz: *cell_xz,
                    cell_y: *cell_y,
                    inv_xz: *inv_xz,
                    inv_y: *inv_y,
                });
            }
            SliceX(input, sx) => self.slice(input, dst, SliceKind::X(*sx)),
            SliceY(input, sy) => self.slice(input, dst, SliceKind::Y(*sy)),
            SliceZ(input, sz) => self.slice(input, dst, SliceKind::Z(*sz)),
            SliceXz(input, sx, sz) => self.slice(input, dst, SliceKind::Xz(*sx, *sz)),
            Spline { root, coordinates } => {
                let coordinates = coordinates.iter().map(|c| self.sub(c)).collect();
                self.ops.push(Op::Spline {
                    dst,
                    root: root.clone(),
                    coordinates,
                });
            }
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                self.emit(upper_bound, dst);
                self.ops.push(Op::FindTopSurface {
                    dst,
                    density: (**density).clone(),
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                });
            }
            BlendDensity(input) => self.emit(input, dst),
            ContextBound(key, fallback) => {
                let fallback = self.sub(fallback);
                self.ops.push(Op::ContextBound {
                    dst,
                    key: *key,
                    fallback,
                });
            }
            Custom(custom) => self.ops.push(Op::Custom {
                dst,
                custom: custom.clone(),
            }),
            Compiled(c) => self.emit(&c.tree, dst),
        }
    }

    fn binary(&mut self, left: &Sampler, right: &Sampler, dst: usize, kind: BinKind) {
        self.emit(left, dst);
        self.emit(right, dst + 1);
        self.ops.push(Op::Binary { dst, kind });
    }

    fn constant(&mut self, input: &Sampler, dst: usize, kind: ConstKind, value: f32) {
        self.emit(input, dst);
        self.ops.push(Op::Const { dst, kind, value });
    }

    fn slice(&mut self, input: &Sampler, dst: usize, kind: SliceKind) {
        let input = self.sub(input);
        self.ops.push(Op::Slice { dst, kind, input });
    }
}

fn split<'a, 'b>(
    slots: &'a mut [&'b mut [f32]],
    dst: usize,
) -> (&'a mut [f32], &'a mut [&'b mut [f32]]) {
    let (d, rest) = slots[dst..].split_first_mut().expect("register");
    (&mut **d, rest)
}

#[inline(always)]
fn map(out: &mut [f32], f: impl Fn(f32) -> f32) {
    for v in out.iter_mut() {
        *v = f(*v);
    }
}

#[inline(always)]
fn zip(out: &mut [f32], other: &[f32], f: impl Fn(f32, f32) -> f32) {
    for (o, r) in out.iter_mut().zip(other) {
        *o = f(*o, *r);
    }
}

#[inline(always)]
fn unary(out: &mut [f32], kind: UnaryKind) {
    match kind {
        UnaryKind::Abs => map(out, f32::abs),
        UnaryKind::Square => map(out, |v| v * v),
        UnaryKind::Cube => map(out, |v| v * v * v),
        UnaryKind::Sqrt => map(out, |v| UnaryKind::Sqrt.apply(v)),
        UnaryKind::LeakyRelu(f) => map(out, |v| UnaryKind::LeakyRelu(f).apply(v)),
        UnaryKind::Reciprocal => map(out, |v| 1.0 / v),
        UnaryKind::Negate => map(out, |v| -v),
        UnaryKind::Squeeze => map(out, |v| UnaryKind::Squeeze.apply(v)),
        UnaryKind::Log => map(out, |v| UnaryKind::Log.apply(v)),
        UnaryKind::Sign => map(out, |v| UnaryKind::Sign.apply(v)),
    }
}

#[inline(always)]
fn lerp_select(out: &mut [f32], first: &[f32], second: &[f32]) {
    let n = out.len();
    let (first, second) = (&first[..n], &second[..n]);
    for i in 0..n {
        let a = out[i];
        let (f, s) = (first[i], second[i]);
        let blended = lerp(a, f, s);
        let at_one = if a == 1.0 { s } else { blended };
        out[i] = if a == 0.0 { f } else { at_one };
    }
}

#[inline(always)]
fn range_choice(out: &mut [f32], values: &[f32], other: &[f32], min: f32, max: f32) {
    let n = out.len();
    let (values, other) = (&values[..n], &other[..n]);
    for i in 0..n {
        let v = values[i];
        let outside = (v < min) | (v >= max);
        out[i] = if outside { other[i] } else { out[i] };
    }
}

impl Op {
    #[inline(always)]
    fn exec(&self, ctx: &mut Context, slots: &mut [&mut [f32]], volume: &DensityVolume) {
        match self {
            Op::Fill { dst, value } => slots[*dst].fill(*value),
            Op::Noise {
                dst,
                noise,
                xz_scale,
                y_scale,
            } => {
                let out = &mut *slots[*dst];
                out.fill(0.0);
                noise.add_to_volume(out, volume, *xz_scale, *y_scale, 1.0);
            }
            Op::ShiftedNoise {
                dst,
                has_y,
                noise,
                xz_scale,
                y_scale,
            } => {
                let (out, rest) = split(slots, *dst);
                let (shift_y, shift_z) = if *has_y {
                    (Some(&*rest[0]), &*rest[1])
                } else {
                    (None, &*rest[0])
                };
                let mut index = 0;
                for z in 0..volume.size_z {
                    let base_z = volume.block_z(z) as f64 * xz_scale;
                    for x in 0..volume.size_x {
                        let base_x = volume.block_x(x) as f64 * xz_scale;
                        for y in 0..volume.size_y {
                            let nx = base_x + out[index] as f64;
                            let ny = match shift_y {
                                None => volume.block_y(y) as f64 * y_scale,
                                Some(b) => volume.block_y(y) as f64 * y_scale + b[index] as f64,
                            };
                            let nz = base_z + shift_z[index] as f64;
                            out[index] = noise.get(nx, ny, nz);
                            index += 1;
                        }
                    }
                }
            }
            Op::ShiftB { dst, noise } => {
                let out = &mut *slots[*dst];
                let transposed = DensityVolume::stepped(
                    [volume.size_z, volume.size_x, 1],
                    [volume.min_z, volume.min_x, 0],
                    [volume.step_z, volume.step_x, 1],
                );
                let mut buffer = ctx.acquire(transposed.size());
                buffer.fill(0.0);
                noise.add_to_volume(&mut buffer, &transposed, 0.25, 0.25, 4.0);
                for z in 0..volume.size_z {
                    for x in 0..volume.size_x {
                        let value = buffer[transposed.index(z, x, 0)];
                        let start = volume.index(x, 0, z);
                        out[start..start + volume.size_y as usize].fill(value);
                    }
                }
                ctx.release(buffer);
            }
            Op::Columns { dst, node } => {
                let out = &mut *slots[*dst];
                for z in 0..volume.size_z {
                    let bz = volume.block_z(z);
                    for x in 0..volume.size_x {
                        let bx = volume.block_x(x);
                        let value = node.sample_value(ctx, bx, 0, bz);
                        let start = volume.index(x, 0, z);
                        out[start..start + volume.size_y as usize].fill(value);
                    }
                }
            }
            Op::Naive { dst, node } => {
                let out = &mut *slots[*dst];
                let mut index = 0;
                for z in 0..volume.size_z {
                    let bz = volume.block_z(z);
                    for x in 0..volume.size_x {
                        let bx = volume.block_x(x);
                        for y in 0..volume.size_y {
                            out[index] = node.sample_value(ctx, bx, volume.block_y(y), bz);
                            index += 1;
                        }
                    }
                }
            }
            Op::Gradient { dst, gradient: g } => {
                let out = &mut *slots[*dst];
                match g.axis {
                    Axis::X => {
                        for x in 0..volume.size_x {
                            let value = g.compute(volume.block_x(x));
                            for z in 0..volume.size_z {
                                let start = volume.index(x, 0, z);
                                out[start..start + volume.size_y as usize].fill(value);
                            }
                        }
                    }
                    Axis::Y => {
                        let size_y = volume.size_y as usize;
                        let out = &mut out[..volume.size()];
                        if size_y > 0
                            && let Some((first, rest)) = out.split_at_mut_checked(size_y)
                        {
                            for (y, slot) in first.iter_mut().enumerate() {
                                *slot = g.compute(volume.block_y(y as i32));
                            }
                            for column in rest.chunks_exact_mut(size_y) {
                                column.copy_from_slice(first);
                            }
                        }
                    }
                    Axis::Z => {
                        for z in 0..volume.size_z {
                            let value = g.compute(volume.block_z(z));
                            let start = volume.index(0, 0, z);
                            out[start..start + (volume.size_x * volume.size_y) as usize]
                                .fill(value);
                        }
                    }
                }
            }
            Op::Unary { dst, kind } => {
                unary(&mut *slots[*dst], *kind);
            }
            Op::Binary { dst, kind } => {
                let (out, rest) = split(slots, *dst);
                let other = &*rest[0];
                match kind {
                    BinKind::Add => zip(out, other, |a, b| a + b),
                    BinKind::Sub => zip(out, other, |a, b| a + -b),
                    BinKind::Mul => zip(out, other, |a, b| a * b),
                    BinKind::Div => zip(out, other, |a, b| a / b),
                    BinKind::Pow => zip(out, other, |a, b| (a as f64).powf(b as f64) as f32),
                    BinKind::Round(mode) => {
                        let mode = *mode;
                        zip(out, other, move |a, b| mode.round_to_multiple(a, b))
                    }
                    BinKind::Min => zip(out, other, |a, b| if b < a { b } else { a }),
                    BinKind::Max => zip(out, other, |a, b| if b > a { b } else { a }),
                }
            }
            Op::Const { dst, kind, value } => {
                let c = *value;
                let out = &mut *slots[*dst];
                match kind {
                    ConstKind::Add => map(out, |v| v + c),
                    ConstKind::Sub => map(out, |v| c - v),
                    ConstKind::Mul => map(out, |v| v * c),
                    ConstKind::Div => map(out, |v| c / v),
                    ConstKind::Min => map(out, |v| if c < v { c } else { v }),
                    ConstKind::Max => map(out, |v| if c > v { c } else { v }),
                }
            }
            Op::Clamp { dst, min, max } => {
                let (min, max) = (*min, *max);
                map(&mut *slots[*dst], |v| clamp(v, min, max));
            }
            Op::Lerp { dst } => {
                let (out, rest) = split(slots, *dst);
                lerp_select(out, &*rest[0], &*rest[1]);
            }
            Op::RangeChoice { dst, min, max } => {
                let (out, rest) = split(slots, *dst);
                range_choice(out, &*rest[0], &*rest[1], *min, *max);
            }
            Op::IntervalSelect {
                dst,
                thresholds,
                count,
            } => {
                let (out, rest) = split(slots, *dst);
                for (i, slot) in out.iter_mut().enumerate() {
                    let index = select_index(thresholds, *count, *slot);
                    *slot = rest[index][i];
                }
            }
            Op::SelectUsed {
                dst,
                thresholds,
                samplers,
            } => {
                let out = &mut *slots[*dst];
                let size = volume.size();
                let mut chosen = ctx.acquire(size);
                let mut used = 0u64;
                for (c, v) in chosen.iter_mut().zip(out.iter()) {
                    let index = select_index(thresholds, samplers.len(), *v);
                    used |= 1 << index;
                    *c = index as f32;
                }
                let mut buffer = ctx.acquire(size);
                for (k, sampler) in samplers.iter().enumerate() {
                    if used >> k & 1 == 0 {
                        continue;
                    }
                    sampler.run(ctx, &mut buffer, volume);
                    let wanted = k as f32;
                    for ((o, c), b) in out.iter_mut().zip(chosen.iter()).zip(buffer.iter()) {
                        if *c == wanted {
                            *o = *b;
                        }
                    }
                }
                ctx.release(buffer);
                ctx.release(chosen);
            }
            Op::Cache { dst, id, input } => {
                ctx.sample_volume_cached(
                    *id,
                    &mut *slots[*dst],
                    volume,
                    &mut |ctx, out, volume| input.run(ctx, out, volume),
                );
            }
            Op::Call { dst, program } => program.run(ctx, &mut *slots[*dst], volume),
            Op::Interpolated {
                dst,
                input,
                cell_xz,
                cell_y,
                inv_xz,
                inv_y,
            } => interpolated_volume(
                ctx,
                &mut *slots[*dst],
                volume,
                &mut |ctx, out, volume| input.run(ctx, out, volume),
                *cell_xz,
                *cell_y,
                *inv_xz,
                *inv_y,
            ),
            Op::Slice { dst, kind, input } => {
                exec_slice(ctx, &mut *slots[*dst], volume, *kind, input)
            }
            Op::Spline {
                dst,
                root,
                coordinates,
            } => {
                let out = &mut *slots[*dst];
                let mut buffers: Vec<Option<Buf>> = (0..coordinates.len()).map(|_| None).collect();
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = spline_sample(root, &spline_shape, &mut |index| {
                        if buffers[index].is_none() {
                            let mut buffer = ctx.acquire(volume.size());
                            coordinates[index].run(ctx, &mut buffer, volume);
                            buffers[index] = Some(buffer);
                        }
                        buffers[index].as_ref().unwrap()[i]
                    });
                }
                for buffer in buffers.into_iter().flatten() {
                    ctx.release(buffer);
                }
            }
            Op::FindTopSurface {
                dst,
                density,
                lower_bound,
                cell_height,
            } => {
                assert!(volume.size_y == 1, "top surface volumes are one block tall");
                let out = &mut *slots[*dst];
                let mut index = 0;
                for z in 0..volume.size_z {
                    let bz = volume.block_z(z);
                    for x in 0..volume.size_x {
                        let bx = volume.block_x(x);
                        let upper = out[index];
                        out[index] = find_top_surface(
                            ctx,
                            density,
                            *lower_bound,
                            *cell_height,
                            bx,
                            bz,
                            upper,
                        );
                        index += 1;
                    }
                }
            }
            Op::ContextBound { dst, key, fallback } => {
                let out = &mut *slots[*dst];
                match ctx.bound(*key) {
                    Some(bound) => bound.sample_volume(out, volume),
                    None => fallback.run(ctx, out, volume),
                }
            }
            Op::Custom { dst, custom } => custom.sample_volume(ctx, &mut *slots[*dst], volume),
        }
    }
}

fn exec_slice(
    ctx: &mut Context,
    out: &mut [f32],
    volume: &DensityVolume,
    kind: SliceKind,
    input: &Program,
) {
    let steps = [volume.step_x, volume.step_y, volume.step_z];
    match kind {
        SliceKind::X(sx) => {
            if volume.size_x == 1 && volume.min_x == sx {
                input.run(ctx, out, volume);
                return;
            }
            let inner = DensityVolume::stepped(
                [1, volume.size_y, volume.size_z],
                [sx, volume.min_y, volume.min_z],
                steps,
            );
            let mut buffer = ctx.acquire(inner.size());
            input.run(ctx, &mut buffer, &inner);
            let mut index = 0;
            for z in 0..volume.size_z {
                for _ in 0..volume.size_x {
                    for y in 0..volume.size_y {
                        out[index] = buffer[inner.index(0, y, z)];
                        index += 1;
                    }
                }
            }
            ctx.release(buffer);
        }
        SliceKind::Y(sy) => {
            if volume.size_y == 1 && volume.min_y == sy {
                input.run(ctx, out, volume);
                return;
            }
            let inner = DensityVolume::stepped(
                [volume.size_x, 1, volume.size_z],
                [volume.min_x, sy, volume.min_z],
                steps,
            );
            let mut buffer = ctx.acquire(inner.size());
            input.run(ctx, &mut buffer, &inner);
            for z in 0..volume.size_z {
                for x in 0..volume.size_x {
                    let value = buffer[inner.index(x, 0, z)];
                    let start = volume.index(x, 0, z);
                    out[start..start + volume.size_y as usize].fill(value);
                }
            }
            ctx.release(buffer);
        }
        SliceKind::Z(sz) => {
            if volume.size_z == 1 && volume.min_z == sz {
                input.run(ctx, out, volume);
                return;
            }
            let inner = DensityVolume::stepped(
                [volume.size_x, volume.size_y, 1],
                [volume.min_x, volume.min_y, sz],
                steps,
            );
            let mut buffer = ctx.acquire(inner.size());
            input.run(ctx, &mut buffer, &inner);
            let mut index = 0;
            for _ in 0..volume.size_z {
                for x in 0..volume.size_x {
                    for y in 0..volume.size_y {
                        out[index] = buffer[inner.index(x, y, 0)];
                        index += 1;
                    }
                }
            }
            ctx.release(buffer);
        }
        SliceKind::Xz(sx, sz) => {
            if volume.size_x == 1 && volume.size_z == 1 && volume.min_x == sx && volume.min_z == sz
            {
                input.run(ctx, out, volume);
                return;
            }
            let inner =
                DensityVolume::stepped([1, volume.size_y, 1], [sx, volume.min_y, sz], steps);
            let mut buffer = ctx.acquire(inner.size());
            input.run(ctx, &mut buffer, &inner);
            let size_y = volume.size_y as usize;
            if size_y > 0 {
                let column = &buffer[..size_y];
                for target in out[..volume.size()].chunks_exact_mut(size_y) {
                    target.copy_from_slice(column);
                }
            }
            ctx.release(buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::density::sampler::{GradientKind, GradientSampler};

    #[derive(Debug)]
    struct Wild(u64);

    fn mix(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn wild_value(r: u64) -> f32 {
        match r % 20 {
            0 => f32::NAN,
            1 => -0.0,
            2 | 3 => 0.0,
            4 | 5 => 1.0,
            6 => -1.0,
            7 => f32::INFINITY,
            8 => f32::NEG_INFINITY,
            9 => 1e-40,
            10 => 0.5,
            11 => -0.5,
            _ => ((mix(r) >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 6.0,
        }
    }

    impl CustomSampler for Wild {
        fn sample_value(&self, _: &mut Context, x: i32, y: i32, z: i32) -> f32 {
            let h =
                mix(self.0 ^ mix(x as u32 as u64 | (z as u32 as u64) << 32) ^ mix(y as u32 as u64));
            wild_value(h)
        }
    }

    fn wild(salt: u64) -> Box<Sampler> {
        Box::new(Sampler::Custom(Arc::new(Wild(salt))))
    }

    fn gradient(axis: Axis, kind: GradientKind) -> Box<Sampler> {
        Box::new(Sampler::Gradient(GradientSampler {
            axis,
            kind,
            from: -3,
            from_value: 0.25,
            factor: 0.125,
        }))
    }

    fn volumes() -> Vec<DensityVolume> {
        vec![
            DensityVolume::blocks([1, 1, 1], [0, 0, 0]),
            DensityVolume::blocks([1, 17, 1], [-5, -64, 9]),
            DensityVolume::blocks([3, 5, 7], [-100, 31, -2]),
            DensityVolume::blocks([16, 40, 16], [-32, -64, 48]),
            DensityVolume::blocks([5, 49, 5], [7, -64, -7]),
            DensityVolume::stepped([5, 49, 5], [-16, -64, 32], [4, 8, 4]),
            DensityVolume::stepped([7, 13, 3], [3, -9, -11], [2, 3, 5]),
            DensityVolume::blocks([16, 1, 16], [0, 320, 0]),
        ]
    }

    fn canon(values: &[f32]) -> Vec<u32> {
        values
            .iter()
            .map(|v| {
                if v.is_nan() {
                    f32::NAN.to_bits()
                } else {
                    v.to_bits()
                }
            })
            .collect()
    }

    fn assert_matches_tree(name: &str, sampler: &Sampler) {
        for volume in volumes() {
            let mut expected = vec![0.0; volume.size()];
            let mut actual = vec![0.0; volume.size()];
            sampler.sample_volume_tree(&mut Context::uncached(), &mut expected, &volume);
            sampler.sample_volume(&mut Context::uncached(), &mut actual, &volume);
            assert_eq!(canon(&actual), canon(&expected), "{name} {volume:?}");
        }
    }

    fn count_ops(program: &Program, f: &impl Fn(&Op) -> bool) -> usize {
        program.ops.iter().filter(|op| f(op)).count()
    }

    #[tuff_core::watchdog]
    #[test]
    fn unary_ops_match_the_tree() {
        for (i, kind) in [
            UnaryKind::Abs,
            UnaryKind::Square,
            UnaryKind::Cube,
            UnaryKind::Sqrt,
            UnaryKind::LeakyRelu(0.25),
            UnaryKind::LeakyRelu(-0.0),
            UnaryKind::Reciprocal,
            UnaryKind::Negate,
            UnaryKind::Squeeze,
            UnaryKind::Log,
            UnaryKind::Sign,
        ]
        .into_iter()
        .enumerate()
        {
            assert_matches_tree(&format!("{kind:?}"), &Sampler::Unary(kind, wild(i as u64)));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn binary_and_constant_ops_match_the_tree() {
        use Sampler::*;
        let (a, b) = (|| wild(1), || wild(2));
        let cases = [
            ("add", Add(a(), b())),
            ("sub", Sub(a(), b())),
            ("mul", Mul(a(), b())),
            ("div", Div(a(), b())),
            ("min", Min(a(), b(), 0.0)),
            ("max", Max(a(), b(), 0.0)),
            ("clamp", Clamp(a(), -1.0, 1.0)),
            ("clamp_zero", Clamp(a(), -0.0, 0.0)),
        ];
        for (name, sampler) in &cases {
            assert_matches_tree(name, sampler);
        }
        for c in [0.75f32, -0.0, 0.0, f32::NAN, f32::INFINITY, -2.0] {
            for sampler in [
                ConstAdd(a(), c),
                ConstSub(c, a()),
                ConstMul(a(), c),
                ConstDiv(c, a()),
                ConstMin(a(), c),
                ConstMax(a(), c),
            ] {
                assert_matches_tree(&format!("const {c} {sampler:?}"), &sampler);
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn lerp_and_range_choice_match_the_tree() {
        use Sampler::*;
        assert_matches_tree("lerp", &Lerp(wild(3), wild(4), wild(5)));
        for (min, max) in [
            (-0.5, 0.5),
            (0.0, 1.0),
            (-0.0, 0.0),
            (f32::NAN, 1.0),
            (-1.0, f32::NAN),
        ] {
            assert_matches_tree(
                &format!("range {min} {max}"),
                &RangeChoice {
                    input: wild(6),
                    min,
                    max,
                    in_range: wild(7),
                    out_of_range: wild(8),
                },
            );
        }
    }

    fn select(selector: Box<Sampler>, thresholds: Vec<f32>, wild_variant: bool) -> Sampler {
        let mut samplers = vec![
            *gradient(Axis::Y, GradientKind::Clamped { min: -8, max: 40 }),
            Sampler::ConstMul(gradient(Axis::X, GradientKind::Repeat { range: 5 }), 3.0),
            Sampler::Unary(
                UnaryKind::Square,
                gradient(Axis::Z, GradientKind::Mirrored { range: 7 }),
            ),
            Sampler::Constant(7.0),
            Sampler::Add(
                gradient(Axis::Y, GradientKind::Repeat { range: 3 }),
                gradient(Axis::X, GradientKind::Clamped { min: 0, max: 9 }),
            ),
        ];
        samplers.truncate(thresholds.len() + 1);
        if wild_variant {
            samplers[1] = *wild(99);
        }
        Sampler::IntervalSelect {
            input: selector,
            thresholds,
            samplers,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn interval_select_skips_unselected_variants_and_matches_the_tree() {
        let is_lazy = |op: &Op| matches!(op, Op::SelectUsed { .. });
        let is_eager = |op: &Op| matches!(op, Op::IntervalSelect { .. });
        for (name, thresholds) in [
            ("two", vec![0.0]),
            ("four", vec![-1.0, 0.0, 1.0]),
            ("five", vec![-2.0, -0.5, 0.5, 2.0]),
            ("tiny", vec![-100.0, -99.0, -98.0]),
        ] {
            for salt in 0..6u64 {
                let sampler = select(wild(salt), thresholds.clone(), false);
                let program = Program::build(&sampler, &mut ProgramMemo::default());
                assert_eq!(count_ops(&program, &is_lazy), 1, "{name} lazy");
                assert_eq!(count_ops(&program, &is_eager), 0);
                assert_matches_tree(&format!("{name} {salt}"), &sampler);

                let eager = select(wild(salt), thresholds.clone(), true);
                let program = Program::build(&eager, &mut ProgramMemo::default());
                assert_eq!(count_ops(&program, &is_lazy), 0, "{name} eager");
                assert_eq!(count_ops(&program, &is_eager), 1);
                assert_matches_tree(&format!("{name} eager {salt}"), &eager);
            }
        }
        for selector in [-5.0f32, -0.0, 0.0, 0.5, 3.0, f32::NAN] {
            let sampler = select(
                Box::new(Sampler::Constant(selector)),
                vec![-1.0, 0.0, 1.0],
                false,
            );
            assert_matches_tree(&format!("constant selector {selector}"), &sampler);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn column_broadcasts_match_the_tree() {
        for kind in [
            GradientKind::Clamped { min: -40, max: 70 },
            GradientKind::Repeat { range: 6 },
            GradientKind::Mirrored { range: 9 },
        ] {
            assert_matches_tree(&format!("gradient y {kind:?}"), &gradient(Axis::Y, kind));
            assert_matches_tree(&format!("gradient x {kind:?}"), &gradient(Axis::X, kind));
        }
        for (sx, sz) in [(0, 0), (-7, 12), (3, -2)] {
            assert_matches_tree("slice xz wild", &Sampler::SliceXz(wild(11), sx, sz));
            assert_matches_tree(
                "slice xz gradient",
                &Sampler::SliceXz(gradient(Axis::Y, GradientKind::Repeat { range: 4 }), sx, sz),
            );
        }
    }

    fn scalar_lerp(out: &mut [f32], f: &[f32], s: &[f32]) {
        for i in 0..out.len() {
            let a = out[i];
            out[i] = if a == 0.0 {
                f[i]
            } else if a == 1.0 {
                s[i]
            } else {
                lerp(a, f[i], s[i])
            };
        }
    }

    fn scalar_range(out: &mut [f32], values: &[f32], other: &[f32], min: f32, max: f32) {
        for i in 0..out.len() {
            let v = values[i];
            if v < min || v >= max {
                out[i] = other[i];
            }
        }
    }

    fn random_slice(salt: u64, len: usize) -> Vec<f32> {
        (0..len as u64)
            .map(|i| wild_value(mix(salt ^ mix(i))))
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn elementwise_kernels_match_their_scalar_definitions() {
        for len in (0..80).chain([255, 256, 1225]) {
            for salt in 0..40u64 {
                let (a, f, s) = (
                    random_slice(salt * 3, len),
                    random_slice(salt * 3 + 1, len),
                    random_slice(salt * 3 + 2, len),
                );
                let (mut fast, mut slow) = (a.clone(), a.clone());
                lerp_select(&mut fast, &f, &s);
                scalar_lerp(&mut slow, &f, &s);
                assert_eq!(canon(&fast), canon(&slow), "lerp len {len} salt {salt}");

                for (min, max) in [(-0.5, 0.5), (0.0, 1.0), (-0.0, 0.0), (f32::NAN, 0.5)] {
                    let (mut fast, mut slow) = (f.clone(), f.clone());
                    range_choice(&mut fast, &a, &s, min, max);
                    scalar_range(&mut slow, &a, &s, min, max);
                    assert_eq!(canon(&fast), canon(&slow), "range len {len} salt {salt}");
                }

                for kind in [
                    UnaryKind::Abs,
                    UnaryKind::Square,
                    UnaryKind::Cube,
                    UnaryKind::Sqrt,
                    UnaryKind::LeakyRelu(0.3),
                    UnaryKind::Reciprocal,
                    UnaryKind::Negate,
                    UnaryKind::Squeeze,
                    UnaryKind::Log,
                    UnaryKind::Sign,
                ] {
                    let (mut fast, mut slow) = (a.clone(), a.clone());
                    unary(&mut fast, kind);
                    for v in &mut slow {
                        *v = kind.apply(*v);
                    }
                    assert_eq!(canon(&fast), canon(&slow), "{kind:?} len {len} salt {salt}");
                }
            }
        }
    }
}

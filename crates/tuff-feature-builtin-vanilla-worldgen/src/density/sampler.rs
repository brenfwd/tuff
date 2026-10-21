//! Compiled density functions. A [`Sampler`] tree is immutable and shared
//! by every generation task; all mutable state (caches, scratch buffers,
//! the structures bound for this chunk) lives in the per-task [`Context`].
//!
//! Every node evaluates single blocks and whole volumes separately, in the
//! game's order, because the two paths round differently and caches hand
//! volume results to later single-block lookups.

use std::sync::Arc;

use super::function::{Axis, ContextKey, DistanceMetric, SplineShape, spline_sample};
use super::program::{Compiled, Program, ProgramMemo};
use super::{DensityVolume, java_max, java_min};
use crate::noise::{NoiseStack, Simplex, lerp, lerp3};

/// A sampler supplied through the context: the structure beardifier, or a
/// plugin's binding.
pub trait BoundSampler: Send + Sync {
    fn sample_value(&self, x: i32, y: i32, z: i32) -> f32;
    fn sample_volume(&self, out: &mut [f32], volume: &DensityVolume);
}

/// Pooled buffer that keeps its high-water length so resizing within it never zero-fills.
#[derive(Default)]
pub struct Buf {
    data: Vec<f32>,
    len: usize,
}

impl Buf {
    pub(super) fn set_len(&mut self, size: usize) {
        if self.data.len() < size {
            self.data.resize(size, 0.0);
        }
        self.len = size;
        #[cfg(debug_assertions)]
        self.data[..size].fill(f32::NAN);
    }
}

impl std::ops::Deref for Buf {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.data[..self.len]
    }
}

impl std::ops::DerefMut for Buf {
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.data[..self.len]
    }
}

#[derive(Default)]
struct CacheCell {
    volume: Option<DensityVolume>,
    buffer: Buf,
    value_key: Option<(i32, i32, i32)>,
    value: f32,
}

#[derive(Default)]
struct Scratch {
    pool: Vec<Buf>,
    registers: Vec<(usize, Vec<Buf>)>,
}

const MAX_KEPT_REGISTER_SETS: usize = 512;

thread_local! {
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::default());
}

fn take_scratch() -> Scratch {
    SCRATCH
        .try_with(|s| std::mem::take(&mut *s.borrow_mut()))
        .unwrap_or_default()
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        let mut registers = std::mem::take(&mut self.registers);
        registers.truncate(MAX_KEPT_REGISTER_SETS);
        let scratch = Scratch {
            pool: std::mem::take(&mut self.pool),
            registers,
        };
        let _ = SCRATCH.try_with(|s| *s.borrow_mut() = scratch);
    }
}

/// Per-task evaluation state; scratch buffers persist per thread across contexts.
pub struct Context<'a> {
    caches: Option<Vec<CacheCell>>,
    pool: Vec<Buf>,
    registers: Vec<(usize, Vec<Buf>)>,
    beardifier: Option<&'a dyn BoundSampler>,
}

#[derive(Default)]
pub struct ContextBuilder<'a> {
    caches: bool,
    beardifier: Option<&'a dyn BoundSampler>,
}

impl<'a> ContextBuilder<'a> {
    pub fn caches(mut self) -> Self {
        self.caches = true;
        self
    }

    pub fn beardifier(mut self, beardifier: &'a dyn BoundSampler) -> Self {
        self.beardifier = Some(beardifier);
        self
    }

    pub fn build(self) -> Context<'a> {
        let scratch = take_scratch();
        Context {
            caches: self.caches.then(Vec::new),
            pool: scratch.pool,
            registers: scratch.registers,
            beardifier: self.beardifier,
        }
    }
}

impl<'a> Context<'a> {
    pub fn builder() -> ContextBuilder<'a> {
        ContextBuilder::default()
    }

    pub fn uncached() -> Context<'static> {
        let scratch = take_scratch();
        Context {
            caches: None,
            pool: scratch.pool,
            registers: scratch.registers,
            beardifier: None,
        }
    }

    /// Returns a buffer of `size` elements with unspecified contents: callers
    /// must write every element before reading it (debug builds poison it with NaN).
    pub fn acquire(&mut self, size: usize) -> Buf {
        let mut buffer = self.pool.pop().unwrap_or_default();
        buffer.set_len(size);
        buffer
    }

    pub fn release(&mut self, buffer: Buf) {
        self.pool.push(buffer);
    }

    pub(super) fn take_registers(&mut self, key: usize) -> Vec<Buf> {
        match self.registers.iter().position(|(k, _)| *k == key) {
            Some(i) => self.registers.swap_remove(i).1,
            None => Vec::new(),
        }
    }

    pub(super) fn put_registers(&mut self, key: usize, set: Vec<Buf>) {
        self.registers.push((key, set));
    }

    pub(super) fn bound(&self, key: ContextKey) -> Option<&'a dyn BoundSampler> {
        match key {
            ContextKey::Beardifier => self.beardifier,
            ContextKey::BlendAlpha | ContextKey::BlendOffset => None,
        }
    }

    fn cell(&mut self, id: usize) -> Option<&mut CacheCell> {
        let cells = self.caches.as_mut()?;
        if id >= cells.len() {
            let new_len = (id + 1).div_ceil(16) * 16;
            cells.resize_with(new_len, || CacheCell {
                value: f32::NAN,
                ..CacheCell::default()
            });
        }
        Some(&mut cells[id])
    }

    pub(super) fn sample_volume_cached(
        &mut self,
        id: usize,
        out: &mut [f32],
        volume: &DensityVolume,
        eval: &mut dyn FnMut(&mut Context, &mut [f32], &DensityVolume),
    ) {
        let Some(cell) = self.cell(id) else {
            eval(self, out, volume);
            return;
        };
        if cell.volume.as_ref() != Some(volume) {
            let old = std::mem::take(&mut cell.buffer);
            cell.volume = Some(*volume);
            self.release(old);
            let mut buffer = self.acquire(volume.size());
            eval(self, &mut buffer, volume);
            let cell = self.cell(id).expect("cell exists");
            cell.buffer = buffer;
        }
        let cell = self.cell(id).expect("cell exists");
        out.copy_from_slice(&cell.buffer[..out.len()]);
    }

    fn sample_value_cached(&mut self, id: usize, input: &Sampler, x: i32, y: i32, z: i32) -> f32 {
        let Some(cell) = self.cell(id) else {
            return input.sample_value(self, x, y, z);
        };
        if cell.value_key == Some((x, y, z)) && !cell.value.is_nan() {
            return cell.value;
        }
        if let Some(volume) = cell.volume
            && !cell.buffer.is_empty()
            && let Some(index) = volume.index_of_block(x, y, z)
        {
            return cell.buffer[index];
        }
        let value = input.sample_value(self, x, y, z);
        let cell = self.cell(id).expect("cell exists");
        cell.value_key = Some((x, y, z));
        cell.value = value;
        value
    }
}

#[derive(Debug, Clone, Copy)]
pub enum UnaryKind {
    Abs,
    Square,
    Cube,
    Sqrt,
    LeakyRelu(f32),
    Reciprocal,
    Negate,
    Squeeze,
    Log,
    Sign,
}

impl UnaryKind {
    pub(super) fn apply(self, v: f32) -> f32 {
        match self {
            UnaryKind::Abs => v.abs(),
            UnaryKind::Square => v * v,
            UnaryKind::Cube => v * v * v,
            UnaryKind::Sqrt => (v as f64).sqrt() as f32,
            UnaryKind::LeakyRelu(f) => super::function::leaky_relu(f, v),
            UnaryKind::Reciprocal => 1.0 / v,
            UnaryKind::Negate => -v,
            UnaryKind::Squeeze => super::function::squeeze(v),
            UnaryKind::Log => (v as f64).ln() as f32,
            UnaryKind::Sign => super::interval::java_signum(v),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum GradientKind {
    Clamped { min: i32, max: i32 },
    Repeat { range: i32 },
    Mirrored { range: i32 },
}

#[derive(Debug, Clone)]
pub struct GradientSampler {
    pub axis: Axis,
    pub kind: GradientKind,
    pub from: i32,
    pub from_value: f32,
    pub factor: f32,
}

impl GradientSampler {
    pub(super) fn compute(&self, coordinate: i32) -> f32 {
        match self.kind {
            GradientKind::Clamped { min, max } => {
                let relative = coordinate.clamp(min, max) - self.from;
                self.from_value + relative as f32 * self.factor
            }
            GradientKind::Repeat { range } => {
                let relative = coordinate - self.from;
                self.from_value + relative.rem_euclid(range) as f32 * self.factor
            }
            GradientKind::Mirrored { range } => {
                let relative = coordinate - self.from;
                let tile = relative.div_euclid(range);
                let local = relative - tile * range;
                if tile & 1 == 0 {
                    self.from_value + local as f32 * self.factor
                } else {
                    self.from_value + (range - local) as f32 * self.factor
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum SplineNode {
    Constant(f32),
    Multipoint {
        coordinate: usize,
        locations: Vec<f32>,
        values: Vec<SplineNode>,
        derivatives: Vec<f32>,
    },
}

pub(super) fn spline_shape(node: &SplineNode) -> SplineShape<'_, SplineNode> {
    match node {
        SplineNode::Constant(v) => SplineShape::Constant(*v),
        SplineNode::Multipoint {
            coordinate,
            locations,
            values,
            derivatives,
        } => SplineShape::Multipoint {
            coordinate: *coordinate,
            locations,
            values,
            derivatives,
        },
    }
}

#[derive(Debug, Clone)]
pub enum Sampler {
    Constant(f32),
    Noise {
        noise: Arc<NoiseStack>,
        xz_scale: f64,
        y_scale: f64,
    },
    ShiftedNoise {
        shift_x: Box<Sampler>,
        shift_y: Option<Box<Sampler>>,
        shift_z: Box<Sampler>,
        noise: Arc<NoiseStack>,
        xz_scale: f64,
        y_scale: f64,
    },
    ShiftB(Arc<NoiseStack>),
    EndIslands(Arc<Simplex>),
    Distance {
        point: [i32; 3],
        metric: DistanceMetric,
    },
    Gradient(GradientSampler),
    Unary(UnaryKind, Box<Sampler>),
    Add(Box<Sampler>, Box<Sampler>),
    ConstAdd(Box<Sampler>, f32),
    Sub(Box<Sampler>, Box<Sampler>),
    ConstSub(f32, Box<Sampler>),
    Mul(Box<Sampler>, Box<Sampler>),
    ConstMul(Box<Sampler>, f32),
    Div(Box<Sampler>, Box<Sampler>),
    ConstDiv(f32, Box<Sampler>),
    Pow(Box<Sampler>, Box<Sampler>),
    Round(super::function::RoundMode, Box<Sampler>, Box<Sampler>),
    Min(Box<Sampler>, Box<Sampler>, f32),
    ConstMin(Box<Sampler>, f32),
    Max(Box<Sampler>, Box<Sampler>, f32),
    ConstMax(Box<Sampler>, f32),
    Clamp(Box<Sampler>, f32, f32),
    Lerp(Box<Sampler>, Box<Sampler>, Box<Sampler>),
    RangeChoice {
        input: Box<Sampler>,
        min: f32,
        max: f32,
        in_range: Box<Sampler>,
        out_of_range: Box<Sampler>,
    },
    IntervalSelect {
        input: Box<Sampler>,
        thresholds: Vec<f32>,
        samplers: Vec<Sampler>,
    },
    Cache {
        id: usize,
        input: Arc<Sampler>,
    },
    Shared(Arc<Sampler>),
    Interpolated {
        input: Box<Sampler>,
        cell_xz: i32,
        cell_y: i32,
        inv_xz: f32,
        inv_y: f32,
    },
    SliceX(Box<Sampler>, i32),
    SliceY(Box<Sampler>, i32),
    SliceZ(Box<Sampler>, i32),
    SliceXz(Box<Sampler>, i32, i32),
    Spline {
        root: SplineNode,
        coordinates: Vec<Sampler>,
    },
    FindTopSurface {
        density: Box<Sampler>,
        upper_bound: Box<Sampler>,
        lower_bound: i32,
        cell_height: i32,
    },
    BlendDensity(Box<Sampler>),
    ContextBound(ContextKey, Box<Sampler>),
    Custom(Arc<dyn CustomSampler>),
    Compiled(Box<Compiled>),
}

/// A plugin-provided sampler.
pub trait CustomSampler: Send + Sync + std::fmt::Debug {
    fn sample_value(&self, ctx: &mut Context, x: i32, y: i32, z: i32) -> f32;
    fn sample_volume(&self, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
        let mut index = 0;
        for z in 0..volume.size_z {
            for x in 0..volume.size_x {
                for y in 0..volume.size_y {
                    out[index] = self.sample_value(
                        ctx,
                        volume.block_x(x),
                        volume.block_y(y),
                        volume.block_z(z),
                    );
                    index += 1;
                }
            }
        }
    }
}

fn floor_mod(a: i32, b: i32) -> i32 {
    a.rem_euclid(b)
}

impl Sampler {
    pub fn sample_value(&self, ctx: &mut Context, x: i32, y: i32, z: i32) -> f32 {
        use Sampler::*;
        match self {
            Constant(v) => *v,
            Noise {
                noise,
                xz_scale,
                y_scale,
            } => noise.get(x as f64 * xz_scale, y as f64 * y_scale, z as f64 * xz_scale),
            ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                let nx = x as f64 * xz_scale + shift_x.sample_value(ctx, x, y, z) as f64;
                let ny = match shift_y {
                    None => y as f64 * y_scale,
                    Some(shift) => y as f64 * y_scale + shift.sample_value(ctx, x, y, z) as f64,
                };
                let nz = z as f64 * xz_scale + shift_z.sample_value(ctx, x, y, z) as f64;
                noise.get(nx, ny, nz)
            }
            ShiftB(noise) => noise.get(z as f64 * 0.25, x as f64 * 0.25, 0.0) * 4.0,
            EndIslands(noise) => (end_island_height(noise, x / 8, z / 8) - 8.0) / 128.0,
            Distance { point, metric } => metric.compute(
                (point[0] - x) as f32,
                (point[1] - y) as f32,
                (point[2] - z) as f32,
            ),
            Gradient(g) => g.compute(g.axis.choose(x, y, z)),
            Unary(kind, input) => kind.apply(input.sample_value(ctx, x, y, z)),
            Add(l, r) => l.sample_value(ctx, x, y, z) + r.sample_value(ctx, x, y, z),
            ConstAdd(l, c) => l.sample_value(ctx, x, y, z) + c,
            Sub(l, r) => l.sample_value(ctx, x, y, z) - r.sample_value(ctx, x, y, z),
            ConstSub(c, r) => c - r.sample_value(ctx, x, y, z),
            Mul(l, r) => {
                let left = l.sample_value(ctx, x, y, z);
                if left == 0.0 {
                    return 0.0;
                }
                left * r.sample_value(ctx, x, y, z)
            }
            ConstMul(l, c) => l.sample_value(ctx, x, y, z) * c,
            Div(l, r) => {
                let left = l.sample_value(ctx, x, y, z);
                if left == 0.0 {
                    return 0.0;
                }
                left / r.sample_value(ctx, x, y, z)
            }
            ConstDiv(c, r) => c / r.sample_value(ctx, x, y, z),
            Pow(l, r) => (l.sample_value(ctx, x, y, z) as f64)
                .powf(r.sample_value(ctx, x, y, z) as f64) as f32,
            Round(mode, l, r) => {
                mode.round_to_multiple(l.sample_value(ctx, x, y, z), r.sample_value(ctx, x, y, z))
            }
            Min(l, r, right_min) => {
                let left = l.sample_value(ctx, x, y, z);
                if left <= *right_min {
                    return left;
                }
                java_min(left, r.sample_value(ctx, x, y, z))
            }
            ConstMin(l, c) => java_min(l.sample_value(ctx, x, y, z), *c),
            Max(l, r, right_max) => {
                let left = l.sample_value(ctx, x, y, z);
                if left >= *right_max {
                    return left;
                }
                java_max(left, r.sample_value(ctx, x, y, z))
            }
            ConstMax(l, c) => java_max(l.sample_value(ctx, x, y, z), *c),
            Clamp(input, min, max) => clamp(input.sample_value(ctx, x, y, z), *min, *max),
            Lerp(alpha, first, second) => {
                let a = alpha.sample_value(ctx, x, y, z);
                if a == 0.0 {
                    return first.sample_value(ctx, x, y, z);
                }
                if a == 1.0 {
                    return second.sample_value(ctx, x, y, z);
                }
                let f = first.sample_value(ctx, x, y, z);
                let s = second.sample_value(ctx, x, y, z);
                lerp(a, f, s)
            }
            RangeChoice {
                input,
                min,
                max,
                in_range,
                out_of_range,
            } => {
                let v = input.sample_value(ctx, x, y, z);
                if v >= *min && v < *max {
                    in_range.sample_value(ctx, x, y, z)
                } else {
                    out_of_range.sample_value(ctx, x, y, z)
                }
            }
            IntervalSelect {
                input,
                thresholds,
                samplers,
            } => {
                let v = input.sample_value(ctx, x, y, z);
                samplers[select_index(thresholds, samplers.len(), v)].sample_value(ctx, x, y, z)
            }
            Cache { id, input } => ctx.sample_value_cached(*id, input, x, y, z),
            Shared(inner) => inner.sample_value(ctx, x, y, z),
            Interpolated {
                input,
                cell_xz,
                cell_y,
                ..
            } => {
                let (cx, cy) = (*cell_xz, *cell_y);
                let (ix, iy, iz) = (floor_mod(x, cx), floor_mod(y, cy), floor_mod(z, cx));
                if ix == 0 && iy == 0 && iz == 0 {
                    return input.sample_value(ctx, x, y, z);
                }
                let volume =
                    DensityVolume::stepped([2, 2, 2], [x - ix, y - iy, z - iz], [cx, cy, cx]);
                let mut buffer = ctx.acquire(volume.size());
                input.sample_volume(ctx, &mut buffer, &volume);
                let at = |x, y, z| buffer[volume.index(x, y, z)];
                let value = lerp3(
                    ix as f32 / cx as f32,
                    iy as f32 / cy as f32,
                    iz as f32 / cx as f32,
                    at(0, 0, 0),
                    at(1, 0, 0),
                    at(0, 1, 0),
                    at(1, 1, 0),
                    at(0, 0, 1),
                    at(1, 0, 1),
                    at(0, 1, 1),
                    at(1, 1, 1),
                );
                ctx.release(buffer);
                value
            }
            SliceX(input, sx) => input.sample_value(ctx, *sx, y, z),
            SliceY(input, sy) => input.sample_value(ctx, x, *sy, z),
            SliceZ(input, sz) => input.sample_value(ctx, x, y, *sz),
            SliceXz(input, sx, sz) => input.sample_value(ctx, *sx, y, *sz),
            Spline { root, coordinates } => {
                let mut cached = vec![f32::NAN; coordinates.len()];
                spline_sample(root, &spline_shape, &mut |index| {
                    if !cached[index].is_nan() {
                        return cached[index];
                    }
                    let value = coordinates[index].sample_value(ctx, x, y, z);
                    cached[index] = value;
                    value
                })
            }
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let upper = upper_bound.sample_value(ctx, x, y, z);
                find_top_surface(ctx, density, *lower_bound, *cell_height, x, z, upper)
            }
            BlendDensity(input) => input.sample_value(ctx, x, y, z),
            ContextBound(key, fallback) => match ctx.bound(*key) {
                Some(bound) => bound.sample_value(x, y, z),
                None => fallback.sample_value(ctx, x, y, z),
            },
            Custom(custom) => custom.sample_value(ctx, x, y, z),
            Compiled(c) => c.tree.sample_value(ctx, x, y, z),
        }
    }

    /// True when evaluating this leaves no trace in the context (no caches
    /// or bound samplers), so evaluations may be skipped or reordered.
    pub fn is_stateless(&self) -> bool {
        match self {
            Sampler::Compiled(c) => c.program.is_stateless(),
            other => Program::build(other, &mut ProgramMemo::default()).is_stateless(),
        }
    }

    /// The cache cells this sampler touches, or `None` when it has other
    /// context state.
    pub fn cache_ids(&self) -> Option<Vec<usize>> {
        let mut ids = Vec::new();
        let ok = match self {
            Sampler::Compiled(c) => c.program.cache_ids(&mut ids),
            other => Program::build(other, &mut ProgramMemo::default()).cache_ids(&mut ids),
        };
        ok.then_some(ids)
    }

    pub fn sample_volume(&self, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
        match self {
            Sampler::Compiled(c) if c.interpreted => c.tree.sample_volume_tree(ctx, out, volume),
            Sampler::Compiled(c) => c.program.run(ctx, out, volume),
            other => Program::build(other, &mut ProgramMemo::default()).run(ctx, out, volume),
        }
    }

    /// Fills `out` by walking the tree node by node, without the op-list
    /// program: the reference the programs are tested against, and the
    /// interpreter loaded data runs on when compiling is off.
    pub(crate) fn sample_volume_tree(
        &self,
        ctx: &mut Context,
        out: &mut [f32],
        volume: &DensityVolume,
    ) {
        use Sampler::*;
        match self {
            Constant(v) => out.fill(*v),
            Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                out.fill(0.0);
                noise.add_to_volume(out, volume, *xz_scale, *y_scale, 1.0);
            }
            ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                noise,
                xz_scale,
                y_scale,
            } => {
                shift_x.sample_volume_tree(ctx, out, volume);
                let shift_y_buffer = shift_y.as_ref().map(|shift| {
                    let mut buffer = ctx.acquire(volume.size());
                    shift.sample_volume_tree(ctx, &mut buffer, volume);
                    buffer
                });
                let mut shift_z_buffer = ctx.acquire(volume.size());
                shift_z.sample_volume_tree(ctx, &mut shift_z_buffer, volume);
                let mut index = 0;
                for z in 0..volume.size_z {
                    let base_z = volume.block_z(z) as f64 * xz_scale;
                    for x in 0..volume.size_x {
                        let base_x = volume.block_x(x) as f64 * xz_scale;
                        for y in 0..volume.size_y {
                            let nx = base_x + out[index] as f64;
                            let ny = match &shift_y_buffer {
                                None => volume.block_y(y) as f64 * y_scale,
                                Some(b) => volume.block_y(y) as f64 * y_scale + b[index] as f64,
                            };
                            let nz = base_z + shift_z_buffer[index] as f64;
                            out[index] = noise.get(nx, ny, nz);
                            index += 1;
                        }
                    }
                }
                ctx.release(shift_z_buffer);
                if let Some(b) = shift_y_buffer {
                    ctx.release(b);
                }
            }
            ShiftB(noise) => {
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
            EndIslands(_) => {
                for z in 0..volume.size_z {
                    let bz = volume.block_z(z);
                    for x in 0..volume.size_x {
                        let bx = volume.block_x(x);
                        let value = self.sample_value(ctx, bx, 0, bz);
                        let start = volume.index(x, 0, z);
                        out[start..start + volume.size_y as usize].fill(value);
                    }
                }
            }
            Distance { .. } => naive_volume(self, ctx, out, volume),
            Gradient(g) => match g.axis {
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
                    for y in 0..volume.size_y {
                        let value = g.compute(volume.block_y(y));
                        for z in 0..volume.size_z {
                            for x in 0..volume.size_x {
                                out[volume.index(x, y, z)] = value;
                            }
                        }
                    }
                }
                Axis::Z => {
                    for z in 0..volume.size_z {
                        let value = g.compute(volume.block_z(z));
                        let start = volume.index(0, 0, z);
                        out[start..start + (volume.size_x * volume.size_y) as usize].fill(value);
                    }
                }
            },
            Unary(kind, input) => {
                input.sample_volume_tree(ctx, out, volume);
                for v in out.iter_mut() {
                    *v = kind.apply(*v);
                }
            }
            Add(l, r) => binary_volume(ctx, out, volume, l, r, |a, b| a + b),
            Sub(l, r) => binary_volume(ctx, out, volume, l, r, |a, b| a + -b),
            Mul(l, r) => binary_volume(ctx, out, volume, l, r, |a, b| a * b),
            Div(l, r) => binary_volume(ctx, out, volume, l, r, |a, b| a / b),
            Pow(l, r) => binary_volume(ctx, out, volume, l, r, |a, b| {
                (a as f64).powf(b as f64) as f32
            }),
            Round(mode, l, r) => {
                let mode = *mode;
                binary_volume(ctx, out, volume, l, r, move |a, b| {
                    mode.round_to_multiple(a, b)
                })
            }
            Min(l, r, _) => binary_volume(ctx, out, volume, l, r, |a, b| if b < a { b } else { a }),
            Max(l, r, _) => binary_volume(ctx, out, volume, l, r, |a, b| if b > a { b } else { a }),
            ConstAdd(l, c) => {
                l.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| *v += c);
            }
            ConstSub(c, r) => {
                r.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| *v = c - *v);
            }
            ConstMul(l, c) => {
                l.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| *v *= c);
            }
            ConstDiv(c, r) => {
                r.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| *v = c / *v);
            }
            ConstMin(l, c) => {
                l.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| {
                    if *c < *v {
                        *v = *c
                    }
                });
            }
            ConstMax(l, c) => {
                l.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| {
                    if *c > *v {
                        *v = *c
                    }
                });
            }
            Clamp(input, min, max) => {
                input.sample_volume_tree(ctx, out, volume);
                out.iter_mut().for_each(|v| *v = clamp(*v, *min, *max));
            }
            Lerp(alpha, first, second) => {
                alpha.sample_volume_tree(ctx, out, volume);
                let mut f = ctx.acquire(volume.size());
                first.sample_volume_tree(ctx, &mut f, volume);
                let mut s = ctx.acquire(volume.size());
                second.sample_volume_tree(ctx, &mut s, volume);
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
                ctx.release(s);
                ctx.release(f);
            }
            RangeChoice {
                input,
                min,
                max,
                in_range,
                out_of_range,
            } => {
                in_range.sample_volume_tree(ctx, out, volume);
                let mut values = ctx.acquire(volume.size());
                input.sample_volume_tree(ctx, &mut values, volume);
                let mut other = ctx.acquire(volume.size());
                out_of_range.sample_volume_tree(ctx, &mut other, volume);
                for i in 0..out.len() {
                    let v = values[i];
                    if v < *min || v >= *max {
                        out[i] = other[i];
                    }
                }
                ctx.release(other);
                ctx.release(values);
            }
            IntervalSelect {
                input,
                thresholds,
                samplers,
            } => {
                input.sample_volume_tree(ctx, out, volume);
                let mut buffers = Vec::with_capacity(samplers.len());
                for sampler in samplers {
                    let mut buffer = ctx.acquire(volume.size());
                    sampler.sample_volume_tree(ctx, &mut buffer, volume);
                    buffers.push(buffer);
                }
                for (i, slot) in out.iter_mut().enumerate() {
                    let index = select_index(thresholds, samplers.len(), *slot);
                    *slot = buffers[index][i];
                }
                for buffer in buffers {
                    ctx.release(buffer);
                }
            }
            Cache { id, input } => {
                ctx.sample_volume_cached(*id, out, volume, &mut |ctx, out, volume| {
                    input.sample_volume_tree(ctx, out, volume)
                })
            }
            Shared(inner) => inner.sample_volume_tree(ctx, out, volume),
            Interpolated {
                input,
                cell_xz,
                cell_y,
                inv_xz,
                inv_y,
            } => interpolated_volume(
                ctx,
                out,
                volume,
                &mut |ctx, out, volume| input.sample_volume_tree(ctx, out, volume),
                *cell_xz,
                *cell_y,
                *inv_xz,
                *inv_y,
            ),
            SliceX(input, sx) => {
                if volume.size_x == 1 && volume.min_x == *sx {
                    input.sample_volume_tree(ctx, out, volume);
                    return;
                }
                let inner = DensityVolume::stepped(
                    [1, volume.size_y, volume.size_z],
                    [*sx, volume.min_y, volume.min_z],
                    [volume.step_x, volume.step_y, volume.step_z],
                );
                let mut buffer = ctx.acquire(inner.size());
                input.sample_volume_tree(ctx, &mut buffer, &inner);
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
            SliceY(input, sy) => {
                if volume.size_y == 1 && volume.min_y == *sy {
                    input.sample_volume_tree(ctx, out, volume);
                    return;
                }
                let inner = DensityVolume::stepped(
                    [volume.size_x, 1, volume.size_z],
                    [volume.min_x, *sy, volume.min_z],
                    [volume.step_x, volume.step_y, volume.step_z],
                );
                let mut buffer = ctx.acquire(inner.size());
                input.sample_volume_tree(ctx, &mut buffer, &inner);
                for z in 0..volume.size_z {
                    for x in 0..volume.size_x {
                        let value = buffer[inner.index(x, 0, z)];
                        let start = volume.index(x, 0, z);
                        out[start..start + volume.size_y as usize].fill(value);
                    }
                }
                ctx.release(buffer);
            }
            SliceZ(input, sz) => {
                if volume.size_z == 1 && volume.min_z == *sz {
                    input.sample_volume_tree(ctx, out, volume);
                    return;
                }
                let inner = DensityVolume::stepped(
                    [volume.size_x, volume.size_y, 1],
                    [volume.min_x, volume.min_y, *sz],
                    [volume.step_x, volume.step_y, volume.step_z],
                );
                let mut buffer = ctx.acquire(inner.size());
                input.sample_volume_tree(ctx, &mut buffer, &inner);
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
            SliceXz(input, sx, sz) => {
                if volume.size_x == 1
                    && volume.size_z == 1
                    && volume.min_x == *sx
                    && volume.min_z == *sz
                {
                    input.sample_volume_tree(ctx, out, volume);
                    return;
                }
                let inner = DensityVolume::stepped(
                    [1, volume.size_y, 1],
                    [*sx, volume.min_y, *sz],
                    [volume.step_x, volume.step_y, volume.step_z],
                );
                let mut buffer = ctx.acquire(inner.size());
                input.sample_volume_tree(ctx, &mut buffer, &inner);
                for y in 0..volume.size_y {
                    let value = buffer[inner.index(0, y, 0)];
                    let mut index = volume.index(0, y, 0);
                    for _ in 0..volume.size_z {
                        for _ in 0..volume.size_x {
                            out[index] = value;
                            index += volume.size_y as usize;
                        }
                    }
                }
                ctx.release(buffer);
            }
            Spline { root, coordinates } => {
                let mut buffers: Vec<Option<Buf>> = (0..coordinates.len()).map(|_| None).collect();
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = spline_sample(root, &spline_shape, &mut |index| {
                        if buffers[index].is_none() {
                            let mut buffer = ctx.acquire(volume.size());
                            coordinates[index].sample_volume_tree(ctx, &mut buffer, volume);
                            buffers[index] = Some(buffer);
                        }
                        buffers[index].as_ref().unwrap()[i]
                    });
                }
                for buffer in buffers.into_iter().flatten() {
                    ctx.release(buffer);
                }
            }
            FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                assert!(volume.size_y == 1, "top surface volumes are one block tall");
                upper_bound.sample_volume_tree(ctx, out, volume);
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
            BlendDensity(input) => input.sample_volume_tree(ctx, out, volume),
            ContextBound(key, fallback) => match ctx.bound(*key) {
                Some(bound) => bound.sample_volume(out, volume),
                None => fallback.sample_volume_tree(ctx, out, volume),
            },
            Custom(custom) => custom.sample_volume(ctx, out, volume),
            Compiled(c) => c.tree.sample_volume_tree(ctx, out, volume),
        }
    }
}

pub(super) fn clamp(v: f32, min: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

pub(super) fn select_index(thresholds: &[f32], count: usize, input: f32) -> usize {
    thresholds
        .iter()
        .position(|&t| input < t)
        .unwrap_or(count - 1)
}

fn naive_volume(sampler: &Sampler, ctx: &mut Context, out: &mut [f32], volume: &DensityVolume) {
    let mut index = 0;
    for z in 0..volume.size_z {
        let bz = volume.block_z(z);
        for x in 0..volume.size_x {
            let bx = volume.block_x(x);
            for y in 0..volume.size_y {
                out[index] = sampler.sample_value(ctx, bx, volume.block_y(y), bz);
                index += 1;
            }
        }
    }
}

fn binary_volume(
    ctx: &mut Context,
    out: &mut [f32],
    volume: &DensityVolume,
    left: &Sampler,
    right: &Sampler,
    op: impl Fn(f32, f32) -> f32,
) {
    left.sample_volume_tree(ctx, out, volume);
    let mut buffer = ctx.acquire(volume.size());
    right.sample_volume_tree(ctx, &mut buffer, volume);
    for (o, r) in out.iter_mut().zip(buffer.iter()) {
        *o = op(*o, *r);
    }
    ctx.release(buffer);
}

pub(super) fn find_top_surface(
    ctx: &mut Context,
    density: &Sampler,
    lower_bound: i32,
    cell_height: i32,
    x: i32,
    z: i32,
    upper: f32,
) -> f32 {
    let top = (upper / cell_height as f32).floor() as i32 * cell_height;
    if top <= lower_bound {
        return lower_bound as f32;
    }
    let mut probe = top;
    while probe >= lower_bound {
        let sampled = density.sample_value(ctx, x, probe, z);
        if sampled.is_nan() || sampled > 0.0 {
            return probe as f32;
        }
        probe -= cell_height;
    }
    lower_bound as f32
}

const ISLAND_SEARCH_RADIUS: i32 = 12;
const ISLAND_FREE_RADIUS_SQ: i64 = 64 * 64;
const ISLAND_FLOOR: f32 = -100.0;

fn end_island_height(noise: &Simplex, section_x: i32, section_z: i32) -> f32 {
    let (home_x, home_z) = (section_x / 2, section_z / 2);
    let (half_x, half_z) = (section_x % 2, section_z % 2);
    let mut highest = ISLAND_FLOOR;
    for dx in -ISLAND_SEARCH_RADIUS..=ISLAND_SEARCH_RADIUS {
        let island_x = (home_x + dx) as i64;
        let reach_x = (half_x - 2 * dx) as f32;
        for dz in -ISLAND_SEARCH_RADIUS..=ISLAND_SEARCH_RADIUS {
            let island_z = (home_z + dz) as i64;
            if island_x * island_x + island_z * island_z <= ISLAND_FREE_RADIUS_SQ {
                continue;
            }
            let reach_z = (half_z - 2 * dz) as f32;
            let peak = island_peak(island_x, island_z, reach_x, reach_z);
            if peak > highest && noise.get_2d(island_x as f64, island_z as f64) < -0.9 {
                highest = peak;
            }
        }
    }
    highest
}

fn island_peak(island_x: i64, island_z: i64, reach_x: f32, reach_z: f32) -> f32 {
    let falloff = ((island_x as f32).abs() * 3439.0 + (island_z as f32).abs() * 147.0) % 13.0 + 9.0;
    let distance = ((reach_x * reach_x + reach_z * reach_z) as f64).sqrt() as f32;
    (100.0 - distance * falloff).clamp(ISLAND_FLOOR, 80.0)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn interpolated_volume(
    ctx: &mut Context,
    out: &mut [f32],
    volume: &DensityVolume,
    eval: &mut dyn FnMut(&mut Context, &mut [f32], &DensityVolume),
    cell_xz: i32,
    cell_y: i32,
    inv_xz: f32,
    inv_y: f32,
) {
    let aligned = (volume.step_x == cell_xz || volume.size_x == 1)
        && (volume.step_y == cell_y || volume.size_y == 1)
        && (volume.step_z == cell_xz || volume.size_z == 1)
        && floor_mod(volume.min_x, cell_xz) == 0
        && floor_mod(volume.min_y, cell_y) == 0
        && floor_mod(volume.min_z, cell_xz) == 0;
    if aligned {
        eval(ctx, out, volume);
    } else if volume.step_x != 1 || volume.step_y != 1 || volume.step_z != 1 {
        let blocks = DensityVolume::blocks(
            [
                volume.size_x * volume.step_x,
                volume.size_y * volume.step_y,
                volume.size_z * volume.step_z,
            ],
            [volume.min_x, volume.min_y, volume.min_z],
        );
        let mut buffer = ctx.acquire(blocks.size());
        interpolate_blocks(
            ctx,
            &mut buffer,
            &blocks,
            eval,
            cell_xz,
            cell_y,
            inv_xz,
            inv_y,
        );
        for z in 0..volume.size_z {
            for x in 0..volume.size_x {
                for y in 0..volume.size_y {
                    out[volume.index(x, y, z)] = buffer
                        [blocks.index(x * volume.step_x, y * volume.step_y, z * volume.step_z)];
                }
            }
        }
        ctx.release(buffer);
    } else {
        interpolate_blocks(ctx, out, volume, eval, cell_xz, cell_y, inv_xz, inv_y);
    }
}

#[allow(clippy::too_many_arguments)]
fn interpolate_blocks(
    ctx: &mut Context,
    out: &mut [f32],
    volume: &DensityVolume,
    eval: &mut dyn FnMut(&mut Context, &mut [f32], &DensityVolume),
    cell_xz: i32,
    cell_y: i32,
    inv_xz: f32,
    inv_y: f32,
) {
    let min_cx = volume.min_x.div_euclid(cell_xz);
    let min_cy = volume.min_y.div_euclid(cell_y);
    let min_cz = volume.min_z.div_euclid(cell_xz);
    let count_x = volume.max_x().div_euclid(cell_xz) - min_cx + 1;
    let count_y = volume.max_y().div_euclid(cell_y) - min_cy + 1;
    let count_z = volume.max_z().div_euclid(cell_xz) - min_cz + 1;
    let extra = |max: i32, cell: i32, count: i32| {
        if floor_mod(max, cell) == 0 {
            count
        } else {
            count + 1
        }
    };
    let cells = DensityVolume::stepped(
        [
            extra(volume.max_x(), cell_xz, count_x),
            extra(volume.max_y(), cell_y, count_y),
            extra(volume.max_z(), cell_xz, count_z),
        ],
        [min_cx * cell_xz, min_cy * cell_y, min_cz * cell_xz],
        [cell_xz, cell_y, cell_xz],
    );
    let mut buffer = ctx.acquire(cells.size());
    eval(ctx, &mut buffer, &cells);
    for cz in 0..count_z {
        let next_z = (cz + 1).min(cells.size_z - 1);
        for cx in 0..count_x {
            let next_x = (cx + 1).min(cells.size_x - 1);
            let mut v000 = buffer[cells.index(cx, 0, cz)];
            let mut v100 = buffer[cells.index(next_x, 0, cz)];
            let mut v001 = buffer[cells.index(cx, 0, next_z)];
            let mut v101 = buffer[cells.index(next_x, 0, next_z)];
            for cy in 0..count_y {
                let next_y = (cy + 1).min(cells.size_y - 1);
                let v010 = buffer[cells.index(cx, next_y, cz)];
                let v110 = buffer[cells.index(next_x, next_y, cz)];
                let v011 = buffer[cells.index(cx, next_y, next_z)];
                let v111 = buffer[cells.index(next_x, next_y, next_z)];
                let origin = [
                    cells.block_x(cx) - volume.min_x,
                    cells.block_y(cy) - volume.min_y,
                    cells.block_z(cz) - volume.min_z,
                ];
                let x0 = 0.max(-origin[0]);
                let y0 = 0.max(-origin[1]);
                let z0 = 0.max(-origin[2]);
                let x1 = cell_xz.min(volume.size_x - origin[0]) - 1;
                let y1 = cell_y.min(volume.size_y - origin[1]) - 1;
                let z1 = cell_xz.min(volume.size_z - origin[2]) - 1;
                for z in z0..=z1 {
                    let alpha_z = z as f32 * inv_xz;
                    let v00 = lerp(alpha_z, v000, v001);
                    let v01 = lerp(alpha_z, v010, v011);
                    let v10 = lerp(alpha_z, v100, v101);
                    let v11 = lerp(alpha_z, v110, v111);
                    for x in x0..=x1 {
                        let alpha_x = x as f32 * inv_xz;
                        let bottom = lerp(alpha_x, v00, v10);
                        let top = lerp(alpha_x, v01, v11);
                        let step = (top - bottom) * inv_y;
                        let mut value = bottom + step * y0 as f32;
                        let index = volume.index(origin[0] + x, origin[1] + y0, origin[2] + z);
                        for slot in &mut out[index..index + (y1 - y0 + 1) as usize] {
                            *slot = value;
                            value += step;
                        }
                    }
                }
                v000 = v010;
                v100 = v110;
                v001 = v011;
                v101 = v111;
            }
        }
    }
    ctx.release(buffer);
}

#[cfg(test)]
mod end_island_tests {
    use super::*;
    use tuff_item::random::{LegacyRandom, Random, RandomSource};

    fn noise() -> Simplex {
        let mut random = Random::Legacy(LegacyRandom::new(12345));
        random.consume_count(17292);
        Simplex::new(&mut random, true)
    }

    fn grid_hash(noise: &Simplex, step: i32) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for sx in (-300..300).step_by(step as usize) {
            for sz in (-300..300).step_by(step as usize) {
                let bits = end_island_height(noise, sx * 7, sz * 5).to_bits() as u64;
                hash = (hash ^ bits).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn end_island_height_is_pinned_for_a_fixed_seed() {
        assert_eq!(grid_hash(&noise(), 3), 18181909352103509649);
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "release timing probe"]
    fn end_island_height_timing() {
        let noise = noise();
        let start = std::time::Instant::now();
        let mut acc = 0u64;
        for _ in 0..3 {
            acc ^= grid_hash(&noise, 1);
        }
        eprintln!("end_island_height timing: {:?} ({acc})", start.elapsed());
    }
}

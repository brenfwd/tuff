//! Noise-based aquifers: underground fluid pockets with their own levels,
//! lava in the deep ones, and barriers of stone between pockets whose
//! levels differ. One [`Aquifer`] serves one chunk's generation.

use std::collections::HashMap;
use std::sync::Arc;

use tuff_vanilla_data::{Block, BlockState};

use crate::density::{Context, DensityVolume, Sampler};
use tuff_item::random::{PositionalRandom, RandomSource};

const WAY_BELOW_MIN_Y: i32 = -2032 * 2;
const SURFACE_OFFSETS: [[i32; 2]; 13] = [
    [0, 0],
    [-2, -1],
    [-1, -1],
    [0, -1],
    [1, -1],
    [-3, 0],
    [-2, 0],
    [-1, 0],
    [1, 0],
    [-2, 1],
    [-1, 1],
    [0, 1],
    [1, 1],
];

fn similarity(a: i32, b: i32) -> f64 {
    1.0 - (b - a) as f64 / 25.0
}

const FLOWING_UPDATE_SIMILARITY: f64 = 1.0 - (144 - 100) as f64 / 25.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidStatus {
    pub level: i32,
    pub fluid: BlockState,
}

impl FluidStatus {
    pub fn at(&self, y: i32) -> BlockState {
        if y < self.level {
            self.fluid
        } else {
            Block::Air.default_state()
        }
    }
}

/// The dimension-wide fluid levels: lava below y -54 (or sea level, if
/// lower), the default fluid up to sea level.
#[derive(Debug, Clone, Copy)]
pub struct FluidPicker {
    pub sea_level: i32,
    pub default_fluid: BlockState,
    /// Off under the debug switch that generates no fluids.
    pub fluids: bool,
}

impl FluidPicker {
    pub fn at(&self, _x: i32, y: i32, _z: i32) -> FluidStatus {
        if !self.fluids {
            FluidStatus {
                level: WAY_BELOW_MIN_Y,
                fluid: Block::Air.default_state(),
            }
        } else if y < (-54).min(self.sea_level) {
            FluidStatus {
                level: -54,
                fluid: Block::Lava.default_state(),
            }
        } else {
            FluidStatus {
                level: self.sea_level,
                fluid: self.default_fluid,
            }
        }
    }
}

/// The compiled aquifer noises of a dimension.
pub struct AquiferSamplers {
    pub barrier: Arc<Sampler>,
    pub floodedness: Arc<Sampler>,
    pub spread: Arc<Sampler>,
    pub lava: Arc<Sampler>,
    pub exclusion: Arc<Sampler>,
    pub surface_level: Arc<Sampler>,
    pub random: PositionalRandom,
}

pub enum Aquifer<'s> {
    Disabled(FluidPicker),
    Noise(Box<NoiseAquifer<'s>>),
}

impl<'s> Aquifer<'s> {
    pub fn compute_substance(
        &mut self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
    ) -> Option<BlockState> {
        match self {
            Aquifer::Disabled(picker) => {
                if density > 0.0 {
                    None
                } else {
                    Some(picker.at(x, y, z).at(y))
                }
            }
            Aquifer::Noise(aquifer) => {
                let state = aquifer.compute_substance(ctx, x, y, z, density);
                if aquifer.picker.fluids {
                    state
                } else {
                    state.map(|_| Block::Air.default_state())
                }
            }
        }
    }

    pub fn should_schedule_fluid_update(&self) -> bool {
        match self {
            Aquifer::Disabled(_) => false,
            Aquifer::Noise(aquifer) => aquifer.schedule_update,
        }
    }
}

pub struct NoiseAquifer<'s> {
    samplers: &'s AquiferSamplers,
    picker: FluidPicker,
    statuses: Vec<Option<FluidStatus>>,
    locations: Vec<Option<(i32, i32, i32)>>,
    skip_above: i32,
    min_grid: [i32; 3],
    size_x: i32,
    size_z: i32,
    surface_levels: SurfaceLevels,
    schedule_update: bool,
}

/// Surface levels by quart column: a dense grid over the chunk's region
/// and a map for the far points aquifer neighbours reach.
#[derive(Default)]
struct SurfaceLevels {
    grid: Option<SurfaceGrid>,
    far: HashMap<(i32, i32), i32>,
}

struct SurfaceGrid {
    min_qx: i32,
    min_qz: i32,
    size_x: i32,
    size_z: i32,
    levels: Vec<i32>,
}

impl SurfaceLevels {
    fn get(&self, x: i32, z: i32) -> Option<i32> {
        if let Some(grid) = &self.grid {
            let (dx, dz) = ((x >> 2) - grid.min_qx, (z >> 2) - grid.min_qz);
            if (0..grid.size_x).contains(&dx) && (0..grid.size_z).contains(&dz) {
                return Some(grid.levels[(dz * grid.size_x + dx) as usize]);
            }
        }
        self.far.get(&(x, z)).copied()
    }
}

type StatusTable = Vec<Option<FluidStatus>>;
type LocationTable = Vec<Option<(i32, i32, i32)>>;

thread_local! {
    static TABLES: std::cell::RefCell<(StatusTable, LocationTable)> = const {
        std::cell::RefCell::new((Vec::new(), Vec::new()))
    };
}

fn take_tables() -> (StatusTable, LocationTable) {
    TABLES
        .try_with(|t| std::mem::take(&mut *t.borrow_mut()))
        .unwrap_or_default()
}

impl Drop for NoiseAquifer<'_> {
    fn drop(&mut self) {
        let mut statuses = std::mem::take(&mut self.statuses);
        let mut locations = std::mem::take(&mut self.locations);
        statuses.clear();
        locations.clear();
        let _ = TABLES.try_with(|t| *t.borrow_mut() = (statuses, locations));
    }
}

fn grid_x(c: i32) -> i32 {
    c >> 4
}

fn grid_y(c: i32) -> i32 {
    c.div_euclid(12)
}

fn from_grid_x(g: i32, offset: i32) -> i32 {
    (g << 4) + offset
}

fn from_grid_y(g: i32, offset: i32) -> i32 {
    g * 12 + offset
}

fn quart(c: i32) -> i32 {
    (c >> 2) << 2
}

impl<'s> NoiseAquifer<'s> {
    pub fn new(
        ctx: &mut Context,
        samplers: &'s AquiferSamplers,
        volume: &DensityVolume,
        picker: FluidPicker,
    ) -> Self {
        let min_x = grid_x(volume.min_x - 5);
        let max_x = grid_x(volume.max_x() - 5) + 1;
        let min_y = grid_y(volume.min_y + 1) - 1;
        let max_y = grid_y(volume.max_y() + 1) + 1;
        let min_z = grid_x(volume.min_z - 5);
        let max_z = grid_x(volume.max_z() - 5) + 1;
        let size_x = max_x - min_x + 1;
        let size_y = max_y - min_y + 1;
        let size_z = max_z - min_z + 1;
        let total = (size_x * size_y * size_z) as usize;
        let (mut statuses, mut locations) = take_tables();
        statuses.resize(total, None);
        locations.resize(total, None);
        let mut aquifer = Self {
            samplers,
            picker,
            statuses,
            locations,
            skip_above: 0,
            min_grid: [min_x, min_y, min_z],
            size_x,
            size_z,
            surface_levels: SurfaceLevels::default(),
            schedule_update: false,
        };
        let max_surface = aquifer.max_surface_level(
            ctx,
            from_grid_x(min_x, 0),
            from_grid_x(min_z, 0),
            from_grid_x(max_x, 9),
            from_grid_x(max_z, 9),
        ) + 8;
        let skip_grid = grid_y(max_surface + 12) + 1;
        aquifer.skip_above = from_grid_y(skip_grid, 11) - 1;
        aquifer
    }

    fn max_surface_level(
        &mut self,
        ctx: &mut Context,
        min_x: i32,
        min_z: i32,
        max_x: i32,
        max_z: i32,
    ) -> i32 {
        let (qx0, qx1, qz0, qz1) = (min_x >> 2, max_x >> 2, min_z >> 2, max_z >> 2);
        let volume = DensityVolume::stepped(
            [qx1 - qx0 + 1, 1, qz1 - qz0 + 1],
            [qx0 << 2, 0, qz0 << 2],
            [4, 1, 4],
        );
        let mut buffer = ctx.acquire(volume.size());
        self.samplers
            .surface_level
            .sample_volume(ctx, &mut buffer, &volume);
        let mut max = i32::MIN;
        let mut grid = Vec::with_capacity(volume.size());
        for z in 0..volume.size_z {
            for x in 0..volume.size_x {
                let level = (buffer[volume.index(x, 0, z)] as f64).floor() as i32;
                grid.push(level);
                max = max.max(level);
            }
        }
        self.surface_levels.grid = Some(SurfaceGrid {
            min_qx: qx0,
            min_qz: qz0,
            size_x: volume.size_x,
            size_z: volume.size_z,
            levels: grid,
        });
        ctx.release(buffer);
        max
    }

    fn surface_level(&mut self, ctx: &mut Context, x: i32, z: i32) -> i32 {
        let (qx, qz) = (quart(x), quart(z));
        if let Some(level) = self.surface_levels.get(qx, qz) {
            return level;
        }
        let level =
            (self.samplers.surface_level.sample_value(ctx, qx, 0, qz) as f64).floor() as i32;
        self.surface_levels.far.insert((qx, qz), level);
        level
    }

    fn index(&self, gx: i32, gy: i32, gz: i32) -> usize {
        let x = gx - self.min_grid[0];
        let y = gy - self.min_grid[1];
        let z = gz - self.min_grid[2];
        ((y * self.size_z + z) * self.size_x + x) as usize
    }

    fn location(&mut self, gx: i32, gy: i32, gz: i32) -> (usize, (i32, i32, i32)) {
        let index = self.index(gx, gy, gz);
        if let Some(location) = self.locations[index] {
            return (index, location);
        }
        let mut random = self.samplers.random.at(gx, gy, gz);
        let x = from_grid_x(gx, random.next_int_bounded(10));
        let y = from_grid_y(gy, random.next_int_bounded(9));
        let z = from_grid_x(gz, random.next_int_bounded(10));
        self.locations[index] = Some((x, y, z));
        (index, (x, y, z))
    }

    fn status(&mut self, ctx: &mut Context, index: usize) -> FluidStatus {
        if let Some(status) = self.statuses[index] {
            return status;
        }
        let (x, y, z) = self.locations[index].expect("location computed first");
        let status = self.compute_fluid(ctx, x, y, z);
        self.statuses[index] = Some(status);
        status
    }

    pub fn compute_substance(
        &mut self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
    ) -> Option<BlockState> {
        let (state, schedule) = self.substance_and_flow(ctx, x, y, z, density);
        self.schedule_update = schedule;
        state
    }

    #[inline]
    fn substance_and_flow(
        &mut self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
    ) -> (Option<BlockState>, bool) {
        if density > 0.0 {
            return (None, false);
        }
        let sea = self.picker.at(x, y, z).at(y);
        if y > self.skip_above {
            return (Some(sea), false);
        }
        if sea.block() == Block::Lava {
            return (Some(Block::Lava.default_state()), false);
        }
        let near = self.nearest_cells(x, y, z);
        let first = self.status(ctx, near.cell[0]);
        let here = first.at(y);
        let s01 = similarity(near.dist[0], near.dist[1]);
        if s01 <= 0.0 {
            let flowing =
                s01 >= FLOWING_UPDATE_SIMILARITY && first != self.status(ctx, near.cell[1]);
            return (Some(here), flowing);
        }
        if here.block() == Block::Water
            && self.picker.at(x, y - 1, z).at(y - 1).block() == Block::Lava
        {
            return (Some(here), true);
        }
        let mut barrier = LazyBarrier::default();
        let second = self.status(ctx, near.cell[1]);
        let walled = |weight: f64, p: f64| density + weight * p > 0.0;
        let p01 = self.pressure(ctx, x, y, z, &mut barrier, first, second);
        if walled(s01, p01) {
            return (None, false);
        }
        let third = self.status(ctx, near.cell[2]);
        let s02 = similarity(near.dist[0], near.dist[2]);
        if s02 > 0.0 {
            let p02 = self.pressure(ctx, x, y, z, &mut barrier, first, third);
            if walled(s01 * s02, p02) {
                return (None, false);
            }
        }
        let s12 = similarity(near.dist[1], near.dist[2]);
        if s12 > 0.0 {
            let p12 = self.pressure(ctx, x, y, z, &mut barrier, second, third);
            if walled(s01 * s12, p12) {
                return (None, false);
            }
        }
        let close02 = s02 >= FLOWING_UPDATE_SIMILARITY;
        let flowing = first != second
            || (s12 >= FLOWING_UPDATE_SIMILARITY && second != third)
            || (close02 && first != third)
            || (close02
                && similarity(near.dist[0], near.dist[3]) >= FLOWING_UPDATE_SIMILARITY
                && first != self.status(ctx, near.cell[3]));
        (Some(here), flowing)
    }

    fn nearest_cells(&mut self, x: i32, y: i32, z: i32) -> Nearest {
        let (base_x, base_y, base_z) = (grid_x(x - 5), grid_y(y + 1), grid_x(z - 5));
        let mut near = Nearest {
            dist: [i32::MAX; 4],
            cell: [0; 4],
        };
        for gx in base_x..=base_x + 1 {
            for gy in base_y - 1..=base_y + 1 {
                for gz in base_z..=base_z + 1 {
                    let (cell, (cx, cy, cz)) = self.location(gx, gy, gz);
                    let d = (cx - x).pow(2) + (cy - y).pow(2) + (cz - z).pow(2);
                    near.offer(d, cell);
                }
            }
        }
        near
    }

    #[allow(clippy::too_many_arguments)]
    fn pressure(
        &self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        barrier: &mut LazyBarrier,
        a: FluidStatus,
        b: FluidStatus,
    ) -> f64 {
        let mixes = matches!(
            (a.at(y).block(), b.at(y).block()),
            (Block::Lava, Block::Water) | (Block::Water, Block::Lava)
        );
        if mixes {
            return 2.0;
        }
        if a.level == b.level {
            return 0.0;
        }
        let half_gap = (a.level - b.level).abs() as f64 / 2.0;
        let offset = y as f64 + 0.5 - 0.5 * (a.level + b.level) as f64;
        let inside = half_gap - offset.abs();
        let slope = match (offset > 0.0, inside > 0.0) {
            (true, true) => inside / 1.5,
            (true, false) => inside / 2.5,
            (false, _) => {
                let lifted = inside + 3.0;
                lifted / if lifted > 0.0 { 3.0 } else { 10.0 }
            }
        };
        let noise = if (-2.0..=2.0).contains(&slope) {
            barrier.get(|| self.samplers.barrier.sample_value(ctx, x, y, z) as f64)
        } else {
            0.0
        };
        2.0 * (noise + slope)
    }

    fn compute_fluid(&mut self, ctx: &mut Context, x: i32, y: i32, z: i32) -> FluidStatus {
        let sea = self.picker.at(x, y, z);
        let (lowest, submerged) = match self.probe_surface(ctx, x, y, z, sea) {
            Probe::Settled(status) => return status,
            Probe::Open { lowest, submerged } => (lowest, submerged),
        };
        let level = self.flooded_level(ctx, x, y, z, sea, lowest, submerged);
        FluidStatus {
            level,
            fluid: self.pick_fluid(ctx, x, y, z, sea, level),
        }
    }

    fn probe_surface(
        &mut self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        sea: FluidStatus,
    ) -> Probe {
        let reach_top = y + 12;
        let flooded_at = |picker: &FluidPicker, px: i32, top: i32, pz: i32| {
            let status = picker.at(px, top, pz);
            (status.at(top).block() != Block::Air).then_some(status)
        };
        let own = self.surface_level(ctx, x, z);
        let own_top = own + 8;
        if y - 12 > own_top {
            return Probe::Settled(sea);
        }
        let own_fluid = flooded_at(&self.picker, x, own_top, z);
        if let Some(status) = own_fluid
            && reach_top > own_top
        {
            return Probe::Settled(status);
        }
        let mut lowest = own;
        for [ox, oz] in &SURFACE_OFFSETS[1..] {
            let (px, pz) = (x + (ox << 4), z + (oz << 4));
            let surface = self.surface_level(ctx, px, pz);
            let top = surface + 8;
            if reach_top > top
                && let Some(status) = flooded_at(&self.picker, px, top, pz)
            {
                return Probe::Settled(status);
            }
            lowest = lowest.min(surface);
        }
        Probe::Open {
            lowest,
            submerged: own_fluid.is_some(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn flooded_level(
        &self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        sea: FluidStatus,
        lowest: i32,
        submerged: bool,
    ) -> i32 {
        let excluded = self.samplers.exclusion.sample_value(ctx, x, y, z) as f64 > 0.0;
        let (over_partial, over_full) = if excluded {
            (-1.0, -1.0)
        } else {
            let depth = lowest.wrapping_add(8).wrapping_sub(y) as f64;
            let t = if submerged {
                1.0 - (depth / 64.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let flood =
                (self.samplers.floodedness.sample_value(ctx, x, y, z) as f64).clamp(-1.0, 1.0);
            (
                flood - threshold(t, -0.8, 0.4),
                flood - threshold(t, -0.3, 0.8),
            )
        };
        if over_full > 0.0 {
            return sea.level;
        }
        if over_partial <= 0.0 {
            return WAY_BELOW_MIN_Y;
        }
        let band = y.div_euclid(40);
        let spread =
            self.samplers
                .spread
                .sample_value(ctx, x.div_euclid(16), band, z.div_euclid(16));
        let jitter = ((spread * 10.0) as f64 / 3.0).floor() as i32 * 3;
        lowest.min(band * 40 + 20 + jitter)
    }

    fn pick_fluid(
        &self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        z: i32,
        sea: FluidStatus,
        level: i32,
    ) -> BlockState {
        let lava = Block::Lava.default_state();
        let deep_pocket = level <= -10 && level != WAY_BELOW_MIN_Y && sea.fluid != lava;
        if deep_pocket
            && (self.samplers.lava.sample_value(
                ctx,
                x.div_euclid(64),
                y.div_euclid(40),
                z.div_euclid(64),
            ) as f64)
                .abs()
                > 0.3
        {
            lava
        } else {
            sea.fluid
        }
    }
}

enum Probe {
    Settled(FluidStatus),
    Open { lowest: i32, submerged: bool },
}

struct Nearest {
    dist: [i32; 4],
    cell: [usize; 4],
}

impl Nearest {
    #[inline]
    fn offer(&mut self, d: i32, cell: usize) {
        if d > self.dist[3] {
            return;
        }
        let mut slot = 3;
        while slot > 0 && self.dist[slot - 1] >= d {
            self.dist[slot] = self.dist[slot - 1];
            self.cell[slot] = self.cell[slot - 1];
            slot -= 1;
        }
        self.dist[slot] = d;
        self.cell[slot] = cell;
    }
}

#[derive(Default)]
struct LazyBarrier(Option<f64>);

impl LazyBarrier {
    fn get(&mut self, sample: impl FnOnce() -> f64) -> f64 {
        *self.0.get_or_insert_with(sample)
    }
}

fn threshold(t: f64, at_full: f64, at_none: f64) -> f64 {
    let u = (t - 1.0) / -1.0;
    at_full + u * (at_none - at_full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn surface_levels_read_the_grid_then_the_far_map() {
        let mut levels = SurfaceLevels::default();
        assert_eq!(levels.get(-8, 4), None);
        levels.grid = Some(SurfaceGrid {
            min_qx: -2,
            min_qz: 1,
            size_x: 3,
            size_z: 2,
            levels: vec![10, 11, 12, 20, 21, 22],
        });
        assert_eq!(levels.get(-8, 4), Some(10));
        assert_eq!(levels.get(0, 4), Some(12));
        assert_eq!(levels.get(-4, 8), Some(21));
        assert_eq!(levels.get(4, 4), None);
        assert_eq!(levels.get(-8, 12), None);
        levels.far.insert((4, 4), 7);
        assert_eq!(levels.get(4, 4), Some(7));
    }
}

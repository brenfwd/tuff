//! Carvers (caves and canyons): each chunk within eight chunks may start
//! carvers that cut into this chunk. Carved positions collect in a mask,
//! which is then filled through the aquifer, with grass restored on the dirt
//! a carve exposes. Carver kinds are keyed by resource id.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tuff_vanilla_data::worldgen::{Carver, CarverConfig};
use tuff_vanilla_data::{Block, BlockState};

use crate::providers;
use crate::surface::GenContext;
use tuff_core::mth;
use tuff_item::random::{LegacyRandom, RandomSource, WorldgenRandom};

use tuff_world_model::pos::ChunkPos;

/// The positions carvers removed in one chunk.
pub struct CarvingMask {
    pub min_y: i32,
    pub max_y: i32,
    height: i32,
    bits: Vec<u64>,
}

impl CarvingMask {
    pub fn new(min_y: i32, max_y: i32) -> Self {
        let height = max_y - min_y + 1;
        Self {
            min_y,
            max_y,
            height,
            bits: vec![0; (256 * height as usize).div_ceil(64)],
        }
    }

    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y - self.min_y + (z + (x << 4)) * self.height) as usize
    }

    pub fn carve(&mut self, x: i32, y: i32, z: i32) {
        let i = self.index(x, y, z);
        self.bits[i / 64] |= 1 << (i % 64);
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|b| *b == 0)
    }

    fn next_set(&self, mut i: usize, total: usize) -> Option<usize> {
        while i < total {
            let word = self.bits[i / 64] >> (i % 64);
            if word != 0 {
                let at = i + word.trailing_zeros() as usize;
                return (at < total).then_some(at);
            }
            i = (i / 64 + 1) * 64;
        }
        None
    }

    fn next_clear(&self, mut i: usize, total: usize) -> usize {
        while i < total {
            let offset = i % 64;
            let zeros = (!(self.bits[i / 64] >> offset)).trailing_zeros() as usize;
            i += zeros;
            if offset + zeros < 64 {
                break;
            }
        }
        i.min(total)
    }

    /// Each carved run as `(x, z, bottom, top)`, in the game's order.
    pub fn runs(&self) -> Vec<(i32, i32, i32, i32)> {
        let total = (256 * self.height) as usize;
        let mut runs = Vec::new();
        let mut i = 0;
        while let Some(start) = self.next_set(i, total) {
            i = self.next_clear(start, total);
            let end = i - 1;
            let (start_column, end_column) =
                (start / self.height as usize, end / self.height as usize);
            for column in start_column..=end_column {
                let base = column * self.height as usize;
                let bottom = start.saturating_sub(base) as i32 + self.min_y;
                let top = (end - base).min(self.height as usize - 1) as i32 + self.min_y;
                runs.push((
                    ((column >> 4) & 15) as i32,
                    (column & 15) as i32,
                    bottom,
                    top,
                ));
            }
        }
        runs
    }
}

/// A carver kind, instantiated from its data.
pub trait CarverKind: Send + Sync {
    fn is_start_chunk(&self, random: &mut WorldgenRandom) -> bool;
    fn carve(
        &self,
        span: GenContext,
        random: &mut WorldgenRandom,
        chunk: ChunkPos,
        source: ChunkPos,
        mask: &mut CarvingMask,
    );
}

pub type CarverFactory = fn(&'static CarverConfig) -> Option<Arc<dyn CarverKind>>;

/// Carver kinds by resource id.
pub struct CarverKinds {
    factories: HashMap<String, CarverFactory>,
}

impl CarverKinds {
    pub fn builtin() -> Self {
        let mut kinds = Self {
            factories: HashMap::new(),
        };
        kinds.register("minecraft:cave", |config| match config {
            CarverConfig::Cave { .. } => Some(Arc::new(Cave(config))),
            _ => None,
        });
        kinds.register("minecraft:canyon", |config| match config {
            CarverConfig::Canyon { .. } => Some(Arc::new(Canyon(config))),
            _ => None,
        });
        kinds
    }

    pub fn register(&mut self, kind: &str, factory: CarverFactory) {
        self.factories.insert(kind.to_string(), factory);
    }

    pub fn create(&self, carver: &'static Carver) -> Option<Arc<dyn CarverKind>> {
        (self.factories.get(carver.kind)?)(&carver.config)
    }
}

/// The carvers of every vanilla configured carver, by name.
pub fn configured(name: &str) -> Option<Arc<dyn CarverKind>> {
    static CARVERS: OnceLock<HashMap<&'static str, Arc<dyn CarverKind>>> = OnceLock::new();
    CARVERS
        .get_or_init(|| {
            let kinds = CarverKinds::builtin();
            tuff_vanilla_data::worldgen::CARVERS
                .iter()
                .filter_map(|(name, carver)| Some((*name, kinds.create(carver)?)))
                .collect()
        })
        .get(name)
        .cloned()
}

/// The carvers a biome runs, in order, each with its index in the
/// biome's carver list (which seeds it).
pub fn carvers_of(biome: u16) -> &'static [(usize, Arc<dyn CarverKind>)] {
    type Table = Vec<Box<[(usize, Arc<dyn CarverKind>)]>>;
    static TABLE: OnceLock<Table> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        tuff_vanilla_data::data_registry::WORLDGEN_BIOME
            .entries
            .iter()
            .map(|name| {
                let Some(data) = tuff_vanilla_data::biome::Biome::get(name) else {
                    return Box::default();
                };
                data.carvers
                    .iter()
                    .enumerate()
                    .filter_map(|(index, name)| Some((index, configured(name)?)))
                    .collect()
            })
            .collect()
    });
    match table.get(biome as usize) {
        Some(carvers) => carvers,
        None => &table[tuff_world_model::chunk::biome_id("minecraft:plains").unwrap() as usize],
    }
}

const RANGE: i32 = 4;

fn can_reach(chunk: ChunkPos, x: f64, z: f64, step: i32, total: i32, thickness: f32) -> bool {
    let xd = x - (chunk.x * 16 + 8) as f64;
    let zd = z - (chunk.z * 16 + 8) as f64;
    let remaining = (total - step) as f64;
    let rr = (thickness + 2.0 + 16.0) as f64;
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}

#[allow(clippy::too_many_arguments)]
fn carve_ellipsoid(
    chunk: ChunkPos,
    x: f64,
    y: f64,
    z: f64,
    horizontal: f64,
    vertical: f64,
    mask: &mut CarvingMask,
    skip: &dyn Fn(f64, f64, f64, i32) -> bool,
) {
    let center_x = (chunk.x * 16 + 8) as f64;
    let center_z = (chunk.z * 16 + 8) as f64;
    let max_delta = 16.0 + horizontal * 2.0;
    if (x - center_x).abs() > max_delta || (z - center_z).abs() > max_delta {
        return;
    }
    let (cx, cz) = (chunk.x * 16, chunk.z * 16);
    let min_xi = ((x - horizontal).floor() as i32 - cx - 1).max(0);
    let max_xi = ((x + horizontal).floor() as i32 - cx).min(15);
    let min_y = ((y - vertical).floor() as i32 - 1).max(mask.min_y);
    let max_y = ((y + vertical).floor() as i32 + 1).min(mask.max_y);
    let min_zi = ((z - horizontal).floor() as i32 - cz - 1).max(0);
    let max_zi = ((z + horizontal).floor() as i32 - cz).min(15);
    for xi in min_xi..=max_xi {
        let xd = ((cx + xi) as f64 + 0.5 - x) / horizontal;
        for zi in min_zi..=max_zi {
            let zd = ((cz + zi) as f64 + 0.5 - z) / horizontal;
            if xd * xd + zd * zd < 1.0 {
                let mut wy = max_y;
                while wy > min_y {
                    let yd = (wy as f64 - 0.5 - y) / vertical;
                    if !skip(xd, yd, zd, wy) {
                        mask.carve(xi, wy, zi);
                    }
                    wy -= 1;
                }
            }
        }
    }
}

struct Cave(&'static CarverConfig);

struct CaveParams {
    horizontal_multiplier: f64,
    vertical_multiplier: f64,
    floor: f64,
}

impl Cave {
    fn thickness(&self, random: &mut WorldgenRandom) -> f32 {
        let CarverConfig::Cave {
            thickness,
            weird_thickness_bias,
            ..
        } = self.0
        else {
            unreachable!()
        };
        let mut t = providers::float(thickness, random);
        if *weird_thickness_bias && random.next_int_bounded(10) == 0 {
            t *= random.next_float() * random.next_float() * 3.0 + 1.0;
        }
        t
    }

    #[allow(clippy::too_many_arguments)]
    fn tunnel(
        &self,
        chunk: ChunkPos,
        seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        params: &CaveParams,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        step: i32,
        dist: i32,
        y_scale: f64,
        mask: &mut CarvingMask,
    ) {
        let mut random = LegacyRandom::new(seed);
        let split = random.next_int_bounded(dist / 2) + dist / 4;
        let steep = random.next_int_bounded(6) == 0;
        let mut y_rota = 0.0f32;
        let mut x_rota = 0.0f32;
        let floor = params.floor;
        let skip = move |xd: f64, yd: f64, zd: f64, _y: i32| {
            yd <= floor || xd * xd + yd * yd + zd * zd >= 1.0
        };
        for current in step..dist {
            let horizontal = 1.5
                + (mth::sin((std::f32::consts::PI * current as f32 / dist as f32) as f64)
                    * thickness) as f64;
            let vertical = horizontal * y_scale;
            let cos_x = mth::cos(vertical_rotation as f64);
            x += (mth::cos(horizontal_rotation as f64) * cos_x) as f64;
            y += mth::sin(vertical_rotation as f64) as f64;
            z += (mth::sin(horizontal_rotation as f64) * cos_x) as f64;
            vertical_rotation = vertical_rotation * if steep { 0.92 } else { 0.7 } + x_rota * 0.1;
            horizontal_rotation += y_rota * 0.1;
            x_rota = x_rota * 0.9
                + (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota = y_rota * 0.75
                + (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if current == split && thickness > 1.0 {
                let s1 = random.next_long();
                let t1 = random.next_float() * 0.5 + 0.5;
                self.tunnel(
                    chunk,
                    s1,
                    x,
                    y,
                    z,
                    params,
                    t1,
                    horizontal_rotation - std::f32::consts::FRAC_PI_2,
                    vertical_rotation / 3.0,
                    current,
                    dist,
                    1.0,
                    mask,
                );
                let s2 = random.next_long();
                let t2 = random.next_float() * 0.5 + 0.5;
                self.tunnel(
                    chunk,
                    s2,
                    x,
                    y,
                    z,
                    params,
                    t2,
                    horizontal_rotation + std::f32::consts::FRAC_PI_2,
                    vertical_rotation / 3.0,
                    current,
                    dist,
                    1.0,
                    mask,
                );
                return;
            }
            if random.next_int_bounded(4) != 0 {
                if !can_reach(chunk, x, z, current, dist, thickness) {
                    return;
                }
                carve_ellipsoid(
                    chunk,
                    x,
                    y,
                    z,
                    horizontal * params.horizontal_multiplier,
                    vertical * params.vertical_multiplier,
                    mask,
                    &skip,
                );
            }
        }
    }
}

impl CarverKind for Cave {
    fn is_start_chunk(&self, random: &mut WorldgenRandom) -> bool {
        let CarverConfig::Cave { probability, .. } = self.0 else {
            unreachable!()
        };
        random.next_float() <= *probability
    }

    fn carve(
        &self,
        span: GenContext,
        random: &mut WorldgenRandom,
        chunk: ChunkPos,
        source: ChunkPos,
        mask: &mut CarvingMask,
    ) {
        let CarverConfig::Cave {
            y: y_provider,
            count,
            room_vertical_radius_multiplier,
            horizontal_radius_multiplier,
            vertical_radius_multiplier,
            start_vertical_radius_multiplier,
            floor_level,
            ..
        } = self.0
        else {
            unreachable!()
        };
        let max_distance = (RANGE * 2 - 1) << 4;
        let caves = providers::int(count, random);
        for _ in 0..caves {
            let x = (source.x * 16 + random.next_int_bounded(16)) as f64;
            let y = providers::height(y_provider, random, span) as f64;
            let z = (source.z * 16 + random.next_int_bounded(16)) as f64;
            let params = CaveParams {
                horizontal_multiplier: providers::float(horizontal_radius_multiplier, random)
                    as f64,
                vertical_multiplier: providers::float(vertical_radius_multiplier, random) as f64,
                floor: 0.0,
            };
            let start_multiplier =
                providers::float(start_vertical_radius_multiplier, random) as f64;
            let params = CaveParams {
                floor: providers::float(floor_level, random) as f64,
                ..params
            };
            let mut tunnels = 1;
            if random.next_int_bounded(4) == 0 {
                let y_scale = providers::float(room_vertical_radius_multiplier, random) as f64;
                let thickness = 1.0 + random.next_float() * 6.0;
                let horizontal = 1.5 + (mth::sin(1.5707963705062866) * thickness) as f64;
                let vertical = horizontal * y_scale;
                let floor = params.floor;
                carve_ellipsoid(
                    chunk,
                    x + 1.0,
                    y,
                    z,
                    horizontal,
                    vertical,
                    mask,
                    &|xd, yd, zd, _| yd <= floor || xd * xd + yd * yd + zd * zd >= 1.0,
                );
                tunnels = 1 + random.next_int_bounded(4);
            }
            for _ in 0..tunnels {
                let horizontal_rotation = random.next_float() * 6.2831855;
                let vertical_rotation = (random.next_float() - 0.5) / 4.0;
                let thickness = self.thickness(random);
                let distance = max_distance - random.next_int_bounded(max_distance / 4);
                let seed = random.next_long();
                self.tunnel(
                    chunk,
                    seed,
                    x,
                    y,
                    z,
                    &params,
                    thickness,
                    horizontal_rotation,
                    vertical_rotation,
                    0,
                    distance,
                    start_multiplier,
                    mask,
                );
            }
        }
    }
}

struct Canyon(&'static CarverConfig);

impl CarverKind for Canyon {
    fn is_start_chunk(&self, random: &mut WorldgenRandom) -> bool {
        let CarverConfig::Canyon { probability, .. } = self.0 else {
            unreachable!()
        };
        random.next_float() <= *probability
    }

    fn carve(
        &self,
        span: GenContext,
        random: &mut WorldgenRandom,
        chunk: ChunkPos,
        source: ChunkPos,
        mask: &mut CarvingMask,
    ) {
        let CarverConfig::Canyon {
            y: y_provider,
            vertical_rotation,
            distance_factor,
            thickness,
            y_scale,
            ..
        } = self.0
        else {
            unreachable!()
        };
        let max_distance = (RANGE * 2 - 1) * 16;
        let x = (source.x * 16 + random.next_int_bounded(16)) as f64;
        let y = providers::height(y_provider, random, span);
        let z = (source.z * 16 + random.next_int_bounded(16)) as f64;
        let horizontal_rotation = random.next_float() * 6.2831855;
        let vertical_rotation = providers::float(vertical_rotation, random);
        let y_scale = providers::float(y_scale, random) as f64;
        let thickness = providers::float(thickness, random);
        let distance = (max_distance as f32 * providers::float(distance_factor, random)) as i32;
        let seed = random.next_long();
        self.canyon(
            span,
            chunk,
            seed,
            x,
            y as f64,
            z,
            thickness,
            horizontal_rotation,
            vertical_rotation,
            distance,
            y_scale,
            mask,
        );
    }
}

impl Canyon {
    #[allow(clippy::too_many_arguments)]
    fn canyon(
        &self,
        span: GenContext,
        chunk: ChunkPos,
        seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        distance: i32,
        y_scale: f64,
        mask: &mut CarvingMask,
    ) {
        let CarverConfig::Canyon {
            width_smoothness,
            horizontal_radius_factor,
            vertical_radius_default_factor,
            vertical_radius_center_factor,
            ..
        } = self.0
        else {
            unreachable!()
        };
        let mut random = LegacyRandom::new(seed);
        let depth = span.height;
        let mut widths = vec![0f32; depth as usize];
        let mut width = 1.0f32;
        for (index, w) in widths.iter_mut().enumerate() {
            if index == 0 || random.next_int_bounded(*width_smoothness) == 0 {
                width = 1.0 + random.next_float() * random.next_float();
            }
            *w = width * width;
        }
        let mut y_rota = 0.0f32;
        let mut x_rota = 0.0f32;
        let min_gen_y = span.min_y;
        let skip = |xd: f64, yd: f64, zd: f64, wy: i32| {
            let index = (wy - min_gen_y) as usize;
            (xd * xd + zd * zd) * widths[index - 1] as f64 + yd * yd / 6.0 >= 1.0
        };
        for current in 0..distance {
            let horizontal = 1.5
                + (mth::sin((current as f32 * std::f32::consts::PI / distance as f32) as f64)
                    * thickness) as f64;
            let vertical = horizontal * y_scale;
            let horizontal =
                horizontal * providers::float(horizontal_radius_factor, &mut random) as f64;
            let multiplier = 1.0 - (0.5 - current as f32 / distance as f32).abs() * 2.0;
            let factor =
                vertical_radius_default_factor + vertical_radius_center_factor * multiplier;
            let vertical =
                factor as f64 * vertical * mth::random_between(&mut random, 0.75, 1.0) as f64;
            let xc = mth::cos(vertical_rotation as f64);
            let xs = mth::sin(vertical_rotation as f64);
            x += (mth::cos(horizontal_rotation as f64) * xc) as f64;
            y += xs as f64;
            z += (mth::sin(horizontal_rotation as f64) * xc) as f64;
            vertical_rotation = vertical_rotation * 0.7 + x_rota * 0.05;
            horizontal_rotation += y_rota * 0.05;
            x_rota = x_rota * 0.8
                + (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota = y_rota * 0.5
                + (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if random.next_int_bounded(4) != 0 {
                if !can_reach(chunk, x, z, current, distance, thickness) {
                    return;
                }
                carve_ellipsoid(chunk, x, y, z, horizontal, vertical, mask, &skip);
            }
        }
    }
}

/// Whether carvers may remove this block.
pub fn carvable(state: BlockState) -> bool {
    !crate::tags::uncarvable().contains_state(state)
}

pub fn is_grassy(state: BlockState) -> bool {
    matches!(state.block(), Block::GrassBlock | Block::Mycelium)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_runs(mask: &CarvingMask) -> Vec<(i32, i32, i32, i32)> {
        let get = |i: usize| mask.bits[i / 64] >> (i % 64) & 1 == 1;
        let total = (256 * mask.height) as usize;
        let mut runs = Vec::new();
        let mut i = 0;
        while i < total {
            if !get(i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < total && get(i) {
                i += 1;
            }
            let end = i - 1;
            let (start_column, end_column) =
                (start / mask.height as usize, end / mask.height as usize);
            for column in start_column..=end_column {
                let base = column * mask.height as usize;
                let bottom = start.saturating_sub(base) as i32 + mask.min_y;
                let top = (end - base).min(mask.height as usize - 1) as i32 + mask.min_y;
                runs.push((
                    ((column >> 4) & 15) as i32,
                    (column & 15) as i32,
                    bottom,
                    top,
                ));
            }
        }
        runs
    }

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[tuff_core::watchdog]
    #[test]
    fn word_scan_runs_match_the_bitwise_scan() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for height in [1, 2, 3, 7, 63, 64, 65, 127, 128, 129, 250, 313, 376] {
            let min_y = -61;
            let mut masks = vec![
                CarvingMask::new(min_y, min_y + height - 1),
                CarvingMask::new(min_y, min_y + height - 1),
            ];
            masks[1].bits.fill(u64::MAX);
            let total = 256 * height as usize;
            let used = masks[1].bits.len() * 64;
            for bit in total..used {
                masks[1].bits[bit / 64] &= !(1 << (bit % 64));
            }
            for density in [1u64, 2, 8, 32, 60, 63] {
                for _ in 0..24 {
                    let mut mask = CarvingMask::new(min_y, min_y + height - 1);
                    for word in 0..mask.bits.len() {
                        let mut bits = 0u64;
                        for _ in 0..density.min(8) {
                            bits |= next(&mut state);
                        }
                        let fill = match next(&mut state) % 4 {
                            0 => 0,
                            1 => u64::MAX,
                            _ => next(&mut state) >> (next(&mut state) % 64),
                        };
                        mask.bits[word] = if density > 32 {
                            bits | fill
                        } else {
                            bits & fill
                        };
                    }
                    for bit in total..mask.bits.len() * 64 {
                        mask.bits[bit / 64] &= !(1 << (bit % 64));
                    }
                    masks.push(mask);
                }
            }
            for mask in &masks {
                assert_eq!(mask.runs(), reference_runs(mask), "height {height}");
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn carver_table_matches_the_per_biome_name_lookup() {
        let count = tuff_vanilla_data::data_registry::WORLDGEN_BIOME
            .entries
            .len() as u16;
        for biome in 0..count + 3 {
            let name = tuff_world_model::chunk::biome_name(biome);
            let expected: Vec<(usize, Arc<dyn CarverKind>)> =
                tuff_vanilla_data::biome::Biome::get(name)
                    .map(|data| {
                        data.carvers
                            .iter()
                            .enumerate()
                            .filter_map(|(i, n)| Some((i, configured(n)?)))
                            .collect()
                    })
                    .unwrap_or_default();
            let table = carvers_of(biome);
            assert_eq!(table.len(), expected.len(), "{name}");
            for ((a, x), (b, y)) in table.iter().zip(&expected) {
                assert!(a == b && Arc::ptr_eq(x, y), "{name}");
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn carved_blocks_come_back_as_column_runs() {
        let mut mask = CarvingMask::new(-10, 20);
        for y in [-10, -9, 20] {
            mask.carve(3, y, 4);
        }
        mask.carve(15, 20, 15);
        mask.carve(0, -10, 0);
        assert_eq!(
            mask.runs(),
            vec![
                (0, 0, -10, -10),
                (3, 4, -10, -9),
                (3, 4, 20, 20),
                (15, 15, 20, 20)
            ]
        );
    }
}

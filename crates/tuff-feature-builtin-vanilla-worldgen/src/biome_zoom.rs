//! Block-resolution biome lookups: the game's "fiddled" nearest quart
//! corner, over the biomes stored in a chunk and its neighbours.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::density::Context;
use crate::generator::NoiseGenerator;
use tuff_item::random::lcg_next;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

/// The biome zoom seed: the first eight bytes of SHA-256 over the world
/// seed's little-endian bytes, read little-endian.
pub fn obfuscate_seed(seed: i64) -> i64 {
    let digest = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(digest[..8].try_into().unwrap())
}

fn fiddle(rval: i64) -> f64 {
    let uniform = (rval >> 24).rem_euclid(1024) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}

fn fiddles(seed: i64, x: i32, y: i32, z: i32) -> [f64; 3] {
    let mut rval = lcg_next(seed, x as i64);
    rval = lcg_next(rval, y as i64);
    rval = lcg_next(rval, z as i64);
    rval = lcg_next(rval, x as i64);
    rval = lcg_next(rval, y as i64);
    rval = lcg_next(rval, z as i64);
    let fx = fiddle(rval);
    let rval = lcg_next(rval, seed);
    let fy = fiddle(rval);
    let fz = fiddle(lcg_next(rval, seed));
    [fx, fy, fz]
}

/// The eight corner fiddles of one zoom cell, reused by every block in it.
#[derive(Clone, Copy)]
pub struct ZoomCell {
    cell: [i32; 3],
    fiddles: [[f64; 3]; 8],
}

impl ZoomCell {
    pub const fn empty() -> Self {
        Self {
            cell: [i32::MIN; 3],
            fiddles: [[0.0; 3]; 8],
        }
    }
}

/// The quart position whose biome a block uses.
pub fn zoom(seed: i64, x: i32, y: i32, z: i32) -> [i32; 3] {
    zoom_cached(seed, &mut ZoomCell::empty(), x, y, z)
}

/// `zoom` reusing the corner fiddles while consecutive calls stay in one cell.
pub fn zoom_cached(seed: i64, cache: &mut ZoomCell, x: i32, y: i32, z: i32) -> [i32; 3] {
    let (ax, ay, az) = (x - 2, y - 2, z - 2);
    let (px, py, pz) = (ax >> 2, ay >> 2, az >> 2);
    if cache.cell != [px, py, pz] {
        let old = cache.fiddles;
        let [ox, oy, oz] = cache.cell;
        for (i, slot) in cache.fiddles.iter_mut().enumerate() {
            let cx = if i & 4 == 0 { px } else { px + 1 };
            let cy = if i & 2 == 0 { py } else { py + 1 };
            let cz = if i & 1 == 0 { pz } else { pz + 1 };
            let (dx, dy, dz) = (
                (cx as i64) - (ox as i64),
                (cy as i64) - (oy as i64),
                (cz as i64) - (oz as i64),
            );
            *slot = if (0..=1).contains(&dx) && (0..=1).contains(&dy) && (0..=1).contains(&dz) {
                old[(dx as usize) << 2 | (dy as usize) << 1 | dz as usize]
            } else {
                fiddles(seed, cx, cy, cz)
            };
        }
        cache.cell = [px, py, pz];
    }
    let fx = (ax & 3) as f64 / 4.0;
    let fy = (ay & 3) as f64 / 4.0;
    let fz = (az & 3) as f64 / 4.0;
    let mut min_i = 0;
    let mut min = f64::INFINITY;
    for (i, [ox, oy, oz]) in cache.fiddles.iter().copied().enumerate() {
        let dx = if i & 4 == 0 { fx } else { fx - 1.0 };
        let dy = if i & 2 == 0 { fy } else { fy - 1.0 };
        let dz = if i & 1 == 0 { fz } else { fz - 1.0 };
        let next = (dz + oz) * (dz + oz) + (dy + oy) * (dy + oy) + (dx + ox) * (dx + ox);
        if min > next {
            min_i = i;
            min = next;
        }
    }
    [
        if min_i & 4 == 0 { px } else { px + 1 },
        if min_i & 2 == 0 { py } else { py + 1 },
        if min_i & 1 == 0 { pz } else { pz + 1 },
    ]
}

/// Block-resolution biome lookup, as material rules and features read it.
pub trait BiomeLookup {
    fn biome(&mut self, x: i32, y: i32, z: i32) -> u16;
}

/// Biomes sampled straight from the biome source (the game's uncached
/// resolver), through the zoom.
pub struct SampledBiomes<'g> {
    pub generator: &'g NoiseGenerator,
    pub last: Option<usize>,
    pub cell: ZoomCell,
}

impl BiomeLookup for SampledBiomes<'_> {
    fn biome(&mut self, x: i32, y: i32, z: i32) -> u16 {
        let quart = zoom_cached(self.generator.zoom_seed, &mut self.cell, x, y, z);
        let mut ctx = Context::uncached();
        self.generator.biome_source.biome_at(
            &self.generator.state.router,
            &mut ctx,
            &mut self.last,
            quart,
        )
    }
}

/// The stored biomes of a chunk and, fetched on demand the way each would
/// have been generated, its neighbours'. The zoom reaches at most one
/// chunk out, so the 3x3 around the centre is held directly.
pub struct StoredBiomes<'g> {
    generator: &'g NoiseGenerator,
    shape: ChunkShape,
    zoom_seed: i64,
    zoom_cell: ZoomCell,
    center: ChunkPos,
    around: [Option<Arc<[u16]>>; 9],
}

impl<'g> StoredBiomes<'g> {
    pub fn new(
        generator: &'g NoiseGenerator,
        shape: ChunkShape,
        center: ChunkPos,
        biomes: Arc<[u16]>,
    ) -> Self {
        let mut around: [Option<Arc<[u16]>>; 9] = Default::default();
        around[4] = Some(biomes);
        Self {
            generator,
            shape,
            zoom_seed: generator.zoom_seed,
            zoom_cell: ZoomCell::empty(),
            center,
            around,
        }
    }

    /// The stored biome at a quart position, clamped to the height.
    pub fn noise_biome(&mut self, qx: i32, qy: i32, qz: i32) -> u16 {
        let pos = ChunkPos::new(qx >> 2, qz >> 2);
        let shape = self.shape;
        let generator = self.generator;
        let (dx, dz) = (pos.x - self.center.x, pos.z - self.center.z);
        let far;
        let biomes: &[u16] = if dx.abs() <= 1 && dz.abs() <= 1 {
            self.around[((dz + 1) * 3 + dx + 1) as usize]
                .get_or_insert_with(|| generator.chunk_biomes(pos, shape))
        } else {
            far = generator.chunk_biomes(pos, shape);
            &far
        };
        let min = shape.min_y >> 2;
        let max = min + shape.height / 4 - 1;
        let qy = qy.clamp(min, max) - min;
        biomes[(qy * 16 + (qz & 3) * 4 + (qx & 3)) as usize]
    }
}

impl BiomeLookup for StoredBiomes<'_> {
    fn biome(&mut self, x: i32, y: i32, z: i32) -> u16 {
        let [qx, qy, qz] = zoom_cached(self.zoom_seed, &mut self.zoom_cell, x, y, z);
        self.noise_biome(qx, qy, qz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn cached_zoom_matches_uncached() {
        let mut cell = ZoomCell::empty();
        for x in -9..9 {
            for z in -9..9 {
                for y in -5..11 {
                    assert_eq!(zoom_cached(1234, &mut cell, x, y, z), zoom(1234, x, y, z));
                }
            }
        }
    }
}

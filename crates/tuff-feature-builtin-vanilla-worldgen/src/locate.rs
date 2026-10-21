//! The searches behind `/locate`: the nearest start of some structures and
//! the nearest biome, visiting candidates in the game's order. Each search
//! is long, so callers run it off the actors; candidates at the same
//! search distance are checked in parallel and the first in order wins.

use rayon::prelude::*;
use tuff_vanilla_data::structure::StructurePlacement;

use crate::density::Context;
use crate::generator::NoiseGenerator;
use crate::structure::placement::{Set, potential_chunk};
use tuff_core::world::generator::SkipChunk;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

/// How many placement cells out a structure search goes.
pub const STRUCTURE_RADIUS: i32 = 100;
/// How far a biome search goes, in blocks, and its sample spacing.
pub const BIOME_RADIUS: i32 = 6400;
pub const BIOME_STEP_HORIZONTAL: i32 = 32;
pub const BIOME_STEP_VERTICAL: i32 = 64;

/// Columns of a biome search checked in one parallel batch.
const BIOME_BATCH: usize = 2048;

fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let (dx, dy, dz) = (
        f64::from(a.x - b.x),
        f64::from(a.y - b.y),
        f64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

fn locate_pos(set: &Set, chunk: ChunkPos) -> BlockPos {
    let offset = set.data.placement.spread().locate_offset;
    BlockPos::new(
        (chunk.x << 4) + offset[0],
        offset[1],
        (chunk.z << 4) + offset[2],
    )
}

/// The wanted structure (an index into the generator's structures) that
/// starts in `pos` for `set`.
fn generating_at(
    generator: &NoiseGenerator,
    set: &Set,
    wanted: &[usize],
    pos: ChunkPos,
    shape: ChunkShape,
    skip: Option<SkipChunk<'_>>,
) -> Option<usize> {
    if skip.is_some_and(|skip| skip(pos)) {
        return None;
    }
    let structures = generator.structures();
    if !set.is_structure_chunk(structures, generator, pos) {
        return None;
    }
    if !wanted
        .iter()
        .any(|index| structures.can_start(generator, *index, pos, shape))
    {
        return None;
    }
    let start = structures.set_start(generator, set, pos, shape)?;
    let started = structures.index_of(start.structure.name)?;
    wanted.contains(&started).then_some(started)
}

/// The cells on the edge of the square `radius` cells out, x outermost.
fn ring(radius: i32) -> impl Iterator<Item = (i32, i32)> {
    (-radius..=radius).flat_map(move |x| {
        (-radius..=radius)
            .filter(move |z| x.abs() == radius || z.abs() == radius)
            .map(move |z| (x, z))
    })
}

/// The locate position and structure index of the nearest start of any
/// of `wanted` (indices into the generator's structures) from `from`.
pub fn nearest_structure(
    generator: &NoiseGenerator,
    wanted: &[usize],
    from: BlockPos,
    shape: ChunkShape,
) -> Option<(BlockPos, usize)> {
    nearest_structure_within(generator, wanted, from, STRUCTURE_RADIUS, shape, None)
}

/// [`nearest_structure`], searching `radius` placement cells out.
pub fn nearest_structure_within(
    generator: &NoiseGenerator,
    wanted: &[usize],
    from: BlockPos,
    radius_limit: i32,
    shape: ChunkShape,
    skip: Option<SkipChunk<'_>>,
) -> Option<(BlockPos, usize)> {
    let structures = generator.structures();
    let mut placements: Vec<(&Set, Vec<usize>)> = Vec::new();
    for &index in wanted {
        for set in structures
            .reachable_sets()
            .filter(|s| s.structures.iter().any(|(i, _)| *i == index))
        {
            match placements.iter_mut().find(|(s, _)| std::ptr::eq(*s, set)) {
                Some((_, list)) => list.push(index),
                None => placements.push((set, vec![index])),
            }
        }
    }
    let mut nearest: Option<(f64, BlockPos, usize)> = None;
    let consider = |pos: BlockPos, index: usize, nearest: &mut Option<(f64, BlockPos, usize)>| {
        let distance = dist_sqr(from, pos);
        if nearest.is_none_or(|(d, _, _)| distance < d) {
            *nearest = Some((distance, pos, index));
        }
    };
    let mut spread = Vec::new();
    for (set, list) in &placements {
        match &set.data.placement {
            StructurePlacement::ConcentricRings { .. } => {
                let positions = set.ring_positions(generator, structures.seed());
                let mut closest: Option<(f64, ChunkPos, usize)> = None;
                for &pos in positions {
                    let middle = BlockPos::new((pos.x << 4) + 8, 32, (pos.z << 4) + 8);
                    let distance = dist_sqr(middle, from);
                    if closest.is_some_and(|(d, _, _)| distance >= d) {
                        continue;
                    }
                    if let Some(index) = generating_at(generator, set, list, pos, shape, skip) {
                        closest = Some((distance, pos, index));
                    }
                }
                if let Some((_, pos, index)) = closest {
                    consider(locate_pos(set, pos), index, &mut nearest);
                }
            }
            StructurePlacement::DimensionOrigin { .. } => {
                let origin = ChunkPos::new(0, 0);
                if let Some(index) = generating_at(generator, set, list, origin, shape, skip) {
                    consider(locate_pos(set, origin), index, &mut nearest);
                }
            }
            StructurePlacement::RandomSpread {
                spacing,
                separation,
                spread_type,
                spread: data,
            } => spread.push((*set, list, *spacing, *separation, *spread_type, data.salt)),
        }
    }
    if spread.is_empty() {
        return nearest.map(|(_, pos, index)| (pos, index));
    }
    let (origin_x, origin_z) = (from.x >> 4, from.z >> 4);
    for radius in 0..=radius_limit {
        let mut found = false;
        for &(set, list, spacing, separation, spread_type, salt) in &spread {
            let cells: Vec<ChunkPos> = ring(radius)
                .map(|(x, z)| {
                    let sector = ChunkPos::new(origin_x + spacing * x, origin_z + spacing * z);
                    potential_chunk(
                        structures.seed(),
                        sector,
                        spacing,
                        separation,
                        spread_type,
                        salt,
                    )
                })
                .collect();
            let tags = tuff_datapack::tags::current();
            let hit = cells
                .par_iter()
                .map(|pos| {
                    tuff_datapack::tags::sync_scope(tags.clone(), || {
                        generating_at(generator, set, list, *pos, shape, skip).map(|i| (*pos, i))
                    })
                })
                .find_first(Option::is_some)
                .flatten();
            if let Some((pos, index)) = hit {
                found = true;
                consider(locate_pos(set, pos), index, &mut nearest);
            }
        }
        if found {
            break;
        }
    }
    nearest.map(|(_, pos, index)| (pos, index))
}

/// [`nearest_structure`] by resource ids: the locate position and id.
pub fn structure(
    generator: &NoiseGenerator,
    names: &[&str],
    from: BlockPos,
    shape: ChunkShape,
) -> Option<(BlockPos, &'static str)> {
    let known = generator.structures();
    let wanted: Vec<usize> = names.iter().filter_map(|n| known.index_of(n)).collect();
    let (pos, index) = nearest_structure(generator, &wanted, from, shape)?;
    Some((pos, known.entries[index].data.name))
}

/// [`structure`], searching `radius` placement cells out.
pub fn structure_within(
    generator: &NoiseGenerator,
    names: &[&str],
    from: BlockPos,
    radius: i32,
    shape: ChunkShape,
    skip: Option<SkipChunk<'_>>,
) -> Option<(BlockPos, &'static str)> {
    let known = generator.structures();
    let wanted: Vec<usize> = names.iter().filter_map(|n| known.index_of(n)).collect();
    let (pos, index) = nearest_structure_within(generator, &wanted, from, radius, shape, skip)?;
    Some((pos, known.entries[index].data.name))
}

/// The biome generated at each of `positions`.
pub fn biomes_at(generator: &NoiseGenerator, positions: &[BlockPos]) -> Vec<&'static str> {
    positions
        .par_iter()
        .map(|pos| {
            let mut ctx = Context::uncached();
            let mut last = None;
            let biome = generator.biome_source.biome_at(
                &generator.state.router,
                &mut ctx,
                &mut last,
                [pos.x >> 2, pos.y >> 2, pos.z >> 2],
            );
            tuff_vanilla_data::data_registry::WORLDGEN_BIOME.entries[biome as usize]
        })
        .collect()
}

/// [`nearest_biome`] by resource ids: the position and the biome's id.
pub fn biome(
    generator: &NoiseGenerator,
    names: &[&str],
    from: BlockPos,
    shape: ChunkShape,
) -> Option<(BlockPos, &'static str)> {
    let biomes = generator.biomes();
    let ids: Vec<u16> = names.iter().filter_map(|n| biomes.id(n)).collect();
    let mut wanted = vec![false; ids.iter().map(|&b| b as usize + 1).max()?];
    for id in ids {
        wanted[id as usize] = true;
    }
    let (pos, biome) = nearest_biome(generator, &wanted, from, shape)?;
    Some((pos, generator.biome_data(biome).name))
}

/// The columns of a square spiral out from the origin, starting east then
/// turning south, `radius` steps out.
struct Spiral {
    x: i32,
    z: i32,
    legs: i32,
    leg: i32,
    leg_size: i32,
    leg_index: i32,
    done: bool,
}

const SPIRAL_DIRECTIONS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

impl Spiral {
    fn new(radius: i32) -> Self {
        Self {
            x: 0,
            z: 1,
            legs: 4 * radius,
            leg: -1,
            leg_size: 0,
            leg_index: 0,
            done: false,
        }
    }
}

impl Iterator for Spiral {
    type Item = (i32, i32);

    fn next(&mut self) -> Option<(i32, i32)> {
        if self.done {
            return None;
        }
        let (dx, dz) = SPIRAL_DIRECTIONS[((self.leg + 4) % 4) as usize];
        self.x += dx;
        self.z += dz;
        if self.leg_index >= self.leg_size {
            if self.leg >= self.legs {
                self.done = true;
                return None;
            }
            self.leg += 1;
            self.leg_index = 0;
            self.leg_size = self.leg / 2 + 1;
        }
        self.leg_index += 1;
        Some((self.x, self.z))
    }
}

/// Heights from `origin` (clamped into the bounds) outward, alternating
/// below and above, `step` apart.
fn out_from_origin(origin: i32, lower: i32, upper: i32, step: i32) -> Vec<i32> {
    let center = origin.clamp(lower, upper);
    let mut out = Vec::new();
    let mut cursor = center;
    loop {
        let distance = (center - cursor).abs();
        if center - distance < lower && center + distance > upper {
            return out;
        }
        out.push(cursor);
        let below = cursor <= center;
        let can_rise = center + distance + step <= upper;
        let mut next = center + distance + step;
        if !below || !can_rise {
            let attempt = center - distance - if below { step } else { 0 };
            if attempt >= lower {
                next = attempt;
            }
        }
        cursor = next;
    }
}

/// The nearest sampled position whose biome is wanted (indexed by biome
/// id), and that biome.
pub fn nearest_biome(
    generator: &NoiseGenerator,
    wanted: &[bool],
    from: BlockPos,
    shape: ChunkShape,
) -> Option<(BlockPos, u16)> {
    let is_wanted = |b: u16| wanted.get(b as usize).copied().unwrap_or(false);
    if !generator
        .biome_source
        .possible_biomes()
        .into_iter()
        .any(is_wanted)
    {
        return None;
    }
    let heights = out_from_origin(
        from.y,
        shape.min_y + 1,
        shape.min_y + shape.height,
        BIOME_STEP_VERTICAL,
    );
    let sample = |(sx, sz): (i32, i32)| {
        let (x, z) = (
            from.x + sx * BIOME_STEP_HORIZONTAL,
            from.z + sz * BIOME_STEP_HORIZONTAL,
        );
        let mut ctx = Context::uncached();
        let mut last = None;
        heights.iter().find_map(|&y| {
            let biome = generator.biome_source.biome_at(
                &generator.state.router,
                &mut ctx,
                &mut last,
                [x >> 2, y >> 2, z >> 2],
            );
            is_wanted(biome).then_some((BlockPos::new(x, y, z), biome))
        })
    };
    let mut spiral = Spiral::new(BIOME_RADIUS.div_euclid(BIOME_STEP_HORIZONTAL));
    loop {
        let batch: Vec<(i32, i32)> = spiral.by_ref().take(BIOME_BATCH).collect();
        if batch.is_empty() {
            return None;
        }
        if let Some(found) = batch
            .par_iter()
            .map(|column| sample(*column))
            .find_first(Option::is_some)
            .flatten()
        {
            return Some(found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn spiral_winds_outward_east_then_south() {
        let first: Vec<(i32, i32)> = Spiral::new(2).take(10).collect();
        assert_eq!(
            first,
            [
                (0, 0),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
                (0, -1),
                (1, -1),
                (2, -1)
            ]
        );
        let all: Vec<(i32, i32)> = Spiral::new(2).collect();
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), 25);
        assert_eq!(all.len(), 25);
        assert!(all.iter().all(|(x, z)| x.abs() <= 2 && z.abs() <= 2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn heights_alternate_out_from_the_origin() {
        assert_eq!(
            out_from_origin(70, -63, 320, 64),
            [70, 134, 6, 198, -58, 262]
        );
        assert_eq!(
            out_from_origin(-100, -63, 320, 64),
            [-63, 1, 65, 129, 193, 257]
        );
        assert_eq!(out_from_origin(0, 0, 0, 64), [0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rings_visit_only_the_edge_in_order() {
        assert_eq!(ring(0).collect::<Vec<_>>(), [(0, 0)]);
        let one: Vec<_> = ring(1).collect();
        assert_eq!(
            one,
            [
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, -1),
                (0, 1),
                (1, -1),
                (1, 0),
                (1, 1)
            ]
        );
        assert_eq!(ring(3).count(), 24);
    }
}

//! Structure set placement: which chunks a structure set may start in.

use std::sync::OnceLock;

use tuff_vanilla_data::structure::{
    FrequencyReduction, Spread, SpreadType, StructurePlacement, StructureSet,
};

use crate::density::Context;
use crate::generator::NoiseGenerator;
use crate::structure::{Entry, Structures};
use tuff_item::random::{LegacyRandom, Random, RandomSource, WorldgenRandom};
use tuff_world_model::chunk::biome_id;
use tuff_world_model::pos::ChunkPos;

const ARBITRARY_SALT: i32 = 10387320;

/// A structure set some biome of the generator can hold.
pub struct Set {
    pub data: &'static StructureSet,
    /// `(structure index, weight)`.
    pub structures: Vec<(usize, i32)>,
    /// Whether any of the structures has an implemented type.
    pub generates: bool,
    rings: OnceLock<Vec<ChunkPos>>,
    reachable: OnceLock<bool>,
}

impl Set {
    /// Whether a biome the generator can produce allows one of the
    /// structures, judged by the tags in effect on first use.
    pub fn is_reachable(&self, entries: &[Entry], possible: &[bool]) -> bool {
        *self.reachable.get_or_init(|| {
            self.structures.iter().any(|(index, _)| {
                possible
                    .iter()
                    .enumerate()
                    .any(|(biome, p)| *p && entries[*index].allows(biome as u16))
            })
        })
    }

    pub fn new(data: &'static StructureSet, entries: &[Entry]) -> Self {
        let structures: Vec<(usize, i32)> = data
            .structures
            .iter()
            .map(|(name, weight)| {
                let index = entries
                    .iter()
                    .position(|e| e.data.name == *name)
                    .expect("structure sets name known structures");
                (index, *weight)
            })
            .collect();
        Self {
            generates: structures
                .iter()
                .any(|(index, _)| entries[*index].kind.is_some()),
            data,
            structures,
            rings: OnceLock::new(),
            reachable: OnceLock::new(),
        }
    }

    /// Whether a structure of this set may start in `pos`.
    pub fn is_structure_chunk(
        &self,
        structures: &Structures,
        generator: &NoiseGenerator,
        pos: ChunkPos,
    ) -> bool {
        self.generates
            && self.is_reachable(&structures.entries, &structures.possible)
            && self.is_placement(structures, generator, pos)
    }

    pub(crate) fn is_placement(
        &self,
        structures: &Structures,
        generator: &NoiseGenerator,
        pos: ChunkPos,
    ) -> bool {
        let seed = structures.seed;
        let spread = self.data.placement.spread();
        let placed = match &self.data.placement {
            StructurePlacement::RandomSpread {
                spacing,
                separation,
                spread_type,
                spread,
            } => {
                potential_chunk(seed, pos, *spacing, *separation, *spread_type, spread.salt) == pos
            }
            StructurePlacement::ConcentricRings { .. } => {
                self.ring_positions(generator, seed).contains(&pos)
            }
            StructurePlacement::DimensionOrigin { .. } => pos == ChunkPos::new(0, 0),
        };
        placed
            && passes_frequency(spread, seed, pos)
            && !spread.exclusion_zone.as_ref().is_some_and(|zone| {
                let Some(other) = structures
                    .sets
                    .iter()
                    .find(|s| s.data.name == zone.other_set)
                else {
                    return false;
                };
                let range = zone.chunk_count;
                (pos.x - range..=pos.x + range).any(|x| {
                    (pos.z - range..=pos.z + range)
                        .any(|z| other.is_placement(structures, generator, ChunkPos::new(x, z)))
                })
            })
    }

    /// The ring positions of a concentric rings placement, computed once.
    pub fn ring_positions(&self, generator: &NoiseGenerator, seed: i64) -> &[ChunkPos] {
        self.rings.get_or_init(|| {
            let StructurePlacement::ConcentricRings {
                distance,
                ring_spread,
                count,
                preferred_biomes,
                preferred_tag,
                ..
            } = &self.data.placement
            else {
                return Vec::new();
            };
            let tagged = preferred_tag.map(|tag| {
                tuff_datapack::tags::with(|tags| {
                    tags.get("minecraft:worldgen/biome", tag)
                        .map_or_else(Vec::new, <[String]>::to_vec)
                })
            });
            let tagged: Option<Vec<&str>> = tagged
                .as_ref()
                .map(|names| names.iter().map(String::as_str).collect());
            ring_positions(
                generator,
                seed,
                *distance,
                *ring_spread,
                *count,
                tagged.as_deref().unwrap_or(preferred_biomes),
            )
        })
    }
}

/// The chunk a random spread placement picks in the grid cell of `pos`.
pub fn potential_chunk(
    seed: i64,
    pos: ChunkPos,
    spacing: i32,
    separation: i32,
    spread_type: SpreadType,
    salt: i32,
) -> ChunkPos {
    let grid_x = pos.x.div_euclid(spacing);
    let grid_z = pos.z.div_euclid(spacing);
    let mut random = WorldgenRandom::legacy(0);
    random.set_large_feature_with_salt(seed, grid_x, grid_z, salt);
    let limit = spacing - separation;
    let evaluate = |random: &mut WorldgenRandom| match spread_type {
        SpreadType::Linear => random.next_int_bounded(limit),
        SpreadType::Triangular => {
            (random.next_int_bounded(limit) + random.next_int_bounded(limit)) / 2
        }
    };
    let x = evaluate(&mut random);
    let z = evaluate(&mut random);
    ChunkPos::new(grid_x * spacing + x, grid_z * spacing + z)
}

fn passes_frequency(spread: &Spread, seed: i64, pos: ChunkPos) -> bool {
    if spread.frequency >= 1.0 {
        return true;
    }
    let probability = spread.frequency;
    let mut random = WorldgenRandom::legacy(0);
    match spread.frequency_reduction {
        FrequencyReduction::Default => {
            random.set_large_feature_with_salt(seed, spread.salt, pos.x, pos.z);
            random.next_float() < probability
        }
        FrequencyReduction::LegacyType1 => {
            let (cx, cz) = (pos.x >> 4, pos.z >> 4);
            random.set_seed((cx ^ (cz << 4)) as i64 ^ seed);
            random.next_int();
            random.next_int_bounded((1.0 / probability) as i32) == 0
        }
        FrequencyReduction::LegacyType2 => {
            random.set_large_feature_with_salt(seed, pos.x, pos.z, ARBITRARY_SALT);
            random.next_float() < probability
        }
        FrequencyReduction::LegacyType3 => {
            random.set_large_feature_seed(seed, pos.x, pos.z);
            random.next_double() < probability as f64
        }
    }
}

/// The chunks a concentric rings placement starts in: `count` positions on
/// rings around the origin, each moved to a preferred biome nearby.
pub fn ring_positions(
    generator: &NoiseGenerator,
    seed: i64,
    distance: i32,
    spread: i32,
    count: i32,
    preferred: &[&str],
) -> Vec<ChunkPos> {
    if count == 0 {
        return Vec::new();
    }
    let preferred: Vec<u16> = preferred
        .iter()
        .filter_map(|b| biome_id(b).or_else(|| generator.biomes().id(b)))
        .collect();
    let mut random = LegacyRandom::new(seed);
    let mut angle = random.next_double() * std::f64::consts::PI * 2.0;
    let mut spread = spread;
    let mut position_in_circle = 0;
    let mut circle = 0;
    let mut tasks = Vec::with_capacity(count as usize);
    for i in 0..count {
        let dist = (4 * distance + distance * circle * 6) as f64
            + (random.next_double() - 0.5) * distance as f64 * 2.5;
        let initial_x = (angle.cos() * dist + 0.5).floor() as i32;
        let initial_z = (angle.sin() * dist + 0.5).floor() as i32;
        tasks.push((initial_x, initial_z, random.fork()));
        angle += std::f64::consts::PI * 2.0 / spread as f64;
        position_in_circle += 1;
        if position_in_circle == spread {
            circle += 1;
            position_in_circle = 0;
            spread = (spread + 2 * spread / (circle + 1)).min(count - i);
            angle += random.next_double() * std::f64::consts::PI * 2.0;
        }
    }
    // Ring positions are computed inside a lazily initialised cell that
    // rayon jobs wait on, so the search must not run on the rayon pool: a
    // worker waiting for its own tasks could pick up a job that waits on
    // the same cell.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let per_thread = tasks.len().div_ceil(threads).max(1);
    let search = |(x, z, mut random): (i32, i32, Random)| -> ChunkPos {
        match find_biome_horizontal(
            generator,
            (x << 4) + 8,
            0,
            (z << 4) + 8,
            112,
            &preferred,
            &mut random,
        ) {
            Some((bx, bz)) => ChunkPos::new(bx >> 4, bz >> 4),
            None => ChunkPos::new(x, z),
        }
    };
    std::thread::scope(|scope| {
        let mut rest = tasks;
        let mut handles = Vec::new();
        while !rest.is_empty() {
            let batch: Vec<_> = rest.drain(..per_thread.min(rest.len())).collect();
            let search = &search;
            let thread = std::thread::Builder::new()
                .name(format!("ring-search-{}", handles.len()))
                .spawn_scoped(scope, move || {
                    batch.into_iter().map(search).collect::<Vec<_>>()
                })
                .expect("cannot start a ring search thread");
            handles.push(thread);
        }
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("ring search panicked"))
            .collect()
    })
}

/// A random position (in blocks) among the quart columns of a square of
/// `radius` blocks whose biome is allowed, reservoir sampled.
pub fn find_biome_horizontal(
    generator: &NoiseGenerator,
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    allowed: &[u16],
    random: &mut Random,
) -> Option<(i32, i32)> {
    let (cx, cz, r, qy) = (x >> 2, z >> 2, radius >> 2, y >> 2);
    let mut ctx = Context::builder().caches().build();
    let mut last = None;
    let mut result = None;
    let mut found = 0;
    for dz in -r..=r {
        for dx in -r..=r {
            let (qx, qz) = (cx + dx, cz + dz);
            let biome = generator.biome_source.biome_at(
                &generator.state.router,
                &mut ctx,
                &mut last,
                [qx, qy, qz],
            );
            if allowed.contains(&biome) {
                if result.is_none() || random.next_int_bounded(found + 1) == 0 {
                    result = Some((qx << 2, qz << 2));
                }
                found += 1;
            }
        }
    }
    result
}

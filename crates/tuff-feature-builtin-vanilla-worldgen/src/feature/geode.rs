//! Geodes: layered shells around a hollow, shaped by a few random points
//! and a noise, with an optional crack and crystals growing inward.

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{DIRECTIONS, Fluid, fluid, step};
use crate::feature::provider::StateProvider;
use crate::feature::{Feature, OwnedIntProvider, PlaceContext, Registry};
use crate::json::{self, Result, obj};
use crate::noise::NoiseStack;
use crate::normal_noise::NormalNoise;
use crate::tags::BlockSet;
use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:geode", |r, v| Ok(Arc::new(Geode::parse(r, v)?)));
}

struct Geode {
    filling: Arc<StateProvider>,
    inner_layer: Arc<StateProvider>,
    alternate_inner_layer: Arc<StateProvider>,
    middle_layer: Arc<StateProvider>,
    outer_layer: Arc<StateProvider>,
    inner_placements: Vec<BlockState>,
    cannot_replace: BlockSet,
    invalid_blocks: BlockSet,
    layers: [f64; 4],
    generate_crack_chance: f64,
    base_crack_size: f64,
    crack_point_offset: i32,
    use_potential_placements_chance: f64,
    use_alternate_layer0_chance: f64,
    placements_require_layer0_alternate: bool,
    outer_wall_distance: OwnedIntProvider,
    distribution_points: OwnedIntProvider,
    point_offset: OwnedIntProvider,
    min_gen_offset: i32,
    max_gen_offset: i32,
    noise_multiplier: f64,
    invalid_blocks_threshold: i32,
    noise: OnceLock<(i64, NoiseStack)>,
}

fn int_provider_or(value: Option<&Value>, default: OwnedIntProvider) -> Result<OwnedIntProvider> {
    match value {
        Some(value) => OwnedIntProvider::parse(value),
        None => Ok(default),
    }
}

impl Geode {
    fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "blocks",
                "layers",
                "crack",
                "use_potential_placements_chance",
                "use_alternate_layer0_chance",
                "placements_require_layer0_alternate",
                "outer_wall_distance",
                "distribution_points",
                "point_offset",
                "min_gen_offset",
                "max_gen_offset",
                "noise_multiplier",
                "invalid_blocks_threshold",
            ],
            "geode",
        )?;
        let blocks = obj(
            o.req("blocks")?,
            &[
                "filling_provider",
                "inner_layer_provider",
                "alternate_inner_layer_provider",
                "middle_layer_provider",
                "outer_layer_provider",
                "inner_placements",
                "cannot_replace",
                "invalid_blocks",
            ],
            "geode blocks",
        )?;
        let layers = obj(
            o.req("layers")?,
            &["filling", "inner_layer", "middle_layer", "outer_layer"],
            "geode layers",
        )?;
        let crack = obj(
            o.req("crack")?,
            &[
                "generate_crack_chance",
                "base_crack_size",
                "crack_point_offset",
            ],
            "geode crack",
        )?;
        let inner_placements = blocks
            .list("inner_placements")?
            .iter()
            .map(json::block_state)
            .collect::<Result<Vec<_>>>()?;
        if inner_placements.is_empty() {
            return Err("geode inner placements are empty".into());
        }
        Ok(Self {
            filling: StateProvider::parse(registry, blocks.req("filling_provider")?)?,
            inner_layer: StateProvider::parse(registry, blocks.req("inner_layer_provider")?)?,
            alternate_inner_layer: StateProvider::parse(
                registry,
                blocks.req("alternate_inner_layer_provider")?,
            )?,
            middle_layer: StateProvider::parse(registry, blocks.req("middle_layer_provider")?)?,
            outer_layer: StateProvider::parse(registry, blocks.req("outer_layer_provider")?)?,
            inner_placements,
            cannot_replace: json::block_set(blocks.req("cannot_replace")?)?,
            invalid_blocks: json::block_set(blocks.req("invalid_blocks")?)?,
            layers: [
                layers.f64_or("filling", 1.7)?,
                layers.f64_or("inner_layer", 2.2)?,
                layers.f64_or("middle_layer", 3.2)?,
                layers.f64_or("outer_layer", 4.2)?,
            ],
            generate_crack_chance: crack.f64_or("generate_crack_chance", 1.0)?,
            base_crack_size: crack.f64_or("base_crack_size", 2.0)?,
            crack_point_offset: crack.i32_or("crack_point_offset", 2)?,
            use_potential_placements_chance: o.f64_or("use_potential_placements_chance", 0.35)?,
            use_alternate_layer0_chance: o.f64_or("use_alternate_layer0_chance", 0.0)?,
            placements_require_layer0_alternate: o
                .bool_or("placements_require_layer0_alternate", true)?,
            outer_wall_distance: int_provider_or(
                o.get("outer_wall_distance"),
                OwnedIntProvider::Uniform(4, 5),
            )?,
            distribution_points: int_provider_or(
                o.get("distribution_points"),
                OwnedIntProvider::Uniform(3, 4),
            )?,
            point_offset: int_provider_or(o.get("point_offset"), OwnedIntProvider::Uniform(1, 2))?,
            min_gen_offset: o.i32_or("min_gen_offset", -16)?,
            max_gen_offset: o.i32_or("max_gen_offset", 16)?,
            noise_multiplier: o.f64_or("noise_multiplier", 0.05)?,
            invalid_blocks_threshold: o.i32("invalid_blocks_threshold")?,
            noise: OnceLock::new(),
        })
    }

    fn noise(&self, seed: i64) -> &NoiseStack {
        let (created_for, noise) = self.noise.get_or_init(|| {
            let parameters = NormalNoise::parity(-4, &[1.0]);
            (
                seed,
                parameters.create(&mut Random::Legacy(LegacyRandom::new(seed))),
            )
        });
        assert_eq!(*created_for, seed, "a feature registry serves one seed");
        noise
    }

    fn safe_set(&self, level: &mut Region, pos: BlockPos, state: BlockState) {
        if !self.cannot_replace.contains_state(level.block(pos)) {
            level.set_block(pos, state);
        }
    }
}

/// One seed point of the shell field and the softening added to its
/// squared distance.
struct Seed {
    at: BlockPos,
    softening: f64,
}

/// Sum of `1 / sqrt(d^2 + softening) + jitter` over the seeds; the jitter
/// is added once per seed.
fn field(cell: BlockPos, seeds: impl Iterator<Item = (BlockPos, f64)>, jitter: f64) -> f64 {
    seeds.fold(0.0, |sum, (at, softening)| {
        let (dx, dy, dz) = (
            (cell.x - at.x) as f64,
            (cell.y - at.y) as f64,
            (cell.z - at.z) as f64,
        );
        sum + (1.0 / (dx * dx + dy * dy + dz * dz + softening).sqrt() + jitter)
    })
}

/// Horizontal offsets of the crack line, picked by a roll of four, in units
/// of the crack's reach.
const CRACK_SIDES: [(i32, i32); 4] = [(1, 0), (0, 1), (1, 1), (0, 0)];
const CRACK_HEIGHTS: [i32; 3] = [7, 5, 1];

/// The band of the shell a cell falls in, from the hollow outwards.
enum Band {
    Hollow,
    Lining,
    Middle,
    Outer,
}

/// Field strengths where each band starts, hollow first.
struct Bands {
    hollow: f64,
    lining: f64,
    middle: f64,
    outer: f64,
}

impl Bands {
    fn of(&self, strength: f64) -> Option<Band> {
        if strength < self.outer {
            None
        } else if strength >= self.hollow {
            Some(Band::Hollow)
        } else if strength >= self.lining {
            Some(Band::Lining)
        } else if strength >= self.middle {
            Some(Band::Middle)
        } else {
            Some(Band::Outer)
        }
    }
}

fn can_cluster_grow_at(state: BlockState) -> bool {
    is_air(state) || (state.block() == Block::Water && state.get("level") == Some("0"))
}

impl Geode {
    fn bands(&self, widening: f64) -> Bands {
        let edge = |at: f64| 1.0 / at.sqrt();
        let [hollow, lining, middle, outer] = self.layers;
        Bands {
            hollow: edge(hollow),
            lining: edge(lining + widening),
            middle: edge(middle + widening),
            outer: edge(outer + widening),
        }
    }

    /// Rolls the seed points, or `None` when too many land in air or
    /// invalid blocks.
    fn seeds(&self, ctx: &mut PlaceContext, origin: BlockPos, count: i32) -> Option<Vec<Seed>> {
        let mut seeds = Vec::with_capacity(count.max(0) as usize);
        let mut rejected = 0;
        for _ in 0..count {
            let mut axis = || self.outer_wall_distance.sample(ctx.random);
            let (x, y, z) = (axis(), axis(), axis());
            let at = BlockPos::new(origin.x + x, origin.y + y, origin.z + z);
            let found = ctx.level.block(at);
            if is_air(found) || self.invalid_blocks.contains_state(found) {
                rejected += 1;
                if rejected > self.invalid_blocks_threshold {
                    return None;
                }
            }
            let softening = self.point_offset.sample(ctx.random) as f64;
            seeds.push(Seed { at, softening });
        }
        Some(seeds)
    }

    fn lining(&self, ctx: &mut PlaceContext, cell: BlockPos, crystals: &mut Vec<BlockPos>) {
        let alternate = (ctx.random.next_float() as f64) < self.use_alternate_layer0_chance;
        let provider = if alternate {
            &self.alternate_inner_layer
        } else {
            &self.inner_layer
        };
        let state = provider.state(ctx.level, ctx.random, cell);
        self.safe_set(ctx.level, cell, state);
        let may_grow = alternate || !self.placements_require_layer0_alternate;
        if may_grow && (ctx.random.next_float() as f64) < self.use_potential_placements_chance {
            crystals.push(cell);
        }
    }

    fn open_crack(&self, level: &mut Region, cell: BlockPos) {
        self.safe_set(level, cell, Block::Air.default_state());
        for direction in DIRECTIONS {
            let next = step(cell, direction);
            let state = level.block(next);
            level.schedule_own_fluid_tick(next, state);
        }
    }

    /// Grows a crystal from `base` into the first open neighbour.
    fn grow_crystal(&self, ctx: &mut PlaceContext, base: BlockPos) {
        let pick = ctx
            .random
            .next_int_bounded(self.inner_placements.len() as i32);
        let mut crystal = self.inner_placements[pick as usize];
        let faces = crystal.get("facing").is_some();
        let logs = crystal.get("waterlogged").is_some();
        for direction in DIRECTIONS {
            if faces {
                crystal = crystal.with("facing", direction.name()).unwrap_or(crystal);
            }
            let target = step(base, direction);
            let there = ctx.level.block(target);
            if logs {
                let source = matches!(fluid(there), Fluid::Water | Fluid::Lava);
                crystal = crystal
                    .with("waterlogged", if source { "true" } else { "false" })
                    .unwrap_or(crystal);
            }
            if can_cluster_grow_at(there) {
                self.safe_set(ctx.level, target, crystal);
                return;
            }
        }
    }
}

impl Feature for Geode {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let count = self.distribution_points.sample(ctx.random);
        let noise = self.noise(ctx.level.seed());
        let widening = count as f64 / self.outer_wall_distance.max() as f64;
        let bands = self.bands(widening);
        let crack_extra = if count > 3 { widening } else { 0.0 };
        let crack_threshold =
            1.0 / (self.base_crack_size + ctx.random.next_double() / 2.0 + crack_extra).sqrt();
        let cracked = (ctx.random.next_float() as f64) < self.generate_crack_chance;
        let Some(seeds) = self.seeds(ctx, origin, count) else {
            return false;
        };
        let crack_line = cracked.then(|| {
            let (sx, sz) = CRACK_SIDES[ctx.random.next_int_bounded(4) as usize];
            let reach = count * 2 + 1;
            CRACK_HEIGHTS
                .map(|h| BlockPos::new(origin.x + sx * reach, origin.y + h, origin.z + sz * reach))
        });
        let crack_softening = self.crack_point_offset as f64;
        let mut crystals = Vec::new();
        let lo = |c: i32| c + self.min_gen_offset;
        let hi = |c: i32| c + self.max_gen_offset;
        for z in lo(origin.z)..=hi(origin.z) {
            for y in lo(origin.y)..=hi(origin.y) {
                for x in lo(origin.x)..=hi(origin.x) {
                    let cell = BlockPos::new(x, y, z);
                    let jitter =
                        noise.get(x as f64, y as f64, z as f64) as f64 * self.noise_multiplier;
                    let strength = field(cell, seeds.iter().map(|s| (s.at, s.softening)), jitter);
                    let Some(band) = bands.of(strength) else {
                        continue;
                    };
                    let in_crack = |line: &[BlockPos; 3]| {
                        let pull = field(cell, line.iter().map(|&p| (p, crack_softening)), jitter);
                        pull >= crack_threshold
                    };
                    let provider = match band {
                        Band::Hollow => &self.filling,
                        _ if crack_line.as_ref().is_some_and(in_crack) => {
                            self.open_crack(ctx.level, cell);
                            continue;
                        }
                        Band::Lining => {
                            self.lining(ctx, cell, &mut crystals);
                            continue;
                        }
                        Band::Middle => &self.middle_layer,
                        Band::Outer => &self.outer_layer,
                    };
                    let state = provider.state(ctx.level, ctx.random, cell);
                    self.safe_set(ctx.level, cell, state);
                }
            }
        }
        for base in crystals {
            self.grow_crystal(ctx, base);
        }
        true
    }
}

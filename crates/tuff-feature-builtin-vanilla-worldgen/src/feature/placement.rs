//! Placement modifiers: each turns one position into zero or more.

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;

use crate::feature::predicate::{BlockPredicate, parse_direction, step};
use crate::feature::{OwnedHeightProvider, OwnedIntProvider, PlaceContext};
use crate::json::{self, Result, obj};
use crate::noise::Simplex;
use crate::proto::HeightmapKind;
use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_world_model::BlockPos;

/// A placement modifier supplied from outside (a plugin).
pub trait CustomPlacement: Send + Sync {
    fn modify(&self, ctx: &mut PlaceContext, pos: BlockPos, out: &mut Vec<BlockPos>);
}

pub enum Placement {
    Count(OwnedIntProvider),
    InSquare,
    Biome,
    BlockPredicateFilter(BlockPredicate),
    Heightmap(HeightmapKind),
    Offset(OwnedIntProvider, OwnedIntProvider, OwnedIntProvider),
    HeightRange(OwnedHeightProvider),
    RarityFilter(i32),
    RandomChance(f32),
    SurfaceWaterDepthFilter(i32),
    EnvironmentScan {
        direction: Facing,
        target: BlockPredicate,
        allowed: BlockPredicate,
        max_steps: i32,
    },
    CountOnEveryLayer(OwnedIntProvider),
    NoiseThresholdCount {
        noise_level: f64,
        below: i32,
        above: i32,
    },
    NoiseBasedCount {
        ratio: i32,
        factor: f64,
        offset: f64,
    },
    SurfaceRelativeThreshold {
        heightmap: HeightmapKind,
        min: i32,
        max: i32,
    },
    Fixed(Vec<BlockPos>),
    Cuboid {
        xz: OwnedIntProvider,
        y: OwnedIntProvider,
        edges: bool,
        interior: bool,
    },
    RandomlySelected(Vec<Placement>),
    Custom(Arc<dyn CustomPlacement>),
}

/// The biome info noise count placements read.
fn biome_info_noise() -> &'static Simplex {
    static NOISE: OnceLock<Simplex> = OnceLock::new();
    NOISE.get_or_init(|| Simplex::new(&mut Random::Legacy(LegacyRandom::new(2345)), true))
}

fn heightmap(value: &Value) -> Result<HeightmapKind> {
    let name = value.as_str().ok_or("heightmaps are named")?;
    HeightmapKind::from_name(name).ok_or_else(|| format!("unknown heightmap {name}"))
}

impl Placement {
    pub fn parse(value: &Value) -> Result<Self> {
        let what = "placement";
        Ok(match json::qualify(json::kind(value)?).as_str() {
            "minecraft:count" => Self::Count(OwnedIntProvider::parse(
                obj(value, &["count"], what)?.req("count")?,
            )?),
            "minecraft:in_square" => Self::InSquare,
            "minecraft:biome" => Self::Biome,
            "minecraft:block_predicate_filter" => Self::BlockPredicateFilter(
                BlockPredicate::parse(obj(value, &["predicate"], what)?.req("predicate")?)?,
            ),
            "minecraft:heightmap" => Self::Heightmap(heightmap(
                obj(value, &["heightmap"], what)?.req("heightmap")?,
            )?),
            "minecraft:offset" => {
                let o = obj(value, &["x", "y", "z"], what)?;
                let part = |key: &str| match o.get(key) {
                    Some(v) => OwnedIntProvider::parse(v),
                    None => Ok(OwnedIntProvider::Constant(0)),
                };
                Self::Offset(part("x")?, part("y")?, part("z")?)
            }
            "minecraft:height_range" => Self::HeightRange(OwnedHeightProvider::parse(
                obj(value, &["height"], what)?.req("height")?,
            )?),
            "minecraft:rarity_filter" => {
                Self::RarityFilter(obj(value, &["chance"], what)?.i32("chance")?)
            }
            "minecraft:random_chance" => {
                Self::RandomChance(obj(value, &["chance"], what)?.f32("chance")?)
            }
            "minecraft:surface_water_depth_filter" => Self::SurfaceWaterDepthFilter(
                obj(value, &["max_water_depth"], what)?.i32("max_water_depth")?,
            ),
            "minecraft:environment_scan" => {
                let o = obj(
                    value,
                    &[
                        "direction_of_search",
                        "target_condition",
                        "allowed_search_condition",
                        "max_steps",
                    ],
                    what,
                )?;
                Self::EnvironmentScan {
                    direction: parse_direction(o.str("direction_of_search")?)?,
                    target: BlockPredicate::parse(o.req("target_condition")?)?,
                    allowed: match o.get("allowed_search_condition") {
                        Some(v) => BlockPredicate::parse(v)?,
                        None => BlockPredicate::True,
                    },
                    max_steps: o.i32("max_steps")?,
                }
            }
            "minecraft:count_on_every_layer" => Self::CountOnEveryLayer(OwnedIntProvider::parse(
                obj(value, &["count"], what)?.req("count")?,
            )?),
            "minecraft:noise_threshold_count" => {
                let o = obj(value, &["noise_level", "below_noise", "above_noise"], what)?;
                Self::NoiseThresholdCount {
                    noise_level: o.f64("noise_level")?,
                    below: o.i32("below_noise")?,
                    above: o.i32("above_noise")?,
                }
            }
            "minecraft:noise_based_count" => {
                let o = obj(
                    value,
                    &["noise_to_count_ratio", "noise_factor", "noise_offset"],
                    what,
                )?;
                Self::NoiseBasedCount {
                    ratio: o.i32("noise_to_count_ratio")?,
                    factor: o.f64("noise_factor")?,
                    offset: o.f64_or("noise_offset", 0.0)?,
                }
            }
            "minecraft:surface_relative_threshold_filter" => {
                let o = obj(
                    value,
                    &["heightmap", "min_inclusive", "max_inclusive"],
                    what,
                )?;
                Self::SurfaceRelativeThreshold {
                    heightmap: heightmap(o.req("heightmap")?)?,
                    min: o.i32_or("min_inclusive", i32::MIN)?,
                    max: o.i32_or("max_inclusive", i32::MAX)?,
                }
            }
            "minecraft:fixed_placement" => {
                let o = obj(value, &["positions"], what)?;
                Self::Fixed(
                    o.list("positions")?
                        .iter()
                        .map(|p| json::vec3i(p).map(|[x, y, z]| BlockPos::new(x, y, z)))
                        .collect::<Result<_>>()?,
                )
            }
            "minecraft:cuboid" => {
                let o = obj(
                    value,
                    &["xz_size", "y_size", "include_edges", "include_interior"],
                    what,
                )?;
                Self::Cuboid {
                    xz: OwnedIntProvider::parse(o.req("xz_size")?)?,
                    y: OwnedIntProvider::parse(o.req("y_size")?)?,
                    edges: o.bool_or("include_edges", true)?,
                    interior: o.bool_or("include_interior", true)?,
                }
            }
            "minecraft:randomly_selected" => {
                let o = obj(value, &["placements"], what)?;
                Self::RandomlySelected(
                    o.list("placements")?
                        .iter()
                        .map(Self::parse)
                        .collect::<Result<_>>()?,
                )
            }
            other => return Err(format!("unmodeled placement modifier {other}")),
        })
    }

    pub fn modify(
        &self,
        ctx: &mut PlaceContext,
        top: Option<&str>,
        pos: BlockPos,
        out: &mut Vec<BlockPos>,
    ) {
        match self {
            Self::Count(count) => {
                let n = count.sample(ctx.random);
                for _ in 0..n {
                    out.push(pos);
                }
            }
            Self::InSquare => {
                let x = ctx.random.next_int_bounded(16) + pos.x;
                let z = ctx.random.next_int_bounded(16) + pos.z;
                out.push(BlockPos::new(x, pos.y, z));
            }
            Self::Biome => {
                let Some(top) = top else {
                    return;
                };
                let biome = ctx.level.biome(pos);
                if ctx.level.biome_has_feature(biome, top) {
                    out.push(pos);
                }
            }
            Self::BlockPredicateFilter(predicate) => {
                if predicate.test(ctx.level, pos) {
                    out.push(pos);
                }
            }
            Self::Heightmap(kind) => {
                let height = ctx.level.height(*kind, pos.x, pos.z);
                if height > ctx.level.min_y() {
                    out.push(BlockPos::new(pos.x, height, pos.z));
                }
            }
            Self::Offset(x, y, z) => {
                let dx = x.sample(ctx.random);
                let dy = y.sample(ctx.random);
                let dz = z.sample(ctx.random);
                out.push(BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz));
            }
            Self::HeightRange(height) => {
                let y = height.sample(ctx.random, ctx.level.span);
                out.push(BlockPos::new(pos.x, y, pos.z));
            }
            Self::RarityFilter(chance) => {
                if ctx.random.next_float() < 1.0 / *chance as f32 {
                    out.push(pos);
                }
            }
            Self::RandomChance(chance) => {
                if ctx.random.next_float() < *chance {
                    out.push(pos);
                }
            }
            Self::SurfaceWaterDepthFilter(max) => {
                let floor = ctx.level.height(HeightmapKind::OceanFloor, pos.x, pos.z);
                let surface = ctx.level.height(HeightmapKind::WorldSurface, pos.x, pos.z);
                if surface - floor <= *max {
                    out.push(pos);
                }
            }
            Self::EnvironmentScan {
                direction,
                target,
                allowed,
                max_steps,
            } => {
                let mut p = pos;
                if !allowed.test(ctx.level, p) {
                    return;
                }
                for _ in 0..*max_steps {
                    if target.test(ctx.level, p) {
                        out.push(p);
                        return;
                    }
                    p = step(p, *direction);
                    if ctx.level.is_outside_build_height(p.y) {
                        return;
                    }
                    if !allowed.test(ctx.level, p) {
                        break;
                    }
                }
                if target.test(ctx.level, p) {
                    out.push(p);
                }
            }
            Self::CountOnEveryLayer(count) => {
                let mut layer = 0;
                loop {
                    let mut found = false;
                    let mut i = 0;
                    while i < count.sample(ctx.random) {
                        let x = ctx.random.next_int_bounded(16) + pos.x;
                        let z = ctx.random.next_int_bounded(16) + pos.z;
                        let start = ctx.level.height(HeightmapKind::MotionBlocking, x, z);
                        if let Some(y) = on_ground_y(ctx, x, start, z, layer) {
                            out.push(BlockPos::new(x, y, z));
                            found = true;
                        }
                        i += 1;
                    }
                    layer += 1;
                    if !found {
                        break;
                    }
                }
            }
            Self::NoiseThresholdCount {
                noise_level,
                below,
                above,
            } => {
                let noise =
                    biome_info_noise().get_2d(pos.x as f64 / 200.0, pos.z as f64 / 200.0) as f64;
                let n = if noise < *noise_level { *below } else { *above };
                for _ in 0..n {
                    out.push(pos);
                }
            }
            Self::NoiseBasedCount {
                ratio,
                factor,
                offset,
            } => {
                let noise =
                    biome_info_noise().get_2d(pos.x as f64 / factor, pos.z as f64 / factor) as f64;
                let n = ((noise + offset) * *ratio as f64).ceil() as i32;
                for _ in 0..n {
                    out.push(pos);
                }
            }
            Self::SurfaceRelativeThreshold {
                heightmap,
                min,
                max,
            } => {
                let surface = ctx.level.height(*heightmap, pos.x, pos.z) as i64;
                let (lo, hi) = (surface + *min as i64, surface + *max as i64);
                if lo <= pos.y as i64 && pos.y as i64 <= hi {
                    out.push(pos);
                }
            }
            Self::Fixed(positions) => {
                let (cx, cz) = (pos.x >> 4, pos.z >> 4);
                for p in positions {
                    if p.x >> 4 == cx && p.z >> 4 == cz {
                        out.push(*p);
                    }
                }
            }
            Self::Cuboid {
                xz,
                y,
                edges,
                interior,
            } => {
                let height = y.sample(ctx.random);
                let width = xz.sample(ctx.random);
                let length = xz.sample(ctx.random);
                for x in 0..=width {
                    for dy in 0..=height {
                        for z in 0..=length {
                            let x_edge = x == 0 || x == width;
                            let y_edge = dy == 0 || dy == height;
                            let z_edge = z == 0 || z == length;
                            let keep = (*edges || !x_edge || !y_edge)
                                && (*edges || !z_edge || !y_edge)
                                && (*edges || !x_edge || !z_edge)
                                && (*interior || x_edge || y_edge || z_edge);
                            if keep {
                                out.push(BlockPos::new(pos.x + x, pos.y + dy, pos.z + z));
                            }
                        }
                    }
                }
            }
            Self::RandomlySelected(list) => {
                let chosen = &list[ctx.random.next_int_bounded(list.len() as i32) as usize];
                chosen.modify(ctx, top, pos, out);
            }
            Self::Custom(custom) => custom.modify(ctx, pos, out),
        }
    }
}

fn is_empty(block: Block) -> bool {
    matches!(
        block,
        Block::Air | Block::CaveAir | Block::VoidAir | Block::Water | Block::Lava
    )
}

fn on_ground_y(ctx: &mut PlaceContext, x: i32, start: i32, z: i32, layer: i32) -> Option<i32> {
    let mut current_layer = 0;
    let mut current = ctx.level.block(BlockPos::new(x, start, z));
    let mut y = start;
    while y > ctx.level.min_y() {
        let below = ctx.level.block(BlockPos::new(x, y - 1, z));
        if !is_empty(below.block()) && is_empty(current.block()) && below.block() != Block::Bedrock
        {
            if current_layer == layer {
                return Some(y - 1 + 1);
            }
            current_layer += 1;
        }
        current = below;
        y -= 1;
    }
    None
}

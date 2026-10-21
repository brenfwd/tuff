//! Speleothems: single pointed blocks, clusters of them, and large
//! dripstone columns.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{Fluid, fluid, step};
use crate::feature::provider::{clamped_map, random_direction};
use crate::feature::rocks::scan_column;
use crate::feature::value::{OwnedFloatProvider, OwnedIntProvider};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, obj};
use crate::proto::HeightmapKind;
use crate::tags::{self, BlockSet};
use tuff_core::mth::{self, clamp_f32, normal, random_between, random_between_inclusive};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:speleothem", |_, v| {
        let o = obj(
            v,
            &[
                "base_block",
                "pointed_block",
                "replaceable_blocks",
                "chance_of_taller_generation",
                "chance_of_directional_spread",
                "chance_of_spread_radius2",
                "chance_of_spread_radius3",
            ],
            "speleothem",
        )?;
        Ok(Arc::new(Speleothem {
            blocks: Blocks::parse(&o)?,
            taller: o.f32_or("chance_of_taller_generation", 0.2)?,
            directional_spread: o.f32_or("chance_of_directional_spread", 0.7)?,
            spread_radius2: o.f32_or("chance_of_spread_radius2", 0.5)?,
            spread_radius3: o.f32_or("chance_of_spread_radius3", 0.5)?,
        }))
    });
    registry.register("minecraft:speleothem_cluster", |_, v| {
        let o = obj(
            v,
            &[
                "base_block",
                "pointed_block",
                "replaceable_blocks",
                "floor_to_ceiling_search_range",
                "height",
                "radius",
                "max_stalagmite_stalactite_height_diff",
                "height_deviation",
                "speleothem_block_layer_thickness",
                "density",
                "wetness",
                "chance_of_speleothem_at_max_distance_from_center",
                "max_distance_from_edge_affecting_chance_of_speleothem",
                "max_distance_from_center_affecting_height_bias",
            ],
            "speleothem_cluster",
        )?;
        Ok(Arc::new(Cluster {
            blocks: Blocks::parse(&o)?,
            search_range: o.i32("floor_to_ceiling_search_range")?,
            height: OwnedIntProvider::parse(o.req("height")?)?,
            radius: OwnedIntProvider::parse(o.req("radius")?)?,
            max_height_diff: o.i32("max_stalagmite_stalactite_height_diff")?,
            height_deviation: o.i32("height_deviation")?,
            layer_thickness: OwnedIntProvider::parse(o.req("speleothem_block_layer_thickness")?)?,
            density: OwnedFloatProvider::parse(o.req("density")?)?,
            wetness: OwnedFloatProvider::parse(o.req("wetness")?)?,
            edge_chance: o.f32("chance_of_speleothem_at_max_distance_from_center")?,
            edge_distance: o.i32("max_distance_from_edge_affecting_chance_of_speleothem")?,
            center_distance: o.i32("max_distance_from_center_affecting_height_bias")?,
        }))
    });
    registry.register("minecraft:large_dripstone", |_, v| {
        let o = obj(
            v,
            &[
                "replaceable_blocks",
                "floor_to_ceiling_search_range",
                "column_radius",
                "height_scale",
                "max_column_radius_to_cave_height_ratio",
                "stalactite_bluntness",
                "stalagmite_bluntness",
                "wind_speed",
                "min_radius_for_wind",
                "min_bluntness_for_wind",
            ],
            "large_dripstone",
        )?;
        Ok(Arc::new(LargeDripstone {
            replaceable: json::block_set(o.req("replaceable_blocks")?)?,
            search_range: o.i32_or("floor_to_ceiling_search_range", 30)?,
            column_radius: OwnedIntProvider::parse(o.req("column_radius")?)?,
            height_scale: OwnedFloatProvider::parse(o.req("height_scale")?)?,
            radius_ratio: o.f32("max_column_radius_to_cave_height_ratio")?,
            stalactite_bluntness: OwnedFloatProvider::parse(o.req("stalactite_bluntness")?)?,
            stalagmite_bluntness: OwnedFloatProvider::parse(o.req("stalagmite_bluntness")?)?,
            wind_speed: OwnedFloatProvider::parse(o.req("wind_speed")?)?,
            min_radius_for_wind: o.i32("min_radius_for_wind")?,
            min_bluntness_for_wind: o.f32("min_bluntness_for_wind")?,
        }))
    });
}

fn is_empty_or_water(state: BlockState) -> bool {
    is_air(state) || state.block() == Block::Water
}

fn is_empty_or_water_or_lava(state: BlockState) -> bool {
    is_empty_or_water(state) || state.block() == Block::Lava
}

fn is_water_fluid(state: BlockState) -> bool {
    matches!(fluid(state), Fluid::Water | Fluid::FlowingWater)
}

/// The base block, the pointed block and the blocks the base may replace.
struct Blocks {
    base: Block,
    pointed: Block,
    replaceable: BlockSet,
}

impl Blocks {
    fn parse(o: &json::Obj) -> json::Result<Self> {
        Ok(Self {
            base: json::block_state(o.req("base_block")?)?.block(),
            pointed: json::block_state(o.req("pointed_block")?)?.block(),
            replaceable: json::block_set(o.req("replaceable_blocks")?)?,
        })
    }

    fn is_base(&self, state: BlockState) -> bool {
        state.block() == self.base || self.replaceable.contains_state(state)
    }

    fn place_base(&self, level: &mut Region, pos: BlockPos) -> bool {
        if self.replaceable.contains_state(level.block(pos)) {
            level.set_block(pos, self.base.default_state());
            true
        } else {
            false
        }
    }

    fn pointed_state(&self, direction: Facing, thickness: &str) -> BlockState {
        let state = self.pointed.default_state();
        let state = state
            .with("vertical_direction", direction.name())
            .unwrap_or(state);
        state.with("thickness", thickness).unwrap_or(state)
    }

    /// Grows a pointed column from `start` toward `tip`, base first, if the
    /// block behind `start` can hold it.
    fn grow(
        &self,
        level: &mut Region,
        start: BlockPos,
        tip: Facing,
        height: i32,
        merged_tip: bool,
    ) {
        if !self.is_base(level.block(step(start, tip.opposite()))) {
            return;
        }
        let mut states = Vec::new();
        if height >= 3 {
            states.push(self.pointed_state(tip, "base"));
            for _ in 0..height - 3 {
                states.push(self.pointed_state(tip, "middle"));
            }
        }
        if height >= 2 {
            states.push(self.pointed_state(tip, "frustum"));
        }
        if height >= 1 {
            states.push(self.pointed_state(tip, if merged_tip { "tip_merge" } else { "tip" }));
        }
        let mut pos = start;
        for state in states {
            let wet = is_water_fluid(level.block(pos));
            let state = state
                .with("waterlogged", if wet { "true" } else { "false" })
                .unwrap_or(state);
            level.set_block(pos, state);
            pos = step(pos, tip);
        }
    }
}

struct Speleothem {
    blocks: Blocks,
    taller: f32,
    directional_spread: f32,
    spread_radius2: f32,
    spread_radius3: f32,
}

impl Feature for Speleothem {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let above = self
            .blocks
            .is_base(ctx.level.block(step(origin, Facing::Up)));
        let below = self
            .blocks
            .is_base(ctx.level.block(step(origin, Facing::Down)));
        let tip = match (above, below) {
            (true, true) => {
                if ctx.random.next_bool() {
                    Facing::Down
                } else {
                    Facing::Up
                }
            }
            (true, false) => Facing::Down,
            (false, true) => Facing::Up,
            (false, false) => return false,
        };
        let root = step(origin, tip.opposite());
        self.blocks.place_base(ctx.level, root);
        for direction in HORIZONTAL {
            if ctx.random.next_float() > self.directional_spread {
                continue;
            }
            let pos1 = step(root, direction);
            self.blocks.place_base(ctx.level, pos1);
            if ctx.random.next_float() > self.spread_radius2 {
                continue;
            }
            let pos2 = step(pos1, random_direction(ctx.random));
            self.blocks.place_base(ctx.level, pos2);
            if ctx.random.next_float() <= self.spread_radius3 {
                let pos3 = step(pos2, random_direction(ctx.random));
                self.blocks.place_base(ctx.level, pos3);
            }
        }
        let height = if ctx.random.next_float() >= self.taller
            || !is_empty_or_water(ctx.level.block(step(origin, tip)))
        {
            1
        } else {
            2
        };
        self.blocks.grow(ctx.level, origin, tip, height, false);
        true
    }
}

/// A column between a floor and a ceiling found by scanning from a block;
/// either end may be missing.
#[derive(Clone, Copy)]
struct Column {
    floor: Option<i32>,
    ceiling: Option<i32>,
}

impl Column {
    fn scan(
        level: &Region,
        pos: BlockPos,
        range: i32,
        inside: impl Fn(BlockState) -> bool + Copy,
        edge: impl Fn(BlockState) -> bool + Copy,
    ) -> Option<Self> {
        if !inside(level.block(pos)) {
            return None;
        }
        Some(Self {
            ceiling: scan_column(level, pos, range, inside, edge, Facing::Up),
            floor: scan_column(level, pos, range, inside, edge, Facing::Down),
        })
    }

    fn height(&self) -> Option<i32> {
        Some(self.ceiling? - self.floor? - 1)
    }
}

fn clamped_map_f32(v: f32, from_min: f32, from_max: f32, to_min: f32, to_max: f32) -> f32 {
    let t = (v - from_min) / (from_max - from_min);
    if t < 0.0 {
        to_min
    } else if t > 1.0 {
        to_max
    } else {
        to_min + t * (to_max - to_min)
    }
}

struct Cluster {
    blocks: Blocks,
    search_range: i32,
    height: OwnedIntProvider,
    radius: OwnedIntProvider,
    max_height_diff: i32,
    height_deviation: i32,
    layer_thickness: OwnedIntProvider,
    density: OwnedFloatProvider,
    wetness: OwnedFloatProvider,
    edge_chance: f32,
    edge_distance: i32,
    center_distance: i32,
}

impl Cluster {
    fn can_place_pool(&self, level: &Region, pos: BlockPos) -> bool {
        let state = level.block(pos);
        if state.block() == Block::Water
            || state.block() == self.blocks.base
            || state.block() == self.blocks.pointed
            || is_water_fluid(level.block(step(pos, Facing::Up)))
        {
            return false;
        }
        let adjacent_ok = |p: BlockPos| {
            let s = level.block(p);
            tags::base_stone_overworld().contains_state(s) || is_water_fluid(s)
        };
        HORIZONTAL.iter().all(|d| adjacent_ok(step(pos, *d)))
            && adjacent_ok(step(pos, Facing::Down))
    }

    fn replace_with_base(
        &self,
        level: &mut Region,
        first: BlockPos,
        count: i32,
        direction: Facing,
    ) {
        let mut pos = first;
        let mut i = 0;
        while i < count && self.blocks.place_base(level, pos) {
            pos = step(pos, direction);
            i += 1;
        }
    }

    fn speleothem_height(
        &self,
        random: &mut WorldgenRandom,
        dx: i32,
        dz: i32,
        density: f32,
        max_height: i32,
    ) -> i32 {
        if random.next_float() > density {
            return 0;
        }
        let distance = dx.abs() + dz.abs();
        let mean = clamped_map(
            distance as f64,
            0.0,
            self.center_distance as f64,
            max_height as f64 / 2.0,
            0.0,
        ) as f32;
        clamp_f32(
            normal(random, mean, self.height_deviation as f32),
            0.0,
            max_height as f32,
        ) as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn place_column(
        &self,
        ctx: &mut PlaceContext,
        pos: BlockPos,
        dx: i32,
        dz: i32,
        wetness: f32,
        chance: f64,
        cluster_height: i32,
        density: f32,
    ) {
        let Some(base_column) =
            Column::scan(ctx.level, pos, self.search_range, is_empty_or_water, |s| {
                !is_empty_or_water(s)
            })
        else {
            return;
        };
        let ceiling = base_column.ceiling;
        if ceiling.is_none() && base_column.floor.is_none() {
            return;
        }
        let at = |y: i32| BlockPos::new(pos.x, y, pos.z);
        let want_pool = ctx.random.next_float() < wetness;
        let column = match base_column.floor {
            Some(floor) if want_pool && self.can_place_pool(ctx.level, at(floor)) => {
                ctx.level.set_block(at(floor), Block::Water.default_state());
                Column {
                    floor: Some(floor - 1),
                    ceiling,
                }
            }
            _ => base_column,
        };
        let floor = column.floor;
        let want_stalactite = ctx.random.next_double() < chance;
        let stalactite_height = match ceiling {
            Some(c) if want_stalactite && ctx.level.block(at(c)).block() != Block::Lava => {
                let thickness = self.layer_thickness.sample(ctx.random);
                self.replace_with_base(ctx.level, at(c), thickness, Facing::Up);
                let max = match floor {
                    Some(f) => cluster_height.min(c - f),
                    None => cluster_height,
                };
                self.speleothem_height(ctx.random, dx, dz, density, max)
            }
            _ => 0,
        };
        let want_stalagmite = ctx.random.next_double() < chance;
        let stalagmite_height = match floor {
            Some(f) if want_stalagmite && ctx.level.block(at(f)).block() != Block::Lava => {
                let thickness = self.layer_thickness.sample(ctx.random);
                self.replace_with_base(ctx.level, at(f), thickness, Facing::Down);
                if ceiling.is_some() {
                    0.max(
                        stalactite_height
                            + random_between_inclusive(
                                ctx.random,
                                -self.max_height_diff,
                                self.max_height_diff,
                            ),
                    )
                } else {
                    self.speleothem_height(ctx.random, dx, dz, density, cluster_height)
                }
            }
            _ => 0,
        };
        let (actual_stalactite, actual_stalagmite) = match (ceiling, floor) {
            (Some(c), Some(f)) if c - stalactite_height <= f + stalagmite_height => {
                let lowest_bottom = (c - stalactite_height).max(f + 1);
                let highest_top = (f + stalagmite_height).min(c - 1);
                let bottom = random_between_inclusive(ctx.random, lowest_bottom, highest_top + 1);
                (c - bottom, bottom - 1 - f)
            }
            _ => (stalactite_height, stalagmite_height),
        };
        let merge_tips = ctx.random.next_bool()
            && actual_stalactite > 0
            && actual_stalagmite > 0
            && column.height() == Some(actual_stalactite + actual_stalagmite);
        if let Some(c) = ceiling {
            self.blocks.grow(
                ctx.level,
                at(c - 1),
                Facing::Down,
                actual_stalactite,
                merge_tips,
            );
        }
        if let Some(f) = floor {
            self.blocks.grow(
                ctx.level,
                at(f + 1),
                Facing::Up,
                actual_stalagmite,
                merge_tips,
            );
        }
    }
}

impl Feature for Cluster {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_empty_or_water(ctx.level.block(origin)) {
            return false;
        }
        let height = self.height.sample(ctx.random);
        let wetness = self.wetness.sample(ctx.random);
        let density = self.density.sample(ctx.random);
        let x_radius = self.radius.sample(ctx.random);
        let z_radius = self.radius.sample(ctx.random);
        for dx in -x_radius..=x_radius {
            for dz in -z_radius..=z_radius {
                let from_edge = (x_radius - dx.abs()).min(z_radius - dz.abs());
                let chance = clamped_map_f32(
                    from_edge as f32,
                    0.0,
                    self.edge_distance as f32,
                    self.edge_chance,
                    1.0,
                ) as f64;
                let pos = BlockPos::new(origin.x + dx, origin.y, origin.z + dz);
                self.place_column(ctx, pos, dx, dz, wetness, chance, height, density);
            }
        }
        true
    }
}

/// The height of a speleothem's cone at a distance from its axis.
fn speleothem_height(distance: f64, radius: f64, scale: f64, bluntness: f64) -> f64 {
    let distance = if distance < bluntness {
        bluntness
    } else {
        distance
    };
    let r = distance / radius * 0.384;
    let part1 = 0.75 * r.powf(1.3333333333333333);
    let part2 = r.powf(0.6666666666666666);
    let part3 = 0.3333333333333333 * r.ln();
    let relative = scale * (part1 - part2 - part3);
    relative.max(0.0) / 0.384 * radius
}

fn rim_in_stone(level: &Region, center: BlockPos, radius: i32) -> bool {
    if is_empty_or_water_or_lava(level.block(center)) {
        return false;
    }
    let increment = 6.0 / radius as f32;
    let mut angle = 0.0f32;
    while angle < std::f32::consts::TAU {
        let dx = (mth::cos(angle as f64) * radius as f32) as i32;
        let dz = (mth::sin(angle as f64) * radius as f32) as i32;
        if is_empty_or_water_or_lava(level.block(BlockPos::new(
            center.x + dx,
            center.y,
            center.z + dz,
        ))) {
            return false;
        }
        angle += increment;
    }
    true
}

struct Wind {
    origin_y: i32,
    speed: Option<(f64, f64)>,
    max_offset: i32,
}

impl Wind {
    fn none() -> Self {
        Self {
            origin_y: 0,
            speed: None,
            max_offset: 0,
        }
    }

    fn offset(&self, pos: BlockPos) -> BlockPos {
        let Some((sx, sz)) = self.speed else {
            return pos;
        };
        let dy = (self.origin_y - pos.y) as f64;
        let dx = ((sx * dy).floor() as i32).clamp(-self.max_offset, self.max_offset);
        let dz = ((sz * dy).floor() as i32).clamp(-self.max_offset, self.max_offset);
        BlockPos::new(pos.x + dx, pos.y, pos.z + dz)
    }
}

struct Dripstone {
    root: BlockPos,
    pointing_up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl Dripstone {
    fn height_at(&self, distance: f32) -> i32 {
        speleothem_height(
            distance as f64,
            self.radius as f64,
            self.scale,
            self.bluntness,
        ) as i32
    }

    fn suitable_for_wind(&self, min_radius: i32, min_bluntness: f32) -> bool {
        self.radius >= min_radius && self.bluntness >= min_bluntness as f64
    }

    /// Slides the base away from the tip until its rim sits in stone,
    /// halving the radius whenever ten steps (or the height) are not enough.
    fn embed(&mut self, level: &Region, wind: &Wind) -> bool {
        let away = if self.pointing_up { -1 } else { 1 };
        while self.radius > 1 {
            let tries = self.height_at(0.0).min(10);
            for shift in 0..tries.max(0) {
                let base = BlockPos::new(self.root.x, self.root.y + shift * away, self.root.z);
                if level.block(base).block() == Block::Lava {
                    return false;
                }
                if rim_in_stone(level, wind.offset(base), self.radius) {
                    self.root = base;
                    return true;
                }
            }
            self.radius /= 2;
        }
        false
    }

    /// Grows one column of the cone from `base` towards the tip.
    fn grow_column(&self, level: &mut Region, mut cursor: BlockPos, length: i32, wind: &Wind) {
        let (rise, limit) = if self.pointing_up {
            (
                1,
                level.height(HeightmapKind::WorldSurfaceWg, cursor.x, cursor.z),
            )
        } else {
            (-1, i32::MAX)
        };
        let mut emerged = false;
        for _ in 0..length {
            if cursor.y >= limit {
                return;
            }
            let at = wind.offset(cursor);
            let found = level.block(at);
            if is_empty_or_water_or_lava(found) {
                emerged = true;
                level.set_block(at, Block::DripstoneBlock.default_state());
            } else if emerged && tags::base_stone_overworld().contains_state(found) {
                return;
            }
            cursor.y += rise;
        }
    }

    fn place_blocks(&self, level: &mut Region, random: &mut WorldgenRandom, wind: &Wind) {
        let r = self.radius;
        for dx in -r..=r {
            for dz in -r..=r {
                let off_axis = ((dx * dx + dz * dz) as f32 as f64).sqrt() as f32;
                if off_axis > r as f32 {
                    continue;
                }
                let full = self.height_at(off_axis);
                if full <= 0 {
                    continue;
                }
                let length = if (random.next_float() as f64) < 0.2 {
                    (full as f32 * random_between(random, 0.8, 1.0)) as i32
                } else {
                    full
                };
                let base = BlockPos::new(self.root.x + dx, self.root.y, self.root.z + dz);
                self.grow_column(level, base, length, wind);
            }
        }
    }
}

struct LargeDripstone {
    replaceable: BlockSet,
    search_range: i32,
    column_radius: OwnedIntProvider,
    height_scale: OwnedFloatProvider,
    radius_ratio: f32,
    stalactite_bluntness: OwnedFloatProvider,
    stalagmite_bluntness: OwnedFloatProvider,
    wind_speed: OwnedFloatProvider,
    min_radius_for_wind: i32,
    min_bluntness_for_wind: f32,
}

impl Feature for LargeDripstone {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_empty_or_water(ctx.level.block(origin)) {
            return false;
        }
        let edge = |s: BlockState| {
            s.block() == Block::DripstoneBlock
                || self.replaceable.contains_state(s)
                || s.block() == Block::Lava
        };
        let Some(column) = Column::scan(
            ctx.level,
            origin,
            self.search_range,
            is_empty_or_water,
            edge,
        ) else {
            return false;
        };
        let (Some(floor), Some(ceiling), Some(height)) =
            (column.floor, column.ceiling, column.height())
        else {
            return false;
        };
        if height < 4 {
            return false;
        }
        let by_height = (height as f32 * self.radius_ratio) as i32;
        let max_radius = by_height.clamp(self.column_radius.min(), self.column_radius.max());
        let radius = random_between_inclusive(ctx.random, self.column_radius.min(), max_radius);
        let mut make = |y: i32, up: bool, bluntness: &OwnedFloatProvider| Dripstone {
            root: BlockPos::new(origin.x, y, origin.z),
            pointing_up: up,
            radius,
            bluntness: bluntness.sample(ctx.random) as f64,
            scale: self.height_scale.sample(ctx.random) as f64,
        };
        let mut stalactite = make(ceiling - 1, false, &self.stalactite_bluntness);
        let mut stalagmite = make(floor + 1, true, &self.stalagmite_bluntness);
        let wind = if stalactite
            .suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
            && stalagmite.suitable_for_wind(self.min_radius_for_wind, self.min_bluntness_for_wind)
        {
            let speed = self.wind_speed.sample(ctx.random);
            let direction = random_between(ctx.random, 0.0, std::f32::consts::PI);
            Wind {
                origin_y: origin.y,
                speed: Some((
                    (mth::cos(direction as f64) * speed) as f64,
                    (mth::sin(direction as f64) * speed) as f64,
                )),
                max_offset: 16 - radius,
            }
        } else {
            Wind::none()
        };
        let stalactite_embedded = stalactite.embed(ctx.level, &wind);
        let stalagmite_embedded = stalagmite.embed(ctx.level, &wind);
        if stalactite_embedded {
            stalactite.place_blocks(ctx.level, ctx.random, &wind);
        }
        if stalagmite_embedded {
            stalagmite.place_blocks(ctx.level, ctx.random, &wind);
        }
        true
    }
}

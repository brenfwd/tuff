//! The simpler feature kinds: single blocks, selectors, columns, piles,
//! disks, lakes, springs and snow.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::predicate::{BlockPredicate, Fluid, RuleTest, fluid, parse_direction, step};
use crate::feature::provider::StateProvider;
use crate::feature::{Feature, OwnedIntProvider, PlaceContext, PlacedFeature, Registry};
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::tags::BlockSet;
use crate::temperature;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:simple_block", |r, v| {
        let o = obj(v, &["to_place", "schedule_tick"], "simple_block")?;
        Ok(Arc::new(SimpleBlock(
            StateProvider::parse(r, o.req("to_place")?)?,
            o.bool_or("schedule_tick", false)?,
        )))
    });
    registry.register("minecraft:replace_single_block", |_, v| {
        let o = obj(v, &["targets"], "replace_single_block")?;
        let mut targets = Vec::new();
        for entry in o.list("targets")? {
            let t = obj(entry, &["target", "state"], "block replacement")?;
            targets.push((
                RuleTest::parse(t.req("target")?)?,
                json::block_state(t.req("state")?)?,
            ));
        }
        Ok(Arc::new(ReplaceSingleBlock(targets)))
    });
    registry.register("minecraft:random_selector", |r, v| {
        let o = obj(v, &["features", "default"], "random_selector")?;
        let mut features = Vec::new();
        for entry in o.list("features")? {
            let e = obj(entry, &["feature", "chance"], "random_selector entry")?;
            features.push((r.placed(e.req("feature")?)?, e.f32("chance")?));
        }
        Ok(Arc::new(RandomSelector {
            features,
            default: r.placed(o.req("default")?)?,
        }))
    });
    registry.register("minecraft:simple_random_selector", |r, v| {
        let o = obj(v, &["features"], "simple_random_selector")?;
        Ok(Arc::new(SimpleRandomSelector(placed_list(
            r,
            o.req("features")?,
        )?)))
    });
    registry.register("minecraft:weighted_random_selector", |r, v| {
        let o = obj(v, &["features"], "weighted_random_selector")?;
        let mut entries = Vec::new();
        for entry in o.list("features")? {
            let e = obj(entry, &["data", "weight"], "weighted entry")?;
            entries.push((r.placed(e.req("data")?)?, e.i32("weight")?));
        }
        Ok(Arc::new(WeightedRandomSelector(entries)))
    });
    registry.register("minecraft:random_boolean_selector", |r, v| {
        let o = obj(
            v,
            &["feature_true", "feature_false"],
            "random_boolean_selector",
        )?;
        Ok(Arc::new(RandomBoolean(
            r.placed(o.req("feature_true")?)?,
            r.placed(o.req("feature_false")?)?,
        )))
    });
    registry.register("minecraft:sequence", |r, v| {
        let o = obj(v, &["features"], "sequence")?;
        Ok(Arc::new(Sequence(placed_list(r, o.req("features")?)?)))
    });
    registry.register("minecraft:overlay", |r, v| {
        let o = obj(v, &["features"], "overlay")?;
        Ok(Arc::new(Overlay(placed_list(r, o.req("features")?)?)))
    });
    registry.register("minecraft:block_column", |r, v| {
        let o = obj(
            v,
            &["layers", "direction", "allowed_placement", "prioritize_tip"],
            "block_column",
        )?;
        let mut layers = Vec::new();
        for layer in o.list("layers")? {
            let l = obj(
                layer,
                &["height", "provider", "state"],
                "block_column layer",
            )?;
            let provider = l
                .get("provider")
                .or_else(|| l.get("state"))
                .ok_or("layer without provider")?;
            layers.push((
                OwnedIntProvider::parse(l.req("height")?)?,
                StateProvider::parse(r, provider)?,
            ));
        }
        Ok(Arc::new(BlockColumn {
            layers,
            direction: parse_direction(o.str("direction")?)?,
            allowed: BlockPredicate::parse(o.req("allowed_placement")?)?,
            prioritize_tip: o.bool("prioritize_tip")?,
        }))
    });
    registry.register("minecraft:fill_layer", |_, v| {
        let o = obj(v, &["height", "state"], "fill_layer")?;
        let height = o.i32("height")?;
        if !(0..=4096).contains(&height) {
            return Err(format!("fill_layer height {height} is out of range"));
        }
        Ok(Arc::new(FillLayer {
            height,
            state: json::block_state(o.req("state")?)?,
        }))
    });
    registry.register("minecraft:block_pile", |r, v| {
        let o = obj(v, &["state_provider"], "block_pile")?;
        Ok(Arc::new(BlockPile(StateProvider::parse(
            r,
            o.req("state_provider")?,
        )?)))
    });
    registry.register("minecraft:disk", |r, v| {
        let o = obj(
            v,
            &["state_provider", "target", "radius", "half_height"],
            "disk",
        )?;
        Ok(Arc::new(Disk {
            provider: StateProvider::parse(r, o.req("state_provider")?)?,
            target: BlockPredicate::parse(o.req("target")?)?,
            radius: OwnedIntProvider::parse(o.req("radius")?)?,
            half_height: o.i32("half_height")?,
        }))
    });
    registry.register("minecraft:spring_feature", |_, v| {
        let o = obj(
            v,
            &[
                "state",
                "requires_block_below",
                "rock_count",
                "hole_count",
                "valid_blocks",
            ],
            "spring",
        )?;
        let state = o.req("state")?;
        let fluid = state
            .get("Name")
            .or_else(|| state.get("id"))
            .and_then(Value::as_str)
            .ok_or("spring fluid")?;
        let block = match json::qualify(fluid).as_str() {
            "minecraft:water" | "minecraft:flowing_water" => Block::Water,
            "minecraft:lava" | "minecraft:flowing_lava" => Block::Lava,
            other => return Err(format!("unknown spring fluid {other}")),
        };
        Ok(Arc::new(Spring {
            fluid: block.default_state(),
            requires_block_below: o.bool_or("requires_block_below", true)?,
            rock_count: o.i32_or("rock_count", 4)?,
            hole_count: o.i32_or("hole_count", 1)?,
            valid: json::block_set(o.req("valid_blocks")?)?,
        }))
    });
    registry.register("minecraft:no_op", |_, v| {
        obj(v, &[], "no_op")?;
        Ok(Arc::new(NoOp))
    });
    registry.register("minecraft:freeze_top_layer", |_, v| {
        obj(v, &[], "freeze_top_layer")?;
        Ok(Arc::new(FreezeTopLayer))
    });
    registry.register("minecraft:lake", |r, v| {
        let o = obj(
            v,
            &[
                "fluid",
                "barrier",
                "can_place_feature",
                "can_replace_with_air_or_fluid",
                "can_replace_with_barrier",
            ],
            "lake",
        )?;
        Ok(Arc::new(Lake {
            fluid: StateProvider::parse(r, o.req("fluid")?)?,
            barrier: StateProvider::parse(r, o.req("barrier")?)?,
            can_place: BlockPredicate::parse(o.req("can_place_feature")?)?,
            replace_with_air_or_fluid: BlockPredicate::parse(
                o.req("can_replace_with_air_or_fluid")?,
            )?,
            replace_with_barrier: BlockPredicate::parse(o.req("can_replace_with_barrier")?)?,
        }))
    });
}

fn placed_list(registry: &mut Registry, value: &Value) -> Result<Vec<Arc<PlacedFeature>>> {
    match value {
        Value::Array(list) => list.iter().map(|v| registry.placed(v)).collect(),
        Value::String(name) if name.starts_with('#') => {
            Err(format!("placed feature tags are unmodeled: {name}"))
        }
        other => Ok(vec![registry.placed(other)?]),
    }
}

fn place_nested(feature: &PlacedFeature, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
    feature.place(ctx, origin, None)
}

pub(crate) struct FillLayer {
    pub height: i32,
    pub state: BlockState,
}

impl Feature for FillLayer {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let y = ctx.level.min_y() + self.height;
        for dx in 0..16 {
            for dz in 0..16 {
                let pos = BlockPos::new(origin.x + dx, y, origin.z + dz);
                if is_air(ctx.level.block(pos)) {
                    ctx.level.set_block(pos, self.state);
                }
            }
        }
        true
    }
}

struct SimpleBlock(Arc<StateProvider>, bool);

pub fn has_same_fluid(a: BlockState, b: BlockState) -> bool {
    fluid(a) == fluid(b)
}

struct ReplaceSingleBlock(Vec<(RuleTest, BlockState)>);

impl Feature for ReplaceSingleBlock {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let current = ctx.level.block(origin);
        for (test, state) in &self.0 {
            if test.test(current, origin, ctx.random) {
                ctx.level.set_block(origin, *state);
                break;
            }
        }
        true
    }
}

impl Feature for SimpleBlock {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let Some(state) = self.0.optional_state(ctx.level, ctx.random, origin) else {
            return false;
        };
        let level = &*ctx.level;
        if !level.can_survive(state, origin) {
            return false;
        }
        let block = state.block();
        if block.kind() == tuff_vanilla_data::BlockKind::DoublePlant {
            let above_pos = BlockPos::new(origin.x, origin.y + 1, origin.z);
            let above = ctx.level.block(above_pos);
            if is_air(above) || (has_same_fluid(state, above) && above.is_replaceable()) {
                place_double_plant(ctx, state, origin);
            } else {
                return false;
            }
        } else if block == Block::PaleMossCarpet {
            place_moss_carpet(ctx, origin);
        } else {
            ctx.level.set_block(origin, state);
        }
        if self.1 {
            let block = ctx.level.block(origin).block();
            ctx.level.schedule_block_tick(origin, block, 1);
        }
        true
    }
}

const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

fn carpet_side(state: BlockState, direction: Facing) -> &'static str {
    state.get(direction.name()).unwrap_or("none")
}

fn carpet_updated_state(
    level: &crate::feature::level::Region<'_>,
    mut state: BlockState,
    pos: BlockPos,
) -> BlockState {
    for direction in HORIZONTAL {
        let neighbour = level.block(step(pos, direction));
        let face = direction.opposite();
        let supported =
            neighbour.is_face_sturdy(face, Support::Full) || neighbour.is_collision_face_full(face);
        let mut side = if supported { "low" } else { "none" };
        if side == "low" {
            let above = level.block(BlockPos::new(pos.x, pos.y + 1, pos.z));
            if above.block() == Block::PaleMossCarpet
                && carpet_side(above, direction) != "none"
                && above.bottom() != Some(true)
            {
                side = "tall";
            }
            if state.bottom() != Some(true) {
                let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
                if below.block() == Block::PaleMossCarpet && carpet_side(below, direction) == "none"
                {
                    side = "none";
                }
            }
        }
        if let Some(next) = state.with(direction.name(), side) {
            state = next;
        }
    }
    state
}

fn carpet_topper(
    level: &crate::feature::level::Region<'_>,
    random: &mut tuff_item::random::WorldgenRandom,
    pos: BlockPos,
) -> Option<BlockState> {
    let up = BlockPos::new(pos.x, pos.y + 1, pos.z);
    let previous = level.block(up);
    let carpet_above = previous.block() == Block::PaleMossCarpet;
    if (carpet_above && previous.bottom() == Some(true))
        || (!carpet_above && !previous.is_replaceable())
    {
        return None;
    }
    let base = Block::PaleMossCarpet.default_state().with_bottom(false)?;
    let mut state = carpet_updated_state(level, base, up);
    for direction in HORIZONTAL {
        if carpet_side(state, direction) != "none"
            && !random.next_bool()
            && let Some(next) = state.with(direction.name(), "none")
        {
            state = next;
        }
    }
    let has_faces = HORIZONTAL
        .into_iter()
        .any(|d| carpet_side(state, d) != "none");
    (has_faces && state != previous).then_some(state)
}

fn place_moss_carpet(ctx: &mut PlaceContext, pos: BlockPos) {
    let layer = carpet_updated_state(ctx.level, Block::PaleMossCarpet.default_state(), pos);
    ctx.level.set_block(pos, layer);
    if let Some(top) = carpet_topper(ctx.level, ctx.random, pos) {
        ctx.level
            .set_block(BlockPos::new(pos.x, pos.y + 1, pos.z), top);
        let bottom = carpet_updated_state(ctx.level, layer, pos);
        ctx.level.set_block(pos, bottom);
    }
}

/// Places both halves of a tall plant, each waterlogged if it replaces
/// water.
pub fn place_double_plant(ctx: &mut PlaceContext, state: BlockState, pos: BlockPos) {
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    let waterlog = |s: BlockState, at: BlockState| -> BlockState {
        if s.get("waterlogged").is_some() {
            let wet = fluid(at) == Fluid::Water;
            s.with("waterlogged", if wet { "true" } else { "false" })
                .unwrap_or(s)
        } else {
            s
        }
    };
    let lower = state.with("half", "lower").unwrap_or(state);
    let upper = state.with("half", "upper").unwrap_or(state);
    let lower = waterlog(lower, ctx.level.block(pos));
    let upper = waterlog(upper, ctx.level.block(above));
    ctx.level.set_block(pos, lower);
    ctx.level.set_block(above, upper);
}

struct RandomSelector {
    features: Vec<(Arc<PlacedFeature>, f32)>,
    default: Arc<PlacedFeature>,
}

impl Feature for RandomSelector {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        for (feature, chance) in &self.features {
            if ctx.random.next_float() < *chance {
                return place_nested(feature, ctx, origin);
            }
        }
        place_nested(&self.default, ctx, origin)
    }
}

struct SimpleRandomSelector(Vec<Arc<PlacedFeature>>);

impl Feature for SimpleRandomSelector {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let index = ctx.random.next_int_bounded(self.0.len() as i32) as usize;
        place_nested(&self.0[index], ctx, origin)
    }
}

struct WeightedRandomSelector(Vec<(Arc<PlacedFeature>, i32)>);

impl Feature for WeightedRandomSelector {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let total: i32 = self.0.iter().map(|(_, w)| w).sum();
        if total == 0 {
            return false;
        }
        let mut selection = ctx.random.next_int_bounded(total);
        for (feature, weight) in &self.0 {
            selection -= weight;
            if selection < 0 {
                return place_nested(feature, ctx, origin);
            }
        }
        false
    }
}

struct RandomBoolean(Arc<PlacedFeature>, Arc<PlacedFeature>);

impl Feature for RandomBoolean {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if ctx.random.next_bool() {
            place_nested(&self.0, ctx, origin)
        } else {
            place_nested(&self.1, ctx, origin)
        }
    }
}

struct Sequence(Vec<Arc<PlacedFeature>>);

impl Feature for Sequence {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        for feature in &self.0 {
            if !place_nested(feature, ctx, origin) {
                return false;
            }
        }
        true
    }
}

struct Overlay(Vec<Arc<PlacedFeature>>);

impl Feature for Overlay {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut placed = false;
        for feature in &self.0 {
            placed |= place_nested(feature, ctx, origin);
        }
        placed
    }
}

struct BlockColumn {
    layers: Vec<(OwnedIntProvider, Arc<StateProvider>)>,
    direction: Facing,
    allowed: BlockPredicate,
    prioritize_tip: bool,
}

impl Feature for BlockColumn {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut heights: Vec<i32> = self
            .layers
            .iter()
            .map(|(h, _)| h.sample(ctx.random))
            .collect();
        let total: i32 = heights.iter().sum();
        if total == 0 {
            return false;
        }
        let mut next = step(origin, self.direction);
        for y in 0..total {
            if !self.allowed.test(ctx.level, next) {
                truncate(&mut heights, total, y, self.prioritize_tip);
                break;
            }
            next = step(next, self.direction);
        }
        let mut pos = origin;
        for (i, (_, provider)) in self.layers.iter().enumerate() {
            for _ in 0..heights[i] {
                let state = provider.state(ctx.level, ctx.random, pos);
                ctx.level.set_block(pos, state);
                pos = step(pos, self.direction);
            }
        }
        true
    }
}

fn truncate(heights: &mut [i32], total: i32, new_height: i32, prioritize_tip: bool) {
    let mut remove = total - new_height;
    let len = heights.len() as i32;
    let (dir, start, end) = if prioritize_tip {
        (1, 0, len)
    } else {
        (-1, len - 1, -1)
    };
    let mut i = start;
    while i != end && remove > 0 {
        let take = heights[i as usize].min(remove);
        remove -= take;
        heights[i as usize] -= take;
        i += dir;
    }
}

struct BlockPile(Arc<StateProvider>);

impl BlockPile {
    fn may_place_on(ctx: &mut PlaceContext, pos: BlockPos) -> bool {
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let state = ctx.level.block(below);
        if state.block() == Block::DirtPath {
            return ctx.random.next_bool();
        }
        state.is_face_sturdy(Facing::Up, Support::Full)
    }

    fn try_place(&self, ctx: &mut PlaceContext, pos: BlockPos) {
        if is_air(ctx.level.block(pos)) && Self::may_place_on(ctx, pos) {
            let state = self.0.state(ctx.level, ctx.random, pos);
            ctx.level.set_block(pos, state);
        }
    }
}

impl Feature for BlockPile {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if origin.y < ctx.level.min_y() + 5 {
            return false;
        }
        let xr = 2 + ctx.random.next_int_bounded(2);
        let zr = 2 + ctx.random.next_int_bounded(2);
        for z in origin.z - zr..=origin.z + zr {
            for y in origin.y..=origin.y + 1 {
                for x in origin.x - xr..=origin.x + xr {
                    let pos = BlockPos::new(x, y, z);
                    let (xd, zd) = (origin.x - x, origin.z - z);
                    let threshold = ctx.random.next_float() * 10.0 - ctx.random.next_float() * 6.0;
                    if ((xd * xd + zd * zd) as f32) <= threshold
                        || (ctx.random.next_float() as f64) < 0.031
                    {
                        self.try_place(ctx, pos);
                    }
                }
            }
        }
        true
    }
}

struct Disk {
    provider: Arc<StateProvider>,
    target: BlockPredicate,
    radius: OwnedIntProvider,
    half_height: i32,
}

/// Marks the one or two blocks above `pos` for a later shape update, as
/// features do after replacing the ground under them.
pub fn mark_above_for_post_processing(ctx: &mut PlaceContext, pos: BlockPos) {
    for i in 1..=2 {
        let above = BlockPos::new(pos.x, pos.y + i, pos.z);
        if is_air(ctx.level.block(above)) {
            return;
        }
        ctx.level.mark_post_processing(above);
    }
}

impl Feature for Disk {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut placed = false;
        let top = origin.y + self.half_height;
        let bottom = origin.y - self.half_height - 1;
        let r = self.radius.sample(ctx.random);
        for z in origin.z - r..=origin.z + r {
            for x in origin.x - r..=origin.x + r {
                let (xd, zd) = (x - origin.x, z - origin.z);
                if xd * xd + zd * zd > r * r {
                    continue;
                }
                let mut placed_above = false;
                let mut y = top;
                while y > bottom {
                    let pos = BlockPos::new(x, y, z);
                    if self.target.test(ctx.level, pos) {
                        if let Some(state) =
                            self.provider.optional_state(ctx.level, ctx.random, pos)
                        {
                            ctx.level.set_block(pos, state);
                            if !placed_above {
                                mark_above_for_post_processing(ctx, pos);
                            }
                            placed = true;
                            placed_above = true;
                        }
                    } else {
                        placed_above = false;
                    }
                    y -= 1;
                }
            }
        }
        placed
    }
}

struct Spring {
    fluid: BlockState,
    requires_block_below: bool,
    rock_count: i32,
    hole_count: i32,
    valid: BlockSet,
}

impl Feature for Spring {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let at =
            |dx: i32, dy: i32, dz: i32| BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
        let valid = |ctx: &PlaceContext, p: BlockPos| self.valid.contains_state(ctx.level.block(p));
        if !valid(ctx, at(0, 1, 0)) {
            return false;
        }
        if self.requires_block_below && !valid(ctx, at(0, -1, 0)) {
            return false;
        }
        let current = ctx.level.block(origin);
        if !is_air(current) && !self.valid.contains_state(current) {
            return false;
        }
        let sides = [
            at(-1, 0, 0),
            at(1, 0, 0),
            at(0, 0, -1),
            at(0, 0, 1),
            at(0, -1, 0),
        ];
        let rocks = sides.iter().filter(|p| valid(ctx, **p)).count() as i32;
        let holes = sides
            .iter()
            .filter(|p| is_air(ctx.level.block(**p)))
            .count() as i32;
        if rocks == self.rock_count && holes == self.hole_count {
            ctx.level.set_block(origin, self.fluid);
            ctx.level.schedule_own_fluid_tick(origin, self.fluid);
            return true;
        }
        false
    }
}

struct NoOp;

impl Feature for NoOp {
    fn place(&self, _: &mut PlaceContext, _: BlockPos) -> bool {
        true
    }
}

struct FreezeTopLayer;

fn should_freeze(ctx: &mut PlaceContext, biome: u16, pos: BlockPos) -> bool {
    let data = ctx.level.biome_data(biome);
    let mut column = temperature::Column::of_data(data, pos.x, pos.z, ctx.level.sea_level());
    should_freeze_in(ctx, &mut column, pos)
}

fn should_freeze_in(
    ctx: &mut PlaceContext,
    column: &mut temperature::Column,
    pos: BlockPos,
) -> bool {
    if column.at(pos.y) >= 0.15 || ctx.level.is_outside_build_height(pos.y) {
        return false;
    }
    ctx.level.block(pos) == Block::Water.default_state()
}

fn should_snow_in(
    ctx: &mut PlaceContext,
    biome: u16,
    column: &mut temperature::Column,
    pos: BlockPos,
) -> bool {
    let data = ctx.level.biome_data(biome);
    if !data.has_precipitation || column.at(pos.y) >= 0.15 {
        return false;
    }
    if ctx.level.is_outside_build_height(pos.y) {
        return false;
    }
    let state = ctx.level.block(pos);
    if !is_air(state) && state.block() != Block::Snow {
        return false;
    }
    let level = &*ctx.level;
    level.can_survive(Block::Snow.default_state(), pos)
}

/// Whether no column of the chunk being decorated can freeze or snow: every
/// biome its positions can read is warm at any height, and the heightmap
/// the columns would read is already there (so reading it is not what
/// computes it).
pub(crate) fn chunk_stays_warm(ctx: &PlaceContext, origin: BlockPos) -> bool {
    let level = &*ctx.level;
    let center = level.center;
    if origin.x >> 4 != center.x || origin.z >> 4 != center.z {
        return false;
    }
    let primed = level
        .chunk(center.x, center.z)
        .is_some_and(|c| c.heightmap(HeightmapKind::MotionBlocking).is_some());
    if !primed {
        return false;
    }
    let (sea_level, top) = (level.sea_level(), level.max_y() + 1);
    let mut checked: Vec<u16> = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let Some(chunk) = level.chunk(center.x + dx, center.z + dz) else {
                return false;
            };
            let mut last = None;
            for &biome in chunk.biomes() {
                if last == Some(biome) {
                    continue;
                }
                last = Some(biome);
                if !checked.contains(&biome) {
                    if !temperature::data_never_freezes(level.biome_data(biome), top, sea_level) {
                        return false;
                    }
                    checked.push(biome);
                }
            }
        }
    }
    true
}

impl Feature for FreezeTopLayer {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if chunk_stays_warm(ctx, origin) {
            return true;
        }
        freeze_columns(ctx, origin);
        true
    }
}

/// Freezes and snows the columns of the chunk at `origin`, returning how
/// many blocks it changed.
pub(crate) fn freeze_columns(ctx: &mut PlaceContext, origin: BlockPos) -> usize {
    let mut changed = 0;
    let sea_level = ctx.level.sea_level();
    for dx in 0..16 {
        for dz in 0..16 {
            let (x, z) = (origin.x + dx, origin.z + dz);
            let y = ctx.level.height(HeightmapKind::MotionBlocking, x, z);
            let top = BlockPos::new(x, y, z);
            let below = BlockPos::new(x, y - 1, z);
            let biome = ctx.level.biome(top);
            let data = ctx.level.biome_data(biome);
            let mut column = temperature::Column::of_data(data, x, z, sea_level);
            if should_freeze_in(ctx, &mut column, below) {
                ctx.level.set_block(below, Block::Ice.default_state());
                changed += 1;
            }
            if should_snow_in(ctx, biome, &mut column, top) {
                ctx.level.set_block(top, Block::Snow.default_state());
                changed += 1;
                let below_state = ctx.level.block(below);
                if below_state.get("snowy").is_some() {
                    let snowy = below_state.with("snowy", "true").unwrap_or(below_state);
                    ctx.level.set_block(below, snowy);
                }
            }
        }
    }
    changed
}

struct Lake {
    fluid: Arc<StateProvider>,
    barrier: Arc<StateProvider>,
    can_place: BlockPredicate,
    replace_with_air_or_fluid: BlockPredicate,
    replace_with_barrier: BlockPredicate,
}

fn is_liquid(state: BlockState) -> bool {
    matches!(state.block(), Block::Water | Block::Lava)
}

impl Feature for Lake {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if origin.y <= ctx.level.min_y() + 4 {
            return false;
        }
        let base = BlockPos::new(origin.x - 8, origin.y - 4, origin.z - 8);
        let mut grid = vec![false; 2048];
        let index = |x: i32, z: i32, y: i32| ((x * 16 + z) * 8 + y) as usize;
        let spots = ctx.random.next_int_bounded(4) + 4;
        for _ in 0..spots {
            let xr = ctx.random.next_double() * 6.0 + 3.0;
            let yr = ctx.random.next_double() * 4.0 + 2.0;
            let zr = ctx.random.next_double() * 6.0 + 3.0;
            let xp = ctx.random.next_double() * (16.0 - xr - 2.0) + 1.0 + xr / 2.0;
            let yp = ctx.random.next_double() * (8.0 - yr - 4.0) + 2.0 + yr / 2.0;
            let zp = ctx.random.next_double() * (16.0 - zr - 2.0) + 1.0 + zr / 2.0;
            for xx in 1..15 {
                for zz in 1..15 {
                    for yy in 1..7 {
                        let xd = (xx as f64 - xp) / (xr / 2.0);
                        let yd = (yy as f64 - yp) / (yr / 2.0);
                        let zd = (zz as f64 - zp) / (zr / 2.0);
                        if xd * xd + yd * yd + zd * zd < 1.0 {
                            grid[index(xx, zz, yy)] = true;
                        }
                    }
                }
            }
        }
        let edge = |grid: &[bool], x: i32, z: i32, y: i32| {
            !grid[index(x, z, y)]
                && ((x < 15 && grid[index(x + 1, z, y)])
                    || (x > 0 && grid[index(x - 1, z, y)])
                    || (z < 15 && grid[index(x, z + 1, y)])
                    || (z > 0 && grid[index(x, z - 1, y)])
                    || (y < 7 && grid[index(x, z, y + 1)])
                    || (y > 0 && grid[index(x, z, y - 1)]))
        };
        let fluid_state = self.fluid.state(ctx.level, ctx.random, base);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..8 {
                    if edge(&grid, x, z, y) {
                        let pos = BlockPos::new(base.x + x, base.y + y, base.z + z);
                        let state = ctx.level.block(pos);
                        if y >= 4 && is_liquid(state) {
                            return false;
                        }
                        if (y < 4 && !state.is_solid() && state != fluid_state)
                            || !self.can_place.test(ctx.level, pos)
                        {
                            return false;
                        }
                    }
                }
            }
        }
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..8 {
                    if grid[index(x, z, y)] {
                        let pos = BlockPos::new(base.x + x, base.y + y, base.z + z);
                        if self.replace_with_air_or_fluid.test(ctx.level, pos) {
                            let air = y >= 4;
                            ctx.level.set_block(
                                pos,
                                if air {
                                    Block::CaveAir.default_state()
                                } else {
                                    fluid_state
                                },
                            );
                            if air {
                                ctx.level.schedule_block_tick(pos, Block::CaveAir, 0);
                                mark_above_for_post_processing(ctx, pos);
                            }
                        }
                    }
                }
            }
        }
        let barrier = self.barrier.state(ctx.level, ctx.random, base);
        if !is_air(barrier) {
            for x in 0..16 {
                for z in 0..16 {
                    for y in 0..8 {
                        if edge(&grid, x, z, y) && (y < 4 || ctx.random.next_int_bounded(2) != 0) {
                            let pos = BlockPos::new(base.x + x, base.y + y, base.z + z);
                            if ctx.level.block(pos).is_solid()
                                && self.replace_with_barrier.test(ctx.level, pos)
                            {
                                ctx.level.set_block(pos, barrier);
                                mark_above_for_post_processing(ctx, pos);
                            }
                        }
                    }
                }
            }
        }
        if fluid(fluid_state) == Fluid::Water {
            for x in 0..16 {
                for z in 0..16 {
                    let pos = BlockPos::new(base.x + x, base.y + 4, base.z + z);
                    let biome = ctx.level.biome(pos);
                    if should_freeze(ctx, biome, pos)
                        && self.replace_with_air_or_fluid.test(ctx.level, pos)
                    {
                        ctx.level.set_block(pos, Block::Ice.default_state());
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::level::Region;
    use crate::proto::ProtoChunk;
    use crate::surface::GenContext;
    use serde_json::json;
    use tuff_item::random::{Random, WorldgenRandom, XoroshiroRandom};
    use tuff_world_model::chunk::ChunkShape;
    use tuff_world_model::pos::ChunkPos;

    #[tuff_core::watchdog]
    #[test]
    fn simple_block_moss_carpet_hugs_neighbouring_walls() {
        let shape = ChunkShape::OVERWORLD;
        let chunks: Vec<ProtoChunk> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(dx, dz)))
            .map(|pos| ProtoChunk::new(pos, shape))
            .collect();
        let span = GenContext {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: 63,
        };
        let mut region = Region::live(ChunkPos::new(0, 0), chunks, span, 1, 1);
        let stone = Block::Stone.default_state();
        for x in 0..16 {
            for z in 0..16 {
                region.set_block(BlockPos::new(x, 64, z), stone);
            }
        }
        region.set_block(BlockPos::new(6, 65, 5), stone);
        let feature = Registry::builtin()
            .feature(&json!({
                "type": "minecraft:simple_block",
                "to_place": {
                    "type": "minecraft:simple",
                    "state": {"Name": "minecraft:pale_moss_carpet"}
                }
            }))
            .unwrap();
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(7)));
        let mut ctx = PlaceContext {
            level: &mut region,
            random: &mut random,
        };
        assert!(feature.place(&mut ctx, BlockPos::new(5, 65, 5)));
        let placed = region.block(BlockPos::new(5, 65, 5));
        assert_eq!(placed.block(), Block::PaleMossCarpet);
        assert_eq!(placed.get("east"), Some("low"));
        assert_eq!(placed.get("west"), Some("none"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn replace_single_block_applies_the_first_matching_target_only() {
        let shape = ChunkShape::OVERWORLD;
        let chunks: Vec<ProtoChunk> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(dx, dz)))
            .map(|pos| ProtoChunk::new(pos, shape))
            .collect();
        let span = GenContext {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: 63,
        };
        let mut region = Region::live(ChunkPos::new(0, 0), chunks, span, 1, 1);
        region.set_block(BlockPos::new(5, 65, 5), Block::Stone.default_state());
        let feature = Registry::builtin()
            .feature(&json!({
                "type": "minecraft:replace_single_block",
                "targets": [
                    {
                        "target": {"predicate_type": "minecraft:block_match", "block": "minecraft:dirt"},
                        "state": {"Name": "minecraft:glass"}
                    },
                    {
                        "target": {"predicate_type": "minecraft:block_match", "block": "minecraft:stone"},
                        "state": {"Name": "minecraft:gold_block"}
                    },
                    {
                        "target": {"predicate_type": "minecraft:always_true"},
                        "state": {"Name": "minecraft:diamond_block"}
                    }
                ]
            }))
            .unwrap();
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(7)));
        let mut ctx = PlaceContext {
            level: &mut region,
            random: &mut random,
        };
        assert!(feature.place(&mut ctx, BlockPos::new(5, 65, 5)));
        assert_eq!(
            region.block(BlockPos::new(5, 65, 5)).block(),
            Block::GoldBlock
        );
    }
}

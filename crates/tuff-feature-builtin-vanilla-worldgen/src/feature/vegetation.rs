//! Vegetation patches (moss, clay pools) and root systems (rooted azalea
//! trees).

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::predicate::{BlockPredicate, Fluid, fluid, step};
use crate::feature::provider::StateProvider;
use crate::feature::{Feature, OwnedIntProvider, PlaceContext, PlacedFeature, Registry};
use crate::java_set::PosSet;
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::tags::BlockSet;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:vegetation_patch", |r, v| {
        Ok(Arc::new(VegetationPatch::parse(r, v, false)?))
    });
    registry.register("minecraft:waterlogged_vegetation_patch", |r, v| {
        Ok(Arc::new(VegetationPatch::parse(r, v, true)?))
    });
    registry.register("minecraft:root_system", |r, v| {
        Ok(Arc::new(RootSystem::parse(r, v)?))
    });
}

struct VegetationPatch {
    waterlogged: bool,
    replaceable: BlockSet,
    ground: Arc<StateProvider>,
    vegetation: Arc<PlacedFeature>,
    inwards: Facing,
    depth: OwnedIntProvider,
    extra_bottom_block_chance: f32,
    vertical_range: i32,
    vegetation_chance: f32,
    xz_radius: OwnedIntProvider,
    extra_edge_column_chance: f32,
}

impl VegetationPatch {
    fn parse(registry: &mut Registry, value: &Value, waterlogged: bool) -> Result<Self> {
        let o = obj(
            value,
            &[
                "replaceable",
                "ground_state",
                "vegetation_feature",
                "surface",
                "depth",
                "extra_bottom_block_chance",
                "vertical_range",
                "vegetation_chance",
                "xz_radius",
                "extra_edge_column_chance",
            ],
            "vegetation_patch",
        )?;
        let inwards = match o.str("surface")? {
            "floor" => Facing::Down,
            "ceiling" => Facing::Up,
            other => return Err(format!("unknown cave surface {other}")),
        };
        Ok(Self {
            waterlogged,
            replaceable: json::block_set(o.req("replaceable")?)?,
            ground: StateProvider::parse(registry, o.req("ground_state")?)?,
            vegetation: registry.placed(o.req("vegetation_feature")?)?,
            inwards,
            depth: OwnedIntProvider::parse(o.req("depth")?)?,
            extra_bottom_block_chance: o.f32("extra_bottom_block_chance")?,
            vertical_range: o.i32("vertical_range")?,
            vegetation_chance: o.f32("vegetation_chance")?,
            xz_radius: OwnedIntProvider::parse(o.req("xz_radius")?)?,
            extra_edge_column_chance: o.f32("extra_edge_column_chance")?,
        })
    }

    fn place_ground_patch(
        &self,
        ctx: &mut PlaceContext,
        origin: BlockPos,
        x_radius: i32,
        z_radius: i32,
    ) -> PosSet {
        let outwards = self.inwards.opposite();
        let mut surface = PosSet::new();
        for dx in -x_radius..=x_radius {
            let x_edge = dx == -x_radius || dx == x_radius;
            for dz in -z_radius..=z_radius {
                let z_edge = dz == -z_radius || dz == z_radius;
                let corner = x_edge && z_edge;
                let edge_not_corner = (x_edge || z_edge) && !corner;
                if corner
                    || (edge_not_corner
                        && (self.extra_edge_column_chance == 0.0
                            || ctx.random.next_float() > self.extra_edge_column_chance))
                {
                    continue;
                }
                let mut pos = BlockPos::new(origin.x + dx, origin.y, origin.z + dz);
                let mut offset = 0;
                while is_air(ctx.level.block(pos)) && offset < self.vertical_range {
                    pos = step(pos, self.inwards);
                    offset += 1;
                }
                let mut offset = 0;
                while !is_air(ctx.level.block(pos)) && offset < self.vertical_range {
                    pos = step(pos, outwards);
                    offset += 1;
                }
                let below = step(pos, self.inwards);
                if is_air(ctx.level.block(pos))
                    && ctx
                        .level
                        .block(below)
                        .is_face_sturdy(outwards, Support::Full)
                {
                    let depth = self.depth.sample(ctx.random);
                    let extra = self.extra_bottom_block_chance > 0.0
                        && ctx.random.next_float() < self.extra_bottom_block_chance;
                    let depth = depth + i32::from(extra);
                    if self.place_ground(ctx, below, depth) {
                        surface.insert(below);
                    }
                }
            }
        }
        if !self.waterlogged {
            return surface;
        }
        let mut water = PosSet::new();
        for pos in surface.iter() {
            if !is_exposed(ctx, pos) {
                water.insert(pos);
            }
        }
        for pos in water.iter() {
            ctx.level.set_block(pos, Block::Water.default_state());
        }
        water
    }

    fn place_ground(&self, ctx: &mut PlaceContext, mut pos: BlockPos, depth: i32) -> bool {
        for i in 0..depth {
            let state = self.ground.state(ctx.level, ctx.random, pos);
            let existing = ctx.level.block(pos);
            if state.block() != existing.block() {
                if !self.replaceable.contains_state(existing) {
                    return i != 0;
                }
                ctx.level.set_block(pos, state);
                pos = step(pos, self.inwards);
            }
        }
        true
    }

    fn place_vegetation(&self, ctx: &mut PlaceContext, pos: BlockPos) {
        let outwards = self.inwards.opposite();
        if !self.waterlogged {
            self.vegetation.place(ctx, step(pos, outwards), None);
            return;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if self.vegetation.place(ctx, step(below, outwards), None) {
            let placed = ctx.level.block(pos);
            if placed.get("waterlogged") == Some("false") {
                let wet = placed.with("waterlogged", "true").unwrap_or(placed);
                ctx.level.set_block(pos, wet);
            }
        }
    }
}

fn is_exposed(ctx: &PlaceContext, pos: BlockPos) -> bool {
    [
        Facing::North,
        Facing::East,
        Facing::South,
        Facing::West,
        Facing::Down,
    ]
    .into_iter()
    .any(|d| {
        !ctx.level
            .block(step(pos, d))
            .is_face_sturdy(d.opposite(), Support::Full)
    })
}

impl Feature for VegetationPatch {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let x_radius = self.xz_radius.sample(ctx.random) + 1;
        let z_radius = self.xz_radius.sample(ctx.random) + 1;
        let surface = self.place_ground_patch(ctx, origin, x_radius, z_radius);
        for pos in surface.iter() {
            if self.vegetation_chance > 0.0 && ctx.random.next_float() < self.vegetation_chance {
                self.place_vegetation(ctx, pos);
            }
        }
        !surface.is_empty()
    }
}

struct RootSystem {
    feature: Arc<PlacedFeature>,
    required_vertical_space: i32,
    level_test_distance: i32,
    max_level_deviation: i32,
    root_radius: i32,
    root_replaceable: BlockSet,
    root_state: Arc<StateProvider>,
    root_placement_attempts: i32,
    root_column_max_height: i32,
    hanging_root_radius: i32,
    hanging_roots_vertical_span: i32,
    hanging_root_state: Arc<StateProvider>,
    hanging_root_placement_attempts: i32,
    allowed_vertical_water: i32,
    allowed_tree_position: BlockPredicate,
}

impl RootSystem {
    fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "feature",
                "required_vertical_space_for_tree",
                "level_test_distance",
                "max_level_deviation",
                "root_radius",
                "root_replaceable",
                "root_state_provider",
                "root_placement_attempts",
                "root_column_max_height",
                "hanging_root_radius",
                "hanging_roots_vertical_span",
                "hanging_root_state_provider",
                "hanging_root_placement_attempts",
                "allowed_vertical_water_for_tree",
                "allowed_tree_position",
            ],
            "root_system",
        )?;
        Ok(Self {
            feature: registry.placed(o.req("feature")?)?,
            required_vertical_space: o.i32("required_vertical_space_for_tree")?,
            level_test_distance: o.i32("level_test_distance")?,
            max_level_deviation: o.i32("max_level_deviation")?,
            root_radius: o.i32("root_radius")?,
            root_replaceable: json::block_set(o.req("root_replaceable")?)?,
            root_state: StateProvider::parse(registry, o.req("root_state_provider")?)?,
            root_placement_attempts: o.i32("root_placement_attempts")?,
            root_column_max_height: o.i32("root_column_max_height")?,
            hanging_root_radius: o.i32("hanging_root_radius")?,
            hanging_roots_vertical_span: o.i32("hanging_roots_vertical_span")?,
            hanging_root_state: StateProvider::parse(
                registry,
                o.req("hanging_root_state_provider")?,
            )?,
            hanging_root_placement_attempts: o.i32("hanging_root_placement_attempts")?,
            allowed_vertical_water: o.i32("allowed_vertical_water_for_tree")?,
            allowed_tree_position: BlockPredicate::parse(o.req("allowed_tree_position")?)?,
        })
    }

    fn space_for_tree(&self, ctx: &PlaceContext, pos: BlockPos) -> bool {
        for i in 1..=self.required_vertical_space {
            let state = ctx.level.block(BlockPos::new(pos.x, pos.y + i, pos.z));
            if !is_allowed_tree_space(state, i, self.allowed_vertical_water) {
                return false;
            }
        }
        if self.level_test_distance > 0 {
            for direction in [Facing::South, Facing::West, Facing::North, Facing::East] {
                let (x, _, z) = direction.step();
                let corner = BlockPos::new(
                    pos.x + x * self.level_test_distance,
                    pos.y,
                    pos.z + z * self.level_test_distance,
                );
                let below = ctx.level.block(BlockPos::new(
                    corner.x,
                    corner.y - self.max_level_deviation,
                    corner.z,
                ));
                let above = ctx.level.block(BlockPos::new(
                    corner.x,
                    corner.y + self.max_level_deviation,
                    corner.z,
                ));
                if is_air(below) || !is_air(above) {
                    return false;
                }
            }
        }
        true
    }

    fn place_dirt_and_tree(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut pos = origin;
        for y in 0..self.root_column_max_height {
            pos.y += 1;
            if ctx.level.height(HeightmapKind::WorldSurface, pos.x, pos.z) < pos.y {
                return false;
            }
            if self.allowed_tree_position.test(ctx.level, pos) && self.space_for_tree(ctx, pos) {
                let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
                let below_state = ctx.level.block(below);
                if matches!(fluid(below_state), Fluid::Lava | Fluid::FlowingLava)
                    || !below_state.is_solid()
                {
                    return false;
                }
                if self.feature.place(ctx, pos, None) {
                    for dirt_y in origin.y..origin.y + y {
                        self.place_rooted_dirt(ctx, BlockPos::new(origin.x, dirt_y, origin.z));
                    }
                    return true;
                }
            }
        }
        false
    }

    fn place_rooted_dirt(&self, ctx: &mut PlaceContext, center: BlockPos) {
        for _ in 0..self.root_placement_attempts {
            let dx = spread(ctx.random, self.root_radius);
            let dz = spread(ctx.random, self.root_radius);
            let pos = BlockPos::new(center.x + dx, center.y, center.z + dz);
            if self.root_replaceable.contains_state(ctx.level.block(pos)) {
                let state = self.root_state.state(ctx.level, ctx.random, pos);
                ctx.level.set_block(pos, state);
            }
        }
    }

    fn place_roots(&self, ctx: &mut PlaceContext, origin: BlockPos) {
        for _ in 0..self.hanging_root_placement_attempts {
            let dx = spread(ctx.random, self.hanging_root_radius);
            let dy = spread(ctx.random, self.hanging_roots_vertical_span);
            let dz = spread(ctx.random, self.hanging_root_radius);
            let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
            if !is_air(ctx.level.block(pos)) {
                continue;
            }
            let state = self.hanging_root_state.state(ctx.level, ctx.random, pos);
            let level = &*ctx.level;
            let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
            if level.can_survive(state, pos)
                && level
                    .block(above)
                    .is_face_sturdy(Facing::Down, Support::Full)
            {
                ctx.level.set_block(pos, state);
            }
        }
    }
}

fn spread(random: &mut impl RandomSource, radius: i32) -> i32 {
    let a = random.next_int_bounded(radius);
    a - random.next_int_bounded(radius)
}

fn is_allowed_tree_space(state: BlockState, above_origin: i32, allowed_water: i32) -> bool {
    if is_air(state) {
        return true;
    }
    above_origin < allowed_water && matches!(fluid(state), Fluid::Water | Fluid::FlowingWater)
}

impl Feature for RootSystem {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !is_air(ctx.level.block(origin)) {
            return false;
        }
        if self.place_dirt_and_tree(ctx, origin) {
            self.place_roots(ctx, origin);
        }
        true
    }
}

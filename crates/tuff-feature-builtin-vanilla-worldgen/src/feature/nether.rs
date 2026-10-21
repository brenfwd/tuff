//! Nether features: lava deltas, replacement blobs, basalt columns and
//! pillars, neighbour-spread blocks (glowstone, wart blocks) and huge
//! fungi.

use std::sync::Arc;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{BlockPredicate, DIRECTIONS, parse_direction, step};
use crate::feature::provider::StateProvider;
use crate::feature::value::OwnedIntProvider;
use crate::feature::{Feature, PlaceContext, PlacedFeature, Registry};
use crate::json::{self, obj};
use crate::tags::BlockSet;
use tuff_core::mth::next_int;
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:delta_feature", |_, v| {
        let o = obj(v, &["contents", "rim", "size", "rim_size"], "delta_feature")?;
        Ok(Arc::new(Delta {
            contents: json::block_state(o.req("contents")?)?,
            rim: json::block_state(o.req("rim")?)?,
            size: OwnedIntProvider::parse(o.req("size")?)?,
            rim_size: OwnedIntProvider::parse(o.req("rim_size")?)?,
        }))
    });
    registry.register("minecraft:netherrack_replace_blobs", |_, v| {
        let o = obj(
            v,
            &["target", "state", "radius"],
            "netherrack_replace_blobs",
        )?;
        Ok(Arc::new(ReplaceBlobs {
            target: json::block_state(o.req("target")?)?.block(),
            state: json::block_state(o.req("state")?)?,
            radius: OwnedIntProvider::parse(o.req("radius")?)?,
        }))
    });
    registry.register("minecraft:random_neighbor_spread", |r, v| {
        let o = obj(
            v,
            &[
                "block",
                "accepted_neighbors",
                "can_replace",
                "attempts",
                "xz_offset",
                "y_offset",
            ],
            "random_neighbor_spread",
        )?;
        Ok(Arc::new(NeighborSpread {
            block: StateProvider::parse(r, o.req("block")?)?,
            accepted: json::block_set(o.req("accepted_neighbors")?)?,
            can_replace: BlockPredicate::parse(o.req("can_replace")?)?,
            attempts: OwnedIntProvider::parse(o.req("attempts")?)?,
            xz_offset: OwnedIntProvider::parse(o.req("xz_offset")?)?,
            y_offset: OwnedIntProvider::parse(o.req("y_offset")?)?,
        }))
    });
    registry.register("minecraft:single_block_pillar", |r, v| {
        let o = obj(
            v,
            &[
                "block",
                "can_replace",
                "direction",
                "chance_to_continue",
                "cap_feature",
            ],
            "single_block_pillar",
        )?;
        let direction = parse_direction(o.str("direction")?)?;
        if !matches!(direction, Facing::Up | Facing::Down) {
            return Err(format!("pillar direction {direction:?} is not vertical"));
        }
        Ok(Arc::new(Pillar {
            block: StateProvider::parse(r, o.req("block")?)?,
            can_replace: match o.get("can_replace") {
                Some(p) => BlockPredicate::parse(p)?,
                None => BlockPredicate::True,
            },
            direction,
            chance: o.f32_or("chance_to_continue", 1.0)?,
            cap: match o.get("cap_feature") {
                Some(f) => Some(r.placed(f)?),
                None => None,
            },
        }))
    });
    registry.register("minecraft:projected_random_patchy_square", |r, v| {
        let o = obj(
            v,
            &["block", "project_through", "size", "max_projection_height"],
            "projected_random_patchy_square",
        )?;
        Ok(Arc::new(PatchySquare {
            block: StateProvider::parse(r, o.req("block")?)?,
            project_through: BlockPredicate::parse(o.req("project_through")?)?,
            size: OwnedIntProvider::parse(o.req("size")?)?,
            max_projection: o.i32("max_projection_height")?,
        }))
    });
    registry.register("minecraft:stepped_column_cluster", |r, v| {
        let o = obj(
            v,
            &[
                "block",
                "continue_through",
                "can_replace",
                "cannot_place_on",
                "cluster_reach",
                "column_count",
                "column_reach",
                "height",
            ],
            "stepped_column_cluster",
        )?;
        Ok(Arc::new(ColumnCluster {
            block: StateProvider::parse(r, o.req("block")?)?,
            continue_through: BlockPredicate::parse(o.req("continue_through")?)?,
            can_replace: BlockPredicate::parse(o.req("can_replace")?)?,
            cannot_place_on: json::block_set(o.req("cannot_place_on")?)?,
            cluster_reach: OwnedIntProvider::parse(o.req("cluster_reach")?)?,
            column_count: OwnedIntProvider::parse(o.req("column_count")?)?,
            column_reach: OwnedIntProvider::parse(o.req("column_reach")?)?,
            height: OwnedIntProvider::parse(o.req("height")?)?,
        }))
    });
    registry.register("minecraft:huge_fungus", |_, v| {
        let o = obj(
            v,
            &[
                "valid_base_block",
                "stem_state",
                "hat_state",
                "decor_state",
                "replaceable_blocks",
                "planted",
            ],
            "huge_fungus",
        )?;
        Ok(Arc::new(HugeFungus {
            valid_base: json::block_state(o.req("valid_base_block")?)?.block(),
            stem: json::block_state(o.req("stem_state")?)?,
            hat: json::block_state(o.req("hat_state")?)?,
            decor: json::block_state(o.req("decor_state")?)?,
            replaceable: BlockPredicate::parse(o.req("replaceable_blocks")?)?,
            planted: o.bool_or("planted", false)?,
        }))
    });
}

fn manhattan(a: BlockPos, b: BlockPos) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

/// The positions of a box around `origin` in order of Manhattan distance,
/// visited as the game visits them (each z mirrored right after itself).
struct ManhattanOrdered {
    origin: BlockPos,
    reach: [i32; 3],
    max_depth: i32,
    depth: i32,
    max_x: i32,
    max_y: i32,
    x: i32,
    y: i32,
    mirror: Option<BlockPos>,
}

impl ManhattanOrdered {
    fn new(origin: BlockPos, rx: i32, ry: i32, rz: i32) -> Self {
        Self {
            origin,
            reach: [rx, ry, rz],
            max_depth: rx + ry + rz,
            depth: 0,
            max_x: 0,
            max_y: 0,
            x: 0,
            y: 0,
            mirror: None,
        }
    }
}

impl Iterator for ManhattanOrdered {
    type Item = BlockPos;

    fn next(&mut self) -> Option<BlockPos> {
        if let Some(pos) = self.mirror.take() {
            return Some(pos);
        }
        loop {
            if self.y > self.max_y {
                self.x += 1;
                if self.x > self.max_x {
                    self.depth += 1;
                    if self.depth > self.max_depth {
                        return None;
                    }
                    self.max_x = self.reach[0].min(self.depth);
                    self.x = -self.max_x;
                }
                self.max_y = self.reach[1].min(self.depth - self.x.abs());
                self.y = -self.max_y;
            }
            let (x, y) = (self.x, self.y);
            let z = self.depth - x.abs() - y.abs();
            self.y += 1;
            if z <= self.reach[2] {
                let o = self.origin;
                if z != 0 {
                    self.mirror = Some(BlockPos::new(o.x + x, o.y + y, o.z - z));
                }
                return Some(BlockPos::new(o.x + x, o.y + y, o.z + z));
            }
        }
    }
}

struct Delta {
    contents: BlockState,
    rim: BlockState,
    size: OwnedIntProvider,
    rim_size: OwnedIntProvider,
}

const DELTA_CANNOT_REPLACE: [Block; 7] = [
    Block::Bedrock,
    Block::NetherBricks,
    Block::NetherBrickFence,
    Block::NetherBrickStairs,
    Block::NetherWart,
    Block::Chest,
    Block::Spawner,
];

impl Delta {
    fn is_clear(&self, level: &Region, pos: BlockPos) -> bool {
        let state = level.block(pos);
        if state.block() == self.contents.block() || DELTA_CANNOT_REPLACE.contains(&state.block()) {
            return false;
        }
        DIRECTIONS.iter().all(|d| {
            let air = is_air(level.block(step(pos, *d)));
            if *d == Facing::Up { air } else { !air }
        })
    }
}

impl Feature for Delta {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut placed = false;
        let spawn_rim = ctx.random.next_double() < 0.9;
        let rim_x = if spawn_rim {
            self.rim_size.sample(ctx.random)
        } else {
            0
        };
        let rim_z = if spawn_rim {
            self.rim_size.sample(ctx.random)
        } else {
            0
        };
        let has_rim = spawn_rim && rim_x != 0 && rim_z != 0;
        let radius_x = self.size.sample(ctx.random);
        let radius_z = self.size.sample(ctx.random);
        let limit = radius_x.max(radius_z);
        for pos in ManhattanOrdered::new(origin, radius_x, 0, radius_z) {
            if manhattan(pos, origin) > limit {
                break;
            }
            if !self.is_clear(ctx.level, pos) {
                continue;
            }
            if has_rim {
                placed = true;
                ctx.level.set_block(pos, self.rim);
            }
            let offset = BlockPos::new(pos.x + rim_x, pos.y, pos.z + rim_z);
            if self.is_clear(ctx.level, offset) {
                placed = true;
                ctx.level.set_block(offset, self.contents);
            }
        }
        placed
    }
}

struct ReplaceBlobs {
    target: Block,
    state: BlockState,
    radius: OwnedIntProvider,
}

impl Feature for ReplaceBlobs {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut cursor = origin;
        cursor.y = cursor.y.clamp(ctx.level.min_y() + 1, ctx.level.max_y());
        let center = loop {
            if cursor.y <= ctx.level.min_y() + 1 {
                return false;
            }
            if ctx.level.block(cursor).block() == self.target {
                break cursor;
            }
            cursor.y -= 1;
        };
        let rx = self.radius.sample(ctx.random);
        let ry = self.radius.sample(ctx.random);
        let rz = self.radius.sample(ctx.random);
        let limit = rx.max(ry.max(rz));
        let mut replaced = false;
        for pos in ManhattanOrdered::new(center, rx, ry, rz) {
            if manhattan(pos, center) > limit {
                break;
            }
            if ctx.level.block(pos).block() == self.target {
                ctx.level.set_block(pos, self.state);
                replaced = true;
            }
        }
        replaced
    }
}

struct NeighborSpread {
    block: Arc<StateProvider>,
    accepted: BlockSet,
    can_replace: BlockPredicate,
    attempts: OwnedIntProvider,
    xz_offset: OwnedIntProvider,
    y_offset: OwnedIntProvider,
}

impl Feature for NeighborSpread {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let state = self.block.state(ctx.level, ctx.random, origin);
        ctx.level.set_block(origin, state);
        let attempts = self.attempts.sample(ctx.random);
        for _ in 0..attempts {
            let dx = self.xz_offset.sample(ctx.random);
            let dy = self.y_offset.sample(ctx.random);
            let dz = self.xz_offset.sample(ctx.random);
            let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
            if !self.can_replace.test(ctx.level, pos) {
                continue;
            }
            let mut neighbours = 0;
            for direction in DIRECTIONS {
                if self
                    .accepted
                    .contains_state(ctx.level.block(step(pos, direction)))
                {
                    neighbours += 1;
                }
                if neighbours > 1 {
                    break;
                }
            }
            if neighbours == 1 {
                let state = self.block.state(ctx.level, ctx.random, pos);
                ctx.level.set_block(pos, state);
            }
        }
        true
    }
}

struct Pillar {
    block: Arc<StateProvider>,
    can_replace: BlockPredicate,
    direction: Facing,
    chance: f32,
    cap: Option<Arc<PlacedFeature>>,
}

impl Feature for Pillar {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut pos = origin;
        while self.can_replace.test(ctx.level, pos)
            && ctx.random.next_float() < self.chance
            && !ctx.level.is_outside_build_height(pos.y)
        {
            let state = self.block.state(ctx.level, ctx.random, pos);
            ctx.level.set_block(pos, state);
            pos = step(pos, self.direction);
        }
        pos = step(pos, self.direction.opposite());
        if let Some(cap) = &self.cap {
            cap.place(ctx, pos, None);
        }
        true
    }
}

struct PatchySquare {
    block: Arc<StateProvider>,
    project_through: BlockPredicate,
    size: OwnedIntProvider,
    max_projection: i32,
}

impl Feature for PatchySquare {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let size = self.size.sample(ctx.random);
        let bound = size * size + 1;
        for dx in -size..=size {
            for dz in -size..=size {
                let probability = dx.abs() * dz.abs();
                if ctx.random.next_int_bounded(bound) >= bound - probability {
                    continue;
                }
                let mut base = BlockPos::new(origin.x + dx, origin.y, origin.z + dz);
                let mut drop = self.max_projection;
                while self
                    .project_through
                    .test(ctx.level, step(base, Facing::Down))
                {
                    base.y -= 1;
                    drop -= 1;
                    if drop <= 0 {
                        break;
                    }
                }
                if let Some(state) = self.block.optional_state(ctx.level, ctx.random, base) {
                    ctx.level.set_block(base, state);
                }
            }
        }
        true
    }
}

struct ColumnCluster {
    block: Arc<StateProvider>,
    continue_through: BlockPredicate,
    can_replace: BlockPredicate,
    cannot_place_on: BlockSet,
    cluster_reach: OwnedIntProvider,
    column_count: OwnedIntProvider,
    column_reach: OwnedIntProvider,
    height: OwnedIntProvider,
}

impl ColumnCluster {
    fn can_place_at(&self, level: &mut Region, pos: BlockPos) -> bool {
        if !self.can_replace.test(level, pos) {
            return false;
        }
        let below = level.block(step(pos, Facing::Down));
        !is_air(below) && !self.cannot_place_on.contains_state(below)
    }

    fn find_surface(
        &self,
        level: &mut Region,
        mut cursor: BlockPos,
        mut limit: i32,
    ) -> Option<BlockPos> {
        while cursor.y > level.min_y() + 1 && limit > 0 {
            limit -= 1;
            if self.can_place_at(level, cursor) {
                return Some(cursor);
            }
            cursor.y -= 1;
        }
        None
    }

    fn find_air(&self, level: &Region, mut cursor: BlockPos, mut limit: i32) -> Option<BlockPos> {
        while cursor.y <= level.max_y() && limit > 0 {
            limit -= 1;
            let state = level.block(cursor);
            if self.cannot_place_on.contains_state(state) {
                return None;
            }
            if is_air(state) {
                return Some(cursor);
            }
            cursor.y += 1;
        }
        None
    }

    fn place_column(
        &self,
        level: &mut Region,
        random: &mut WorldgenRandom,
        origin: BlockPos,
        height: i32,
        reach: i32,
    ) -> bool {
        let mut placed = false;
        for z in origin.z - reach..=origin.z + reach {
            for x in origin.x - reach..=origin.x + reach {
                let pos = BlockPos::new(x, origin.y, z);
                let limit = manhattan(pos, origin);
                let column = if self.can_replace.test(level, pos) {
                    self.find_surface(level, pos, limit)
                } else {
                    self.find_air(level, pos, limit)
                };
                let Some(mut cursor) = column else {
                    continue;
                };
                let mut blocks = height - limit / 2;
                while blocks >= 0 {
                    if self.can_replace.test(level, cursor) {
                        let state = self.block.state(level, random, cursor);
                        level.set_block(cursor, state);
                        cursor.y += 1;
                        placed = true;
                    } else {
                        if !self.continue_through.test(level, cursor) {
                            break;
                        }
                        cursor.y += 1;
                    }
                    blocks -= 1;
                }
            }
        }
        placed
    }
}

impl Feature for ColumnCluster {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if !self.can_place_at(ctx.level, origin) {
            return false;
        }
        let height = self.height.sample(ctx.random);
        let reach = height.min(self.cluster_reach.sample(ctx.random));
        let count = self.column_count.sample(ctx.random);
        let width = 2 * reach + 1;
        let mut placed = false;
        for _ in 0..count {
            let x = origin.x - reach + ctx.random.next_int_bounded(width);
            let y = origin.y + ctx.random.next_int_bounded(1);
            let z = origin.z - reach + ctx.random.next_int_bounded(width);
            let pos = BlockPos::new(x, y, z);
            let blocks = height - manhattan(pos, origin);
            if blocks >= 0 {
                let column_reach = self.column_reach.sample(ctx.random);
                placed |= self.place_column(ctx.level, ctx.random, pos, blocks, column_reach);
            }
        }
        placed
    }
}

struct HugeFungus {
    valid_base: Block,
    stem: BlockState,
    hat: BlockState,
    decor: BlockState,
    replaceable: BlockPredicate,
    planted: bool,
}

const HUGE_PROBABILITY: f32 = 0.06;

impl HugeFungus {
    fn is_replaceable(&self, level: &mut Region, pos: BlockPos, check_plants: bool) -> bool {
        level.block(pos).is_replaceable() || (check_plants && self.replaceable.test(level, pos))
    }

    fn place_stem(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32, huge: bool) {
        let radius: i32 = if huge { 1 } else { 0 };
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let corner = huge && dx.abs() == radius && dz.abs() == radius;
                for dy in 0..height {
                    let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                    if !self.is_replaceable(ctx.level, pos, true) {
                        continue;
                    }
                    if self.planted || !corner || ctx.random.next_float() < 0.1 {
                        ctx.level.set_block(pos, self.stem);
                    }
                }
            }
        }
    }

    fn place_hat(&self, ctx: &mut PlaceContext, origin: BlockPos, height: i32, huge: bool) {
        let vines = self.hat.block() == Block::NetherWartBlock;
        let hat_height = (ctx.random.next_int_bounded(1 + height / 3) + 5).min(height);
        let hat_start = height - hat_height;
        for dy in hat_start..=height {
            let mut radius = if dy < height - ctx.random.next_int_bounded(3) {
                2
            } else {
                1
            };
            if hat_height > 8 && dy < hat_start + 4 {
                radius = 3;
            }
            if huge {
                radius += 1;
            }
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let edge_x = dx == -radius || dx == radius;
                    let edge_z = dz == -radius || dz == radius;
                    let inside = !edge_x && !edge_z && dy != height;
                    let corner = edge_x && edge_z;
                    let bottom = dy < hat_start + 3;
                    let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                    if !self.is_replaceable(ctx.level, pos, false) {
                        continue;
                    }
                    if self.planted && !is_air(ctx.level.block(step(pos, Facing::Down))) {
                        ctx.level.set_block(pos, Block::Air.default_state());
                    }
                    if bottom {
                        if !inside {
                            self.place_hat_drop(ctx, pos, vines);
                        }
                    } else if inside {
                        self.place_hat_block(ctx, pos, 0.1, 0.2, if vines { 0.1 } else { 0.0 });
                    } else if corner {
                        self.place_hat_block(ctx, pos, 0.01, 0.7, if vines { 0.083 } else { 0.0 });
                    } else {
                        self.place_hat_block(
                            ctx,
                            pos,
                            5.0e-4,
                            0.98,
                            if vines { 0.07 } else { 0.0 },
                        );
                    }
                }
            }
        }
    }

    fn place_hat_block(
        &self,
        ctx: &mut PlaceContext,
        pos: BlockPos,
        decor: f32,
        hat: f32,
        vines: f32,
    ) {
        if ctx.random.next_float() < decor {
            ctx.level.set_block(pos, self.decor);
        } else if ctx.random.next_float() < hat {
            ctx.level.set_block(pos, self.hat);
            if ctx.random.next_float() < vines {
                place_weeping_vines(ctx, pos);
            }
        }
    }

    fn place_hat_drop(&self, ctx: &mut PlaceContext, pos: BlockPos, vines: bool) {
        if ctx.level.block(step(pos, Facing::Down)).block() == self.hat.block() {
            ctx.level.set_block(pos, self.hat);
            return;
        }
        if (ctx.random.next_float() as f64) < 0.15 {
            ctx.level.set_block(pos, self.hat);
            if vines && ctx.random.next_int_bounded(11) == 0 {
                place_weeping_vines(ctx, pos);
            }
        }
    }
}

fn place_weeping_vines(ctx: &mut PlaceContext, hat: BlockPos) {
    let start = step(hat, Facing::Down);
    if !is_air(ctx.level.block(start)) {
        return;
    }
    let mut goal = next_int(ctx.random, 1, 5);
    if ctx.random.next_int_bounded(7) == 0 {
        goal *= 2;
    }
    let mut pos = start;
    for height in 0..=goal {
        if is_air(ctx.level.block(pos)) {
            if height == goal || !is_air(ctx.level.block(step(pos, Facing::Down))) {
                let age = next_int(ctx.random, 23, 25).to_string();
                let state = Block::WeepingVines.default_state();
                ctx.level
                    .set_block(pos, state.with("age", &age).unwrap_or(state));
                return;
            }
            ctx.level
                .set_block(pos, Block::WeepingVinesPlant.default_state());
        }
        pos.y -= 1;
    }
}

impl Feature for HugeFungus {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        if ctx.level.block(step(origin, Facing::Down)).block() != self.valid_base {
            return false;
        }
        let mut height = next_int(ctx.random, 4, 13);
        if ctx.random.next_int_bounded(12) == 0 {
            height *= 2;
        }
        if !self.planted && origin.y + height + 1 >= ctx.level.generator_span().height {
            return false;
        }
        let huge = !self.planted && ctx.random.next_float() < HUGE_PROBABILITY;
        ctx.level.set_block(origin, Block::Air.default_state());
        self.place_stem(ctx, origin, height, huge);
        self.place_hat(ctx, origin, height, huge);
        true
    }
}

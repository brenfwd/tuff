//! Trees: a trunk placer, a foliage placer, optional roots and decorators,
//! then the leaf distance pass and the shape updates along the tree's
//! outline.

pub mod decorator;
mod foliage;
mod trunk;

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::{Axis, Facing};
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::feature::predicate::{DIRECTIONS, Fluid, fluid, step};
use crate::feature::provider::StateProvider;
use crate::feature::{Feature, OwnedIntProvider, PlaceContext, Registry};
use crate::java_set::PosSet;
use crate::json::{self, Result, obj};
use crate::tags::{BlockSet, leaves, prevents_nearby_leaf_decay, replaceable_by_trees};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_vanilla_common::placement::update_shape;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

use decorator::{Parts, TreeDecorator};
use foliage::FoliagePlacer;
use trunk::TrunkPlacer;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:tree", |r, v| Ok(Arc::new(Tree::parse(r, v)?)));
    registry.register("minecraft:fallen_tree", |r, v| {
        Ok(Arc::new(FallenTree::parse(r, v)?))
    });
}

pub struct Tree {
    trunk_provider: Arc<StateProvider>,
    trunk: TrunkPlacer,
    foliage_provider: Arc<StateProvider>,
    foliage: FoliagePlacer,
    roots: Option<RootPlacer>,
    size: FeatureSize,
    decorators: Vec<TreeDecorator>,
    ignore_vines: bool,
    below_trunk: Arc<StateProvider>,
}

/// Where a foliage placer grows leaves.
#[derive(Clone, Copy)]
pub struct Attachment {
    pub pos: BlockPos,
    pub radius_offset: i32,
    pub height_offset: i32,
    pub double_trunk: bool,
}

impl Attachment {
    pub fn new(pos: BlockPos, radius_offset: i32, double_trunk: bool) -> Self {
        Self {
            pos,
            radius_offset,
            height_offset: 0,
            double_trunk,
        }
    }
}

/// A tree being placed: the level, the random source and every position
/// written so far, by role.
pub struct Grow<'a, 'g> {
    pub level: &'a mut Region<'g>,
    pub random: &'a mut WorldgenRandom,
    roots: PosSet,
    trunks: PosSet,
    foliage: PosSet,
    decorations: PosSet,
}

impl Grow<'_, '_> {
    pub fn block(&self, pos: BlockPos) -> BlockState {
        self.level.block(pos)
    }

    fn write(&mut self, pos: BlockPos, state: BlockState) {
        self.level.set_block_flags(pos, state, false);
    }

    pub fn set_root(&mut self, pos: BlockPos, state: BlockState) {
        self.roots.insert(pos);
        self.write(pos, state);
    }

    pub fn set_trunk(&mut self, pos: BlockPos, state: BlockState) {
        self.trunks.insert(pos);
        self.write(pos, state);
    }

    pub fn set_foliage(&mut self, pos: BlockPos, state: BlockState) {
        self.foliage.insert(pos);
        self.write(pos, state);
    }

    pub fn is_foliage(&self, pos: BlockPos) -> bool {
        self.foliage.contains(pos)
    }

    pub fn set_decoration(&mut self, pos: BlockPos, state: BlockState) {
        self.decorations.insert(pos);
        self.write(pos, state);
    }
}

pub fn valid_tree_pos(state: BlockState) -> bool {
    is_air(state) || replaceable_by_trees().contains_state(state)
}

pub fn is_air_or_leaves(state: BlockState) -> bool {
    is_air(state) || leaves().contains_state(state)
}

pub fn with_axis(state: BlockState, axis: Axis) -> BlockState {
    state.with("axis", axis.name()).unwrap_or(state)
}

pub fn relative(pos: BlockPos, direction: Facing, n: i32) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(pos.x + x * n, pos.y + y * n, pos.z + z * n)
}

/// A shuffled copy, drawing like the game's list shuffle.
pub fn shuffled<T: Copy>(list: &[T], random: &mut impl RandomSource) -> Vec<T> {
    let mut copy = list.to_vec();
    decorator::shuffle(&mut copy, random);
    copy
}

impl Tree {
    fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "type",
                "trunk_provider",
                "trunk_placer",
                "foliage_provider",
                "foliage_placer",
                "root_placer",
                "minimum_size",
                "decorators",
                "ignore_vines",
                "below_trunk_provider",
            ],
            "tree",
        )?;
        let decorators = o
            .list("decorators")?
            .iter()
            .map(|d| TreeDecorator::parse(registry, d))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            trunk_provider: StateProvider::parse(registry, o.req("trunk_provider")?)?,
            trunk: TrunkPlacer::parse(o.req("trunk_placer")?)?,
            foliage_provider: StateProvider::parse(registry, o.req("foliage_provider")?)?,
            foliage: FoliagePlacer::parse(o.req("foliage_placer")?)?,
            roots: o
                .get("root_placer")
                .map(|v| RootPlacer::parse(registry, v))
                .transpose()?,
            size: FeatureSize::parse(o.req("minimum_size")?)?,
            decorators,
            ignore_vines: o.bool_or("ignore_vines", false)?,
            below_trunk: StateProvider::parse(registry, o.req("below_trunk_provider")?)?,
        })
    }

    pub fn place_below_trunk(&self, g: &mut Grow, pos: BlockPos) {
        if let Some(state) = self.below_trunk.optional_state(g.level, g.random, pos) {
            g.set_trunk(pos, state);
        }
    }

    fn max_free_height(&self, g: &Grow, max: i32, origin: BlockPos) -> i32 {
        for y in 0..=max + 1 {
            let r = self.size.size_at(max, y);
            for x in -r..=r {
                for z in -r..=r {
                    let state = g.block(BlockPos::new(origin.x + x, origin.y + y, origin.z + z));
                    if !self.trunk.is_free(state)
                        || (!self.ignore_vines && state.block() == Block::Vine)
                    {
                        return y - 2;
                    }
                }
            }
        }
        max
    }

    fn grow(&self, g: &mut Grow, origin: BlockPos) -> bool {
        let tree_height = self.trunk.tree_height(g.random);
        let foliage_height = self.foliage.foliage_height(g.random, tree_height);
        let trunk_height = tree_height - foliage_height;
        let leaf_radius = self.foliage.foliage_radius(g.random, trunk_height);
        let trunk_origin = match &self.roots {
            Some(roots) => {
                let dy = roots.trunk_offset.sample(g.random);
                BlockPos::new(origin.x, origin.y + dy, origin.z)
            }
            None => origin,
        };
        let min_y = origin.y.min(trunk_origin.y);
        let max_y = origin.y.max(trunk_origin.y) + tree_height + 1;
        if min_y < g.level.min_y() + 1 || max_y > g.level.max_y() + 1 {
            return false;
        }
        let clipped = self.max_free_height(g, tree_height, trunk_origin);
        if clipped < tree_height && self.size.min_clipped.is_none_or(|min| clipped < min) {
            return false;
        }
        if let Some(roots) = &self.roots
            && !roots.place(g, origin, trunk_origin)
        {
            return false;
        }
        for attachment in self.trunk.place(g, self, clipped, trunk_origin) {
            self.foliage
                .create(g, self, attachment, foliage_height, leaf_radius);
        }
        true
    }
}

impl Feature for Tree {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut g = Grow {
            level: &mut *ctx.level,
            random: &mut *ctx.random,
            roots: PosSet::new(),
            trunks: PosSet::new(),
            foliage: PosSet::new(),
            decorations: PosSet::new(),
        };
        if !self.grow(&mut g, origin) || (g.trunks.is_empty() && g.foliage.is_empty()) {
            return false;
        }
        if !self.decorators.is_empty() {
            let sorted = |set: &PosSet| {
                let mut list: Vec<BlockPos> = set.iter().collect();
                list.sort_by_key(|p| p.y);
                list
            };
            let parts = Parts {
                logs: sorted(&g.trunks),
                leaves: sorted(&g.foliage),
                roots: sorted(&g.roots),
            };
            for decorator in &self.decorators {
                decorator.place(&mut g, &parts);
            }
        }
        let all = g
            .roots
            .iter()
            .chain(g.trunks.iter())
            .chain(g.foliage.iter())
            .chain(g.decorations.iter());
        let Some(bounds) = Bounds::around(all) else {
            return false;
        };
        let shape = update_leaves(&mut g, &bounds);
        update_edges(g.level, &shape, bounds.min);
        true
    }
}

struct Bounds {
    min: BlockPos,
    max: BlockPos,
}

impl Bounds {
    fn around(mut positions: impl Iterator<Item = BlockPos>) -> Option<Self> {
        let first = positions.next()?;
        let (mut min, mut max) = (first, first);
        for p in positions {
            min = BlockPos::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
            max = BlockPos::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
        }
        Some(Self { min, max })
    }

    fn contains(&self, p: BlockPos) -> bool {
        (self.min.x..=self.max.x).contains(&p.x)
            && (self.min.y..=self.max.y).contains(&p.y)
            && (self.min.z..=self.max.z).contains(&p.z)
    }
}

/// Filled cells of the tree's bounding box.
pub(crate) struct Voxels {
    size: [i32; 3],
    cells: Vec<bool>,
}

impl Voxels {
    pub(crate) fn with_size(size: [i32; 3]) -> Self {
        Self {
            size,
            cells: vec![false; (size[0] * size[1] * size[2]) as usize],
        }
    }

    fn new(bounds: &Bounds) -> Self {
        let size = [
            bounds.max.x - bounds.min.x + 1,
            bounds.max.y - bounds.min.y + 1,
            bounds.max.z - bounds.min.z + 1,
        ];
        Self {
            size,
            cells: vec![false; (size[0] * size[1] * size[2]) as usize],
        }
    }

    fn index(&self, [x, y, z]: [i32; 3]) -> usize {
        ((x * self.size[1] + y) * self.size[2] + z) as usize
    }

    pub(crate) fn fill(&mut self, at: [i32; 3]) {
        let i = self.index(at);
        self.cells[i] = true;
    }

    fn is_full(&self, at: [i32; 3]) -> bool {
        self.cells[self.index(at)]
    }
}

fn leaf_distance(state: BlockState) -> Option<i32> {
    if prevents_nearby_leaf_decay().contains_state(state) {
        return Some(0);
    }
    state.get("distance").and_then(|d| d.parse().ok())
}

/// Gives every leaf its distance to the nearest log, as the game does
/// right after a tree is placed, and returns the cells that were visited.
fn update_leaves(g: &mut Grow, bounds: &Bounds) -> Voxels {
    let mut shape = Voxels::new(bounds);
    let local = |p: BlockPos| [p.x - bounds.min.x, p.y - bounds.min.y, p.z - bounds.min.z];
    for pos in g.decorations.iter().chain(g.roots.iter()) {
        if bounds.contains(pos) {
            shape.fill(local(pos));
        }
    }
    let mut to_check: Vec<PosSet> = (0..7).map(|_| PosSet::new()).collect();
    for pos in g.trunks.iter() {
        to_check[0].insert(pos);
    }
    let mut smallest = 0usize;
    loop {
        if smallest >= 7 {
            return shape;
        }
        let Some(pos) = to_check[smallest].pop_first() else {
            smallest += 1;
            continue;
        };
        if !bounds.contains(pos) {
            continue;
        }
        if smallest != 0 {
            let state = g.block(pos);
            let state = state
                .with("distance", &smallest.to_string())
                .unwrap_or(state);
            g.level.set_block_flags(pos, state, false);
        }
        shape.fill(local(pos));
        for direction in DIRECTIONS {
            let neighbour = step(pos, direction);
            if !bounds.contains(neighbour) || shape.is_full(local(neighbour)) {
                continue;
            }
            if let Some(distance) = leaf_distance(g.block(neighbour)) {
                let new = distance.min(smallest as i32 + 1);
                if new < 7 {
                    to_check[new as usize].insert(neighbour);
                    smallest = smallest.min(new as usize);
                }
            }
        }
    }
}

/// Lets the blocks on both sides of every outer face of `shape` react to
/// each other, in the game's face order.
pub(crate) fn update_edges(level: &mut Region, shape: &Voxels, min: BlockPos) {
    let [sx, sy, sz] = shape.size;
    let face = |level: &mut Region, direction: Facing, [x, y, z]: [i32; 3]| {
        let pos = BlockPos::new(min.x + x, min.y + y, min.z + z);
        let neighbour_pos = step(pos, direction);
        let state = level.block(pos);
        let neighbour = level.block(neighbour_pos);
        let updated = update_shape(state, pos, direction, neighbour, &|p| level.block(p));
        if updated != state {
            level.set_block_flags(pos, updated, true);
        }
        let neighbour_updated = update_shape(
            neighbour,
            neighbour_pos,
            direction.opposite(),
            updated,
            &|p| level.block(p),
        );
        if neighbour_updated != neighbour {
            level.set_block_flags(neighbour_pos, neighbour_updated, true);
        }
    };
    let axes: [(usize, usize, usize, Facing, Facing); 3] = [
        (0, 1, 2, Facing::North, Facing::South),
        (2, 0, 1, Facing::Down, Facing::Up),
        (1, 2, 0, Facing::West, Facing::East),
    ];
    for (a_axis, b_axis, c_axis, negative, positive) in axes {
        let size = [sx, sy, sz];
        for a in 0..size[a_axis] {
            for b in 0..size[b_axis] {
                let mut last = false;
                for c in 0..=size[c_axis] {
                    let mut at = [0; 3];
                    at[a_axis] = a;
                    at[b_axis] = b;
                    at[c_axis] = c;
                    let full = c != size[c_axis] && shape.is_full(at);
                    if !last && full {
                        face(level, negative, at);
                    }
                    if last && !full {
                        at[c_axis] = c - 1;
                        face(level, positive, at);
                    }
                    last = full;
                }
            }
        }
    }
}

enum FeatureSizeKind {
    Two {
        limit: i32,
        lower: i32,
        upper: i32,
    },
    Three {
        limit: i32,
        upper_limit: i32,
        lower: i32,
        middle: i32,
        upper: i32,
    },
}

struct FeatureSize {
    kind: FeatureSizeKind,
    min_clipped: Option<i32>,
}

impl FeatureSize {
    fn parse(value: &Value) -> Result<Self> {
        let what = "feature size";
        let (o, kind) = match json::qualify(json::kind(value)?).as_str() {
            "minecraft:two_layers_feature_size" => {
                let o = obj(
                    value,
                    &[
                        "type",
                        "limit",
                        "lower_size",
                        "upper_size",
                        "min_clipped_height",
                    ],
                    what,
                )?;
                let kind = FeatureSizeKind::Two {
                    limit: o.i32_or("limit", 1)?,
                    lower: o.i32_or("lower_size", 0)?,
                    upper: o.i32_or("upper_size", 1)?,
                };
                (o, kind)
            }
            "minecraft:three_layers_feature_size" => {
                let o = obj(
                    value,
                    &[
                        "type",
                        "limit",
                        "upper_limit",
                        "lower_size",
                        "middle_size",
                        "upper_size",
                        "min_clipped_height",
                    ],
                    what,
                )?;
                let kind = FeatureSizeKind::Three {
                    limit: o.i32_or("limit", 1)?,
                    upper_limit: o.i32_or("upper_limit", 1)?,
                    lower: o.i32_or("lower_size", 0)?,
                    middle: o.i32_or("middle_size", 1)?,
                    upper: o.i32_or("upper_size", 1)?,
                };
                (o, kind)
            }
            other => return Err(format!("unknown feature size {other}")),
        };
        let min_clipped = o
            .get("min_clipped_height")
            .map(|_| o.i32("min_clipped_height"))
            .transpose()?;
        Ok(Self { kind, min_clipped })
    }

    fn size_at(&self, height: i32, y: i32) -> i32 {
        match self.kind {
            FeatureSizeKind::Two {
                limit,
                lower,
                upper,
            } => {
                if y < limit {
                    lower
                } else {
                    upper
                }
            }
            FeatureSizeKind::Three {
                limit,
                upper_limit,
                lower,
                middle,
                upper,
            } => {
                if y < limit {
                    lower
                } else if y >= height - upper_limit {
                    upper
                } else {
                    middle
                }
            }
        }
    }
}

/// Mangrove-style roots: columns down from the raised trunk, spreading
/// outwards in each horizontal direction.
struct RootPlacer {
    trunk_offset: OwnedIntProvider,
    provider: Arc<StateProvider>,
    above: Option<(Arc<StateProvider>, f32)>,
    grow_through: BlockSet,
    muddy_in: BlockSet,
    muddy_provider: Arc<StateProvider>,
    max_width: i32,
    max_length: i32,
    skew_chance: f32,
}

impl RootPlacer {
    fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let what = "root placer";
        let kind = json::qualify(json::kind(value)?);
        if kind != "minecraft:mangrove_root_placer" {
            return Err(format!("unknown root placer {kind}"));
        }
        let o = obj(
            value,
            &[
                "type",
                "trunk_offset_y",
                "root_provider",
                "above_root_placement",
                "mangrove_root_placement",
            ],
            what,
        )?;
        let above = match o.get("above_root_placement") {
            Some(v) => {
                let a = obj(
                    v,
                    &["above_root_provider", "above_root_placement_chance"],
                    what,
                )?;
                Some((
                    StateProvider::parse(registry, a.req("above_root_provider")?)?,
                    a.f32("above_root_placement_chance")?,
                ))
            }
            None => None,
        };
        let m = obj(
            o.req("mangrove_root_placement")?,
            &[
                "can_grow_through",
                "muddy_roots_in",
                "muddy_roots_provider",
                "max_root_width",
                "max_root_length",
                "random_skew_chance",
            ],
            what,
        )?;
        Ok(Self {
            trunk_offset: OwnedIntProvider::parse(o.req("trunk_offset_y")?)?,
            provider: StateProvider::parse(registry, o.req("root_provider")?)?,
            above,
            grow_through: json::block_set(m.req("can_grow_through")?)?,
            muddy_in: json::block_set(m.req("muddy_roots_in")?)?,
            muddy_provider: StateProvider::parse(registry, m.req("muddy_roots_provider")?)?,
            max_width: m.i32("max_root_width")?,
            max_length: m.i32("max_root_length")?,
            skew_chance: m.f32("random_skew_chance")?,
        })
    }

    fn can_place(&self, state: BlockState) -> bool {
        valid_tree_pos(state) || self.grow_through.contains_state(state)
    }

    fn place(&self, g: &mut Grow, origin: BlockPos, trunk_origin: BlockPos) -> bool {
        let mut column = origin;
        while column.y < trunk_origin.y {
            if !self.can_place(g.block(column)) {
                return false;
            }
            column = BlockPos::new(column.x, column.y + 1, column.z);
        }
        let mut positions = vec![step(trunk_origin, Facing::Down)];
        for direction in Facing::HORIZONTAL {
            let pos = step(trunk_origin, direction);
            let mut in_direction = Vec::new();
            if !self.simulate(g, pos, direction, trunk_origin, &mut in_direction, 0) {
                return false;
            }
            positions.extend(in_direction);
            positions.push(pos);
        }
        for pos in positions {
            self.place_root(g, pos);
        }
        true
    }

    fn simulate(
        &self,
        g: &mut Grow,
        pos: BlockPos,
        direction: Facing,
        origin: BlockPos,
        out: &mut Vec<BlockPos>,
        layer: i32,
    ) -> bool {
        if layer == self.max_length || out.len() as i32 > self.max_length {
            return false;
        }
        for next in self.potential(g.random, pos, direction, origin) {
            if self.can_place(g.block(next)) {
                out.push(next);
                if !self.simulate(g, next, direction, origin, out, layer + 1) {
                    return false;
                }
            }
        }
        true
    }

    fn potential(
        &self,
        random: &mut impl RandomSource,
        pos: BlockPos,
        direction: Facing,
        origin: BlockPos,
    ) -> Vec<BlockPos> {
        let below = step(pos, Facing::Down);
        let next = step(pos, direction);
        let width = (pos.x - origin.x).abs() + (pos.y - origin.y).abs() + (pos.z - origin.z).abs();
        if width > self.max_width - 3 && width <= self.max_width {
            return if random.next_float() < self.skew_chance {
                vec![below, step(next, Facing::Down)]
            } else {
                vec![below]
            };
        }
        if width > self.max_width || random.next_float() < self.skew_chance {
            return vec![below];
        }
        if random.next_bool() {
            vec![next]
        } else {
            vec![below]
        }
    }

    fn waterlogged(g: &Grow, pos: BlockPos, state: BlockState) -> BlockState {
        if state.get("waterlogged").is_none() {
            return state;
        }
        let water = matches!(fluid(g.block(pos)), Fluid::Water | Fluid::FlowingWater);
        state
            .with("waterlogged", if water { "true" } else { "false" })
            .unwrap_or(state)
    }

    fn place_root(&self, g: &mut Grow, pos: BlockPos) {
        let existing = g.block(pos);
        if self.muddy_in.contains_state(existing) {
            let state = self.muddy_provider.state(g.level, g.random, pos);
            let state = Self::waterlogged(g, pos, state);
            g.set_root(pos, state);
            return;
        }
        if !self.can_place(existing) {
            return;
        }
        let state = self.provider.state(g.level, g.random, pos);
        let state = Self::waterlogged(g, pos, state);
        g.set_root(pos, state);
        if let Some((provider, chance)) = &self.above {
            let above = step(pos, Facing::Up);
            if g.random.next_float() < *chance && is_air(g.block(above)) {
                let state = provider.state(g.level, g.random, above);
                let state = Self::waterlogged(g, above, state);
                g.set_root(above, state);
            }
        }
    }
}

/// A stump with a log lying next to it.
struct FallenTree {
    trunk_provider: Arc<StateProvider>,
    length: OwnedIntProvider,
    stump_decorators: Vec<TreeDecorator>,
    log_decorators: Vec<TreeDecorator>,
}

impl FallenTree {
    fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "type",
                "trunk_provider",
                "log_length",
                "stump_decorators",
                "log_decorators",
            ],
            "fallen_tree",
        )?;
        let decorators = |registry: &mut Registry, key: &str| -> Result<Vec<TreeDecorator>> {
            o.list(key)?
                .iter()
                .map(|d| TreeDecorator::parse(registry, d))
                .collect()
        };
        Ok(Self {
            trunk_provider: StateProvider::parse(registry, o.req("trunk_provider")?)?,
            length: OwnedIntProvider::parse(o.req("log_length")?)?,
            stump_decorators: decorators(registry, "stump_decorators")?,
            log_decorators: decorators(registry, "log_decorators")?,
        })
    }

    fn place_log(&self, g: &mut Grow, pos: BlockPos, axis: Option<Axis>) {
        let state = self.trunk_provider.state(g.level, g.random, pos);
        let state = axis.map_or(state, |axis| with_axis(state, axis));
        g.level.set_block(pos, state);
        for i in 1..=2 {
            let above = BlockPos::new(pos.x, pos.y + i, pos.z);
            if is_air(g.block(above)) {
                break;
            }
            g.level.mark_post_processing(above);
        }
    }

    fn decorate(g: &mut Grow, logs: &PosSet, decorators: &[TreeDecorator]) {
        if decorators.is_empty() {
            return;
        }
        let mut list: Vec<BlockPos> = logs.iter().collect();
        list.sort_by_key(|p| p.y);
        let parts = Parts {
            logs: list,
            leaves: Vec::new(),
            roots: Vec::new(),
        };
        for decorator in decorators {
            decorator.place(g, &parts);
        }
    }

    fn over_solid_ground(g: &Grow, pos: BlockPos) -> bool {
        g.block(step(pos, Facing::Down))
            .is_face_sturdy(Facing::Up, tuff_vanilla_data::Support::Full)
    }
}

impl Feature for FallenTree {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let mut g = Grow {
            level: &mut *ctx.level,
            random: &mut *ctx.random,
            roots: PosSet::new(),
            trunks: PosSet::new(),
            foliage: PosSet::new(),
            decorations: PosSet::new(),
        };
        self.place_log(&mut g, origin, None);
        let mut stump = PosSet::new();
        stump.insert(origin);
        Self::decorate(&mut g, &stump, &self.stump_decorators);
        let direction = trunk::horizontal(g.random);
        let length = self.length.sample(g.random) - 2;
        let distance = 2 + g.random.next_int_bounded(2);
        let mut start = relative(origin, direction, distance);
        start = step(start, Facing::Up);
        for _ in 0..6 {
            if valid_tree_pos(g.block(start)) && Self::over_solid_ground(&g, start) {
                break;
            }
            start = step(start, Facing::Down);
        }
        let mut gap = 0;
        let mut pos = start;
        for _ in 0..length {
            if !valid_tree_pos(g.block(pos)) {
                return true;
            }
            if Self::over_solid_ground(&g, pos) {
                gap = 0;
            } else {
                gap += 1;
                if gap > 2 {
                    return true;
                }
            }
            pos = step(pos, direction);
        }
        let mut logs = PosSet::new();
        let mut pos = start;
        for _ in 0..length {
            self.place_log(&mut g, pos, Some(direction.axis()));
            logs.insert(pos);
            pos = step(pos, direction);
        }
        Self::decorate(&mut g, &logs, &self.log_decorators);
        true
    }
}

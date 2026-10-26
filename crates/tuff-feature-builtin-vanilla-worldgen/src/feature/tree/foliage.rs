//! Foliage placers: the leaves around each attachment point a trunk
//! placer reports.

use serde_json::Value;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Axis, Facing};

use crate::feature::OwnedIntProvider;
use crate::feature::predicate::{Fluid, fluid, offset, step};
use crate::feature::tree::{Attachment, Grow, Tree, valid_tree_pos, with_axis};
use crate::json::{self, Result, obj};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;

pub struct FoliagePlacer {
    radius: OwnedIntProvider,
    offset: OwnedIntProvider,
    kind: Kind,
}

enum Kind {
    Blob(i32),
    Fancy(i32),
    Bush(i32),
    Jungle(i32),
    Spruce(OwnedIntProvider),
    Pine(OwnedIntProvider),
    MegaPine(OwnedIntProvider),
    DarkOak,
    Acacia,
    RandomSpread {
        height: OwnedIntProvider,
        attempts: i32,
    },
    Cherry {
        height: OwnedIntProvider,
        wide_bottom_hole: f32,
        corner_hole: f32,
        hanging: f32,
        hanging_extension: f32,
    },
    Poplar {
        height: OwnedIntProvider,
        side_hole: f32,
    },
}

impl FoliagePlacer {
    pub fn parse(value: &Value) -> Result<Self> {
        let what = "foliage placer";
        let name = json::qualify(json::kind(value)?);
        let fields = |extra: &[&'static str]| -> Vec<&'static str> {
            ["type", "radius", "offset"]
                .iter()
                .chain(extra)
                .copied()
                .collect()
        };
        let o = |extra: &[&'static str]| obj(value, &fields(extra), what);
        let int = |o: &json::Obj, key: &str| OwnedIntProvider::parse(o.req(key)?);
        let (o, kind) = match name.as_str() {
            "minecraft:blob_foliage_placer" => {
                let o = o(&["height"])?;
                let h = o.i32("height")?;
                (o, Kind::Blob(h))
            }
            "minecraft:fancy_foliage_placer" => {
                let o = o(&["height"])?;
                let h = o.i32("height")?;
                (o, Kind::Fancy(h))
            }
            "minecraft:bush_foliage_placer" => {
                let o = o(&["height"])?;
                let h = o.i32("height")?;
                (o, Kind::Bush(h))
            }
            "minecraft:jungle_foliage_placer" => {
                let o = o(&["height"])?;
                let h = o.i32("height")?;
                (o, Kind::Jungle(h))
            }
            "minecraft:spruce_foliage_placer" => {
                let o = o(&["trunk_height"])?;
                let k = Kind::Spruce(int(&o, "trunk_height")?);
                (o, k)
            }
            "minecraft:pine_foliage_placer" => {
                let o = o(&["height"])?;
                let k = Kind::Pine(int(&o, "height")?);
                (o, k)
            }
            "minecraft:mega_pine_foliage_placer" => {
                let o = o(&["crown_height"])?;
                let k = Kind::MegaPine(int(&o, "crown_height")?);
                (o, k)
            }
            "minecraft:dark_oak_foliage_placer" => (o(&[])?, Kind::DarkOak),
            "minecraft:acacia_foliage_placer" => (o(&[])?, Kind::Acacia),
            "minecraft:random_spread_foliage_placer" => {
                let o = o(&["foliage_height", "leaf_placement_attempts"])?;
                let k = Kind::RandomSpread {
                    height: int(&o, "foliage_height")?,
                    attempts: o.i32("leaf_placement_attempts")?,
                };
                (o, k)
            }
            "minecraft:cherry_foliage_placer" => {
                let o = o(&[
                    "height",
                    "wide_bottom_layer_hole_chance",
                    "corner_hole_chance",
                    "hanging_leaves_chance",
                    "hanging_leaves_extension_chance",
                ])?;
                let k = Kind::Cherry {
                    height: int(&o, "height")?,
                    wide_bottom_hole: o.f32("wide_bottom_layer_hole_chance")?,
                    corner_hole: o.f32("corner_hole_chance")?,
                    hanging: o.f32("hanging_leaves_chance")?,
                    hanging_extension: o.f32("hanging_leaves_extension_chance")?,
                };
                (o, k)
            }
            "minecraft:poplar_foliage_placer" => {
                let o = o(&["height", "side_hole_chance"])?;
                let k = Kind::Poplar {
                    height: int(&o, "height")?,
                    side_hole: o.f32("side_hole_chance")?,
                };
                (o, k)
            }
            other => return Err(format!("unknown foliage placer {other}")),
        };
        Ok(Self {
            radius: int(&o, "radius")?,
            offset: int(&o, "offset")?,
            kind,
        })
    }

    pub fn foliage_height(&self, random: &mut impl RandomSource, tree_height: i32) -> i32 {
        match &self.kind {
            Kind::Blob(h) | Kind::Fancy(h) | Kind::Bush(h) | Kind::Jungle(h) => *h,
            Kind::Spruce(trunk) => 4.max(tree_height - trunk.sample(random)),
            Kind::Pine(h) | Kind::MegaPine(h) => h.sample(random),
            Kind::DarkOak => 4,
            Kind::Acacia => 0,
            Kind::RandomSpread { height, .. }
            | Kind::Cherry { height, .. }
            | Kind::Poplar { height, .. } => height.sample(random),
        }
    }

    pub fn foliage_radius(&self, random: &mut impl RandomSource, trunk_height: i32) -> i32 {
        let radius = self.radius.sample(random);
        match self.kind {
            Kind::Pine(_) => radius + random.next_int_bounded((trunk_height + 1).max(1)),
            _ => radius,
        }
    }

    pub fn create(
        &self,
        g: &mut Grow,
        tree: &Tree,
        attachment: Attachment,
        foliage_height: i32,
        leaf_radius: i32,
    ) {
        let offset = self.offset.sample(g.random);
        let a = attachment;
        let double = a.double_trunk;
        let height_with_offset = foliage_height + a.height_offset;
        match &self.kind {
            Kind::Blob(_) => {
                let mut yo = offset;
                while yo >= offset - height_with_offset {
                    let r = (leaf_radius + a.radius_offset - 1 - yo / 2).max(0);
                    self.row(g, tree, a.pos, r, yo, double);
                    yo -= 1;
                }
            }
            Kind::Fancy(_) => {
                let mut yo = offset;
                while yo >= offset - foliage_height {
                    let r = leaf_radius
                        + if yo == offset || yo == offset - foliage_height {
                            0
                        } else {
                            1
                        };
                    self.row(g, tree, a.pos, r, yo, double);
                    yo -= 1;
                }
            }
            Kind::Bush(_) => {
                let mut yo = offset;
                while yo >= offset - height_with_offset {
                    let r = leaf_radius + a.radius_offset - 1 - yo;
                    self.row(g, tree, a.pos, r, yo, double);
                    yo -= 1;
                }
            }
            Kind::Jungle(_) => {
                let leaf_height = if double {
                    foliage_height
                } else {
                    1 + g.random.next_int_bounded(2)
                } + a.height_offset;
                let mut yo = offset;
                while yo >= offset - leaf_height {
                    let r = leaf_radius + a.radius_offset + 1 - yo;
                    self.row(g, tree, a.pos, r, yo, double);
                    yo -= 1;
                }
            }
            Kind::Spruce(_) => {
                let mut r = g.random.next_int_bounded(2);
                let mut max = 1;
                let mut min = 0;
                let mut yo = offset;
                while yo >= -height_with_offset {
                    self.row(g, tree, a.pos, r, yo, double);
                    if r >= max {
                        r = min;
                        min = 1;
                        max = (max + 1).min(leaf_radius + a.radius_offset);
                    } else {
                        r += 1;
                    }
                    yo -= 1;
                }
            }
            Kind::Pine(_) => {
                let mut r = 0;
                let mut yo = offset;
                while yo >= offset - height_with_offset {
                    self.row(g, tree, a.pos, r, yo, double);
                    if r >= 1 && yo == offset - height_with_offset + 1 {
                        r -= 1;
                    } else if r < leaf_radius + a.radius_offset {
                        r += 1;
                    }
                    yo -= 1;
                }
            }
            Kind::MegaPine(_) => {
                let mut previous = 0;
                for yy in a.pos.y - height_with_offset + offset..=a.pos.y + offset {
                    let yo = a.pos.y - yy;
                    let smooth = leaf_radius
                        + a.radius_offset
                        + (yo as f32 / height_with_offset as f32 * 3.5).floor() as i32;
                    let jagged = if yo > 0 && smooth == previous && (yy & 1) == 0 {
                        smooth + 1
                    } else {
                        smooth
                    };
                    let pos = BlockPos::new(a.pos.x, yy, a.pos.z);
                    self.row(g, tree, pos, jagged, 0, double);
                    previous = smooth;
                }
            }
            Kind::DarkOak => {
                let pos = up(a.pos, offset);
                if double {
                    self.row(g, tree, pos, leaf_radius + 2, -1, double);
                    self.row(g, tree, pos, leaf_radius + 3, 0, double);
                    self.row(g, tree, pos, leaf_radius + 2, 1, double);
                    if g.random.next_bool() {
                        self.row(g, tree, pos, leaf_radius, 2, double);
                    }
                    return;
                }
                self.row(g, tree, pos, leaf_radius + 2, -1, double);
                self.row(g, tree, pos, leaf_radius + 1, 0, double);
            }
            Kind::Acacia => {
                let pos = up(a.pos, offset);
                self.row(
                    g,
                    tree,
                    pos,
                    leaf_radius + a.radius_offset,
                    -1 - height_with_offset,
                    double,
                );
                self.row(g, tree, pos, leaf_radius - 1, -height_with_offset, double);
                self.row(g, tree, pos, leaf_radius + a.radius_offset - 1, 0, double);
            }
            Kind::RandomSpread { attempts, .. } => {
                for _ in 0..*attempts {
                    let r = &mut *g.random;
                    let dx = r.next_int_bounded(leaf_radius) - r.next_int_bounded(leaf_radius);
                    let dy =
                        r.next_int_bounded(foliage_height) - r.next_int_bounded(foliage_height);
                    let dz = r.next_int_bounded(leaf_radius) - r.next_int_bounded(leaf_radius);
                    try_place_leaf(g, tree, offset_pos(a.pos, dx, dy, dz));
                }
            }
            Kind::Cherry {
                hanging,
                hanging_extension,
                ..
            } => {
                let pos = up(a.pos, offset);
                let r = leaf_radius + a.radius_offset - 1;
                self.row(g, tree, pos, r - 2, height_with_offset - 3, double);
                self.row(g, tree, pos, r - 1, height_with_offset - 4, double);
                let mut y = height_with_offset - 5;
                while y >= 0 {
                    self.row(g, tree, pos, r, y, double);
                    y -= 1;
                }
                self.row_with_hanging(g, tree, pos, r, -1, double, *hanging, *hanging_extension);
                self.row_with_hanging(
                    g,
                    tree,
                    pos,
                    r - 1,
                    -2,
                    double,
                    *hanging,
                    *hanging_extension,
                );
            }
            Kind::Poplar { .. } => self.poplar(g, tree, a, height_with_offset, leaf_radius, offset),
        }
    }

    fn row(&self, g: &mut Grow, tree: &Tree, origin: BlockPos, radius: i32, y: i32, double: bool) {
        let extra = i32::from(double);
        for dx in -radius..=radius + extra {
            for dz in -radius..=radius + extra {
                if !self.skip_signed(g.random, dx, y, dz, radius, double) {
                    try_place_leaf(g, tree, offset_pos(origin, dx, y, dz));
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn row_with_hanging(
        &self,
        g: &mut Grow,
        tree: &Tree,
        origin: BlockPos,
        radius: i32,
        y: i32,
        double: bool,
        chance: f32,
        extension: f32,
    ) {
        self.row(g, tree, origin, radius, y, double);
        let extra = i32::from(double);
        let log = offset(origin, [0, -1, 0]);
        for along in Facing::HORIZONTAL {
            let to_edge = along.clockwise();
            let positive = matches!(to_edge, Facing::East | Facing::South);
            let to_edge_offset = if positive { radius + extra } else { radius };
            let mut pos = offset(origin, [0, y - 1, 0]);
            pos = relative(pos, to_edge, to_edge_offset);
            pos = relative(pos, along, -radius);
            let mut index = -radius;
            while index < radius + extra {
                let leaves_above = g.is_foliage(step(pos, Facing::Up));
                if leaves_above && try_extension(g, tree, chance, log, pos) {
                    try_extension(g, tree, extension, log, step(pos, Facing::Down));
                }
                index += 1;
                pos = step(pos, along);
            }
        }
    }

    fn skip_signed(
        &self,
        random: &mut impl RandomSource,
        dx: i32,
        y: i32,
        dz: i32,
        radius: i32,
        double: bool,
    ) -> bool {
        if matches!(self.kind, Kind::DarkOak)
            && y == 0
            && double
            && (dx == -radius || dx >= radius)
            && (dz == -radius || dz >= radius)
        {
            return true;
        }
        let (mx, mz) = if double {
            (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
        } else {
            (dx.abs(), dz.abs())
        };
        self.skip(random, mx, y, mz, radius, double)
    }

    fn skip(
        &self,
        random: &mut impl RandomSource,
        dx: i32,
        y: i32,
        dz: i32,
        radius: i32,
        double: bool,
    ) -> bool {
        match &self.kind {
            Kind::Blob(_) => {
                dx == radius && dz == radius && (random.next_int_bounded(2) == 0 || y == 0)
            }
            Kind::Fancy(_) => {
                square(dx as f32 + 0.5) + square(dz as f32 + 0.5) > (radius * radius) as f32
            }
            Kind::Bush(_) => dx == radius && dz == radius && random.next_int_bounded(2) == 0,
            Kind::Spruce(_) | Kind::Pine(_) => dx == radius && dz == radius && radius > 0,
            Kind::MegaPine(_) | Kind::Jungle(_) => {
                dx + dz >= 7 || dx * dx + dz * dz > radius * radius
            }
            Kind::DarkOak => {
                if y != -1 || double {
                    y == 1 && dx + dz > radius * 2 - 2
                } else {
                    dx == radius && dz == radius
                }
            }
            Kind::Acacia => {
                if y == 0 {
                    !((dx <= 1 && dz <= 1) || dx == 0 || dz == 0)
                } else {
                    dx == radius && dz == radius && radius > 0
                }
            }
            Kind::RandomSpread { .. } => false,
            Kind::Cherry {
                wide_bottom_hole,
                corner_hole,
                ..
            } => {
                if y == -1
                    && (dx == radius || dz == radius)
                    && random.next_float() < *wide_bottom_hole
                {
                    return true;
                }
                let corner = dx == radius && dz == radius;
                if radius > 2 {
                    corner || (dx + dz > radius * 2 - 2 && random.next_float() < *corner_hole)
                } else {
                    corner && random.next_float() < *corner_hole
                }
            }
            Kind::Poplar { .. } => unreachable!("poplar rows are placed with their own shape"),
        }
    }

    fn poplar(
        &self,
        g: &mut Grow,
        tree: &Tree,
        a: Attachment,
        height: i32,
        leaf_radius: i32,
        offset: i32,
    ) {
        let Kind::Poplar { side_hole, .. } = &self.kind else {
            unreachable!()
        };
        let crown = PoplarCrown {
            center: up(a.pos, offset),
            span: i32::from(a.double_trunk),
            top: height,
            twisted: g.random.next_bool(),
            hole: *side_hole,
        };
        let full = leaf_radius + a.radius_offset - 1;
        crown.layer(g, tree, full - 2, height - 1);
        crown.layer(g, tree, full - 1, height - 2);
        crown.layer(g, tree, full - 1, height - 3);
        for level in (1..=height - 4).rev() {
            crown.layer(g, tree, full, level);
        }
        crown.branches(g, tree, full, height - 4);
        crown.layer(g, tree, full - 1, 0);
        crown.layer(g, tree, (full - 2).clamp(1, 2), -1);
    }
}

/// One poplar crown: diamond-shaped leaf layers, two opposite quadrants
/// trimmed by one, the top two layers widened but hollow at the rim.
struct PoplarCrown {
    center: BlockPos,
    span: i32,
    top: i32,
    twisted: bool,
    hole: f32,
}

impl PoplarCrown {
    fn budget(&self, x: i32, z: i32, reach: i32, tapered: bool) -> i32 {
        let same_sign = (x > 0) == (z > 0) && x != 0 && z != 0;
        let opposite_sign = (x > 0) != (z > 0) && x != 0 && z != 0;
        let trimmed = if self.twisted {
            same_sign
        } else {
            opposite_sign
        };
        let shave = if trimmed {
            reach - 1
        } else {
            reach + i32::from(tapered)
        };
        2 * reach - shave
    }

    fn layer(&self, g: &mut Grow, tree: &Tree, reach: i32, level: i32) {
        let tapered = level >= self.top - 2 && level < self.top;
        for x in -reach..=reach + self.span {
            for z in -reach..=reach + self.span {
                let (mx, mz) = (x.abs(), z.abs());
                if tapered && (mx == reach || mz == reach) {
                    continue;
                }
                let pierced = g.random.next_float() <= self.hole;
                if mx + mz <= self.budget(x, z, reach, tapered) - i32::from(pierced) {
                    try_place_leaf(g, tree, offset_pos(self.center, x, level, z));
                }
            }
        }
    }

    fn branches(&self, g: &mut Grow, tree: &Tree, reach: i32, level: i32) {
        for x in -reach..=reach + self.span {
            for z in -reach..=reach + self.span {
                let (mx, mz) = (x.abs(), z.abs());
                let axis = match (mx, mz) {
                    (_, 0) if reach - mx >= 4 => Axis::X,
                    (0, _) if reach - mz >= 4 => Axis::Z,
                    _ => continue,
                };
                if mx + mz > self.budget(x, z, reach, false) - 2 {
                    continue;
                }
                let at = offset_pos(self.center, x, level, z);
                let found = g.block(at);
                if found == tree.foliage_provider.state(g.level, g.random, at) {
                    let wood = tree.trunk_provider.state(g.level, g.random, at);
                    g.set_foliage(at, with_axis(wood, axis));
                }
            }
        }
    }
}

fn square(v: f32) -> f32 {
    v * v
}

fn up(pos: BlockPos, n: i32) -> BlockPos {
    BlockPos::new(pos.x, pos.y + n, pos.z)
}

fn offset_pos(pos: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

fn relative(pos: BlockPos, direction: Facing, n: i32) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(pos.x + x * n, pos.y + y * n, pos.z + z * n)
}

fn try_extension(g: &mut Grow, tree: &Tree, chance: f32, log: BlockPos, pos: BlockPos) -> bool {
    let manhattan = (pos.x - log.x).abs() + (pos.y - log.y).abs() + (pos.z - log.z).abs();
    if manhattan >= 7 || g.random.next_float() > chance {
        return false;
    }
    try_place_leaf(g, tree, pos)
}

pub fn try_place_leaf(g: &mut Grow, tree: &Tree, pos: BlockPos) -> bool {
    let existing = g.block(pos);
    if existing.get("persistent") == Some("true") || !valid_tree_pos(existing) {
        return false;
    }
    let mut state: BlockState = tree.foliage_provider.state(g.level, g.random, pos);
    if state.get("waterlogged").is_some() {
        let water = fluid(g.block(pos)) == Fluid::Water;
        state = state
            .with("waterlogged", if water { "true" } else { "false" })
            .unwrap_or(state);
    }
    g.set_foliage(pos, state);
    true
}

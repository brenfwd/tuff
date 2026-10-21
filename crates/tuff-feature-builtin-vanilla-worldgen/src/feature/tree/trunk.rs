//! Trunk placers: the logs of a tree and where its foliage attaches.

use serde_json::Value;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Axis, Facing};

use crate::feature::OwnedIntProvider;
use crate::feature::predicate::{DIRECTIONS, offset, step};
use crate::feature::tree::{
    Attachment, Grow, Tree, is_air_or_leaves, relative, shuffled, valid_tree_pos, with_axis,
};
use crate::json::{self, Result, obj};
use crate::tags::{BlockSet, logs};
use tuff_core::mth;
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;

pub struct TrunkPlacer {
    base_height: i32,
    rand_a: i32,
    rand_b: i32,
    kind: Kind,
}

enum Kind {
    Straight,
    Forking,
    Giant,
    MegaJungle,
    DarkOak,
    Fancy,
    Bending {
        min_height_for_leaves: i32,
        bend_length: OwnedIntProvider,
    },
    Cherry {
        branch_count: OwnedIntProvider,
        horizontal_length: OwnedIntProvider,
        start_offset: (i32, i32),
        end_offset: OwnedIntProvider,
    },
    UpwardsBranching {
        extra_steps: OwnedIntProvider,
        branch_probability: f32,
        extra_length: OwnedIntProvider,
        grow_through: BlockSet,
    },
    Poplar {
        above_branches: OwnedIntProvider,
        branch_amount: OwnedIntProvider,
    },
}

const BASE: [&str; 4] = ["type", "base_height", "height_rand_a", "height_rand_b"];

fn fields(extra: &[&'static str]) -> Vec<&'static str> {
    BASE.iter().chain(extra).copied().collect()
}

impl TrunkPlacer {
    pub fn parse(value: &Value) -> Result<Self> {
        let what = "trunk placer";
        let kind_name = json::qualify(json::kind(value)?);
        let o = |extra: &[&'static str]| obj(value, &fields(extra), what);
        let kind = match kind_name.as_str() {
            "minecraft:straight_trunk_placer" => (o(&[])?, Kind::Straight),
            "minecraft:forking_trunk_placer" => (o(&[])?, Kind::Forking),
            "minecraft:giant_trunk_placer" => (o(&[])?, Kind::Giant),
            "minecraft:mega_jungle_trunk_placer" => (o(&[])?, Kind::MegaJungle),
            "minecraft:dark_oak_trunk_placer" => (o(&[])?, Kind::DarkOak),
            "minecraft:fancy_trunk_placer" => (o(&[])?, Kind::Fancy),
            "minecraft:bending_trunk_placer" => {
                let o = o(&["min_height_for_leaves", "bend_length"])?;
                let kind = Kind::Bending {
                    min_height_for_leaves: o.i32_or("min_height_for_leaves", 1)?,
                    bend_length: OwnedIntProvider::parse(o.req("bend_length")?)?,
                };
                (o, kind)
            }
            "minecraft:cherry_trunk_placer" => {
                let o = o(&[
                    "branch_count",
                    "branch_horizontal_length",
                    "branch_start_offset_from_top",
                    "branch_end_offset_from_top",
                ])?;
                let start = obj(
                    o.req("branch_start_offset_from_top")?,
                    &["min_inclusive", "max_inclusive"],
                    "branch start",
                )?;
                let kind = Kind::Cherry {
                    branch_count: OwnedIntProvider::parse(o.req("branch_count")?)?,
                    horizontal_length: OwnedIntProvider::parse(o.req("branch_horizontal_length")?)?,
                    start_offset: (start.i32("min_inclusive")?, start.i32("max_inclusive")?),
                    end_offset: OwnedIntProvider::parse(o.req("branch_end_offset_from_top")?)?,
                };
                (o, kind)
            }
            "minecraft:upwards_branching_trunk_placer" => {
                let o = o(&[
                    "extra_branch_steps",
                    "place_branch_per_log_probability",
                    "extra_branch_length",
                    "can_grow_through",
                ])?;
                let kind = Kind::UpwardsBranching {
                    extra_steps: OwnedIntProvider::parse(o.req("extra_branch_steps")?)?,
                    branch_probability: o.f32("place_branch_per_log_probability")?,
                    extra_length: OwnedIntProvider::parse(o.req("extra_branch_length")?)?,
                    grow_through: json::block_set(o.req("can_grow_through")?)?,
                };
                (o, kind)
            }
            "minecraft:poplar_trunk_placer" => {
                let o = o(&["trunk_height_above_branches", "branch_amount"])?;
                let kind = Kind::Poplar {
                    above_branches: OwnedIntProvider::parse(o.req("trunk_height_above_branches")?)?,
                    branch_amount: OwnedIntProvider::parse(o.req("branch_amount")?)?,
                };
                (o, kind)
            }
            other => return Err(format!("unknown trunk placer {other}")),
        };
        let (o, kind) = kind;
        Ok(Self {
            base_height: o.i32("base_height")?,
            rand_a: o.i32("height_rand_a")?,
            rand_b: o.i32("height_rand_b")?,
            kind,
        })
    }

    pub fn tree_height(&self, random: &mut impl RandomSource) -> i32 {
        self.base_height
            + random.next_int_bounded(self.rand_a + 1)
            + random.next_int_bounded(self.rand_b + 1)
    }

    fn valid_pos(&self, state: BlockState) -> bool {
        valid_tree_pos(state)
            || matches!(&self.kind, Kind::UpwardsBranching { grow_through, .. } if grow_through.contains_state(state))
    }

    pub fn is_free(&self, state: BlockState) -> bool {
        self.valid_pos(state) || logs().contains_state(state)
    }

    fn place_log_with(
        &self,
        g: &mut Grow,
        tree: &Tree,
        pos: BlockPos,
        modify: impl Fn(BlockState) -> BlockState,
    ) -> bool {
        if !self.valid_pos(g.block(pos)) {
            return false;
        }
        let state = tree.trunk_provider.state(g.level, g.random, pos);
        g.set_trunk(pos, modify(state));
        true
    }

    fn place_log(&self, g: &mut Grow, tree: &Tree, pos: BlockPos) -> bool {
        self.place_log_with(g, tree, pos, |s| s)
    }

    fn place_log_if_free(&self, g: &mut Grow, tree: &Tree, pos: BlockPos) {
        if self.is_free(g.block(pos)) {
            self.place_log(g, tree, pos);
        }
    }

    pub fn place(
        &self,
        g: &mut Grow,
        tree: &Tree,
        height: i32,
        origin: BlockPos,
    ) -> Vec<Attachment> {
        match &self.kind {
            Kind::Straight => {
                tree.place_below_trunk(g, below(origin));
                for y in 0..height {
                    self.place_log(g, tree, above(origin, y));
                }
                vec![Attachment::new(above(origin, height), 0, false)]
            }
            Kind::Forking => self.forking(g, tree, height, origin),
            Kind::Giant => self.giant(g, tree, height, origin),
            Kind::MegaJungle => {
                let mut attachments = self.giant(g, tree, height, origin);
                let mut i = height - 2;
                let mut next = g.random.next_int_bounded(4);
                loop {
                    let branch_height = i - next;
                    if branch_height <= height / 2 {
                        return attachments;
                    }
                    let angle = g.random.next_float() * 6.2831855;
                    let (mut bx, mut bz) = (0, 0);
                    for b in 0..5 {
                        bx = (1.5f32 + mth::cos(angle as f64) * b as f32) as i32;
                        bz = (1.5f32 + mth::sin(angle as f64) * b as f32) as i32;
                        let pos = offset(origin, [bx, branch_height - 3 + b / 2, bz]);
                        self.place_log(g, tree, pos);
                    }
                    attachments.push(Attachment::new(
                        offset(origin, [bx, branch_height, bz]),
                        -2,
                        false,
                    ));
                    i = branch_height;
                    next = 2 + g.random.next_int_bounded(4);
                }
            }
            Kind::DarkOak => self.dark_oak(g, tree, height, origin),
            Kind::Fancy => self.fancy(g, tree, height, origin),
            Kind::Bending {
                min_height_for_leaves,
                bend_length,
            } => {
                let direction = horizontal(g.random);
                let log_height = height - 1;
                let mut pos = origin;
                tree.place_below_trunk(g, below(pos));
                let mut points = Vec::new();
                for i in 0..=log_height {
                    if i + 1 >= log_height + g.random.next_int_bounded(2) {
                        pos = step(pos, direction);
                    }
                    if valid_tree_pos(g.block(pos)) {
                        self.place_log(g, tree, pos);
                    }
                    if i >= *min_height_for_leaves {
                        points.push(Attachment::new(pos, 0, false));
                    }
                    pos = above(pos, 1);
                }
                let length = bend_length.sample(g.random);
                for _ in 0..=length {
                    if valid_tree_pos(g.block(pos)) {
                        self.place_log(g, tree, pos);
                    }
                    points.push(Attachment::new(pos, 0, false));
                    pos = step(pos, direction);
                }
                points
            }
            Kind::Cherry { .. } => self.cherry(g, tree, height, origin),
            Kind::UpwardsBranching {
                extra_steps,
                branch_probability,
                extra_length,
                ..
            } => {
                let mut attachments = Vec::new();
                for h in 0..height {
                    let y = origin.y + h;
                    let pos = BlockPos::new(origin.x, y, origin.z);
                    if self.place_log(g, tree, pos)
                        && h < height - 1
                        && g.random.next_float() < *branch_probability
                    {
                        let direction = horizontal(g.random);
                        let length = extra_length.sample(g.random);
                        let branch_pos = (length - extra_length.sample(g.random) - 1).max(0);
                        let steps = extra_steps.sample(g.random);
                        self.upwards_branch(
                            g,
                            tree,
                            height,
                            &mut attachments,
                            pos,
                            y,
                            direction,
                            branch_pos,
                            steps,
                        );
                    }
                    if h == height - 1 {
                        attachments.push(Attachment::new(
                            BlockPos::new(origin.x, y + 1, origin.z),
                            0,
                            false,
                        ));
                    }
                }
                attachments
            }
            Kind::Poplar {
                above_branches,
                branch_amount,
            } => {
                tree.place_below_trunk(g, below(origin));
                let up_to_branches = height - above_branches.sample(g.random);
                for y in 0..height {
                    self.place_log(g, tree, above(origin, y));
                    let directions: Vec<Facing> = shuffled(&DIRECTIONS, g.random)
                        .into_iter()
                        .filter(|d| d.is_horizontal())
                        .collect();
                    if up_to_branches - 1 == y {
                        let branches = branch_amount.sample(g.random);
                        for direction in directions.iter().take(branches.max(0) as usize) {
                            let axis = direction.axis();
                            self.place_log_with(g, tree, step(above(origin, y), *direction), |s| {
                                with_axis(s, axis)
                            });
                        }
                    }
                }
                vec![Attachment::new(above(origin, up_to_branches), 0, false)]
            }
        }
    }

    fn forking(&self, g: &mut Grow, tree: &Tree, height: i32, origin: BlockPos) -> Vec<Attachment> {
        tree.place_below_trunk(g, below(origin));
        let mut attachments = Vec::new();
        let lean = horizontal(g.random);
        let lean_height = height - g.random.next_int_bounded(4) - 1;
        let mut lean_steps = 3 - g.random.next_int_bounded(3);
        let (mut tx, mut tz) = (origin.x, origin.z);
        let mut top = None;
        for yo in 0..height {
            let yy = origin.y + yo;
            if yo >= lean_height && lean_steps > 0 {
                let (sx, _, sz) = lean.step();
                tx += sx;
                tz += sz;
                lean_steps -= 1;
            }
            if self.place_log(g, tree, BlockPos::new(tx, yy, tz)) {
                top = Some(yy + 1);
            }
        }
        if let Some(y) = top {
            attachments.push(Attachment::new(BlockPos::new(tx, y, tz), 1, false));
        }
        let (mut tx, mut tz) = (origin.x, origin.z);
        let branch = horizontal(g.random);
        if branch != lean {
            let branch_pos = lean_height - g.random.next_int_bounded(2) - 1;
            let mut top = None;
            let mut yo = branch_pos;
            let mut steps = 1 + g.random.next_int_bounded(3);
            while yo < height && steps > 0 {
                if yo >= 1 {
                    let yy = origin.y + yo;
                    let (sx, _, sz) = branch.step();
                    tx += sx;
                    tz += sz;
                    if self.place_log(g, tree, BlockPos::new(tx, yy, tz)) {
                        top = Some(yy + 1);
                    }
                }
                yo += 1;
                steps -= 1;
            }
            if let Some(y) = top {
                attachments.push(Attachment::new(BlockPos::new(tx, y, tz), 0, false));
            }
        }
        attachments
    }

    fn giant(&self, g: &mut Grow, tree: &Tree, height: i32, origin: BlockPos) -> Vec<Attachment> {
        place_below_square(g, tree, origin);
        for hh in 0..height {
            self.place_log_if_free(g, tree, offset(origin, [0, hh, 0]));
            if hh < height - 1 {
                self.place_log_if_free(g, tree, offset(origin, [1, hh, 0]));
                self.place_log_if_free(g, tree, offset(origin, [1, hh, 1]));
                self.place_log_if_free(g, tree, offset(origin, [0, hh, 1]));
            }
        }
        vec![Attachment::new(above(origin, height), 0, true)]
    }

    fn dark_oak(
        &self,
        g: &mut Grow,
        tree: &Tree,
        height: i32,
        origin: BlockPos,
    ) -> Vec<Attachment> {
        let mut attachments = Vec::new();
        place_below_square(g, tree, origin);
        let lean = horizontal(g.random);
        let lean_height = height - g.random.next_int_bounded(4);
        let mut lean_steps = 2 - g.random.next_int_bounded(3);
        let (x, y, z) = (origin.x, origin.y, origin.z);
        let (mut tx, mut tz) = (x, z);
        let top = y + height - 1;
        for dy in 0..height {
            if dy >= lean_height && lean_steps > 0 {
                let (sx, _, sz) = lean.step();
                tx += sx;
                tz += sz;
                lean_steps -= 1;
            }
            let pos = BlockPos::new(tx, y + dy, tz);
            if is_air_or_leaves(g.block(pos)) {
                self.place_log(g, tree, pos);
                self.place_log(g, tree, offset(pos, [1, 0, 0]));
                self.place_log(g, tree, offset(pos, [0, 0, 1]));
                self.place_log(g, tree, offset(pos, [1, 0, 1]));
            }
        }
        attachments.push(Attachment::new(BlockPos::new(tx, top, tz), 0, true));
        for ox in -1..=2 {
            for oz in -1..=2 {
                if (!(0..=1).contains(&ox) || !(0..=1).contains(&oz))
                    && g.random.next_int_bounded(3) <= 0
                {
                    let length = g.random.next_int_bounded(3) + 2;
                    for branch_y in 0..length {
                        self.place_log(g, tree, BlockPos::new(x + ox, top - branch_y - 1, z + oz));
                    }
                    attachments.push(Attachment::new(
                        BlockPos::new(x + ox, top, z + oz),
                        0,
                        false,
                    ));
                }
            }
        }
        attachments
    }

    fn fancy(
        &self,
        g: &mut Grow,
        tree: &Tree,
        tree_height: i32,
        origin: BlockPos,
    ) -> Vec<Attachment> {
        let height = tree_height + 2;
        let trunk_height = (height as f64 * 0.618).floor() as i32;
        tree.place_below_trunk(g, below(origin));
        let clusters = 1.min((1.382 + (1.0 * height as f64 / 13.0).powi(2)).floor() as i32);
        let trunk_top = origin.y + trunk_height;
        let mut relative_y = height - 5;
        let mut coords = vec![(above(origin, relative_y), trunk_top)];
        while relative_y >= 0 {
            let shape = fancy_shape(height, relative_y);
            if shape >= 0.0 {
                for _ in 0..clusters {
                    let radius = 1.0 * shape as f64 * (g.random.next_float() as f64 + 0.328);
                    let angle = (g.random.next_float() * 2.0) as f64 * std::f64::consts::PI;
                    let x = radius * angle.sin() + 0.5;
                    let z = radius * angle.cos() + 0.5;
                    let start =
                        offset(origin, [x.floor() as i32, relative_y - 1, z.floor() as i32]);
                    let end = above(start, 5);
                    if self.limb(g, tree, start, end, false) {
                        let dx = origin.x - start.x;
                        let dz = origin.z - start.z;
                        let branch_height =
                            start.y as f64 - ((dx * dx + dz * dz) as f64).sqrt() * 0.381;
                        let branch_top = if branch_height > trunk_top as f64 {
                            trunk_top
                        } else {
                            branch_height as i32
                        };
                        let base = BlockPos::new(origin.x, branch_top, origin.z);
                        if self.limb(g, tree, base, start, false) {
                            coords.push((start, base.y));
                        }
                    }
                }
            }
            relative_y -= 1;
        }
        self.limb(g, tree, origin, above(origin, trunk_height), true);
        let trim = |branch_base: i32| (branch_base - origin.y) as f64 >= height as f64 * 0.2;
        for &(end, branch_base) in &coords {
            let base = BlockPos::new(origin.x, branch_base, origin.z);
            if base != end && trim(branch_base) {
                self.limb(g, tree, base, end, true);
            }
        }
        coords
            .into_iter()
            .filter(|(_, branch_base)| trim(*branch_base))
            .map(|(pos, _)| Attachment::new(pos, 0, false))
            .collect()
    }

    fn limb(&self, g: &mut Grow, tree: &Tree, start: BlockPos, end: BlockPos, place: bool) -> bool {
        if !place && start == end {
            return true;
        }
        let delta = [end.x - start.x, end.y - start.y, end.z - start.z];
        let steps = delta[0].abs().max(delta[1].abs().max(delta[2].abs()));
        let d = delta.map(|v| v as f32 / steps as f32);
        for i in 0..=steps {
            let pos = offset(start, d.map(|v| (0.5f32 + i as f32 * v).floor() as i32));
            if place {
                let axis = log_axis(start, pos);
                self.place_log_with(g, tree, pos, |s| with_axis(s, axis));
            } else if !self.is_free(g.block(pos)) {
                return false;
            }
        }
        true
    }

    fn cherry(&self, g: &mut Grow, tree: &Tree, height: i32, origin: BlockPos) -> Vec<Attachment> {
        let Kind::Cherry {
            branch_count,
            horizontal_length,
            start_offset,
            end_offset,
        } = &self.kind
        else {
            unreachable!()
        };
        tree.place_below_trunk(g, below(origin));
        let first = (height - 1
            + mth::random_between_inclusive(g.random, start_offset.0, start_offset.1))
        .max(0);
        let mut second = (height - 1
            + mth::random_between_inclusive(g.random, start_offset.0, start_offset.1 - 1))
        .max(0);
        if second >= first {
            second += 1;
        }
        let count = branch_count.sample(g.random);
        let middle = count == 3;
        let both = count >= 2;
        let trunk_height = if middle {
            height
        } else if both {
            first.max(second) + 1
        } else {
            first + 1
        };
        for y in 0..trunk_height {
            self.place_log(g, tree, above(origin, y));
        }
        let mut attachments = Vec::new();
        if middle {
            attachments.push(Attachment::new(above(origin, trunk_height), 0, false));
        }
        let direction = horizontal(g.random);
        let axis = direction.axis();
        let branch = |g: &mut Grow, direction: Facing, from_origin: i32, continues: bool| {
            let mut pos = above(origin, from_origin);
            let end_from_origin = height - 1 + end_offset.sample(g.random);
            let away = continues || end_from_origin < from_origin;
            let distance = horizontal_length.sample(g.random) + i32::from(away);
            let end = above(relative(origin, direction, distance), end_from_origin);
            for _ in 0..(if away { 2 } else { 1 }) {
                pos = step(pos, direction);
                self.place_log_with(g, tree, pos, |s| with_axis(s, axis));
            }
            let vertical = if end.y > pos.y {
                Facing::Up
            } else {
                Facing::Down
            };
            loop {
                let distance =
                    (pos.x - end.x).abs() + (pos.y - end.y).abs() + (pos.z - end.z).abs();
                if distance == 0 {
                    return Attachment::new(above(end, 1), 0, false);
                }
                let chance = (end.y - pos.y).abs() as f32 / distance as f32;
                let grow_vertically = g.random.next_float() < chance;
                pos = step(pos, if grow_vertically { vertical } else { direction });
                if grow_vertically {
                    self.place_log(g, tree, pos);
                } else {
                    self.place_log_with(g, tree, pos, |s| with_axis(s, axis));
                }
            }
        };
        attachments.push(branch(g, direction, first, first < trunk_height - 1));
        if both {
            attachments.push(branch(
                g,
                direction.opposite(),
                second,
                second < trunk_height - 1,
            ));
        }
        attachments
    }

    #[allow(clippy::too_many_arguments)]
    fn upwards_branch(
        &self,
        g: &mut Grow,
        tree: &Tree,
        height: i32,
        attachments: &mut Vec<Attachment>,
        log: BlockPos,
        current: i32,
        direction: Facing,
        branch_pos: i32,
        mut steps: i32,
    ) {
        let mut along = current + branch_pos;
        let (mut x, mut z) = (log.x, log.z);
        let mut index = branch_pos;
        while index < height && steps > 0 {
            if index >= 1 {
                let y = current + index;
                let (sx, _, sz) = direction.step();
                x += sx;
                z += sz;
                along = y;
                let pos = BlockPos::new(x, y, z);
                if self.place_log(g, tree, pos) {
                    along += 1;
                }
                attachments.push(Attachment::new(pos, 0, false));
            }
            index += 1;
            steps -= 1;
        }
        if along - current > 1 {
            let pos = BlockPos::new(x, along, z);
            attachments.push(Attachment::new(pos, 0, false));
            attachments.push(Attachment::new(below_n(pos, 2), 0, false));
        }
    }
}

fn place_below_square(g: &mut Grow, tree: &Tree, origin: BlockPos) {
    let below = below(origin);
    tree.place_below_trunk(g, below);
    tree.place_below_trunk(g, offset(below, [1, 0, 0]));
    tree.place_below_trunk(g, offset(below, [0, 0, 1]));
    tree.place_below_trunk(g, offset(below, [1, 0, 1]));
}

fn fancy_shape(height: i32, y: i32) -> f32 {
    let (h, level) = (height as f32, y as f32);
    if level < h * 0.3 {
        return -1.0;
    }
    let half = h / 2.0;
    match (half - level).abs() {
        0.0 => half * 0.5,
        off if off >= half => 0.0,
        off => (half * half - off * off).sqrt() * 0.5,
    }
}

fn log_axis(start: BlockPos, pos: BlockPos) -> Axis {
    let dx = (pos.x - start.x).abs();
    let dz = (pos.z - start.z).abs();
    let max = dx.max(dz);
    if max == 0 {
        Axis::Y
    } else if dx == max {
        Axis::X
    } else {
        Axis::Z
    }
}

pub fn horizontal(random: &mut impl RandomSource) -> Facing {
    Facing::HORIZONTAL[random.next_int_bounded(4) as usize]
}

fn above(pos: BlockPos, n: i32) -> BlockPos {
    BlockPos::new(pos.x, pos.y + n, pos.z)
}

fn below(pos: BlockPos) -> BlockPos {
    below_n(pos, 1)
}

fn below_n(pos: BlockPos, n: i32) -> BlockPos {
    BlockPos::new(pos.x, pos.y - n, pos.z)
}

#[cfg(test)]
mod shape_tests {
    use super::fancy_shape;

    #[tuff_core::watchdog]
    #[test]
    fn crown_radius_profile_is_pinned() {
        let mut acc: u64 = 0;
        for height in 0..40 {
            for y in -2..44 {
                let bits = fancy_shape(height, y).to_bits() as u64;
                acc = acc.wrapping_mul(1_000_003).wrapping_add(bits);
            }
        }
        assert_eq!(acc, 14_293_483_386_655_086_036);
    }
}

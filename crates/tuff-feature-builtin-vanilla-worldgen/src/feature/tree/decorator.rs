//! Tree decorators: vines, cocoa, beehives, propagules, ground cover and
//! the other extras placed once a tree stands.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::predicate::{DIRECTIONS, parse_direction, step};
use crate::feature::provider::StateProvider;
use crate::feature::tree::trunk::horizontal;
use crate::feature::tree::{Grow, shuffled};
use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::tags::logs;
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub enum TreeDecorator {
    AlterGround(Arc<StateProvider>),
    AttachedToLeaves {
        probability: f32,
        exclusion_xz: i32,
        exclusion_y: i32,
        provider: Arc<StateProvider>,
        required_empty: i32,
        directions: Vec<Facing>,
    },
    AttachedToLogs {
        probability: f32,
        provider: Arc<StateProvider>,
        directions: Vec<Facing>,
    },
    Beehive(f32),
    Cocoa(f32),
    CreakingHeart(f32),
    LeaveVine(f32),
    TrunkVine,
    PaleMoss {
        leaves: f32,
        trunk: f32,
        ground: f32,
        patch: Arc<dyn Feature>,
    },
    PlaceOnGround {
        tries: i32,
        radius: i32,
        height: i32,
        provider: Arc<StateProvider>,
    },
    ShelfMushroom(f32),
}

/// The tree's blocks as decorators see them: sorted by height, and within a
/// height in the order the game's sets iterate.
pub struct Parts {
    pub logs: Vec<BlockPos>,
    pub leaves: Vec<BlockPos>,
    pub roots: Vec<BlockPos>,
}

fn directions(value: &Value) -> Result<Vec<Facing>> {
    let list = value.as_array().ok_or("directions are a list")?;
    if list.is_empty() {
        return Err("directions must not be empty".into());
    }
    list.iter()
        .map(|d| parse_direction(d.as_str().ok_or("direction is a string")?))
        .collect()
}

impl TreeDecorator {
    pub fn parse(registry: &mut Registry, value: &Value) -> Result<Self> {
        let what = "tree decorator";
        let name = json::qualify(json::kind(value)?);
        let probability = |value: &Value| -> Result<f32> {
            obj(value, &["type", "probability"], what)?.f32("probability")
        };
        Ok(match name.as_str() {
            "minecraft:alter_ground" => {
                let o = obj(value, &["type", "provider"], what)?;
                Self::AlterGround(StateProvider::parse(registry, o.req("provider")?)?)
            }
            "minecraft:attached_to_leaves" => {
                let o = obj(
                    value,
                    &[
                        "type",
                        "probability",
                        "exclusion_radius_xz",
                        "exclusion_radius_y",
                        "block_provider",
                        "required_empty_blocks",
                        "directions",
                    ],
                    what,
                )?;
                Self::AttachedToLeaves {
                    probability: o.f32("probability")?,
                    exclusion_xz: o.i32("exclusion_radius_xz")?,
                    exclusion_y: o.i32("exclusion_radius_y")?,
                    provider: StateProvider::parse(registry, o.req("block_provider")?)?,
                    required_empty: o.i32("required_empty_blocks")?,
                    directions: directions(o.req("directions")?)?,
                }
            }
            "minecraft:attached_to_logs" => {
                let o = obj(
                    value,
                    &["type", "probability", "block_provider", "directions"],
                    what,
                )?;
                Self::AttachedToLogs {
                    probability: o.f32("probability")?,
                    provider: StateProvider::parse(registry, o.req("block_provider")?)?,
                    directions: directions(o.req("directions")?)?,
                }
            }
            "minecraft:beehive" => Self::Beehive(probability(value)?),
            "minecraft:cocoa" => Self::Cocoa(probability(value)?),
            "minecraft:creaking_heart" => Self::CreakingHeart(probability(value)?),
            "minecraft:leave_vine" => Self::LeaveVine(probability(value)?),
            "minecraft:shelf_mushroom" => Self::ShelfMushroom(probability(value)?),
            "minecraft:trunk_vine" => {
                obj(value, &["type"], what)?;
                Self::TrunkVine
            }
            "minecraft:pale_moss" => {
                let o = obj(
                    value,
                    &[
                        "type",
                        "leaves_probability",
                        "trunk_probability",
                        "ground_probability",
                    ],
                    what,
                )?;
                Self::PaleMoss {
                    leaves: o.f32("leaves_probability")?,
                    trunk: o.f32("trunk_probability")?,
                    ground: o.f32("ground_probability")?,
                    patch: registry.feature(&Value::String("minecraft:pale_moss_patch".into()))?,
                }
            }
            "minecraft:place_on_ground" => {
                let o = obj(
                    value,
                    &["type", "tries", "radius", "height", "block_state_provider"],
                    what,
                )?;
                Self::PlaceOnGround {
                    tries: o.i32_or("tries", 128)?,
                    radius: o.i32_or("radius", 2)?,
                    height: o.i32_or("height", 1)?,
                    provider: StateProvider::parse(registry, o.req("block_state_provider")?)?,
                }
            }
            other => return Err(format!("unknown tree decorator {other}")),
        })
    }

    pub fn place(&self, g: &mut Grow, parts: &Parts) {
        match self {
            Self::AlterGround(provider) => {
                let positions = lowest_trunk_or_root(parts);
                let Some(first) = positions.first() else {
                    return;
                };
                let min_y = first.y;
                for pos in positions.iter().filter(|p| p.y == min_y) {
                    let circle = |g: &mut Grow, center: BlockPos| {
                        for xx in -2i32..=2 {
                            for zz in -2i32..=2 {
                                if xx.abs() != 2 || zz.abs() != 2 {
                                    alter_ground_at(g, provider, at(center, xx, 0, zz));
                                }
                            }
                        }
                    };
                    circle(g, at(*pos, -1, 0, -1));
                    circle(g, at(*pos, 2, 0, -1));
                    circle(g, at(*pos, -1, 0, 2));
                    circle(g, at(*pos, 2, 0, 2));
                    for _ in 0..5 {
                        let placement = g.random.next_int_bounded(64);
                        let (xx, zz) = (placement % 8, placement / 8);
                        if xx == 0 || xx == 7 || zz == 0 || zz == 7 {
                            circle(g, at(*pos, -3 + xx, 0, -3 + zz));
                        }
                    }
                }
            }
            Self::AttachedToLeaves {
                probability,
                exclusion_xz,
                exclusion_y,
                provider,
                required_empty,
                directions,
            } => {
                let mut blacklist = HashSet::new();
                for leaf in shuffled(&parts.leaves, g.random) {
                    let direction =
                        directions[g.random.next_int_bounded(directions.len() as i32) as usize];
                    let placement = step(leaf, direction);
                    if !blacklist.contains(&placement)
                        && g.random.next_float() < *probability
                        && (1..=*required_empty)
                            .all(|i| is_air(g.block(relative(leaf, direction, i))))
                    {
                        for x in -exclusion_xz..=*exclusion_xz {
                            for y in -exclusion_y..=*exclusion_y {
                                for z in -exclusion_xz..=*exclusion_xz {
                                    blacklist.insert(at(placement, x, y, z));
                                }
                            }
                        }
                        let state = provider.state(g.level, g.random, placement);
                        g.set_decoration(placement, state);
                    }
                }
            }
            Self::AttachedToLogs {
                probability,
                provider,
                directions,
            } => {
                for log in shuffled(&parts.logs, g.random) {
                    let direction =
                        directions[g.random.next_int_bounded(directions.len() as i32) as usize];
                    let placement = step(log, direction);
                    if g.random.next_float() <= *probability && is_air(g.block(placement)) {
                        let state = provider.state(g.level, g.random, placement);
                        g.set_decoration(placement, state);
                    }
                }
            }
            Self::Beehive(probability) => beehive(g, parts, *probability),
            Self::Cocoa(probability) => {
                if g.random.next_float() >= *probability {
                    return;
                }
                let Some(first) = parts.logs.first() else {
                    return;
                };
                let tree_y = first.y;
                for log in parts.logs.iter().filter(|p| p.y - tree_y <= 2) {
                    for direction in Facing::HORIZONTAL {
                        if g.random.next_float() <= 0.25 {
                            let pos = step(*log, direction.opposite());
                            if is_air(g.block(pos)) {
                                let age = g.random.next_int_bounded(3).to_string();
                                let state = Block::Cocoa
                                    .default_state()
                                    .with("age", &age)
                                    .and_then(|s| s.with("facing", direction.name()))
                                    .expect("cocoa has an age and a facing");
                                g.set_decoration(pos, state);
                            }
                        }
                    }
                }
            }
            Self::CreakingHeart(probability) => {
                if parts.logs.is_empty() || g.random.next_float() >= *probability {
                    return;
                }
                let candidates = shuffled(&parts.logs, g.random);
                let target = candidates.into_iter().find(|pos| {
                    DIRECTIONS
                        .iter()
                        .all(|d| logs().contains_state(g.block(step(*pos, *d))))
                });
                if let Some(pos) = target {
                    let state = Block::CreakingHeart
                        .default_state()
                        .with("creaking_heart_state", "dormant")
                        .and_then(|s| s.with("natural", "true"))
                        .expect("creaking heart properties");
                    g.set_decoration(pos, state);
                }
            }
            Self::LeaveVine(probability) => {
                for leaf in &parts.leaves {
                    for (direction, face) in VINE_SIDES {
                        if g.random.next_float() < *probability {
                            let pos = step(*leaf, direction);
                            if is_air(g.block(pos)) {
                                hanging_vine(g, pos, face);
                            }
                        }
                    }
                }
            }
            Self::TrunkVine => {
                for log in &parts.logs {
                    for (direction, face) in VINE_SIDES {
                        if g.random.next_int_bounded(3) > 0 {
                            let pos = step(*log, direction);
                            if is_air(g.block(pos)) {
                                g.set_decoration(pos, vine(face));
                            }
                        }
                    }
                }
            }
            Self::PaleMoss {
                leaves,
                trunk,
                ground,
                patch,
            } => {
                let logs = shuffled(&parts.logs, g.random);
                let Some(origin) = logs
                    .iter()
                    .copied()
                    .reduce(|a, b| if b.y < a.y { b } else { a })
                else {
                    return;
                };
                if g.random.next_float() < *ground {
                    let mut ctx = PlaceContext {
                        level: &mut *g.level,
                        random: &mut *g.random,
                    };
                    patch.place(&mut ctx, at(origin, 0, 1, 0));
                }
                for log in &parts.logs {
                    if g.random.next_float() < *trunk {
                        let down = at(*log, 0, -1, 0);
                        if is_air(g.block(down)) {
                            moss_hanger(g, down);
                        }
                    }
                }
                for leaf in &parts.leaves {
                    if g.random.next_float() < *leaves {
                        let down = at(*leaf, 0, -1, 0);
                        if is_air(g.block(down)) {
                            moss_hanger(g, down);
                        }
                    }
                }
            }
            Self::PlaceOnGround {
                tries,
                radius,
                height,
                provider,
            } => {
                let positions = lowest_trunk_or_root(parts);
                let Some(&first) = positions.first() else {
                    return;
                };
                let min_y = first.y;
                let (mut min_x, mut max_x, mut min_z, mut max_z) =
                    (first.x, first.x, first.z, first.z);
                for p in positions.iter().filter(|p| p.y == min_y) {
                    min_x = min_x.min(p.x);
                    max_x = max_x.max(p.x);
                    min_z = min_z.min(p.z);
                    max_z = max_z.max(p.z);
                }
                let (x0, x1) = (min_x - radius, max_x + radius);
                let (y0, y1) = (min_y - height, min_y + height);
                let (z0, z1) = (min_z - radius, max_z + radius);
                for _ in 0..*tries {
                    let x = g.random.next_int_between_inclusive(x0, x1);
                    let y = g.random.next_int_between_inclusive(y0, y1);
                    let z = g.random.next_int_between_inclusive(z0, z1);
                    let pos = BlockPos::new(x, y, z);
                    let above = at(pos, 0, 1, 0);
                    let above_state = g.block(above);
                    if (is_air(above_state) || above_state.block() == Block::Vine)
                        && g.block(pos).is_solid_render()
                        && g.level
                            .height(HeightmapKind::MotionBlockingNoLeaves, pos.x, pos.z)
                            <= above.y
                    {
                        let state = provider.state(g.level, g.random, above);
                        g.set_decoration(above, state);
                    }
                }
            }
            Self::ShelfMushroom(probability) => shelf_mushroom(g, parts, *probability),
        }
    }
}

const VINE_SIDES: [(Facing, &str); 4] = [
    (Facing::West, "east"),
    (Facing::East, "west"),
    (Facing::North, "south"),
    (Facing::South, "north"),
];

fn vine(face: &str) -> BlockState {
    Block::Vine
        .default_state()
        .with(face, "true")
        .expect("vines have side faces")
}

fn hanging_vine(g: &mut Grow, pos: BlockPos, face: &str) {
    g.set_decoration(pos, vine(face));
    let mut pos = at(pos, 0, -1, 0);
    let mut left = 4;
    while is_air(g.block(pos)) && left > 0 {
        g.set_decoration(pos, vine(face));
        pos = at(pos, 0, -1, 0);
        left -= 1;
    }
}

fn moss_hanger(g: &mut Grow, mut pos: BlockPos) {
    let moss = |tip: bool| {
        Block::PaleHangingMoss
            .default_state()
            .with("tip", if tip { "true" } else { "false" })
            .expect("hanging moss has a tip")
    };
    while is_air(g.block(at(pos, 0, -1, 0))) && g.random.next_float() as f64 >= 0.5 {
        g.set_decoration(pos, moss(false));
        pos = at(pos, 0, -1, 0);
    }
    g.set_decoration(pos, moss(true));
}

fn alter_ground_at(g: &mut Grow, provider: &StateProvider, pos: BlockPos) {
    for dy in (-3..=2).rev() {
        let cursor = at(pos, 0, dy, 0);
        if let Some(state) = provider.optional_state(g.level, g.random, cursor) {
            g.set_decoration(cursor, state);
            return;
        }
        if !is_air(g.block(cursor)) && dy < 0 {
            return;
        }
    }
}

fn beehive(g: &mut Grow, parts: &Parts, probability: f32) {
    let (Some(first_log), Some(last_log)) = (parts.logs.first(), parts.logs.last()) else {
        return;
    };
    if g.random.next_float() >= probability {
        return;
    }
    let hive_y = match parts.leaves.first() {
        Some(leaf) => (leaf.y - 1).max(first_log.y + 1),
        None => (first_log.y + 1 + g.random.next_int_bounded(3)).min(last_log.y),
    };
    let mut placements: Vec<BlockPos> = parts
        .logs
        .iter()
        .filter(|p| p.y == hive_y)
        .flat_map(|p| [Facing::East, Facing::South, Facing::West].map(|d| step(*p, d)))
        .collect();
    if placements.is_empty() {
        return;
    }
    shuffle(&mut placements, g.random);
    let Some(pos) = placements
        .into_iter()
        .find(|p| is_air(g.block(*p)) && is_air(g.block(step(*p, Facing::South))))
    else {
        return;
    };
    let nest = Block::BeeNest
        .default_state()
        .with("facing", "south")
        .expect("bee nests face");
    g.set_decoration(pos, nest);
    if !g.level.is_within_write_zone(pos) {
        return;
    }
    let count = 2 + g.random.next_int_bounded(2);
    let mut bees = Vec::new();
    for _ in 0..count {
        let ticks = g.random.next_int_bounded(599);
        bees.push(Tag::Compound(
            Compound::new()
                .with("entity_data", Compound::new().with("id", "minecraft:bee"))
                .with("ticks_in_hive", ticks)
                .with("min_ticks_in_hive", 600),
        ));
    }
    g.level.set_block_entity(
        pos,
        Compound::new()
            .with("id", "minecraft:beehive")
            .with("bees", Tag::List(bees)),
    );
}

fn shelf_mushroom(g: &mut Grow, parts: &Parts, probability: f32) {
    if g.random.next_float() >= probability {
        return;
    }
    let (Some(first), Some(last)) = (parts.logs.first(), parts.logs.last()) else {
        return;
    };
    let is_shelf = |g: &Grow, pos: BlockPos| g.block(pos).block() == Block::ShelfMushroom;
    let replaceable = |g: &Grow, pos: BlockPos| {
        let water = |p: BlockPos| g.block(p).block() == Block::Water;
        g.block(pos).is_replaceable()
            && !(water(pos)
                || water(step(pos, Facing::East))
                || water(step(pos, Facing::West))
                || water(step(pos, Facing::North))
                || water(step(pos, Facing::South)))
    };
    let adjacent_shelf = |g: &Grow, pos: BlockPos| {
        Facing::HORIZONTAL
            .iter()
            .any(|d| is_shelf(g, step(pos, *d)))
    };
    let place = |g: &mut Grow, pos: BlockPos, facing: Facing| {
        let age = g.random.next_int_bounded(2).to_string();
        let state = Block::ShelfMushroom
            .default_state()
            .with("age", &age)
            .and_then(|s| s.with("facing", facing.name()))
            .expect("shelf mushroom properties");
        g.set_decoration(pos, state);
    };
    let mushroom_pos = |log: BlockPos, facing: Facing| step(log, facing);
    if first.y == last.y {
        let directions = if first.x != last.x {
            [Facing::North, Facing::South]
        } else {
            [Facing::East, Facing::West]
        };
        for log in &parts.logs {
            for facing in directions {
                if g.random.next_float() <= 0.25 {
                    let pos = mushroom_pos(*log, facing);
                    if replaceable(g, pos) && !adjacent_shelf(g, pos) && !adjacent_shelf(g, *log) {
                        place(g, pos, facing);
                    }
                }
            }
        }
    } else {
        let first_dir = horizontal(g.random);
        let directions = [first_dir, first_dir.clockwise()];
        let base = first.y;
        for log in &parts.logs {
            let dy = log.y - base;
            if !(1..=4).contains(&dy) {
                continue;
            }
            for facing in directions {
                if g.random.next_float() <= 0.25 {
                    let pos = mushroom_pos(*log, facing);
                    if replaceable(g, pos) && !is_shelf(g, at(pos, 0, -1, 0)) {
                        place(g, pos, facing);
                        break;
                    }
                }
            }
        }
    }
}

fn lowest_trunk_or_root(parts: &Parts) -> Vec<BlockPos> {
    if parts.roots.is_empty() {
        return parts.logs.clone();
    }
    if !parts.logs.is_empty() && parts.roots[0].y == parts.logs[0].y {
        let mut all = parts.logs.clone();
        all.extend_from_slice(&parts.roots);
        return all;
    }
    parts.roots.clone()
}

pub fn shuffle<T>(list: &mut [T], random: &mut impl RandomSource) {
    for i in (2..=list.len()).rev() {
        let swap = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap);
    }
}

fn relative(pos: BlockPos, direction: Facing, n: i32) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(pos.x + x * n, pos.y + y * n, pos.z + z * n)
}

fn at(pos: BlockPos, x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

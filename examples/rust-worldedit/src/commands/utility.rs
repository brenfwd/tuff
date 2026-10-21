// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::edit::{Clip, Op, Registry, is_air};
use crate::jobs::{busy, edit, start};
use crate::mask;
use crate::pattern::{self, Env, join_state, split_state};
use crate::plan::{Cells, Item};
use crate::region::{P, Region, WORLD_Y};
use crate::selection::limit_of;
use crate::session::{here, model, say};
use std::collections::{HashMap, HashSet, VecDeque};

pub const COMMANDS: &[Command] = &[
    Command {
        name: "fill",
        aliases: &[],
        usage: "<pattern> <radius> [depth]",
        summary: "Fill a hole starting at your position",
        node: "worldedit.fill",
        flags: "",
        run: fill,
    },
    Command {
        name: "fillr",
        aliases: &[],
        usage: "<pattern> <radius> [depth]",
        summary: "Fill a hole recursively, spreading at every level",
        node: "worldedit.fill.recursive",
        flags: "",
        run: fillr,
    },
    Command {
        name: "drain",
        aliases: &[],
        usage: "[-w] <radius>",
        summary: "Drain the water and lava around you",
        node: "worldedit.drain",
        flags: "w",
        run: drain,
    },
    Command {
        name: "fixwater",
        aliases: &[],
        usage: "<radius>",
        summary: "Level nearby water",
        node: "worldedit.fixwater",
        flags: "",
        run: fixwater,
    },
    Command {
        name: "fixlava",
        aliases: &[],
        usage: "<radius>",
        summary: "Level nearby lava",
        node: "worldedit.fixlava",
        flags: "",
        run: fixlava,
    },
    Command {
        name: "removeabove",
        aliases: &[],
        usage: "[size] [height]",
        summary: "Remove the blocks above you",
        node: "worldedit.removeabove",
        flags: "",
        run: removeabove,
    },
    Command {
        name: "removebelow",
        aliases: &[],
        usage: "[size] [height]",
        summary: "Remove the blocks below you",
        node: "worldedit.removebelow",
        flags: "",
        run: removebelow,
    },
    Command {
        name: "removenear",
        aliases: &[],
        usage: "<mask> [radius]",
        summary: "Remove nearby blocks matching a mask",
        node: "worldedit.removenear",
        flags: "",
        run: removenear,
    },
    Command {
        name: "replacenear",
        aliases: &["rnear"],
        usage: "<radius> <from-mask> <to-pattern>",
        summary: "Replace nearby blocks",
        node: "worldedit.replacenear",
        flags: "",
        run: replacenear,
    },
    Command {
        name: "snow",
        aliases: &[],
        usage: "[radius]",
        summary: "Simulate snowfall around you",
        node: "worldedit.snow",
        flags: "",
        run: snow,
    },
    Command {
        name: "thaw",
        aliases: &[],
        usage: "[radius]",
        summary: "Melt snow and ice around you",
        node: "worldedit.thaw",
        flags: "",
        run: thaw,
    },
    Command {
        name: "green",
        aliases: &[],
        usage: "[-f] [radius]",
        summary: "Turn exposed dirt around you into grass",
        node: "worldedit.green",
        flags: "f",
        run: green,
    },
    Command {
        name: "extinguish",
        aliases: &["ex", "ext"],
        usage: "[radius]",
        summary: "Put out fires around you",
        node: "worldedit.extinguish",
        flags: "",
        run: extinguish,
    },
];

const MAX_RADIUS: i32 = 256;
const DEFAULT_RADIUS: i32 = 10;
const FIRE_RADIUS: i32 = 40;
const NEAR_RADIUS: i32 = 50;
const WINDOW: [i32; 2] = [32, 64];
const WATER: &str = "minecraft:water[level=0]";
const LAVA: &str = "minecraft:lava[level=0]";
const AIR: &str = "minecraft:air";

fn name(state: &str) -> &str {
    split_state(state).0
}

fn has(state: &str, prop: &str, value: &str) -> bool {
    split_state(state).1.contains(&(prop, value))
}

fn with_prop(state: &str, prop: &str, value: &str) -> String {
    let (n, props) = split_state(state);
    let mut out: Vec<(String, String)> = props
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    match out.iter_mut().find(|(k, _)| k == prop) {
        Some(p) => p.1 = value.to_owned(),
        None => out.push((prop.to_owned(), value.to_owned())),
    }
    join_state(n, &out)
}

fn water_plant(state: &str) -> bool {
    matches!(
        name(state),
        "minecraft:seagrass"
            | "minecraft:tall_seagrass"
            | "minecraft:kelp"
            | "minecraft:kelp_plant"
            | "minecraft:bubble_column"
    )
}

fn is_water(state: &str) -> bool {
    name(state) == "minecraft:water" || water_plant(state) || has(state, "waterlogged", "true")
}

fn is_lava(state: &str) -> bool {
    name(state) == "minecraft:lava"
}

fn is_fluid(state: &str) -> bool {
    is_water(state) || is_lava(state)
}

fn is_source(state: &str) -> bool {
    matches!(name(state), "minecraft:water" | "minecraft:lava") && has(state, "level", "0")
}

fn is_fire(state: &str) -> bool {
    matches!(name(state), "minecraft:fire" | "minecraft:soul_fire")
}

fn in_sphere(c: P, p: P, r: i32) -> bool {
    let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= r * r
}

fn in_circle(c: P, p: P, r: i32) -> bool {
    let d = [p[0] - c[0], p[2] - c[2]];
    d[0] * d[0] + d[1] * d[1] <= r * r
}

fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

const DIRS: [P; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

struct Put {
    cells: Cells,
    known: HashMap<String, u32>,
}

impl Put {
    fn new() -> Put {
        Put {
            cells: Cells::default(),
            known: HashMap::new(),
        }
    }

    fn set(&mut self, pos: P, state: &str) {
        let id = match self.known.get(state) {
            Some(id) => *id,
            None => {
                let id = self.cells.state(state);
                self.known.insert(state.to_owned(), id);
                id
            }
        };
        self.cells.set(pos, id);
    }
}

pub fn fill_plan(
    clip: &Clip,
    origin: P,
    radius: i32,
    depth: i32,
    recursive: bool,
    item: Item,
) -> Cells {
    let mut cells = Cells::default();
    let id = cells.item(item);
    let floor = origin[1] - depth + 1;
    let open = |p: P| p[1] >= floor && clip.at(p).is_some_and(is_air);
    let mut seen: HashSet<P> = HashSet::new();
    let mut queue = VecDeque::new();
    if open(origin) {
        seen.insert(origin);
        queue.push_back(origin);
    }
    while let Some(p) = queue.pop_front() {
        cells.set(p, id);
        for d in DIRS {
            let q = add(p, d);
            let allowed = match d {
                [0, 1, 0] => false,
                [0, -1, 0] => true,
                _ => recursive || p[1] == origin[1],
            };
            if allowed && in_circle(origin, q, radius) && open(q) && seen.insert(q) {
                queue.push_back(q);
            }
        }
    }
    cells
}

pub fn drain_plan(clip: &Clip, origin: P, radius: i32, waterlogged: bool) -> Cells {
    let mut put = Put::new();
    let mut seen: HashSet<P> = HashSet::new();
    let mut queue = VecDeque::new();
    let mut seeds = vec![origin];
    seeds.extend(DIRS.iter().map(|d| add(origin, *d)));
    for s in seeds {
        if clip.at(s).is_some_and(is_fluid) && seen.insert(s) {
            queue.push_back(s);
        }
    }
    while let Some(p) = queue.pop_front() {
        let state = clip.at(p).unwrap_or(AIR);
        if has(state, "waterlogged", "true") {
            if waterlogged {
                put.set(p, &with_prop(state, "waterlogged", "false"));
            }
        } else {
            put.set(p, AIR);
        }
        for d in DIRS {
            let q = add(p, d);
            if in_sphere(origin, q, radius) && clip.at(q).is_some_and(is_fluid) && seen.insert(q) {
                queue.push_back(q);
            }
        }
    }
    put.cells
}

pub fn fix_plan(clip: &Clip, origin: P, radius: i32, lava: bool) -> Cells {
    let kind = |s: &str| match lava {
        true => is_lava(s),
        false => name(s) == "minecraft:water",
    };
    let source = if lava { LAVA } else { WATER };
    let mut put = Put::new();
    let mut visited: HashSet<P> = HashSet::new();
    let (min, max) = (
        [origin[0] - radius, origin[1] - radius, origin[2] - radius],
        [origin[0] + radius, origin[1] + radius, origin[2] + radius],
    );
    for y in (min[1]..=max[1]).rev() {
        for z in min[2]..=max[2] {
            for x in min[0]..=max[0] {
                let start = [x, y, z];
                if visited.contains(&start)
                    || !in_sphere(origin, start, radius)
                    || !clip.at(start).is_some_and(|s| kind(s) && is_source(s))
                {
                    continue;
                }
                let mut queue = VecDeque::from([start]);
                visited.insert(start);
                while let Some(p) = queue.pop_front() {
                    let state = clip.at(p).unwrap_or(AIR);
                    if !is_source(state) {
                        put.set(p, source);
                    }
                    for d in DIRS {
                        let q = add(p, d);
                        let ok = d[1] == 0 || d[1] == -1;
                        let fits = clip
                            .at(q)
                            .is_some_and(|s| is_air(s) || (kind(s) && !is_source(s)));
                        if ok && in_sphere(origin, q, radius) && fits && visited.insert(q) {
                            queue.push_back(q);
                        }
                    }
                }
            }
        }
    }
    put.cells
}

pub fn extinguish_plan(clip: &Clip, origin: P, radius: i32) -> Cells {
    let mut put = Put::new();
    let (min, max) = (clip.origin, add(clip.origin, clip.size));
    for y in min[1]..max[1] {
        for z in min[2]..max[2] {
            for x in min[0]..max[0] {
                let p = [x, y, z];
                if in_sphere(origin, p, radius) && clip.at(p).is_some_and(is_fire) {
                    put.set(p, AIR);
                }
            }
        }
    }
    put.cells
}

fn top_block(clip: &Clip, x: i32, z: i32, lo: i32, hi: i32) -> Option<(i32, &str)> {
    (lo..=hi)
        .rev()
        .find_map(|y| clip.at([x, y, z]).filter(|s| !is_air(s)).map(|s| (y, s)))
}

pub fn snow_plan(
    clip: &Clip,
    origin: P,
    radius: i32,
    lo: i32,
    hi: i32,
    solid: &dyn Fn(&str) -> bool,
) -> Cells {
    let mut put = Put::new();
    for z in origin[2] - radius..=origin[2] + radius {
        for x in origin[0] - radius..=origin[0] + radius {
            if !in_circle(origin, [x, 0, z], radius) {
                continue;
            }
            let Some((y, state)) = top_block(clip, x, z, lo, hi) else {
                continue;
            };
            let n = name(state);
            if n == "minecraft:water" && is_source(state) {
                put.set([x, y, z], "minecraft:ice");
            } else if matches!(
                n,
                "minecraft:grass_block" | "minecraft:podzol" | "minecraft:mycelium"
            ) && y < hi
            {
                put.set([x, y, z], &with_prop(state, "snowy", "true"));
                put.set([x, y + 1, z], "minecraft:snow");
            } else if (solid(state) || n.ends_with("_leaves"))
                && !matches!(n, "minecraft:snow" | "minecraft:ice" | "minecraft:lava")
                && !is_fluid(state)
                && y < hi
            {
                put.set([x, y + 1, z], "minecraft:snow");
            }
        }
    }
    put.cells
}

pub fn thaw_plan(clip: &Clip, origin: P, radius: i32, lo: i32, hi: i32) -> Cells {
    let mut put = Put::new();
    for z in origin[2] - radius..=origin[2] + radius {
        for x in origin[0] - radius..=origin[0] + radius {
            if !in_circle(origin, [x, 0, z], radius) {
                continue;
            }
            for y in lo..=hi {
                let p = [x, y, z];
                let Some(state) = clip.at(p) else { continue };
                match name(state) {
                    "minecraft:snow" => put.set(p, AIR),
                    "minecraft:ice" => put.set(p, WATER),
                    _ if has(state, "snowy", "true") => {
                        put.set(p, &with_prop(state, "snowy", "false"))
                    }
                    _ => {}
                }
            }
        }
    }
    put.cells
}

pub fn green_plan(
    clip: &Clip,
    origin: P,
    radius: i32,
    lo: i32,
    hi: i32,
    coarse: bool,
    solid: &dyn Fn(&str) -> bool,
) -> Cells {
    let mut put = Put::new();
    for z in origin[2] - radius..=origin[2] + radius {
        for x in origin[0] - radius..=origin[0] + radius {
            if !in_circle(origin, [x, 0, z], radius) {
                continue;
            }
            for y in lo..=hi {
                let p = [x, y, z];
                let Some(state) = clip.at(p) else { continue };
                let n = name(state);
                let dirt = n == "minecraft:dirt" || (coarse && n == "minecraft:coarse_dirt");
                let covered = clip
                    .at([x, y + 1, z])
                    .is_none_or(|s| solid(s) || is_fluid(s));
                if dirt && !covered {
                    put.set(p, "minecraft:grass_block[snowy=false]");
                }
            }
        }
    }
    put.cells
}

fn arg_int(ctx: &Ctx, at: usize, default: i32, max: i32) -> Result<i32, String> {
    match ctx.args.get(at) {
        None => Ok(default),
        Some(t) => t
            .parse::<i32>()
            .ok()
            .filter(|n| (0..=max).contains(n))
            .ok_or_else(|| format!("Numbers must be from 0 to {max}")),
    }
}

fn solid(state: &str) -> bool {
    Registry.traits(state) & pattern::TRAIT_SOLID != 0
}

fn run_cells(ctx: &Ctx, dim: String, cells: Cells) -> i32 {
    let limit = model(|m| limit_of(&m.players[&ctx.id]));
    let Some((a, b)) = cells.bounds() else {
        return say("Nothing to change");
    };
    if cells.len() as u64 > limit {
        return say(format!(
            "That edit touches {} blocks, over your limit of {limit}",
            cells.len()
        ));
    }
    if let Some(why) = busy(ctx.id) {
        return say(why);
    }
    let gmask = model(|m| m.players[&ctx.id].gmask.clone());
    let total = cells.len() as u64;
    start(crate::edit::Job::new(ctx.id, dim, Op::Cells(cells), a, b, total).gmask(gmask))
}

fn around(
    ctx: &Ctx,
    radius: i32,
    up: i32,
    down: i32,
    plan: impl FnOnce(&Clip, P, i32, i32) -> Cells,
) -> i32 {
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let o = h.block;
    let lo = (o[1] - down).max(WORLD_Y[0]);
    let hi = (o[1] + up).min(WORLD_Y[1]);
    let volume = (2 * radius as u64 + 1).pow(2) * (hi - lo + 1) as u64;
    let limit = model(|m| limit_of(&m.players[&ctx.id]));
    if volume > limit {
        return say(format!(
            "That area has {volume} blocks, over your limit of {limit}"
        ));
    }
    let Some(clip) = Clip::capture(
        &h.dim,
        [o[0] - radius, lo, o[2] - radius],
        [o[0] + radius, hi, o[2] + radius],
    ) else {
        return say("The blocks could not be read");
    };
    let cells = plan(&clip, o, lo, hi);
    run_cells(ctx, h.dim, cells)
}

fn fill_with(ctx: &Ctx, recursive: bool) -> i32 {
    let command = &COMMANDS[recursive as usize];
    let Some(text) = ctx.args.first() else {
        return say(usage(command));
    };
    let clip = model(|m| m.players[&ctx.id].clip.clone());
    let pattern = match pattern::parse(text, &Registry, clip.as_ref()) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    let radius = match ctx.args.get(1) {
        Some(_) => arg_int(ctx, 1, 0, MAX_RADIUS),
        None => Err(usage(command)),
    };
    let depth = arg_int(ctx, 2, 1, 320);
    let (radius, depth) = match (radius, depth) {
        (Ok(r), Ok(d)) if d >= 1 => (r, d),
        (Err(why), _) | (_, Err(why)) => return say(why),
        _ => return say("Depth must be at least 1"),
    };
    around(ctx, radius, 0, depth - 1, |clip, o, _, _| {
        fill_plan(clip, o, radius, depth, recursive, Item::Pat(pattern))
    })
}

fn fill(ctx: &Ctx) -> i32 {
    fill_with(ctx, false)
}

fn fillr(ctx: &Ctx) -> i32 {
    fill_with(ctx, true)
}

fn radius_arg(ctx: &Ctx, at: usize, default: i32) -> Result<i32, String> {
    arg_int(ctx, at, default, MAX_RADIUS)
}

fn sphere_job(ctx: &Ctx, radius: i32, plan: impl FnOnce(&Clip, P) -> Cells) -> i32 {
    around(ctx, radius, radius, radius, |clip, o, _, _| plan(clip, o))
}

fn drain(ctx: &Ctx) -> i32 {
    match ctx.args.first() {
        None => say(usage(&COMMANDS[2])),
        Some(_) => match radius_arg(ctx, 0, 0) {
            Ok(r) => sphere_job(ctx, r, |c, o| drain_plan(c, o, r, ctx.flag('w'))),
            Err(why) => say(why),
        },
    }
}

fn fix(ctx: &Ctx, lava: bool) -> i32 {
    match radius_arg(ctx, 0, DEFAULT_RADIUS) {
        Ok(r) => sphere_job(ctx, r, |c, o| fix_plan(c, o, r, lava)),
        Err(why) => say(why),
    }
}

fn fixwater(ctx: &Ctx) -> i32 {
    fix(ctx, false)
}

fn fixlava(ctx: &Ctx) -> i32 {
    fix(ctx, true)
}

fn extinguish(ctx: &Ctx) -> i32 {
    match radius_arg(ctx, 0, FIRE_RADIUS) {
        Ok(r) => sphere_job(ctx, r, |c, o| extinguish_plan(c, o, r)),
        Err(why) => say(why),
    }
}

fn column(ctx: &Ctx, above: bool) -> i32 {
    let size = arg_int(ctx, 0, 1, MAX_RADIUS + 1).and_then(|s| match s {
        0 => Err("Size must be at least 1".to_owned()),
        s => Ok(s - 1),
    });
    let height = arg_int(ctx, 1, 0, 1000);
    let (size, height) = match (size, height) {
        (Ok(s), Ok(h)) => (s, h),
        (Err(why), _) | (_, Err(why)) => return say(why),
    };
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let o = h.block;
    let (lo, hi) = match above {
        true => (
            o[1],
            if height == 0 {
                WORLD_Y[1]
            } else {
                o[1] + height - 1
            },
        ),
        false => (
            if height == 0 {
                WORLD_Y[0]
            } else {
                o[1] - height
            },
            o[1] - 1,
        ),
    };
    let (lo, hi) = (lo.max(WORLD_Y[0]), hi.min(WORLD_Y[1]));
    if lo > hi {
        return say("Nothing to remove");
    }
    let region = Region::cuboid(
        [o[0] - size, lo, o[2] - size],
        [o[0] + size, hi, o[2] + size],
    );
    let volume = region.volume();
    let limit = model(|m| limit_of(&m.players[&ctx.id]));
    if volume > limit {
        return say(format!(
            "That area has {volume} blocks, over your limit of {limit}"
        ));
    }
    let air = pattern::parse(AIR, &Registry, None).expect("air is a pattern");
    edit(ctx.id, Op::Set(air), h.dim, region, volume)
}

fn removeabove(ctx: &Ctx) -> i32 {
    column(ctx, true)
}

fn removebelow(ctx: &Ctx) -> i32 {
    column(ctx, false)
}

fn near_region(ctx: &Ctx, radius: i32) -> Result<(String, Region, u64), String> {
    let h = here().ok_or("WorldEdit needs your position")?;
    let o = h.block;
    let region = Region::cuboid(
        [o[0] - radius, o[1] - radius, o[2] - radius],
        [o[0] + radius, o[1] + radius, o[2] + radius],
    );
    let volume = region.volume();
    let limit = model(|m| limit_of(&m.players[&ctx.id]));
    match volume > limit {
        true => Err(format!(
            "That area has {volume} blocks, over your limit of {limit}"
        )),
        false => Ok((h.dim, region, volume)),
    }
}

fn removenear(ctx: &Ctx) -> i32 {
    let Some(text) = ctx.args.first() else {
        return say(usage(&COMMANDS[7]));
    };
    let from = mask::parse(&[text], &Registry);
    let radius = radius_arg(ctx, 1, NEAR_RADIUS);
    let (from, radius) = match (from, radius) {
        (Ok(f), Ok(r)) => (f, r),
        (Err(why), _) | (_, Err(why)) => return say(why),
    };
    let air = pattern::parse(AIR, &Registry, None).expect("air is a pattern");
    match near_region(ctx, radius) {
        Ok((dim, region, volume)) => edit(ctx.id, Op::Replace(from, air), dim, region, volume),
        Err(why) => say(why),
    }
}

fn replacenear(ctx: &Ctx) -> i32 {
    let [radius, from @ .., to] = ctx.args.as_slice() else {
        return say(usage(&COMMANDS[8]));
    };
    if from.is_empty() {
        return say(usage(&COMMANDS[8]));
    }
    let radius = match radius.parse::<i32>() {
        Ok(r) if (0..=MAX_RADIUS).contains(&r) => r,
        _ => return say(format!("Radius must be from 0 to {MAX_RADIUS}")),
    };
    let clip = model(|m| m.players[&ctx.id].clip.clone());
    let (from, to) = match (
        mask::parse(from, &Registry),
        pattern::parse(to, &Registry, clip.as_ref()),
    ) {
        (Ok(f), Ok(t)) => (f, t),
        (Err(why), _) | (_, Err(why)) => return say(why),
    };
    match near_region(ctx, radius) {
        Ok((dim, region, volume)) => edit(ctx.id, Op::Replace(from, to), dim, region, volume),
        Err(why) => say(why),
    }
}

fn windowed(ctx: &Ctx, plan: impl FnOnce(&Clip, P, i32, i32, i32) -> Cells) -> i32 {
    match radius_arg(ctx, 0, DEFAULT_RADIUS) {
        Ok(r) => around(ctx, r, WINDOW[1], WINDOW[0], |c, o, lo, hi| {
            plan(c, o, r, lo, hi)
        }),
        Err(why) => say(why),
    }
}

fn snow(ctx: &Ctx) -> i32 {
    windowed(ctx, |c, o, r, lo, hi| snow_plan(c, o, r, lo, hi, &solid))
}

fn thaw(ctx: &Ctx) -> i32 {
    windowed(ctx, thaw_plan)
}

fn green(ctx: &Ctx) -> i32 {
    let coarse = ctx.flag('f');
    windowed(ctx, |c, o, r, lo, hi| {
        green_plan(c, o, r, lo, hi, coarse, &solid)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(size: P, f: impl Fn(P) -> &'static str) -> Clip {
        let mut palette: Vec<String> = Vec::new();
        let mut idx = Vec::new();
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    let s = f([x, y, z]);
                    let at = palette.iter().position(|p| p == s).unwrap_or_else(|| {
                        palette.push(s.to_owned());
                        palette.len() - 1
                    });
                    idx.push(at as u16);
                }
            }
        }
        Clip {
            palette,
            size,
            idx,
            offset: [0; 3],
            origin: [0; 3],
            biomes: None,
        }
    }

    fn state(cells: &Cells, p: P) -> Option<String> {
        match cells.get(p)? {
            Item::State(s) => Some(s.clone()),
            Item::Pat(_) => Some("pat".to_owned()),
        }
    }

    fn stone_solid(s: &str) -> bool {
        !is_air(s) && !is_fluid(s) && name(s) != "minecraft:snow"
    }

    fn pit(p: P) -> &'static str {
        match (p[1], p[0], p[2]) {
            (0, ..) => "minecraft:stone",
            (1, 2..=3, 2..=3) => "minecraft:air",
            (1, ..) => "minecraft:stone",
            _ => "minecraft:air",
        }
    }

    fn pat() -> Item {
        Item::State("minecraft:stone".to_owned())
    }

    #[test]
    fn drain_removes_connected_fluid_in_range() {
        let c = world([5, 3, 5], |p| match p[1] {
            0 => "minecraft:stone",
            _ if p[0] < 3 => "minecraft:water[level=0]",
            _ => "minecraft:air",
        });
        let cells = drain_plan(&c, [1, 1, 2], 10, false);
        assert_eq!(state(&cells, [0, 1, 0]).as_deref(), Some(AIR));
        assert_eq!(cells.len(), 3 * 5 * 2);
        assert!(cells.get([4, 1, 0]).is_none());
        let far = drain_plan(&c, [1, 1, 2], 1, false);
        assert!(far.get([1, 2, 2]).is_some());
        assert!(far.get([2, 1, 4]).is_none());
    }

    #[test]
    fn drain_waterlogged_only_with_flag() {
        let c = world([3, 2, 3], |p| match p[1] {
            0 => "minecraft:water[level=0]",
            _ => "minecraft:oak_slab[type=bottom,waterlogged=true]",
        });
        assert_eq!(
            state(&drain_plan(&c, [1, 0, 1], 5, true), [1, 1, 1]).as_deref(),
            Some("minecraft:oak_slab[type=bottom,waterlogged=false]")
        );
        assert!(drain_plan(&c, [1, 0, 1], 5, false).get([1, 1, 1]).is_none());
    }

    #[test]
    fn fixwater_levels_flowing_water_and_fills_air() {
        let c = world([5, 3, 1], |p| match (p[1], p[0]) {
            (0, _) => "minecraft:stone",
            (1, 0) => "minecraft:water[level=0]",
            (1, 1) => "minecraft:water[level=3]",
            (1, _) => "minecraft:air",
            (2, 0) => "minecraft:water[level=0]",
            _ => "minecraft:air",
        });
        let cells = fix_plan(&c, [0, 2, 0], 10, false);
        assert_eq!(state(&cells, [1, 1, 0]).as_deref(), Some(WATER));
        assert!(cells.get([0, 1, 0]).is_none());
        let lava = fix_plan(&c, [0, 2, 0], 10, true);
        assert_eq!(lava.len(), 0);
    }

    #[test]
    fn fill_fills_holes_down_to_depth() {
        let c = world([6, 3, 6], pit);
        let all = fill_plan(&c, [2, 2, 2], 4, 2, false, pat());
        assert!(all.get([2, 1, 2]).is_some());
        assert!(all.get([3, 2, 3]).is_some());
        assert!(all.get([1, 1, 1]).is_none());
        let shallow = fill_plan(&c, [2, 2, 2], 4, 1, false, pat());
        assert!(shallow.get([2, 1, 2]).is_none());
        assert!(shallow.get([2, 2, 2]).is_some());
    }

    #[test]
    fn fillr_spreads_at_lower_levels() {
        let c = world([6, 4, 3], |p| match (p[1], p[0]) {
            (0, _) => "minecraft:stone",
            (1, 0..=1) => "minecraft:air",
            (1, 2) => "minecraft:air",
            (1, _) => "minecraft:stone",
            (2, 0) => "minecraft:air",
            (2, _) => "minecraft:stone",
            _ => "minecraft:air",
        });
        let flat = fill_plan(&c, [0, 3, 1], 8, 3, false, pat());
        let deep = fill_plan(&c, [0, 3, 1], 8, 3, true, pat());
        assert!(flat.get([1, 1, 1]).is_none());
        assert!(deep.get([2, 1, 1]).is_some() || deep.get([1, 1, 1]).is_some());
        assert!(deep.len() >= flat.len());
    }

    #[test]
    fn snow_covers_surfaces_and_freezes_water() {
        let c = world([4, 3, 1], |p| match (p[1], p[0]) {
            (0, 0) => "minecraft:grass_block[snowy=false]",
            (0, 1) => "minecraft:water[level=0]",
            (0, 2) => "minecraft:stone",
            (0, 3) => "minecraft:snow[layers=1]",
            _ => "minecraft:air",
        });
        let cells = snow_plan(&c, [1, 0, 0], 8, 0, 2, &stone_solid);
        assert_eq!(state(&cells, [0, 1, 0]).as_deref(), Some("minecraft:snow"));
        assert_eq!(
            state(&cells, [0, 0, 0]).as_deref(),
            Some("minecraft:grass_block[snowy=true]")
        );
        assert_eq!(state(&cells, [1, 0, 0]).as_deref(), Some("minecraft:ice"));
        assert_eq!(state(&cells, [2, 1, 0]).as_deref(), Some("minecraft:snow"));
        assert!(cells.get([3, 1, 0]).is_none());
        assert!(
            snow_plan(&c, [1, 0, 0], 1, 0, 2, &stone_solid)
                .get([3, 1, 0])
                .is_none()
        );
    }

    #[test]
    fn thaw_melts_snow_and_ice() {
        let c = world([4, 2, 1], |p| match (p[1], p[0]) {
            (0, 0) => "minecraft:grass_block[snowy=true]",
            (1, 0) => "minecraft:snow[layers=2]",
            (0, 1) => "minecraft:ice",
            (0, 2) => "minecraft:stone",
            _ => "minecraft:air",
        });
        let cells = thaw_plan(&c, [1, 0, 0], 8, 0, 1);
        assert_eq!(state(&cells, [1, 0, 0]).as_deref(), Some(WATER));
        assert_eq!(state(&cells, [0, 1, 0]).as_deref(), Some(AIR));
        assert_eq!(
            state(&cells, [0, 0, 0]).as_deref(),
            Some("minecraft:grass_block[snowy=false]")
        );
        assert!(cells.get([2, 0, 0]).is_none());
    }

    #[test]
    fn green_only_exposed_dirt_and_coarse_with_flag() {
        let c = world([4, 3, 1], |p| match (p[1], p[0]) {
            (0, 0) => "minecraft:dirt",
            (0, 1) => "minecraft:dirt",
            (1, 1) => "minecraft:stone",
            (0, 2) => "minecraft:coarse_dirt",
            _ => "minecraft:air",
        });
        let plain = green_plan(&c, [1, 0, 0], 8, 0, 2, false, &stone_solid);
        assert!(plain.get([0, 0, 0]).is_some());
        assert!(plain.get([1, 0, 0]).is_none());
        assert!(plain.get([2, 0, 0]).is_none());
        assert!(
            green_plan(&c, [1, 0, 0], 8, 0, 2, true, &stone_solid)
                .get([2, 0, 0])
                .is_some()
        );
    }

    #[test]
    fn extinguish_removes_fire_in_the_sphere() {
        let c = world([5, 1, 1], |p| match p[0] {
            0 | 4 => "minecraft:fire[age=3]",
            1 => "minecraft:soul_fire",
            _ => "minecraft:stone",
        });
        let cells = extinguish_plan(&c, [0, 0, 0], 2);
        assert_eq!(cells.len(), 2);
        assert!(cells.get([4, 0, 0]).is_none());
    }
}

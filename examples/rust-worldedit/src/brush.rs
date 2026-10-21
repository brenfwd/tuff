// SPDX-License-Identifier: MIT
use std::collections::{HashMap, HashSet, VecDeque};

use crate::clipboard::{self, PasteFlags};
use crate::commands::select::trace;
use crate::commands::utility::extinguish_plan;
use crate::commands::{Command, Ctx, usage};
use crate::edit::{Clip, Job, Op, Registry, is_air};
use crate::jobs;
use crate::mask::{self, Mask};
use crate::pattern::{self, Pattern, split_state};
use crate::plan::{self, Cells, Item};
use crate::region::{P, Region, WORLD_Y};
use crate::selection::{limit_of, set_pos};
use crate::session::{model, owner, say};

pub const TAG: &str = "worldedit_tool";
const BRUSH_NODE: &str = "worldedit.brush.use";
const TOOL_NODE: &str = "worldedit.tool.use";
const MAX_SIZE: i32 = 100;
const MAX_RANGE: i32 = 1000;
const DEFAULT_PATTERN: &str = "stone";
const FLOOD_CAP: i32 = 30;
const TREE_BOX: i32 = 12;
const STACK_CAP: i32 = 64;

#[derive(Clone)]
pub enum Brush {
    Sphere { hollow: bool },
    Cylinder { hollow: bool, height: i32 },
    Smooth { iterations: u32 },
    Gravity { from_target: bool },
    Clipboard { skip_air: bool, original: bool },
    Forest { kind: String, density: f64 },
    Raise,
    Lower,
    Extinguish,
    Butcher,
}

#[derive(Clone)]
pub enum Tool {
    Tree(String),
    Repl,
    Info,
    Flood { range: i32 },
    Farwand,
    Lrbuild { right: String },
    Deltree,
    Cycler { prop: usize },
    Stacker { range: i32 },
    Navwand,
}

#[derive(Clone)]
pub enum Kind {
    Brush(Brush),
    Tool(Tool),
}

#[derive(Clone)]
pub struct Bind {
    pub kind: Kind,
    pub pattern: String,
    pub mask: Option<Mask>,
    pub size: i32,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "brush",
        aliases: &["br"],
        usage: "<sphere|cylinder|smooth|gravity|clipboard|forest|raise|lower|extinguish|butcher> [-h -a -p] [args]",
        summary: "Bind a brush to the held item",
        node: BRUSH_NODE,
        flags: "hap",
        run: brush,
    },
    Command {
        name: "tool",
        aliases: &[],
        usage: "<tree|repl|info|floodfill|farwand|lrbuild|deltree|cycler|stacker|navwand|last> [args]",
        summary: "Bind a tool to the held item",
        node: TOOL_NODE,
        flags: "",
        run: tool,
    },
    Command {
        name: "none",
        aliases: &[],
        usage: "",
        summary: "Unbind the held item",
        node: TOOL_NODE,
        flags: "",
        run: none,
    },
    Command {
        name: "mask",
        aliases: &[],
        usage: "[mask]",
        summary: "Set the held brush's mask, or clear it",
        node: BRUSH_NODE,
        flags: "",
        run: mask_cmd,
    },
    Command {
        name: "material",
        aliases: &["mat"],
        usage: "<pattern>",
        summary: "Set the held brush's pattern",
        node: BRUSH_NODE,
        flags: "",
        run: material,
    },
    Command {
        name: "brushsize",
        aliases: &["bsize"],
        usage: "<size>",
        summary: "Set the held brush's size",
        node: BRUSH_NODE,
        flags: "",
        run: brush_size,
    },
    Command {
        name: "range",
        aliases: &[],
        usage: "<blocks>",
        summary: "Set how far brushes and tools reach",
        node: BRUSH_NODE,
        flags: "",
        run: range,
    },
];

fn command(name: &str) -> &'static Command {
    COMMANDS.iter().find(|c| c.name == name).unwrap()
}

fn held() -> Option<String> {
    tuff_plugin_sdk::world::acting::inventory()?
        .held
        .map(|s| s.item)
        .filter(|s| !s.is_empty())
}

fn int_arg(ctx: &Ctx, at: usize, default: i32, max: i32) -> Result<i32, String> {
    match ctx.args.get(at) {
        None => Ok(default),
        Some(text) => match text.parse::<i32>() {
            Ok(n) if (1..=max).contains(&n) => Ok(n),
            _ => Err(format!("Expected a number from 1 to {max}, got {text}")),
        },
    }
}

fn check_pattern(id: u128, text: &str) -> Result<(), String> {
    let clip = model(|m| m.players[&id].clip.clone());
    pattern::parse(text, &Registry, clip.as_ref()).map(|_| ())
}

fn install(ctx: &Ctx, bind: Bind) -> i32 {
    let Some(item) = held() else {
        return say("Hold the item to bind first");
    };
    let fresh = model(|m| {
        let player = m.players.get_mut(&ctx.id).unwrap();
        player.binds.insert(item.clone(), bind).is_none()
    });
    if fresh {
        let _ = tuff_plugin_sdk::world::acting::give_tagged(
            &item,
            1,
            Some(&format!("{{{TAG}:1b}}")),
            Some("WorldEdit tool"),
        );
    }
    say(format!("Bound to {item}; use the marked stack"))
}

fn existing(ctx: &Ctx) -> Option<Bind> {
    let item = held()?;
    model(|m| m.players[&ctx.id].binds.get(&item).cloned())
}

fn brush(ctx: &Ctx) -> i32 {
    let command = command("brush");
    let Some(name) = ctx.args.first().copied() else {
        return say(usage(command));
    };
    let rest = Ctx {
        id: ctx.id,
        args: ctx.args[1..].to_vec(),
        flags: ctx.flags.clone(),
    };
    let built: Result<(Brush, Option<String>, i32), String> = match name {
        "sphere" | "s" => rest.args.first().map_or(
            Err("Usage: //brush sphere [-h] <pattern> [radius]".to_owned()),
            |p| {
                check_pattern(ctx.id, p)?;
                Ok((
                    Brush::Sphere {
                        hollow: rest.flag('h'),
                    },
                    Some((*p).to_owned()),
                    int_arg(&rest, 1, 2, MAX_SIZE)?,
                ))
            },
        ),
        "cylinder" | "cyl" | "c" => rest.args.first().map_or(
            Err("Usage: //brush cylinder [-h] <pattern> [radius] [height]".to_owned()),
            |p| {
                check_pattern(ctx.id, p)?;
                Ok((
                    Brush::Cylinder {
                        hollow: rest.flag('h'),
                        height: int_arg(&rest, 2, 1, MAX_SIZE)?,
                    },
                    Some((*p).to_owned()),
                    int_arg(&rest, 1, 2, MAX_SIZE)?,
                ))
            },
        ),
        "smooth" => int_arg(&rest, 0, 2, MAX_SIZE).and_then(|size| {
            Ok((
                Brush::Smooth {
                    iterations: int_arg(&rest, 1, 4, 64)? as u32,
                },
                None,
                size,
            ))
        }),
        "gravity" => int_arg(&rest, 0, 5, MAX_SIZE).map(|size| {
            (
                Brush::Gravity {
                    from_target: rest.flag('h'),
                },
                None,
                size,
            )
        }),
        "clipboard" | "clip" => Ok((
            Brush::Clipboard {
                skip_air: rest.flag('a'),
                original: rest.flag('p'),
            },
            None,
            1,
        )),
        "forest" => {
            let kind = rest.args.first().copied().unwrap_or("oak");
            if !plan::TREES.contains(&kind) {
                return say(format!(
                    "Unknown tree {kind}; try {}",
                    plan::TREES.join(", ")
                ));
            }
            int_arg(&rest, 1, 5, MAX_SIZE).and_then(|size| {
                let density = match rest.args.get(2) {
                    None => 5.0,
                    Some(d) => d.parse::<f64>().map_err(|_| "Bad density".to_owned())?,
                };
                Ok((
                    Brush::Forest {
                        kind: kind.to_owned(),
                        density: density.clamp(0.0, 100.0) / 100.0,
                    },
                    None,
                    size,
                ))
            })
        }
        "raise" => int_arg(&rest, 0, 5, MAX_SIZE).map(|s| (Brush::Raise, None, s)),
        "lower" => int_arg(&rest, 0, 5, MAX_SIZE).map(|s| (Brush::Lower, None, s)),
        "extinguish" | "ex" => int_arg(&rest, 0, 5, MAX_SIZE).map(|s| (Brush::Extinguish, None, s)),
        "butcher" => int_arg(&rest, 0, 5, MAX_SIZE).map(|s| (Brush::Butcher, None, s)),
        other => Err(format!("Unknown brush {other}. {}", usage(command))),
    };
    let (brush, pattern, size) = match built {
        Ok(b) => b,
        Err(why) => return say(why),
    };
    let old = existing(ctx);
    install(
        ctx,
        Bind {
            kind: Kind::Brush(brush),
            pattern: pattern
                .or_else(|| old.as_ref().map(|b| b.pattern.clone()))
                .unwrap_or_else(|| DEFAULT_PATTERN.to_owned()),
            mask: old.and_then(|b| b.mask),
            size,
        },
    )
}

fn tool(ctx: &Ctx) -> i32 {
    let command = command("tool");
    let Some(name) = ctx.args.first().copied() else {
        return say(usage(command));
    };
    let arg = |i: usize| ctx.args.get(i).copied();
    let built: Result<(Tool, Option<String>), String> = match name {
        "tree" => {
            let kind = arg(1).unwrap_or("oak");
            match plan::TREES.contains(&kind) {
                true => Ok((Tool::Tree(kind.to_owned()), None)),
                false => Err(format!(
                    "Unknown tree {kind}; try {}",
                    plan::TREES.join(", ")
                )),
            }
        }
        "repl" => arg(1).map_or(Err("Usage: //tool repl <pattern>".to_owned()), |p| {
            check_pattern(ctx.id, p)?;
            Ok((Tool::Repl, Some(p.to_owned())))
        }),
        "info" => Ok((Tool::Info, None)),
        "floodfill" | "flood" => arg(1).map_or(
            Err("Usage: //tool floodfill <pattern> <range>".to_owned()),
            |p| {
                check_pattern(ctx.id, p)?;
                Ok((
                    Tool::Flood {
                        range: int_arg(ctx, 2, 3, FLOOD_CAP)?,
                    },
                    Some(p.to_owned()),
                ))
            },
        ),
        "farwand" => Ok((Tool::Farwand, None)),
        "lrbuild" => match (arg(1), arg(2)) {
            (Some(left), Some(right)) => check_pattern(ctx.id, left)
                .and_then(|()| check_pattern(ctx.id, right))
                .map(|()| {
                    (
                        Tool::Lrbuild {
                            right: right.to_owned(),
                        },
                        Some(left.to_owned()),
                    )
                }),
            _ => Err("Usage: //tool lrbuild <left pattern> <right pattern>".to_owned()),
        },
        "deltree" => Ok((Tool::Deltree, None)),
        "cycler" => Ok((Tool::Cycler { prop: 0 }, None)),
        "navwand" => Ok((Tool::Navwand, None)),
        "stacker" => int_arg(ctx, 1, 1, STACK_CAP).map(|range| (Tool::Stacker { range }, None)),
        "last" => {
            let last = model(|m| m.players[&ctx.id].last.clone());
            return say(last.unwrap_or_else(|| "No tool output yet".to_owned()));
        }
        other => Err(format!("Unknown tool {other}. {}", usage(command))),
    };
    match built {
        Ok((tool, pattern)) => install(
            ctx,
            Bind {
                kind: Kind::Tool(tool),
                pattern: pattern.unwrap_or_else(|| DEFAULT_PATTERN.to_owned()),
                mask: None,
                size: 1,
            },
        ),
        Err(why) => say(why),
    }
}

fn none(ctx: &Ctx) -> i32 {
    let Some(item) = held() else {
        return say("Hold the item to unbind");
    };
    let had = model(|m| {
        m.players
            .get_mut(&ctx.id)
            .unwrap()
            .binds
            .remove(&item)
            .is_some()
    });
    say(match had {
        true => "Unbound the held item",
        false => "The held item has no brush or tool",
    })
}

fn edit_bind(ctx: &Ctx, f: impl FnOnce(&mut Bind)) -> i32 {
    let Some(item) = held() else {
        return say("Hold a bound item first");
    };
    let done = model(|m| match m.players[&ctx.id].binds.get(&item) {
        None => false,
        Some(_) => {
            f(m.players
                .get_mut(&ctx.id)
                .unwrap()
                .binds
                .get_mut(&item)
                .unwrap());
            true
        }
    });
    match done {
        true => say("Updated the held item"),
        false => say("The held item has no brush or tool (//brush, //tool)"),
    }
}

fn mask_cmd(ctx: &Ctx) -> i32 {
    let mask = match ctx.args.is_empty() {
        true => None,
        false => match mask::parse(&ctx.args, &Registry) {
            Ok(m) => Some(m),
            Err(why) => return say(why),
        },
    };
    edit_bind(ctx, |b| b.mask = mask)
}

fn material(ctx: &Ctx) -> i32 {
    let Some(text) = ctx.args.first() else {
        return say(usage(command("material")));
    };
    if let Err(why) = check_pattern(ctx.id, text) {
        return say(why);
    }
    let text = (*text).to_owned();
    edit_bind(ctx, |b| b.pattern = text)
}

fn brush_size(ctx: &Ctx) -> i32 {
    match int_arg(ctx, 0, 0, MAX_SIZE) {
        Ok(0) | Err(_) => say(usage(command("brushsize"))),
        Ok(n) => edit_bind(ctx, |b| b.size = n),
    }
}

fn range(ctx: &Ctx) -> i32 {
    match int_arg(ctx, 0, 0, MAX_RANGE) {
        Ok(0) | Err(_) => say(usage(command("range"))),
        Ok(n) => {
            model(|m| m.players.get_mut(&ctx.id).unwrap().range = n);
            say(format!("Range set to {n}"))
        }
    }
}

struct Hit {
    dim: String,
    at: P,
    before: P,
}

fn aim(range: i32, clicked: [i32; 3], dim: &str) -> Option<Hit> {
    let dim = dim.to_string();
    match crate::session::look() {
        None => Some(Hit {
            dim,
            at: clicked,
            before: clicked,
        }),
        Some(_) => trace(&dim, range).map(|(at, before)| Hit { dim, at, before }),
    }
}

fn note(text: impl AsRef<str>) {
    let Some(id) = owner() else {
        return;
    };
    let text = text.as_ref().to_owned();
    crate::session::tell(id, &text);
    model(|m| m.players.get_mut(&id).unwrap().last = Some(text));
}

pub fn click(right: bool, clicked: [i32; 3], dim: &str) -> bool {
    let Some(item) = held() else {
        return false;
    };
    let Some(id) = owner() else {
        return false;
    };
    let Some((bind, range)) = model(|m| {
        let p = &m.players[&id];
        p.binds.get(&item).map(|b| (b.clone(), p.range))
    }) else {
        return false;
    };
    let node = match bind.kind {
        Kind::Brush(_) => BRUSH_NODE,
        Kind::Tool(_) => TOOL_NODE,
    };
    if tuff_plugin_sdk::world::acting::has_permission(crate::commands::BASE_NODE) != Some(true)
        || tuff_plugin_sdk::world::acting::has_permission(node) != Some(true)
    {
        note("You do not have permission to use this WorldEdit tool");
        return true;
    }
    let Some(hit) = aim(range, clicked, dim) else {
        note("No block in sight");
        return true;
    };
    match &bind.kind {
        Kind::Brush(brush) => {
            if right {
                apply_brush(id, &hit, brush, &bind);
            }
        }
        Kind::Tool(tool) => apply_tool(id, &item, &hit, tool, &bind, right),
    }
    true
}

fn run_region(id: u128, hit: &Hit, region: Region, bind: &Bind) {
    let clip = model(|m| m.players[&id].clip.clone());
    let pattern = match pattern::parse(&bind.pattern, &Registry, clip.as_ref()) {
        Ok(p) => p,
        Err(why) => {
            note(why);
            return;
        }
    };
    let volume = region.volume();
    let limit = model(|m| limit_of(&m.players[&id]));
    if volume > limit {
        note(format!(
            "That brush touches {volume} blocks, over your limit of {limit}"
        ));
        return;
    }
    let op = match &bind.mask {
        Some(mask) => Op::Replace(mask.clone(), pattern),
        None => Op::Set(pattern),
    };
    jobs::edit(id, op, hit.dim.clone(), region, volume);
}

fn run_cells(id: u128, dim: &str, cells: Cells) {
    let limit = model(|m| limit_of(&m.players[&id]));
    let Some((a, b)) = cells.bounds() else {
        note("Nothing to change");
        return;
    };
    if cells.len() as u64 > limit {
        note(format!(
            "That edit touches {} blocks, over your limit of {limit}",
            cells.len()
        ));
        return;
    }
    if let Some(why) = jobs::busy(id) {
        note(why);
        return;
    }
    let gmask = model(|m| m.players[&id].gmask.clone());
    let total = cells.len() as u64;
    jobs::start(Job::new(id, dim.to_owned(), Op::Cells(cells), a, b, total).gmask(gmask));
}

fn capture(hit: &Hit, min: P, max: P) -> Option<Clip> {
    let clamp = |y: i32| y.clamp(WORLD_Y[0], WORLD_Y[1]);
    let clip = Clip::capture(
        &hit.dim,
        [min[0], clamp(min[1]), min[2]],
        [max[0], clamp(max[1]), max[2]],
    );
    if clip.is_none() {
        note("The blocks could not be read");
    }
    clip
}

fn around(hit: &Hit, r: i32) -> (P, P) {
    (
        [hit.at[0] - r, hit.at[1] - r, hit.at[2] - r],
        [hit.at[0] + r, hit.at[1] + r, hit.at[2] + r],
    )
}

fn in_circle(c: P, x: i32, z: i32, r: i32) -> bool {
    (x - c[0]).pow(2) + (z - c[2]).pow(2) <= r * r
}

fn apply_brush(id: u128, hit: &Hit, brush: &Brush, bind: &Bind) {
    let r = bind.size;
    let c = hit.at;
    match brush {
        Brush::Sphere { hollow } => {
            let base = Region::Ellipsoid { c, r: [r; 3] };
            let region = match hollow {
                true => Region::Shell {
                    base: Box::new(base),
                    flat: false,
                },
                false => base,
            };
            run_region(id, hit, region, bind);
        }
        Brush::Cylinder { hollow, height } => {
            let base = Region::Cyl {
                c,
                r: [r, r],
                y: [c[1], c[1] + height - 1],
            };
            let region = match hollow {
                true => Region::Shell {
                    base: Box::new(base),
                    flat: true,
                },
                false => base,
            };
            run_region(id, hit, region, bind);
        }
        Brush::Smooth { iterations } => {
            let region = Region::Cuboid {
                min: around(hit, r).0,
                max: around(hit, r).1,
            };
            let Some(clip) = capture(hit, around(hit, r).0, around(hit, r).1) else {
                return;
            };
            let mask = bind.mask.clone();
            let solid = |state: &str, pos| match &mask {
                Some(m) => m.test(&Registry, pos, state),
                None => !is_air(state),
            };
            run_cells(
                id,
                &hit.dim,
                plan::smooth(&clip, &region, *iterations, &solid),
            );
        }
        Brush::Gravity { from_target } => {
            let lo = (c[1] - r).max(WORLD_Y[0]);
            let hi = (c[1] + r).min(WORLD_Y[1]);
            let Some(clip) = capture(hit, [c[0] - r, lo, c[2] - r], [c[0] + r, hi, c[2] + r])
            else {
                return;
            };
            let start = if *from_target { c[1] } else { lo };
            run_cells(id, &hit.dim, gravity(&clip, c, r, start, hi));
        }
        Brush::Clipboard { skip_air, original } => {
            clipboard::paste(
                id,
                hit.dim.clone(),
                c,
                PasteFlags {
                    skip_air: *skip_air,
                    original: *original,
                    ..PasteFlags::default()
                },
            );
        }
        Brush::Forest { kind, density } => {
            let region = Region::Cuboid {
                min: around(hit, r).0,
                max: around(hit, r).1,
            };
            let (min, max) = around(hit, r);
            let Some(clip) = capture(
                hit,
                [min[0] - 2, min[1], min[2] - 2],
                [max[0] + 2, max[1] + 9, max[2] + 2],
            ) else {
                return;
            };
            let cells = plan::forest(&clip, &region, kind, *density, &Registry);
            run_cells(id, &hit.dim, cells);
        }
        Brush::Raise | Brush::Lower => {
            let lo = (c[1] - r).max(WORLD_Y[0]);
            let hi = (c[1] + r + 1).min(WORLD_Y[1]);
            let Some(clip) = capture(hit, [c[0] - r, lo, c[2] - r], [c[0] + r, hi, c[2] + r])
            else {
                return;
            };
            run_cells(
                id,
                &hit.dim,
                shift_terrain(&clip, c, r, lo, hi, matches!(brush, Brush::Raise)),
            );
        }
        Brush::Extinguish => {
            let (min, max) = around(hit, r);
            if let Some(clip) = capture(hit, min, max) {
                run_cells(id, &hit.dim, extinguish_plan(&clip, c, r));
            }
        }
        Brush::Butcher => {
            let world = tuff_plugin_sdk::world::World::new(hit.dim.clone());
            let centre = tuff_plugin_sdk::types::Vec3 {
                x: f64::from(c[0]) + 0.5,
                y: f64::from(c[1]) + 0.5,
                z: f64::from(c[2]) + 0.5,
            };
            let spare = crate::commands::entity::Spare::default();
            match crate::commands::entity::butcher_around(id, &world, centre, r, spare) {
                Ok(n) => note(format!("Butchering {n} entities")),
                Err(why) => note(why),
            }
        }
    }
}

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
        let item = match self.known.get(state) {
            Some(i) => *i,
            None => {
                let i = self.cells.state(state);
                self.known.insert(state.to_owned(), i);
                i
            }
        };
        self.cells.set(pos, item);
    }
}

fn gravity(clip: &Clip, c: P, r: i32, start: i32, hi: i32) -> Cells {
    let mut put = Put::new();
    for z in c[2] - r..=c[2] + r {
        for x in c[0] - r..=c[0] + r {
            if !in_circle(c, x, z, r) {
                continue;
            }
            let stack: Vec<&str> = (start..=hi)
                .filter_map(|y| clip.at([x, y, z]).filter(|s| !is_air(s)))
                .collect();
            for (i, y) in (start..=hi).enumerate() {
                let new = stack.get(i).copied().unwrap_or("minecraft:air");
                if clip.at([x, y, z]).is_some_and(|old| old != new) {
                    put.set([x, y, z], new);
                }
            }
        }
    }
    put.cells
}

fn shift_terrain(clip: &Clip, c: P, r: i32, lo: i32, hi: i32, raise: bool) -> Cells {
    const FACES: [P; 6] = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    let mut put = Put::new();
    for y in lo..=hi {
        for z in c[2] - r..=c[2] + r {
            for x in c[0] - r..=c[0] + r {
                let (dx, dy, dz) = (x - c[0], y - c[1], z - c[2]);
                if dx * dx + dy * dy + dz * dz > r * r {
                    continue;
                }
                let Some(here) = clip.at([x, y, z]) else {
                    continue;
                };
                if is_air(here) != raise {
                    continue;
                }
                let mut counts: Vec<(&str, u32)> = Vec::new();
                for f in FACES {
                    let Some(n) = clip.at([x + f[0], y + f[1], z + f[2]]) else {
                        continue;
                    };
                    if is_air(n) == raise {
                        continue;
                    }
                    match counts.iter_mut().find(|(s, _)| *s == n) {
                        Some(e) => e.1 += 1,
                        None => counts.push((n, 1)),
                    }
                }
                let best = counts.iter().fold(None::<(&str, u32)>, |m, &e| match m {
                    Some(m) if m.1 >= e.1 => Some(m),
                    _ => Some(e),
                });
                if let Some((state, _)) = best {
                    put.set([x, y, z], if raise { state } else { "minecraft:air" });
                }
            }
        }
    }
    put.cells
}

fn single(id: u128, hit: &Hit, at: P, pattern: &str) {
    let clip = model(|m| m.players[&id].clip.clone());
    let pattern: Pattern = match pattern::parse(pattern, &Registry, clip.as_ref()) {
        Ok(p) => p,
        Err(why) => {
            note(why);
            return;
        }
    };
    let mut cells = Cells::default();
    let item = cells.item(Item::Pat(pattern));
    cells.set(at, item);
    run_cells(id, &hit.dim, cells);
}

fn is_tree(state: &str) -> bool {
    let name = split_state(state).0;
    name.ends_with("_log") || name.ends_with("_leaves") || name.ends_with("_wood")
}

fn apply_tool(id: u128, item: &str, hit: &Hit, tool: &Tool, bind: &Bind, right: bool) {
    match tool {
        Tool::Info => {
            let mut buf = [0u8; 512];
            let state = tuff_plugin_sdk::block_get(&hit.dim, hit.at, &mut buf).unwrap_or("unknown");
            note(format!(
                "Position: {} {} {}\nBlock: {state}",
                hit.at[0], hit.at[1], hit.at[2]
            ));
        }
        Tool::Repl => {
            if right {
                single(id, hit, hit.at, &bind.pattern);
                return;
            }
            let mut buf = [0u8; 512];
            let state = tuff_plugin_sdk::block_get(&hit.dim, hit.at, &mut buf)
                .unwrap_or_default()
                .to_owned();
            if state.is_empty() {
                return;
            }
            model(|m| {
                if let Some(b) = m.players.get_mut(&id).unwrap().binds.get_mut(item) {
                    b.pattern = state.clone();
                }
            });
            note(format!("Replacement block set to {state}"));
        }
        Tool::Farwand => {
            set_pos(id, right, hit.at, &hit.dim);
        }
        Tool::Lrbuild { right: other } => {
            let (pattern, at) = match right {
                true => (other.as_str(), hit.before),
                false => (bind.pattern.as_str(), hit.at),
            };
            single(id, hit, at, pattern);
        }
        Tool::Tree(kind) => {
            let region = Region::Cuboid {
                min: hit.at,
                max: hit.at,
            };
            let Some(clip) = capture(
                hit,
                [hit.at[0] - 2, hit.at[1], hit.at[2] - 2],
                [hit.at[0] + 2, hit.at[1] + 9, hit.at[2] + 2],
            ) else {
                return;
            };
            let cells = plan::forest(&clip, &region, kind, 1.0, &Registry);
            match cells.len() {
                0 => {
                    note("A tree cannot grow there");
                }
                _ => run_cells(id, &hit.dim, cells),
            }
        }
        Tool::Deltree => deltree(id, hit),
        Tool::Navwand => {
            match right {
                true => crate::commands::navigation::pass(),
                false => crate::commands::navigation::jump(),
            };
        }
        Tool::Stacker { range } => {
            if right {
                stack(id, hit, *range, bind.mask.as_ref());
            }
        }
        Tool::Cycler { prop } => cycle(id, item, hit, *prop, right),
        Tool::Flood { range } => flood(id, hit, *range, &bind.pattern),
    }
}

const DIRS: [P; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

fn walk(clip: &Clip, start: P, accept: impl Fn(&str, P) -> bool) -> Vec<P> {
    let mut seen = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(p) = queue.pop_front() {
        out.push(p);
        for d in DIRS {
            let n = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
            if !seen.contains(&n) && clip.at(n).is_some_and(|s| accept(s, n)) {
                seen.insert(n);
                queue.push_back(n);
            }
        }
    }
    out
}

fn deltree(id: u128, hit: &Hit) {
    let b = TREE_BOX;
    let Some(clip) = capture(
        hit,
        [hit.at[0] - b, hit.at[1] - 2, hit.at[2] - b],
        [hit.at[0] + b, hit.at[1] + 3 * b, hit.at[2] + b],
    ) else {
        return;
    };
    if !clip.at(hit.at).is_some_and(is_tree) {
        note("That is not part of a tree");
        return;
    }
    let found = walk(&clip, hit.at, |s, _| is_tree(s));
    let mut put = Put::new();
    for p in found {
        put.set(p, "minecraft:air");
    }
    run_cells(id, &hit.dim, put.cells);
}

fn flood(id: u128, hit: &Hit, range: i32, pattern: &str) {
    let (min, max) = around(hit, range);
    let Some(clip) = capture(hit, min, max) else {
        return;
    };
    let Some(target) = clip.at(hit.at).map(str::to_owned) else {
        return;
    };
    let origin = hit.at;
    let found = walk(&clip, hit.at, |s, p| {
        s == target
            && (0..3)
                .map(|i| i64::from(p[i] - origin[i]).pow(2))
                .sum::<i64>()
                <= i64::from(range).pow(2)
    });
    let parsed = {
        let clip = model(|m| m.players[&id].clip.clone());
        pattern::parse(pattern, &Registry, clip.as_ref())
    };
    let pattern = match parsed {
        Ok(p) => p,
        Err(why) => {
            note(why);
            return;
        }
    };
    let mut cells = Cells::default();
    let item = cells.item(Item::Pat(pattern));
    for p in found {
        cells.set(p, item);
    }
    run_cells(id, &hit.dim, cells);
}

fn stack(id: u128, hit: &Hit, range: i32, mask: Option<&Mask>) {
    let step = [
        hit.before[0] - hit.at[0],
        hit.before[1] - hit.at[1],
        hit.before[2] - hit.at[2],
    ];
    if step.iter().map(|v| v.abs()).sum::<i32>() != 1 {
        note("Aim at a block face to stack");
        return;
    }
    let mut buf = [0u8; 512];
    let Some(state) = tuff_plugin_sdk::block_get(&hit.dim, hit.at, &mut buf)
        .ok()
        .map(str::to_owned)
    else {
        return;
    };
    let mut cells = Cells::default();
    for i in 1..=range {
        let at = [
            hit.at[0] + step[0] * i,
            hit.at[1] + step[1] * i,
            hit.at[2] + step[2] * i,
        ];
        if at[1] < WORLD_Y[0] || at[1] > WORLD_Y[1] {
            break;
        }
        let mut b = [0u8; 512];
        let old = tuff_plugin_sdk::block_get(&hit.dim, at, &mut b).unwrap_or_default();
        if mask.is_some_and(|m| !m.test(&Registry, at, old)) {
            continue;
        }
        let item = cells.item(Item::State(state.clone()));
        cells.set(at, item);
    }
    match cells.len() {
        0 => note("Nothing to stack"),
        _ => run_cells(id, &hit.dim, cells),
    }
}

pub fn next_value(values: &[String], current: &str) -> Option<String> {
    let at = values.iter().position(|v| v == current)?;
    Some(values[(at + 1) % values.len()].clone())
}

fn cycle(id: u128, item: &str, hit: &Hit, prop: usize, right: bool) {
    let mut buf = [0u8; 512];
    let Some(state) = tuff_plugin_sdk::block_get(&hit.dim, hit.at, &mut buf)
        .ok()
        .map(str::to_owned)
    else {
        return;
    };
    let (name, props) = split_state(&state);
    let valid = pattern::Env::properties(&Registry, name);
    if valid.is_empty() {
        note("That block has no properties");
        return;
    }
    let prop = prop % valid.len();
    if !right {
        let next = (prop + 1) % valid.len();
        model(|m| {
            if let Some(b) = m.players.get_mut(&id).unwrap().binds.get_mut(item) {
                b.kind = Kind::Tool(Tool::Cycler { prop: next });
            }
        });
        note(format!("Selected property: {}", valid[next].0));
        return;
    }
    let (key, values) = &valid[prop];
    let current = props
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| (*v).to_owned())
        .unwrap_or_else(|| values[0].clone());
    let Some(new) = next_value(values, &current) else {
        return;
    };
    let mut merged: Vec<(String, String)> = props
        .iter()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    merged.push((key.clone(), new.clone()));
    let text = pattern::join_state(name, &merged);
    let mut cells = Cells::default();
    let it = cells.item(Item::State(text));
    cells.set(hit.at, it);
    run_cells(id, &hit.dim, cells);
    note(format!("{key} set to {new}"));
}

#[cfg(test)]
mod tests {
    use super::{Clip, Item, P, next_value, shift_terrain};

    fn ground(size: P, top: impl Fn(i32, i32) -> i32) -> Clip {
        let palette: Vec<String> = ["minecraft:air", "minecraft:stone"]
            .map(str::to_owned)
            .to_vec();
        let mut idx = Vec::new();
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    idx.push(u16::from(y <= top(x, z)));
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

    fn solid(cells: &crate::plan::Cells, p: P) -> Option<bool> {
        match cells.get(p)? {
            Item::State(s) => Some(s != "minecraft:air"),
            Item::Pat(_) => None,
        }
    }

    #[test]
    fn raise_grows_the_surface_only_inside_the_sphere() {
        let clip = ground([11, 11, 11], |_, _| 4);
        let cells = shift_terrain(&clip, [5, 5, 5], 3, 2, 8, true);
        assert_eq!(solid(&cells, [5, 5, 5]), Some(true));
        assert_eq!(solid(&cells, [5, 6, 5]), None);
        assert_eq!(solid(&cells, [8, 5, 6]), None);
        assert_eq!(solid(&cells, [7, 5, 5]), Some(true));
    }

    #[test]
    fn lower_strips_exposed_blocks_only_inside_the_sphere() {
        let clip = ground([11, 11, 11], |x, _| if x == 5 { 7 } else { 4 });
        let cells = shift_terrain(&clip, [5, 5, 5], 3, 2, 8, false);
        assert_eq!(solid(&cells, [5, 7, 5]), Some(false));
        assert_eq!(solid(&cells, [5, 6, 5]), Some(false));
        assert_eq!(solid(&cells, [5, 3, 5]), None);
        assert_eq!(solid(&cells, [8, 4, 5]), None);
        assert_eq!(solid(&cells, [5, 8, 5]), None);
    }

    #[test]
    fn cycling_wraps_around_the_allowed_values() {
        let v: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        assert_eq!(next_value(&v, "a").as_deref(), Some("b"));
        assert_eq!(next_value(&v, "c").as_deref(), Some("a"));
        assert_eq!(next_value(&v, "z"), None);
    }
}

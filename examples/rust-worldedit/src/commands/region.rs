// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::edit::Op;
use crate::edit::Registry;
use crate::edit::{Clip, Job};
use crate::jobs::{busy, edit, start};
use crate::mask;
use crate::pattern::{self, Pattern};
use crate::plan::{self, Cells, Item};
use crate::region::Region;
use crate::selection::{parse3, selection, set_pos};
use crate::session::{OVERWORLD, here, model, say};

pub const COMMANDS: &[Command] = &[
    Command {
        name: "pos1",
        aliases: &[],
        usage: "[x y z]",
        summary: "Set selection position 1",
        node: "worldedit.selection.pos",
        flags: "",
        run: pos1,
    },
    Command {
        name: "pos2",
        aliases: &[],
        usage: "[x y z]",
        summary: "Set selection position 2",
        node: "worldedit.selection.pos",
        flags: "",
        run: pos2,
    },
    Command {
        name: "desel",
        aliases: &["deselect"],
        usage: "",
        summary: "Clear the selection",
        node: "worldedit.selection.pos",
        flags: "",
        run: desel,
    },
    Command {
        name: "set",
        aliases: &[],
        usage: "<pattern>",
        summary: "Fill the selection with a pattern",
        node: "worldedit.region.set",
        flags: "",
        run: set,
    },
    Command {
        name: "regen",
        aliases: &[],
        usage: "[biome] [seed]",
        summary: "Regenerate the selection from the world generator",
        node: "worldedit.regen",
        flags: "",
        run: regen,
    },
    Command {
        name: "replace",
        aliases: &["re", "rep"],
        usage: "[from mask...] <to pattern>",
        summary: "Replace blocks matching a mask with a pattern",
        node: "worldedit.region.replace",
        flags: "",
        run: replace,
    },
    Command {
        name: "gmask",
        aliases: &["globalmask"],
        usage: "[mask...]",
        summary: "Set or clear the mask applied to every edit",
        node: "worldedit.global-mask",
        flags: "",
        run: gmask,
    },
    Command {
        name: "setblock",
        aliases: &[],
        usage: "<x> <y> <z> <state>",
        summary: "Set one block anywhere",
        node: "worldedit.region.set",
        flags: "",
        run: setblock,
    },
    Command {
        name: "walls",
        aliases: &[],
        usage: "<pattern>",
        summary: "Build the four walls of the selection",
        node: "worldedit.region.walls",
        flags: "",
        run: walls,
    },
    Command {
        name: "faces",
        aliases: &["outline"],
        usage: "<pattern>",
        summary: "Build the outer shell of the selection",
        node: "worldedit.region.faces",
        flags: "",
        run: faces,
    },
    Command {
        name: "overlay",
        aliases: &[],
        usage: "<pattern>",
        summary: "Place a pattern on top of the surface",
        node: "worldedit.region.overlay",
        flags: "",
        run: overlay,
    },
    Command {
        name: "center",
        aliases: &["middle"],
        usage: "<pattern>",
        summary: "Set the center block(s) of the selection",
        node: "worldedit.region.center",
        flags: "",
        run: center,
    },
    Command {
        name: "naturalize",
        aliases: &[],
        usage: "",
        summary: "Make the surface grass, dirt and stone",
        node: "worldedit.region.naturalize",
        flags: "",
        run: naturalize,
    },
    Command {
        name: "hollow",
        aliases: &[],
        usage: "[thickness] [pattern]",
        summary: "Hollow out the selection",
        node: "worldedit.region.hollow",
        flags: "",
        run: hollow,
    },
    Command {
        name: "smooth",
        aliases: &[],
        usage: "[iterations] [mask...]",
        summary: "Smooth the surface heightmap",
        node: "worldedit.region.smooth",
        flags: "",
        run: smooth,
    },
    Command {
        name: "move",
        aliases: &[],
        usage: "[count] [direction] [replace pattern]",
        summary: "Move the selection contents",
        node: "worldedit.region.move",
        flags: "sae",
        run: move_,
    },
    Command {
        name: "stack",
        aliases: &[],
        usage: "[count] [direction]",
        summary: "Repeat the selection contents",
        node: "worldedit.region.stack",
        flags: "sae",
        run: stack,
    },
    Command {
        name: "line",
        aliases: &[],
        usage: "<pattern> [thickness]",
        summary: "Draw a line between the two positions",
        node: "worldedit.generation.line",
        flags: "h",
        run: line,
    },
    Command {
        name: "curve",
        aliases: &[],
        usage: "<pattern> [thickness]",
        summary: "Draw a spline through the convex selection points",
        node: "worldedit.generation.curve",
        flags: "h",
        run: curve,
    },
    Command {
        name: "forest",
        aliases: &[],
        usage: "[type] [density]",
        summary: "Plant trees in the selection",
        node: "worldedit.region.forest",
        flags: "",
        run: forest,
    },
    Command {
        name: "flora",
        aliases: &[],
        usage: "[density]",
        summary: "Plant grass and flowers in the selection",
        node: "worldedit.region.flora",
        flags: "",
        run: flora,
    },
];

fn pos(ctx: &Ctx, second: bool, command: &Command) -> i32 {
    let (at, dim) = match (parse3(&ctx.args), here()) {
        (Some(at), Some(h)) => (at, h.dim),
        (None, Some(h)) => (h.block, h.dim),
        (Some(at), None) => (at, OVERWORLD.to_owned()),
        (None, None) => return say(usage(command)),
    };
    set_pos(ctx.id, second, at, &dim)
}

fn pos1(ctx: &Ctx) -> i32 {
    pos(ctx, false, &COMMANDS[0])
}

fn pos2(ctx: &Ctx) -> i32 {
    pos(ctx, true, &COMMANDS[1])
}

fn desel(ctx: &Ctx) -> i32 {
    model(|m| {
        let p = m.players.get_mut(&ctx.id).unwrap();
        p.sel.clear();
    });
    say("Selection cleared")
}

fn pattern_of(id: u128, text: &str) -> Result<Pattern, String> {
    let clip = model(|m| m.players[&id].clip.clone());
    pattern::parse(text, &Registry, clip.as_ref())
}

fn set(ctx: &Ctx) -> i32 {
    let Some(text) = ctx.args.first() else {
        return say(usage(&COMMANDS[3]));
    };
    let pattern = match pattern_of(ctx.id, text) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    match selection(ctx.id) {
        Ok((dim, r, v)) => edit(ctx.id, Op::Set(pattern), dim, r, v),
        Err(why) => say(why),
    }
}

fn regen(ctx: &Ctx) -> i32 {
    let (mut seed, mut biome) = (None, None);
    for arg in &ctx.args {
        match arg.parse::<i64>() {
            Ok(n) if seed.is_none() => seed = Some(n),
            Err(_) if biome.is_none() => biome = Some(super::biome::biome_name(arg)),
            _ => return say(usage(&COMMANDS[4])),
        }
    }
    match selection(ctx.id) {
        Ok((dim, r, v)) => edit(ctx.id, Op::Regen { seed, biome }, dim, r, v),
        Err(why) => say(why),
    }
}

fn replace(ctx: &Ctx) -> i32 {
    let Some((to, from)) = ctx.args.split_last() else {
        return say(usage(&COMMANDS[5]));
    };
    let from = match from.is_empty() {
        true => Ok(mask::Mask::Existing),
        false => mask::parse(from, &Registry),
    };
    let (from, to) = match (from, pattern_of(ctx.id, to)) {
        (Ok(f), Ok(t)) => (f, t),
        (Err(why), _) | (_, Err(why)) => return say(why),
    };
    match selection(ctx.id) {
        Ok((dim, r, v)) => edit(ctx.id, Op::Replace(from, to), dim, r, v),
        Err(why) => say(why),
    }
}

fn gmask(ctx: &Ctx) -> i32 {
    if ctx.args.is_empty() {
        model(|m| m.players.get_mut(&ctx.id).unwrap().gmask = None);
        return say("Global mask disabled");
    }
    match mask::parse(&ctx.args, &Registry) {
        Ok(mask) => {
            model(|m| m.players.get_mut(&ctx.id).unwrap().gmask = Some(mask));
            say("Global mask set")
        }
        Err(why) => say(why),
    }
}

fn setblock(ctx: &Ctx) -> i32 {
    match (parse3(&ctx.args), ctx.args.get(3)) {
        (Some(at), Some(text)) => {
            let pattern = match pattern_of(ctx.id, text) {
                Ok(p) => p,
                Err(why) => return say(why),
            };
            let dim = here().map_or_else(|| OVERWORLD.to_owned(), |h| h.dim);
            edit(ctx.id, Op::Set(pattern), dim, Region::cuboid(at, at), 1)
        }
        _ => say(usage(&COMMANDS[7])),
    }
}

fn index(name: &str) -> usize {
    COMMANDS.iter().position(|c| c.name == name).unwrap()
}

fn run_plan(id: u128, dim: String, cells: Cells) -> i32 {
    let limit = model(|m| crate::selection::limit_of(&m.players[&id]));
    let Some((a, b)) = cells.bounds() else {
        return say("Nothing to change");
    };
    if cells.len() as u64 > limit {
        return say(format!(
            "That edit touches {} blocks, over your limit of {limit}",
            cells.len()
        ));
    }
    if let Some(why) = busy(id) {
        return say(why);
    }
    let gmask = model(|m| m.players[&id].gmask.clone());
    let total = cells.len() as u64;
    start(Job::new(id, dim, Op::Cells(cells), a, b, total).gmask(gmask))
}

fn snapshot(dim: &str, region: &Region, up: i32, pad: i32) -> Result<Clip, String> {
    let (min, max) = region.bounds();
    let hi = [
        max[0] + pad,
        (max[1] + up).min(crate::region::WORLD_Y[1]),
        max[2] + pad,
    ];
    Clip::capture(dim, [min[0] - pad, min[1], min[2] - pad], hi)
        .ok_or_else(|| "The blocks could not be read".to_owned())
}

fn with_pattern(
    ctx: &Ctx,
    name: &str,
    f: impl FnOnce(&Region, &str, u32, &mut Cells, &Clip) -> Option<Cells>,
) -> i32 {
    let command = &COMMANDS[index(name)];
    let Some(text) = ctx.args.first() else {
        return say(usage(command));
    };
    let pattern = match pattern_of(ctx.id, text) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let clip = match snapshot(&dim, &region, 1, 0) {
        Ok(c) => c,
        Err(why) => return say(why),
    };
    let mut scratch = Cells::default();
    let item = scratch.item(Item::Pat(pattern));
    match f(&region, &dim, item, &mut scratch, &clip) {
        Some(cells) => run_plan(ctx.id, dim, cells),
        None => say("Nothing to change"),
    }
}

fn walls(ctx: &Ctx) -> i32 {
    with_pattern(ctx, "walls", |r, _, item, scratch, _| {
        Some(rebase(plan::walls(r, 0), scratch, item))
    })
}

fn faces(ctx: &Ctx) -> i32 {
    with_pattern(ctx, "faces", |r, _, item, scratch, _| {
        Some(rebase(plan::faces(r, 0), scratch, item))
    })
}

fn overlay(ctx: &Ctx) -> i32 {
    with_pattern(ctx, "overlay", |r, _, item, scratch, clip| {
        Some(rebase(plan::overlay(clip, r, 0), scratch, item))
    })
}

fn center(ctx: &Ctx) -> i32 {
    with_pattern(ctx, "center", |r, _, item, scratch, _| {
        let mut cells = Cells::default();
        let id = take(scratch, &mut cells, item);
        plan::at(&mut cells, &plan::center(r), id);
        Some(cells)
    })
}

fn take(scratch: &mut Cells, into: &mut Cells, item: u32) -> u32 {
    into.item(scratch.take(item))
}

fn rebase(mut cells: Cells, scratch: &mut Cells, item: u32) -> Cells {
    cells.replace_item(0, scratch.take(item));
    cells
}

fn naturalize(ctx: &Ctx) -> i32 {
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    match snapshot(&dim, &region, 0, 0) {
        Ok(clip) => run_plan(ctx.id, dim, plan::naturalize(&clip, &region)),
        Err(why) => say(why),
    }
}

fn number(text: Option<&&str>) -> Option<i32> {
    text.and_then(|t| t.parse().ok())
}

fn hollow(ctx: &Ctx) -> i32 {
    let (thickness, rest) = match number(ctx.args.first()) {
        Some(n) => (n.max(1), &ctx.args[1..]),
        None => (1, &ctx.args[..]),
    };
    let item = match rest.first() {
        Some(text) => match pattern_of(ctx.id, text) {
            Ok(p) => Item::Pat(p),
            Err(why) => return say(why),
        },
        None => Item::State("minecraft:air".to_owned()),
    };
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let mut cells = plan::hollow(&region, thickness, 0);
    cells.replace_item(0, item);
    run_plan(ctx.id, dim, cells)
}

fn smooth(ctx: &Ctx) -> i32 {
    let (iterations, rest) = match number(ctx.args.first()) {
        Some(n) => (n.clamp(1, 64) as u32, &ctx.args[1..]),
        None => (1, &ctx.args[..]),
    };
    let mask = match rest.is_empty() {
        true => None,
        false => match mask::parse(rest, &Registry) {
            Ok(m) => Some(m),
            Err(why) => return say(why),
        },
    };
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let clip = match snapshot(&dim, &region, 0, 0) {
        Ok(c) => c,
        Err(why) => return say(why),
    };
    let solid = |state: &str, pos| match &mask {
        Some(m) => m.test(&Registry, pos, state),
        None => !crate::edit::is_air(state),
    };
    run_plan(
        ctx.id,
        dim,
        plan::smooth(&clip, &region, iterations, &solid),
    )
}

fn direction(text: Option<&&str>) -> Result<[i32; 3], String> {
    super::select::unit(text.copied().unwrap_or("me"))
}

fn move_(ctx: &Ctx) -> i32 {
    let count = number(ctx.args.first()).unwrap_or(1).max(1);
    let dir = match direction(ctx.args.get(1)) {
        Ok(d) => d,
        Err(why) => return say(why),
    };
    let clear = match ctx.args.get(2) {
        Some(text) => match pattern_of(ctx.id, text) {
            Ok(p) => Item::Pat(p),
            Err(why) => return say(why),
        },
        None => Item::State("minecraft:air".to_owned()),
    };
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let Some(clip) = Clip::capture(&dim, region.bounds().0, region.bounds().1) else {
        return say("The blocks could not be read");
    };
    let by = dir.map(|v| v * count);
    let mut cells = Cells::default();
    let clear = cells.item(clear);
    plan::moved(&clip, &region, by, clear, &mut cells, ctx.flag('a'));
    if ctx.flag('s') {
        model(|m| m.players.get_mut(&ctx.id).unwrap().sel.shift(by));
    }
    run_plan(ctx.id, dim, cells)
}

fn stack(ctx: &Ctx) -> i32 {
    let count = number(ctx.args.first()).unwrap_or(1).clamp(1, 1024);
    let dir = match direction(ctx.args.get(1)) {
        Ok(d) => d,
        Err(why) => return say(why),
    };
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let Some(clip) = Clip::capture(&dim, region.bounds().0, region.bounds().1) else {
        return say("The blocks could not be read");
    };
    let cells = plan::stack(&clip, &region, dir, count, ctx.flag('a'));
    if ctx.flag('s') {
        let size = region.size();
        let span = (0..3).map(|a| dir[a].abs() * size[a]).sum::<i32>() * count;
        model(|m| {
            let sel = &mut m.players.get_mut(&ctx.id).unwrap().sel;
            if let (true, Some(a), Some(b)) = (sel.boxed(), sel.a, sel.b) {
                let (lo, hi) = crate::edit::order(a, b);
                let grow = |p: [i32; 3], sign: i32| {
                    [0, 1, 2].map(|k| {
                        p[k] + if dir[k] * sign > 0 {
                            span * dir[k].abs()
                        } else {
                            0
                        }
                    })
                };
                sel.a = Some([0, 1, 2].map(|k| if dir[k] < 0 { lo[k] - span } else { lo[k] }));
                sel.b = Some(grow(hi, 1));
            }
        });
    }
    run_plan(ctx.id, dim, cells)
}

fn drawn(ctx: &Ctx, path: Vec<[i32; 3]>, name: &str) -> i32 {
    let Some(text) = ctx.args.first() else {
        return say(usage(&COMMANDS[index(name)]));
    };
    let item = match pattern_of(ctx.id, text) {
        Ok(p) => Item::Pat(p),
        Err(why) => return say(why),
    };
    let thickness = number(ctx.args.get(1)).unwrap_or(0).clamp(0, 16);
    let dim = model(|m| m.players[&ctx.id].dim.clone());
    let mut cells = plan::draw(&path, thickness, ctx.flag('h'), 0);
    cells.replace_item(0, item);
    run_plan(ctx.id, dim, cells)
}

fn line(ctx: &Ctx) -> i32 {
    let ends = model(|m| {
        let sel = &m.players[&ctx.id].sel;
        sel.a.zip(sel.b)
    });
    match ends {
        Some((a, b)) => drawn(ctx, vec![a, b], "line"),
        None => say("Select two positions first (//pos1, //pos2)"),
    }
}

fn curve(ctx: &Ctx) -> i32 {
    let pts = model(|m| {
        let sel = &m.players[&ctx.id].sel;
        (sel.kind == crate::selection::Kind::Convex).then(|| sel.pts.clone())
    });
    match pts {
        Some(pts) if pts.len() >= 2 => drawn(ctx, plan::spline_points(&pts), "curve"),
        Some(_) => say("A curve needs at least two points"),
        None => say("A curve needs a convex selection (//sel convex)"),
    }
}

fn forest(ctx: &Ctx) -> i32 {
    let (kind, density) = match ctx.args.as_slice() {
        [] => ("oak", 5.0),
        [a] if a.parse::<f64>().is_ok() => ("oak", a.parse().unwrap()),
        [kind] => (*kind, 5.0),
        [kind, d, ..] => match d.parse::<f64>() {
            Ok(d) => (*kind, d),
            Err(_) => return say(usage(&COMMANDS[index("forest")])),
        },
    };
    if !plan::TREES.contains(&kind) {
        return say(format!(
            "Unknown tree {kind}; try {}",
            plan::TREES.join(", ")
        ));
    }
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let clip = match snapshot(&dim, &region, 9, 2) {
        Ok(c) => c,
        Err(why) => return say(why),
    };
    let cells = plan::forest(
        &clip,
        &region,
        kind,
        density.clamp(0.0, 100.0) / 100.0,
        &Registry,
    );
    run_plan(ctx.id, dim, cells)
}

fn flora(ctx: &Ctx) -> i32 {
    let density = number(ctx.args.first()).unwrap_or(10).clamp(0, 100);
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let clip = match snapshot(&dim, &region, 1, 0) {
        Ok(c) => c,
        Err(why) => return say(why),
    };
    run_plan(
        ctx.id,
        dim,
        plan::flora(&clip, &region, f64::from(density) / 100.0, &Registry),
    )
}

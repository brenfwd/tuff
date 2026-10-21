// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use super::{Command, Ctx, usage};
use crate::edit::{matches, normal, scan};
use crate::region::{P, WORLD_Y};
use crate::selection::{Kind, current, selection, set_pos};
use crate::session::{here, look, model, say};

const REACH: i32 = 300;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "sel",
        aliases: &["select"],
        usage: "[cuboid|extend|poly|ellipsoid|sphere|cyl|convex]",
        summary: "Choose the selection type, or clear the selection",
        node: "worldedit.selection.select",
        flags: "",
        run: sel,
    },
    Command {
        name: "cui",
        aliases: &[],
        usage: "",
        summary: "Resend the selection to the WorldEditCUI client mod",
        node: "worldedit.cui",
        flags: "",
        run: cui,
    },
    Command {
        name: "drawsel",
        aliases: &[],
        usage: "",
        summary: "Toggle sending the selection to WorldEditCUI",
        node: "worldedit.drawsel",
        flags: "",
        run: drawsel,
    },
    Command {
        name: "hpos1",
        aliases: &[],
        usage: "",
        summary: "Set position 1 to the block you look at",
        node: "worldedit.selection.hpos",
        flags: "",
        run: hpos1,
    },
    Command {
        name: "hpos2",
        aliases: &[],
        usage: "",
        summary: "Set position 2 to the block you look at",
        node: "worldedit.selection.hpos",
        flags: "",
        run: hpos2,
    },
    Command {
        name: "expand",
        aliases: &[],
        usage: "<amount|vert> [reverse] [direction]",
        summary: "Expand the selection",
        node: "worldedit.selection.expand",
        flags: "",
        run: expand,
    },
    Command {
        name: "contract",
        aliases: &[],
        usage: "<amount> [reverse] [direction]",
        summary: "Contract the selection",
        node: "worldedit.selection.contract",
        flags: "",
        run: contract,
    },
    Command {
        name: "shift",
        aliases: &[],
        usage: "<amount> [direction]",
        summary: "Move the selection",
        node: "worldedit.selection.shift",
        flags: "",
        run: shift,
    },
    Command {
        name: "outset",
        aliases: &[],
        usage: "<amount>",
        summary: "Expand the selection in all directions (-h horizontal, -v vertical)",
        node: "worldedit.selection.outset",
        flags: "hv",
        run: outset,
    },
    Command {
        name: "inset",
        aliases: &[],
        usage: "<amount>",
        summary: "Contract the selection in all directions (-h horizontal, -v vertical)",
        node: "worldedit.selection.inset",
        flags: "hv",
        run: inset,
    },
    Command {
        name: "size",
        aliases: &[],
        usage: "",
        summary: "Describe the selection",
        node: "worldedit.selection.size",
        flags: "",
        run: size,
    },
    Command {
        name: "count",
        aliases: &[],
        usage: "<block>",
        summary: "Count the matching blocks in the selection",
        node: "worldedit.analysis.count",
        flags: "",
        run: count,
    },
    Command {
        name: "distr",
        aliases: &["distribution"],
        usage: "[-c]",
        summary: "List the blocks in the selection (-c the clipboard)",
        node: "worldedit.analysis.distr",
        flags: "c",
        run: distr,
    },
    Command {
        name: "chunk",
        aliases: &[],
        usage: "[chunkX chunkZ]",
        summary: "Select the chunk you stand in",
        node: "worldedit.selection.chunk",
        flags: "",
        run: chunk,
    },
];

fn sel(ctx: &Ctx) -> i32 {
    let Some(name) = ctx.args.first() else {
        model(|m| m.players.get_mut(&ctx.id).unwrap().sel.clear());
        return say("Selection cleared");
    };
    let Some(kind) = Kind::parse(name) else {
        return say(usage(&COMMANDS[0]));
    };
    model(|m| m.players.get_mut(&ctx.id).unwrap().sel.set_kind(kind));
    say(format!("Selection type: {} ({})", kind.name(), kind.wand()))
}

fn cui(ctx: &Ctx) -> i32 {
    if !model(|m| m.players[&ctx.id].cui) {
        return say("WorldEditCUI was not detected; install the client mod");
    }
    crate::cui::resend(ctx.id);
    say("WorldEditCUI selection resent")
}

fn drawsel(ctx: &Ctx) -> i32 {
    let on = !model(|m| m.players[&ctx.id].draw);
    crate::cui::set_draw(ctx.id, on);
    say(if on {
        "Selection drawing enabled"
    } else {
        "Selection drawing disabled"
    })
}

pub fn unit(name: &str) -> Result<P, String> {
    let facing = || {
        let (yaw, _, _) = look().ok_or("Your look direction is unknown".to_owned())?;
        Ok::<_, String>(((yaw / 90.0).round() as i32).rem_euclid(4))
    };
    const CARDINAL: [P; 4] = [[0, 0, 1], [-1, 0, 0], [0, 0, -1], [1, 0, 0]];
    let vertical = || {
        let (_, pitch, _) = look().ok_or("Your look direction is unknown".to_owned())?;
        Ok::<_, String>(if pitch > 67.5 {
            Some([0, -1, 0])
        } else if pitch < -67.5 {
            Some([0, 1, 0])
        } else {
            None
        })
    };
    let me = || -> Result<P, String> {
        match vertical()? {
            Some(v) => Ok(v),
            None => Ok(CARDINAL[facing()? as usize]),
        }
    };
    let neg = |p: P| p.map(|v| -v);
    Ok(match name {
        "n" | "north" => [0, 0, -1],
        "s" | "south" => [0, 0, 1],
        "e" | "east" => [1, 0, 0],
        "w" | "west" => [-1, 0, 0],
        "u" | "up" => [0, 1, 0],
        "d" | "down" => [0, -1, 0],
        "me" | "forward" => me()?,
        "back" => neg(me()?),
        "right" => CARDINAL[((facing()? + 1) % 4) as usize],
        "left" => CARDINAL[((facing()? + 3) % 4) as usize],
        _ => return Err(format!("Unknown direction {name}")),
    })
}

fn resolve(ctx: &Ctx, command: usize) -> Result<(i32, Option<i32>, P), String> {
    let usage = usage(&COMMANDS[command]);
    let amount: i32 = ctx
        .args
        .first()
        .and_then(|a| a.parse().ok())
        .filter(|a| *a >= 0)
        .ok_or(usage.clone())?;
    let mut rest = ctx.args[1..].iter();
    let mut reverse = None;
    let mut name = rest.next();
    if let Some(n) = name.and_then(|t| t.parse::<i32>().ok()) {
        reverse = Some(n.max(0));
        name = rest.next();
    }
    let dir = unit(name.copied().unwrap_or("me"))?;
    Ok((amount, reverse, dir))
}

fn grow(dir: P, amount: i32, out: &mut [i32; 6]) {
    for axis in 0..3 {
        match dir[axis].signum() {
            1 => out[axis + 3] += amount,
            -1 => out[axis] += amount,
            _ => {}
        }
    }
}

fn resize(ctx: &Ctx, sign: i32, command: usize) -> i32 {
    if ctx.args.first() == Some(&"vert") && command == 3 {
        let result = model(|m| m.players.get_mut(&ctx.id).unwrap().sel.full_height());
        return match result {
            Ok(()) => say(format!(
                "Expanded vertically to {} - {}",
                WORLD_Y[0], WORLD_Y[1]
            )),
            Err(why) => say(why),
        };
    }
    let (amount, reverse, dir) = match resolve(ctx, command) {
        Ok(v) => v,
        Err(why) => return say(why),
    };
    let mut change = [0; 6];
    grow(dir, sign * amount, &mut change);
    if let Some(r) = reverse {
        grow(dir.map(|v| -v), sign * r, &mut change);
    }
    let result = model(|m| {
        let sel = &mut m.players.get_mut(&ctx.id).unwrap().sel;
        sel.resize(change)?;
        Ok::<_, String>(sel.region().map_or(0, |r| r.volume()))
    });
    match result {
        Ok(volume) => say(format!(
            "Region {}: now {volume} blocks",
            if sign > 0 { "expanded" } else { "contracted" }
        )),
        Err(why) => say(why),
    }
}

fn expand(ctx: &Ctx) -> i32 {
    resize(ctx, 1, 3)
}

fn contract(ctx: &Ctx) -> i32 {
    resize(ctx, -1, 4)
}

fn shift(ctx: &Ctx) -> i32 {
    let amount: i32 = match ctx.args.first().and_then(|a| a.parse().ok()) {
        Some(a) => a,
        None => return say(usage(&COMMANDS[5])),
    };
    let dir = match unit(ctx.args.get(1).copied().unwrap_or("me")) {
        Ok(d) => d,
        Err(why) => return say(why),
    };
    let ok = model(|m| {
        let sel = &mut m.players.get_mut(&ctx.id).unwrap().sel;
        sel.region()
            .map(|_| sel.shift(dir.map(|v| v * amount)))
            .is_some()
    });
    say(if ok {
        "Selection shifted"
    } else {
        "Make a selection first"
    })
}

fn all_around(ctx: &Ctx, sign: i32, command: usize) -> i32 {
    let Some(amount) = ctx
        .args
        .first()
        .and_then(|a| a.parse::<i32>().ok())
        .filter(|a| *a >= 0)
    else {
        return say(usage(&COMMANDS[command]));
    };
    let h = if ctx.flag('v') && !ctx.flag('h') {
        0
    } else {
        sign * amount
    };
    let v = if ctx.flag('h') && !ctx.flag('v') {
        0
    } else {
        sign * amount
    };
    let result = model(|m| {
        let sel = &mut m.players.get_mut(&ctx.id).unwrap().sel;
        sel.resize([h, v, h, h, v, h])?;
        Ok::<_, String>(sel.region().map_or(0, |r| r.volume()))
    });
    match result {
        Ok(volume) => say(format!("Selection is now {volume} blocks")),
        Err(why) => say(why),
    }
}

fn outset(ctx: &Ctx) -> i32 {
    all_around(ctx, 1, 6)
}

fn inset(ctx: &Ctx) -> i32 {
    all_around(ctx, -1, 7)
}

fn size(ctx: &Ctx) -> i32 {
    let Ok((_, region)) = current(ctx.id) else {
        return say("Make a selection first");
    };
    let kind = model(|m| m.players[&ctx.id].sel.kind);
    let (min, max) = region.bounds();
    let s = region.size();
    say(format!(
        "Type: {}\nMinimum: {} {} {}\nMaximum: {} {} {}\nSize: {} x {} x {}\nNumber of blocks: {}",
        kind.name(),
        min[0],
        min[1],
        min[2],
        max[0],
        max[1],
        max[2],
        s[0],
        s[1],
        s[2],
        region.volume()
    ))
}

fn count(ctx: &Ctx) -> i32 {
    let Some(state) = ctx.args.first() else {
        return say(usage(&COMMANDS[9]));
    };
    let from = normal(state);
    let (dim, region, _) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let mut n = 0u64;
    match scan(&dim, &region, |s| n += u64::from(matches(s, &from))) {
        Some(()) => say(format!("Counted: {n}")),
        None => say("Could not read the selection"),
    }
}

fn distr(ctx: &Ctx) -> i32 {
    let mut seen: BTreeMap<String, u64> = BTreeMap::new();
    let mut note = |state: &str| {
        let name = state.split('[').next().unwrap_or(state);
        if !name.is_empty() {
            *seen.entry(name.to_owned()).or_default() += 1;
        }
    };
    if ctx.flag('c') {
        let Some(clip) = model(|m| m.players[&ctx.id].clip.clone()) else {
            return say("Your clipboard is empty (//copy)");
        };
        for i in &clip.idx {
            if let Some(state) = clip.palette.get(*i as usize) {
                note(state);
            }
        }
    } else {
        let (dim, region, _) = match selection(ctx.id) {
            Ok(s) => s,
            Err(why) => return say(why),
        };
        if scan(&dim, &region, &mut note).is_none() {
            return say("Could not read the selection");
        }
    }
    let total: u64 = seen.values().sum();
    let mut rows: Vec<_> = seen.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut text = format!("Total block count: {total}");
    for (name, n) in rows {
        let pct = n as f64 * 100.0 / total.max(1) as f64;
        text.push_str(&format!("\n{n} ({pct:.3}%) {name}"));
    }
    say(text)
}

fn chunk(ctx: &Ctx) -> i32 {
    let at = match (ctx.args.first(), ctx.args.get(1)) {
        (Some(x), Some(z)) => match (x.parse::<i32>(), z.parse::<i32>()) {
            (Ok(x), Ok(z)) => Some((x, z, here().map(|h| h.dim))),
            _ => return say(usage(&COMMANDS[11])),
        },
        (None, _) => here().map(|h| {
            (
                h.block[0].div_euclid(16),
                h.block[2].div_euclid(16),
                Some(h.dim),
            )
        }),
        _ => return say(usage(&COMMANDS[11])),
    };
    let Some((cx, cz, dim)) = at else {
        return say("WorldEdit needs your position");
    };
    model(|m| {
        let p = m.players.get_mut(&ctx.id).unwrap();
        if let Some(dim) = dim {
            p.dim = dim;
        }
        p.sel.set_kind(Kind::Cuboid);
        p.sel.a = Some([cx * 16, WORLD_Y[0], cz * 16]);
        p.sel.b = Some([cx * 16 + 15, WORLD_Y[1], cz * 16 + 15]);
    });
    say(format!("Chunk selected: {cx}, {cz}"))
}

pub fn trace(dim: &str, reach: i32) -> Option<(P, P)> {
    let (yaw, pitch, eye) = look()?;
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let dir = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let mut cell = eye.map(|v| v.floor() as i32);
    let step = dir.map(|d| if d > 0.0 { 1 } else { -1 });
    let delta = dir.map(|d| {
        if d == 0.0 {
            f64::INFINITY
        } else {
            (1.0 / d).abs()
        }
    });
    let mut max = [0, 1, 2].map(|i| {
        if dir[i] == 0.0 {
            f64::INFINITY
        } else {
            let edge = if dir[i] > 0.0 {
                cell[i] as f64 + 1.0
            } else {
                cell[i] as f64
            };
            (edge - eye[i]) / dir[i]
        }
    });
    let mut buf = [0u8; 256];
    let mut prev = cell;
    for _ in 0..reach {
        if let Ok(state) = tuff_plugin_sdk::block_get(dim, cell, &mut buf)
            && !state.is_empty()
            && !state.contains("air")
        {
            return Some((cell, prev));
        }
        prev = cell;
        let axis = (0..3).min_by(|a, b| max[*a].total_cmp(&max[*b])).unwrap();
        cell[axis] += step[axis];
        max[axis] += delta[axis];
    }
    None
}

fn hit(dim: &str) -> Option<P> {
    trace(dim, REACH).map(|(cell, _)| cell)
}

fn hpos(ctx: &Ctx, second: bool) -> i32 {
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    match hit(&h.dim) {
        Some(at) => set_pos(ctx.id, second, at, &h.dim),
        None => say("No block in sight (or your look direction is unknown)"),
    }
}

fn hpos1(ctx: &Ctx) -> i32 {
    hpos(ctx, false)
}

fn hpos2(ctx: &Ctx) -> i32 {
    hpos(ctx, true)
}

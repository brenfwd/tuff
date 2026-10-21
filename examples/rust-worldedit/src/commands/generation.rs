// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::edit::Registry;
use crate::edit::{Clip, Op};
use crate::expr::{Deform, Expr, Frame, Gen};
use crate::jobs::edit;
use crate::pattern;
use crate::region::Region;
use crate::selection::{limit_of, selection};
use crate::session::{here, model, say};
use std::rc::Rc;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "sphere",
        aliases: &["sph"],
        usage: "<pattern> <radius>[,<radiusY>,<radiusZ>] [raised]",
        summary: "Generate a filled sphere or ellipsoid",
        node: "worldedit.generation.sphere",
        flags: "",
        run: sphere,
    },
    Command {
        name: "hsphere",
        aliases: &["hsph"],
        usage: "<pattern> <radius>[,<radiusY>,<radiusZ>] [raised]",
        summary: "Generate a hollow sphere or ellipsoid",
        node: "worldedit.generation.sphere",
        flags: "",
        run: hsphere,
    },
    Command {
        name: "cyl",
        aliases: &[],
        usage: "<pattern> <radius>[,<radiusZ>] [height]",
        summary: "Generate a filled cylinder",
        node: "worldedit.generation.cylinder",
        flags: "",
        run: cyl,
    },
    Command {
        name: "hcyl",
        aliases: &[],
        usage: "<pattern> <radius>[,<radiusZ>] [height]",
        summary: "Generate a hollow cylinder",
        node: "worldedit.generation.cylinder",
        flags: "",
        run: hcyl,
    },
    Command {
        name: "pyramid",
        aliases: &["pyr"],
        usage: "<pattern> <size>",
        summary: "Generate a filled pyramid",
        node: "worldedit.generation.pyramid",
        flags: "",
        run: pyramid,
    },
    Command {
        name: "hpyramid",
        aliases: &["hpyr"],
        usage: "<pattern> <size>",
        summary: "Generate a hollow pyramid",
        node: "worldedit.generation.pyramid",
        flags: "",
        run: hpyramid,
    },
    Command {
        name: "generate",
        aliases: &["gen", "g"],
        usage: "[-hroc] <pattern> <expression>",
        summary: "Fill the selection where an expression is positive",
        node: "worldedit.generation.shape",
        flags: "hroc",
        run: generate,
    },
    Command {
        name: "deform",
        aliases: &[],
        usage: "[-roc] <expression>",
        summary: "Rework the selection by moving each block's source through an expression",
        node: "worldedit.region.deform",
        flags: "roc",
        run: deform,
    },
];

const MAX_RADIUS: i32 = 512;

fn radii(text: &str) -> Result<Vec<i32>, String> {
    let parsed: Result<Vec<i32>, _> = text.split(',').map(str::parse::<i32>).collect();
    match parsed {
        Ok(v) if !v.is_empty() && v.iter().all(|r| (0..=MAX_RADIUS).contains(r)) => Ok(v),
        _ => Err(format!("Radii must be numbers from 0 to {MAX_RADIUS}")),
    }
}

fn expand(list: Vec<i32>, want: usize) -> Result<Vec<i32>, String> {
    match list.len() {
        1 => Ok(vec![list[0]; want]),
        n if n == want => Ok(list),
        _ => Err(format!("Give one radius or {want} radii")),
    }
}

fn spawn(ctx: &Ctx, region: Result<Region, String>, command: &Command) -> i32 {
    let region = match region {
        Ok(r) => r,
        Err(why) => return say(why),
    };
    let Some(text) = ctx.args.first() else {
        return say(usage(command));
    };
    let clip = model(|m| m.players[&ctx.id].clip.clone());
    let pattern = match pattern::parse(text, &Registry, clip.as_ref()) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let region = region.shifted(h.block);
    let volume = region.volume();
    let limit = model(|m| limit_of(&m.players[&ctx.id]));
    if volume > limit {
        return say(format!(
            "That shape has {volume} blocks, over your limit of {limit}"
        ));
    }
    edit(ctx.id, Op::Set(pattern), h.dim, region, volume)
}

fn sphere_region(ctx: &Ctx, hollow: bool, command: &Command) -> Result<Region, String> {
    let Some(text) = ctx.args.get(1) else {
        return Err(usage(command));
    };
    let r = expand(radii(text)?, 3)?;
    let raised = matches!(ctx.args.get(2), Some(w) if *w == "true" || *w == "raised");
    let c = [0, if raised { r[1] } else { 0 }, 0];
    let base = Region::Ellipsoid {
        c,
        r: [r[0], r[1], r[2]],
    };
    Ok(if hollow {
        Region::Shell {
            base: Box::new(base),
            flat: false,
        }
    } else {
        base
    })
}

fn cyl_region(ctx: &Ctx, hollow: bool, command: &Command) -> Result<Region, String> {
    let Some(text) = ctx.args.get(1) else {
        return Err(usage(command));
    };
    let r = expand(radii(text)?, 2)?;
    let height = match ctx.args.get(2) {
        Some(h) => h
            .parse::<i32>()
            .ok()
            .filter(|h| (1..=384).contains(h))
            .ok_or("Height must be from 1 to 384")?,
        None => 1,
    };
    let base = Region::Cyl {
        c: [0, 0, 0],
        r: [r[0], r[1]],
        y: [0, height - 1],
    };
    Ok(if hollow {
        Region::Shell {
            base: Box::new(base),
            flat: true,
        }
    } else {
        base
    })
}

fn pyramid_region(ctx: &Ctx, hollow: bool, command: &Command) -> Result<Region, String> {
    let Some(text) = ctx.args.get(1) else {
        return Err(usage(command));
    };
    match text.parse::<i32>() {
        Ok(n) if (1..=MAX_RADIUS).contains(&n) => Ok(Region::Pyramid {
            c: [0, 0, 0],
            n,
            hollow,
        }),
        _ => Err(format!("Size must be from 1 to {MAX_RADIUS}")),
    }
}

fn sphere(ctx: &Ctx) -> i32 {
    spawn(ctx, sphere_region(ctx, false, &COMMANDS[0]), &COMMANDS[0])
}

fn hsphere(ctx: &Ctx) -> i32 {
    spawn(ctx, sphere_region(ctx, true, &COMMANDS[1]), &COMMANDS[1])
}

fn cyl(ctx: &Ctx) -> i32 {
    spawn(ctx, cyl_region(ctx, false, &COMMANDS[2]), &COMMANDS[2])
}

fn hcyl(ctx: &Ctx) -> i32 {
    spawn(ctx, cyl_region(ctx, true, &COMMANDS[3]), &COMMANDS[3])
}

fn pyramid(ctx: &Ctx) -> i32 {
    spawn(ctx, pyramid_region(ctx, false, &COMMANDS[4]), &COMMANDS[4])
}

fn hpyramid(ctx: &Ctx) -> i32 {
    spawn(ctx, pyramid_region(ctx, true, &COMMANDS[5]), &COMMANDS[5])
}

pub(super) fn frame(ctx: &Ctx, min: [i32; 3], max: [i32; 3]) -> Result<Frame, String> {
    Ok(match (ctx.flag('r'), ctx.flag('o'), ctx.flag('c')) {
        (true, _, _) => Frame::raw([0.0; 3]),
        (_, true, _) => {
            let h = here().ok_or("WorldEdit needs your position")?;
            Frame::raw(h.block.map(f64::from))
        }
        (_, _, true) => Frame::centre(min, max),
        _ => Frame::unit(min, max),
    })
}

fn generate(ctx: &Ctx) -> i32 {
    let command = &COMMANDS[6];
    if ctx.args.len() < 2 {
        return say(usage(command));
    }
    let clip = model(|m| m.players[&ctx.id].clip.clone());
    let pattern = match pattern::parse(ctx.args[0], &Registry, clip.as_ref()) {
        Ok(p) => p,
        Err(why) => return say(why),
    };
    let expr = match Expr::parse(&ctx.args[1..].join(" ")) {
        Ok(e) => e,
        Err(why) => return say(format!("Bad expression: {why}")),
    };
    let (dim, region, volume) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let (min, max) = region.bounds();
    let frame = match frame(ctx, min, max) {
        Ok(f) => f,
        Err(why) => return say(why),
    };
    let gen_ = Gen {
        expr: Rc::new(expr),
        frame,
        min,
        max,
        hollow: ctx.flag('h'),
    };
    if let Err(why) = gen_.check(0) {
        return say(format!("Expression failed: {why}"));
    }
    edit(ctx.id, Op::Gen(gen_, pattern), dim, region, volume)
}

fn deform(ctx: &Ctx) -> i32 {
    if ctx.args.is_empty() {
        return say(usage(&COMMANDS[7]));
    }
    let expr = match Expr::parse(&ctx.args.join(" ")) {
        Ok(e) => e,
        Err(why) => return say(format!("Bad expression: {why}")),
    };
    let (dim, region, volume) = match selection(ctx.id) {
        Ok(s) => s,
        Err(why) => return say(why),
    };
    let (min, max) = region.bounds();
    let frame = match frame(ctx, min, max) {
        Ok(f) => f,
        Err(why) => return say(why),
    };
    let Some(clip) = Clip::capture(&dim, min, max) else {
        return say("The blocks could not be read");
    };
    let d = Deform {
        expr: Rc::new(expr),
        frame,
        clip,
    };
    let probe = Gen {
        expr: d.expr.clone(),
        frame: d.frame.clone(),
        min,
        max,
        hollow: false,
    };
    if let Err(why) = probe.check(0) {
        return say(format!("Expression failed: {why}"));
    }
    edit(ctx.id, Op::Deform(d), dim, region, volume)
}

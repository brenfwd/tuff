// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use std::rc::Rc;

use super::generation::frame;
use super::{Command, Ctx, usage};
use crate::expr::{Expr, Gen};
use crate::edit::Job;
use crate::jobs::{busy, start};
use crate::selection::current;
use crate::session::{here, say};
use tuff_plugin_sdk::world::{World, pos as block_pos};

const INFO_CELLS: u64 = 1024;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "setbiome",
        aliases: &[],
        usage: "<biome>",
        summary: "Set the biome of the selection",
        node: "worldedit.biome.set",
        flags: "",
        run: setbiome,
    },
    Command {
        name: "generatebiome",
        aliases: &["genbiome", "gb"],
        usage: "[-roc] <biome> <expression>",
        summary: "Set the biome where an expression is non-zero",
        node: "worldedit.generation.biome",
        flags: "roc",
        run: generatebiome,
    },
    Command {
        name: "biomeinfo",
        aliases: &[],
        usage: "[-p]",
        summary: "List the biomes of the selection, or of your position with -p",
        node: "worldedit.biome.info",
        flags: "p",
        run: biomeinfo,
    },
];

pub(crate) fn biome_name(text: &str) -> String {
    match text.contains(':') {
        true => text.to_owned(),
        false => format!("minecraft:{text}"),
    }
}

fn setbiome(ctx: &Ctx) -> i32 {
    let Some(text) = ctx.args.first() else {
        return say(usage(&COMMANDS[0]));
    };
    let (dim, region) = match current(ctx.id) {
        Ok(found) => found,
        Err(why) => return say(why),
    };
    if let Some(why) = busy(ctx.id) {
        return say(why);
    }
    start(Job::biome(ctx.id, dim, biome_name(text), None, region))
}

fn generatebiome(ctx: &Ctx) -> i32 {
    if ctx.args.len() < 2 {
        return say(usage(&COMMANDS[1]));
    }
    let expr = match Expr::parse(&ctx.args[1..].join(" ")) {
        Ok(e) => Rc::new(e),
        Err(why) => return say(format!("Bad expression: {why}")),
    };
    let (dim, region) = match current(ctx.id) {
        Ok(found) => found,
        Err(why) => return say(why),
    };
    let (min, max) = region.bounds();
    let frame = match frame(ctx, min, max) {
        Ok(f) => f,
        Err(why) => return say(why),
    };
    let gen_ = Gen {
        expr,
        frame,
        min,
        max,
        hollow: false,
    };
    if let Err(why) = gen_.check(0) {
        return say(format!("Expression failed: {why}"));
    }
    if let Some(why) = busy(ctx.id) {
        return say(why);
    }
    start(Job::biome(
        ctx.id,
        dim,
        biome_name(ctx.args[0]),
        Some(gen_),
        region,
    ))
}

fn biomeinfo(ctx: &Ctx) -> i32 {
    if ctx.flag('p') {
        let Some(here) = here() else {
            return say("Your position is unknown");
        };
        let [x, y, z] = here.block;
        return match World::new(&here.dim).biome(block_pos(x, y, z)) {
            Ok(name) => say(format!("Biome at your position: {name}")),
            Err(_) => say("The biome could not be read"),
        };
    }
    let (dim, region) = match current(ctx.id) {
        Ok(found) => found,
        Err(why) => return say(why),
    };
    let (min, max) = region.bounds();
    let (lo, hi) = (min.map(|v| v >> 2), max.map(|v| v >> 2));
    let cells: u64 = (0..3).map(|a| (hi[a] - lo[a] + 1) as u64).product();
    if cells > INFO_CELLS {
        return say(format!(
            "That selection has {cells} biome cells; //biomeinfo reads at most {INFO_CELLS}. Use -p"
        ));
    }
    let world = World::new(&dim);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                let at = block_pos(x << 2, y << 2, z << 2);
                match world.biome(at) {
                    Ok(name) => *counts.entry(name).or_default() += 1,
                    Err(_) => return say("The biomes could not be read"),
                }
            }
        }
    }
    let list: Vec<String> = counts
        .iter()
        .map(|(name, n)| format!("{name} ({n} cells)"))
        .collect();
    say(format!("Biomes: {}", list.join(", ")))
}

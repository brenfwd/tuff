// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::schematic;
use crate::session::{model, say};

pub const COMMANDS: &[Command] = &[Command {
    name: "schematic",
    aliases: &["schem"],
    usage: "<save [-f] <name> | load <name> | list | delete <name>>",
    summary: "Save, load, list and delete schematics",
    node: "worldedit.schematic",
    flags: "f",
    run: schem,
}];

fn schem(ctx: &Ctx) -> i32 {
    let sub = ctx.args.first().copied().unwrap_or("");
    let name = ctx.args.get(1).copied();
    let (action, node) = match sub {
        "save" => ("save", "worldedit.schematic.save"),
        "load" => ("load", "worldedit.schematic.load"),
        "list" => ("list", "worldedit.schematic.list"),
        "delete" => ("delete", "worldedit.schematic.delete"),
        _ => return say(usage(&COMMANDS[0])),
    };
    if tuff_plugin_sdk::world::acting::has_permission(node) == Some(false) {
        return say("You do not have permission to use this WorldEdit command");
    }
    match (action, name) {
        ("list", _) => {
            let all = schematic::names();
            if all.is_empty() {
                say("No schematics saved")
            } else {
                say(format!("Schematics ({}): {}", all.len(), all.join(", ")))
            }
        }
        ("save", Some(n)) => {
            let Some(clip) = model(|m| m.players[&ctx.id].clip.clone()) else {
                return say("Your clipboard is empty (//copy)");
            };
            match schematic::save(n, &clip, ctx.flag('f')) {
                Ok(bytes) => say(format!("Saved schematic {n} ({bytes} bytes)")),
                Err(why) => say(why),
            }
        }
        ("load", Some(n)) => match schematic::load(n) {
            Ok(clip) => {
                let volume = clip.volume();
                model(|m| m.players.get_mut(&ctx.id).unwrap().clip = Some(clip));
                say(format!(
                    "Loaded schematic {n}: {volume} blocks; paste with //paste"
                ))
            }
            Err(why) => say(why),
        },
        ("delete", Some(n)) => {
            if schematic::delete(n) {
                say(format!("Deleted schematic {n}"))
            } else {
                say(format!("No schematic named {n}"))
            }
        }
        _ => say(usage(&COMMANDS[0])),
    }
}

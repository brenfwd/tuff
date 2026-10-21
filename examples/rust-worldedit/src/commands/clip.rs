// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::clipboard::{self, PasteFlags, Xf};
use crate::edit::Registry;
use crate::mask;
use crate::selection::parse3;
use crate::session::{here, look, say};

pub const COMMANDS: &[Command] = &[
    Command {
        name: "copy",
        aliases: &[],
        usage: "[-e] [-b] [-m <mask>]",
        summary: "Copy the selection to your clipboard",
        node: "worldedit.clipboard.copy",
        flags: "ebm",
        run: copy,
    },
    Command {
        name: "paste",
        aliases: &[],
        usage: "[-a] [-o] [-s] [-n] [-b]",
        summary: "Paste the clipboard at your position",
        node: "worldedit.clipboard.paste",
        flags: "aosnb",
        run: paste,
    },
    Command {
        name: "clone",
        aliases: &[],
        usage: "<x> <y> <z>",
        summary: "Copy the selection to a destination corner",
        node: "worldedit.clipboard.paste",
        flags: "",
        run: clone,
    },
    Command {
        name: "cut",
        aliases: &[],
        usage: "[-e] [-m <mask>] [leave-pattern]",
        summary: "Cut the selection to your clipboard",
        node: "worldedit.clipboard.cut",
        flags: "em",
        run: cut,
    },
    Command {
        name: "rotate",
        aliases: &[],
        usage: "<y-degrees> [x-degrees] [z-degrees]",
        summary: "Rotate your clipboard",
        node: "worldedit.clipboard.rotate",
        flags: "",
        run: rotate,
    },
    Command {
        name: "flip",
        aliases: &[],
        usage: "[direction]",
        summary: "Flip your clipboard",
        node: "worldedit.clipboard.flip",
        flags: "",
        run: flip,
    },
    Command {
        name: "clearclipboard",
        aliases: &[],
        usage: "",
        summary: "Clear your clipboard",
        node: "worldedit.clipboard.clear",
        flags: "",
        run: clear,
    },
    Command {
        name: "undo",
        aliases: &[],
        usage: "[times] [player]",
        summary: "Undo your last edits",
        node: "worldedit.history.undo",
        flags: "",
        run: undo,
    },
    Command {
        name: "redo",
        aliases: &[],
        usage: "[times] [player]",
        summary: "Redo undone edits",
        node: "worldedit.history.redo",
        flags: "",
        run: redo,
    },
    Command {
        name: "clearhistory",
        aliases: &[],
        usage: "",
        summary: "Clear your history",
        node: "worldedit.history.clear",
        flags: "",
        run: clearhistory,
    },
];

fn mask_of(ctx: &Ctx, terms: &[&str]) -> Result<Option<mask::Mask>, String> {
    match ctx.flag('m') {
        true => mask::parse(terms, &Registry).map(Some),
        false => Ok(None),
    }
}

fn copy(ctx: &Ctx) -> i32 {
    match mask_of(ctx, &ctx.args) {
        Ok(mask) => clipboard::copy(ctx.id, mask, ctx.flag('e'), ctx.flag('b')),
        Err(why) => say(why),
    }
}

fn cut(ctx: &Ctx) -> i32 {
    let (leave, terms) = match (ctx.flag('m'), ctx.args.as_slice()) {
        (false, [leave, ..]) => (Some(*leave), &[][..]),
        (true, [leave, rest @ ..]) if !rest.is_empty() => (Some(*leave), rest),
        (_, all) => (None, all),
    };
    match mask_of(ctx, terms) {
        Ok(mask) => clipboard::cut(ctx.id, leave, mask),
        Err(why) => say(why),
    }
}

fn paste(ctx: &Ctx) -> i32 {
    let flags = PasteFlags {
        skip_air: ctx.flag('a'),
        original: ctx.flag('o'),
        select: ctx.flag('s'),
        only_select: ctx.flag('n'),
        biomes: ctx.flag('b'),
    };
    match here() {
        Some(h) => clipboard::paste(ctx.id, h.dim, h.block, flags),
        None => say("Only a player can paste"),
    }
}

fn rotate(ctx: &Ctx) -> i32 {
    let mut steps = Vec::new();
    for (axis, text) in [
        (1, ctx.args.first()),
        (0, ctx.args.get(1)),
        (2, ctx.args.get(2)),
    ] {
        let Some(text) = text else { continue };
        let Ok(deg) = text.parse::<i32>() else {
            return say(usage(&COMMANDS[4]));
        };
        if deg % 90 != 0 {
            return say("Angles must be multiples of 90 degrees");
        }
        steps.push(Xf::Rot(axis, deg / 90));
    }
    if steps.is_empty() {
        return say(usage(&COMMANDS[4]));
    }
    clipboard::transform(ctx.id, &steps)
}

fn flip(ctx: &Ctx) -> i32 {
    let axis = match ctx.args.first().copied() {
        Some("n" | "north" | "s" | "south") => 2,
        Some("e" | "east" | "w" | "west") => 0,
        Some("u" | "up" | "d" | "down") => 1,
        Some("me") | None => match look() {
            Some((_, pitch, _)) if pitch.abs() > 60.0 => 1,
            Some((yaw, ..)) => match ((yaw / 90.0).round() as i32).rem_euclid(4) {
                0 | 2 => 2,
                _ => 0,
            },
            None => return say("Your look direction is unknown"),
        },
        Some(_) => return say(usage(&COMMANDS[5])),
    };
    clipboard::transform(ctx.id, &[Xf::Flip(axis)])
}

fn clear(ctx: &Ctx) -> i32 {
    clipboard::clear(ctx.id)
}

fn clone(ctx: &Ctx) -> i32 {
    match parse3(&ctx.args) {
        Some(at) => clipboard::clone_to(ctx.id, at),
        None => say(usage(&COMMANDS[2])),
    }
}

fn undo(ctx: &Ctx) -> i32 {
    replay(ctx, false)
}

fn redo(ctx: &Ctx) -> i32 {
    replay(ctx, true)
}

fn replay(ctx: &Ctx, redo: bool) -> i32 {
    let (times, who) = match ctx.args.as_slice() {
        [] => (1, None),
        [one] => match one.parse::<u32>() {
            Ok(n) => (n, None),
            Err(_) => (1, Some(*one)),
        },
        [times, who, ..] => match times.parse::<u32>() {
            Ok(n) => (n, Some(*who)),
            Err(_) => return say(usage(&COMMANDS[if redo { 8 } else { 7 }])),
        },
    };
    if times == 0 {
        return say("Times must be a positive number");
    }
    let target = match who {
        None => ctx.id,
        Some(name) => {
            if tuff_plugin_sdk::world::acting::has_permission("worldedit.history.undo.other")
                != Some(true)
            {
                return say("You do not have permission to change other players' history");
            }
            let found = crate::session::model(|m| {
                m.players
                    .iter()
                    .find(|(_, p)| p.name.eq_ignore_ascii_case(name))
                    .map(|(id, _)| *id)
            });
            match found {
                Some(id) => id,
                None => return say(format!("No WorldEdit history for {name}")),
            }
        }
    };
    crate::history::history_many(target, redo, times)
}

fn clearhistory(ctx: &Ctx) -> i32 {
    crate::history::clear(ctx.id)
}

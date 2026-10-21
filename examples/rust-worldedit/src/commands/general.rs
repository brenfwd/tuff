// SPDX-License-Identifier: MIT
use super::{Command, Ctx, find, table, usage};
use crate::selection::limit_of;
use crate::session::{MAX_LIMIT, model, say};

pub const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        aliases: &[],
        usage: "[-a] [command]",
        summary: "List commands (-a with summaries) or describe one",
        node: "worldedit.help",
        flags: "a",
        run: help,
    },
    Command {
        name: "version",
        aliases: &["ver"],
        usage: "",
        summary: "Show the WorldEdit version",
        node: "worldedit.version",
        flags: "",
        run: version,
    },
    Command {
        name: "wand",
        aliases: &[],
        usage: "",
        summary: "Give the selection wand",
        node: "worldedit.wand",
        flags: "",
        run: wand,
    },
    Command {
        name: "toggleeditwand",
        aliases: &[],
        usage: "",
        summary: "Toggle wand selection clicks",
        node: "worldedit.wand.toggle",
        flags: "",
        run: toggle_wand,
    },
    Command {
        name: "timeout",
        aliases: &[],
        usage: "[milliseconds]",
        summary: "Show or set your calculation timeout",
        node: "worldedit.timeout",
        flags: "",
        run: timeout,
    },
    Command {
        name: "fast",
        aliases: &[],
        usage: "[on|off]",
        summary: "Toggle fast mode (no history is recorded when on)",
        node: "worldedit.fast",
        flags: "",
        run: fast,
    },
    Command {
        name: "limit",
        aliases: &[],
        usage: "[blocks]",
        summary: "Show or set your block limit",
        node: "worldedit.limit",
        flags: "",
        run: limit,
    },
    Command {
        name: "perf",
        aliases: &[],
        usage: "",
        summary: "Show the settings that affect edit performance",
        node: "worldedit.perf",
        flags: "",
        run: perf,
    },
    Command {
        name: "searchitem",
        aliases: &["search", "l"],
        usage: "[-b] <query>",
        summary: "Search item ids by name (-b searches blocks)",
        node: "worldedit.searchitem",
        flags: "b",
        run: searchitem,
    },
    Command {
        name: "status",
        aliases: &[],
        usage: "",
        summary: "Show running edits and history",
        node: "worldedit.status",
        flags: "",
        run: status,
    },
];

pub fn overview() -> i32 {
    help(&Ctx {
        id: 0,
        args: Vec::new(),
        flags: Vec::new(),
    })
}

fn help(ctx: &Ctx) -> i32 {
    if let Some(name) = ctx.args.first() {
        return match find(name) {
            Some(c) => {
                let mut text = format!("{}: {}", usage(c), c.summary);
                if !c.aliases.is_empty() {
                    let names: Vec<String> = c.aliases.iter().map(|a| format!("//{a}")).collect();
                    text.push_str(&format!(" (also {})", names.join(", ")));
                }
                say(text)
            }
            None => say(format!("Unknown WorldEdit command {name}")),
        };
    }
    if ctx.flag('a') {
        let lines: Vec<String> = table()
            .iter()
            .map(|c| format!("//{} {}: {}", c.name, c.usage, c.summary))
            .collect();
        return say(lines.join("\n"));
    }
    let names: Vec<&str> = table().iter().map(|c| c.name).collect();
    say(format!(
        "WorldEdit commands (//<name> or /tuff we <name>): {}",
        names.join(", ")
    ))
}

fn version(_: &Ctx) -> i32 {
    say(format!("WorldEdit plugin {}", env!("CARGO_PKG_VERSION")))
}

fn wand(_: &Ctx) -> i32 {
    say("The wand is a wooden axe: left click sets position 1, right click position 2")
}

fn toggle_wand(ctx: &Ctx) -> i32 {
    let on = model(|m| {
        let p = m.players.get_mut(&ctx.id).unwrap();
        p.wand = !p.wand;
        p.wand
    });
    say(if on {
        "Wand selection enabled"
    } else {
        "Wand selection disabled"
    })
}

const DEFAULT_TIMEOUT: u32 = 3000;
const MAX_TIMEOUT: u32 = 30_000;

fn timeout(ctx: &Ctx) -> i32 {
    match ctx.args.first().map(|n| n.parse::<u32>()) {
        None => {
            let ms = model(|m| m.players[&ctx.id].timeout.unwrap_or(DEFAULT_TIMEOUT));
            say(format!("Your calculation timeout is {ms} ms"))
        }
        Some(Ok(n)) if (100..=MAX_TIMEOUT).contains(&n) => {
            model(|m| m.players.get_mut(&ctx.id).unwrap().timeout = Some(n));
            say(format!("Timeout set to {n} ms"))
        }
        _ => say(format!("Timeout must be 100 to {MAX_TIMEOUT}")),
    }
}

fn fast(ctx: &Ctx) -> i32 {
    let on = match ctx.args.first().copied() {
        None => !model(|m| m.players[&ctx.id].fast),
        Some("on") => true,
        Some("off") => false,
        Some(_) => return say("Usage: //fast [on|off]"),
    };
    model(|m| m.players.get_mut(&ctx.id).unwrap().fast = on);
    say(match on {
        true => "Fast mode enabled: edits are not recorded in your history",
        false => "Fast mode disabled",
    })
}

fn limit(ctx: &Ctx) -> i32 {
    match ctx.args.first().map(|n| n.parse::<u64>()) {
        None => {
            let limit = model(|m| limit_of(&m.players[&ctx.id]));
            say(format!(
                "Your limit is {limit} blocks (maximum {MAX_LIMIT})"
            ))
        }
        Some(Ok(n)) if (1..=MAX_LIMIT).contains(&n) => {
            model(|m| m.players.get_mut(&ctx.id).unwrap().limit = Some(n));
            say(format!("Limit set to {n} blocks"))
        }
        _ => say(format!("Limit must be 1 to {MAX_LIMIT}")),
    }
}

fn status(ctx: &Ctx) -> i32 {
    let id = ctx.id;
    let (jobs, undo, redo) = model(|m| {
        let p = &m.players[&id];
        (
            m.jobs.iter().filter(|j| j.owner == id).count(),
            p.undo.len(),
            p.redo.len(),
        )
    });
    say(format!("{jobs} running, {undo} to undo, {redo} to redo"))
}

fn perf(ctx: &Ctx) -> i32 {
    let (fast, timeout, limit, running) = model(|m| {
        let p = &m.players[&ctx.id];
        (
            p.fast,
            p.timeout,
            limit_of(p),
            m.jobs.iter().filter(|j| j.owner == ctx.id).count(),
        )
    });
    let timeout = timeout.map_or("default".to_string(), |t| format!("{t} ms"));
    say(format!(
        "fast {}, timeout {timeout}, limit {limit}, {running} running",
        if fast { "on" } else { "off" }
    ))
}

const ITEM_TAGS: &[&str] = &[
    "planks",
    "logs",
    "wool",
    "sand",
    "stone_bricks",
    "slabs",
    "stairs",
    "walls",
    "fences",
    "doors",
    "trapdoors",
    "buttons",
    "leaves",
    "saplings",
    "flowers",
    "terracotta",
    "beds",
    "candles",
    "boats",
    "swords",
    "axes",
    "pickaxes",
    "shovels",
    "hoes",
    "arrows",
    "coals",
    "fishes",
    "meat",
    "music_discs",
    "banners",
    "signs",
    "anvil",
    "dirt",
    "gold_ores",
    "iron_ores",
    "coal_ores",
    "copper_ores",
    "diamond_ores",
    "emerald_ores",
    "lapis_ores",
    "redstone_ores",
    "wooden_slabs",
    "wooden_stairs",
    "wooden_fences",
    "wooden_doors",
    "wooden_buttons",
    "wooden_trapdoors",
    "wooden_pressure_plates",
    "stone_crafting_materials",
    "trim_materials",
    "trim_templates",
    "smelts_to_glass",
    "soul_fire_base_blocks",
    "shulker_boxes",
    "copper",
    "chest_boats",
    "hanging_signs",
    "skulls",
    "cherry_logs",
    "bamboo_blocks",
];

pub fn search_ids(query: &str, blocks: bool) -> Vec<String> {
    let q = query.to_ascii_lowercase().replace(' ', "_");
    let q = q.strip_prefix("minecraft:").unwrap_or(&q).to_owned();
    let mut found: Vec<String> = Vec::new();
    let exact = format!("minecraft:{q}");
    let known = match blocks {
        true => tuff_plugin_sdk::world::BlockState::new(&exact)
            .normalize()
            .is_ok(),
        false => tuff_plugin_sdk::items::info(&exact).is_some(),
    };
    if known {
        found.push(exact);
    }
    for tag in ITEM_TAGS {
        let members = match blocks {
            true => tuff_plugin_sdk::world::block_tag(tag).ok(),
            false => tuff_plugin_sdk::items::tag(tag),
        };
        for id in members.unwrap_or_default() {
            if id.strip_prefix("minecraft:").unwrap_or(&id).contains(&q) && !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found.sort();
    found
}

fn searchitem(ctx: &Ctx) -> i32 {
    if ctx.args.is_empty() {
        return say("Usage: //searchitem [-b] <query>");
    }
    let found = search_ids(&ctx.args.join(" "), ctx.flag('b'));
    if found.is_empty() {
        return say("No matches");
    }
    let shown: Vec<&str> = found.iter().take(15).map(String::as_str).collect();
    let more = found.len().saturating_sub(shown.len());
    let tail = match more {
        0 => String::new(),
        n => format!(" (+{n} more)"),
    };
    say(format!("Matches: {}{tail}", shown.join(", ")))
}

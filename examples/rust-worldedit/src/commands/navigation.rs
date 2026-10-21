// SPDX-License-Identifier: MIT
use super::{Command, Ctx};
use crate::region::{P, WORLD_Y};
use crate::session::{here, look, say};

const THRU_REACH: f64 = 100.0;
const GLASS: &str = "minecraft:glass";

pub const COMMANDS: &[Command] = &[
    Command {
        name: "unstuck",
        aliases: &[],
        usage: "",
        summary: "Escape from being stuck inside a block",
        node: "worldedit.navigation.unstuck",
        flags: "",
        run: unstuck,
    },
    Command {
        name: "ascend",
        aliases: &["asc"],
        usage: "[levels]",
        summary: "Go up a floor",
        node: "worldedit.navigation.ascend",
        flags: "",
        run: ascend,
    },
    Command {
        name: "descend",
        aliases: &["desc"],
        usage: "[levels]",
        summary: "Go down a floor",
        node: "worldedit.navigation.descend",
        flags: "",
        run: descend,
    },
    Command {
        name: "ceil",
        aliases: &[],
        usage: "[clearance]",
        summary: "Go to the ceiling above you",
        node: "worldedit.navigation.ceiling",
        flags: "",
        run: ceil,
    },
    Command {
        name: "thru",
        aliases: &[],
        usage: "",
        summary: "Pass through the wall you are looking at",
        node: "worldedit.navigation.thru.command",
        flags: "",
        run: thru,
    },
    Command {
        name: "jumpto",
        aliases: &["j"],
        usage: "",
        summary: "Teleport to the block you are looking at",
        node: "worldedit.navigation.jumpto.command",
        flags: "",
        run: jumpto,
    },
    Command {
        name: "up",
        aliases: &[],
        usage: "<distance>",
        summary: "Go up some blocks, placing glass below if needed",
        node: "worldedit.navigation.up",
        flags: "",
        run: up,
    },
];

pub fn is_navigation(node: &str) -> bool {
    node.starts_with("worldedit.navigation.")
}

fn passable(state: &str) -> bool {
    state.is_empty() || state.contains("air")
}

fn open_at(dim: &str, at: P) -> bool {
    let mut buf = [0u8; 256];
    tuff_plugin_sdk::block_get(dim, at, &mut buf).is_ok_and(passable)
}

fn standable(open: &impl Fn(P) -> bool, feet: P) -> bool {
    let [x, y, z] = feet;
    y > WORLD_Y[0] && !open([x, y - 1, z]) && open(feet) && open([x, y + 1, z])
}

pub fn find_up(open: &impl Fn(P) -> bool, from: P) -> Option<P> {
    let [x, y, z] = from;
    (y + 1..=WORLD_Y[1] - 1)
        .map(|y| [x, y, z])
        .find(|at| standable(open, *at))
}

pub fn find_down(open: &impl Fn(P) -> bool, from: P) -> Option<P> {
    let [x, y, z] = from;
    (WORLD_Y[0] + 1..y)
        .rev()
        .map(|y| [x, y, z])
        .find(|at| standable(open, *at))
}

pub fn find_ceiling(open: &impl Fn(P) -> bool, from: P) -> Option<i32> {
    let [x, y, z] = from;
    (y + 2..=WORLD_Y[1]).find(|y| !open([x, *y, z]))
}

pub fn pass_through(open: &impl Fn(P) -> bool, eye: [f64; 3], dir: [f64; 3]) -> Option<P> {
    let cell_at = |t: f64| [0, 1, 2].map(|i| (eye[i] + dir[i] * t).floor() as i32);
    let mut t = 0.0;
    let mut hit = false;
    let mut last = cell_at(0.0);
    while t <= THRU_REACH {
        let cell = cell_at(t);
        if cell != last {
            last = cell;
            if !open(cell) {
                hit = true;
            } else if hit {
                let [x, y, z] = cell;
                if open([x, y + 1, z]) {
                    return Some(cell);
                }
                if open([x, y - 1, z]) {
                    return Some([x, y - 1, z]);
                }
            }
        }
        t += 0.1;
    }
    None
}

fn center(at: P) -> [f64; 3] {
    [at[0] as f64 + 0.5, at[1] as f64, at[2] as f64 + 0.5]
}

fn go(at: P) -> i32 {
    match tuff_plugin_sdk::teleport_self(center(at), None) {
        Ok(()) => say("Whoosh!"),
        Err(_) => say("Could not teleport you"),
    }
}

fn glass_below(dim: &str, at: P) {
    let below = [at[0], at[1] - 1, at[2]];
    if open_at(dim, below) {
        let _ = tuff_plugin_sdk::block_set(dim, below, GLASS, true);
    }
}

fn levels(ctx: &Ctx) -> Option<u32> {
    match ctx.args.first() {
        None => Some(1),
        Some(arg) => arg.parse().ok().filter(|n| *n > 0),
    }
}

fn unstuck(_: &Ctx) -> i32 {
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let open = |at| open_at(&h.dim, at);
    let [x, y, z] = h.block;
    let found = std::iter::once([x, y, z])
        .filter(|at| standable(&open, *at))
        .chain(find_up(&open, [x, y - 1, z]))
        .next();
    match found {
        Some(at) => go(at),
        None => say("No free spot above you found"),
    }
}

fn climb(ctx: &Ctx, up: bool) -> i32 {
    let Some(n) = levels(ctx) else {
        return say("Levels must be a positive number");
    };
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let open = |at| open_at(&h.dim, at);
    let mut at = h.block;
    let mut moved = 0;
    for _ in 0..n {
        let next = if up {
            find_up(&open, at)
        } else {
            find_down(&open, at)
        };
        let Some(next) = next else { break };
        at = next;
        moved += 1;
    }
    if moved == 0 {
        return say(match up {
            true => "No free spot above you found",
            false => "No free spot below you found",
        });
    }
    go(at)
}

fn ascend(ctx: &Ctx) -> i32 {
    climb(ctx, true)
}

fn descend(ctx: &Ctx) -> i32 {
    climb(ctx, false)
}

fn ceil(ctx: &Ctx) -> i32 {
    let clearance = match ctx.args.first() {
        None => 0,
        Some(arg) => match arg.parse::<i32>() {
            Ok(n) if n >= 0 => n,
            _ => return say("Clearance must be zero or more"),
        },
    };
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let open = |at| open_at(&h.dim, at);
    let Some(ceiling) = find_ceiling(&open, h.block) else {
        return say("No block above you found");
    };
    let y = ceiling - 2 - clearance;
    if y < h.block[1] {
        return say("Not enough space above you");
    }
    let at = [h.block[0], y, h.block[2]];
    glass_below(&h.dim, at);
    go(at)
}

fn up(ctx: &Ctx) -> i32 {
    let Some(distance) = ctx.args.first().and_then(|a| a.parse::<i32>().ok()) else {
        return say("Usage: /up <distance>");
    };
    if distance < 1 {
        return say("Distance must be positive");
    }
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let y = (h.block[1] + distance).min(WORLD_Y[1] - 1);
    let at = [h.block[0], y, h.block[2]];
    glass_below(&h.dim, at);
    go(at)
}

fn thru(_: &Ctx) -> i32 {
    pass()
}

pub fn pass() -> i32 {
    let (Some(h), Some((yaw, pitch, eye))) = (here(), look()) else {
        return say("WorldEdit needs your position and look direction");
    };
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let dir = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let open = |at| open_at(&h.dim, at);
    match pass_through(&open, eye, dir) {
        Some(at) => go(at),
        None => say("No free spot ahead of you found"),
    }
}

fn jumpto(_: &Ctx) -> i32 {
    jump()
}

pub fn jump() -> i32 {
    let Some(h) = here() else {
        return say("WorldEdit needs your position");
    };
    let Some((cell, _)) = super::select::trace(&h.dim, 300) else {
        return say("No block in sight!");
    };
    let open = |at| open_at(&h.dim, at);
    match find_up(&open, [cell[0], cell[1] - 1, cell[2]]) {
        Some(at) => go(at),
        None => say("No free spot above that block"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn world(solid: &[P]) -> impl Fn(P) -> bool {
        let set: HashSet<P> = solid.iter().copied().collect();
        move |at| !set.contains(&at)
    }

    #[test]
    fn ascend_and_descend_find_the_next_floor() {
        let mut blocks = vec![[0, 10, 0], [0, 14, 0], [0, 15, 0], [0, 20, 0]];
        blocks.push([0, 5, 0]);
        let open = world(&blocks);
        assert_eq!(find_up(&open, [0, 11, 0]), Some([0, 16, 0]));
        assert_eq!(find_up(&open, [0, 16, 0]), Some([0, 21, 0]));
        assert_eq!(find_down(&open, [0, 11, 0]), Some([0, 6, 0]));
        assert_eq!(find_ceiling(&open, [0, 11, 0]), Some(14));
    }

    #[test]
    fn thru_exits_the_far_side_of_a_wall() {
        let mut blocks = vec![[3, 11, 0], [4, 11, 0]];
        blocks.push([3, 10, 0]);
        blocks.push([4, 10, 0]);
        let open = world(&blocks);
        let at = pass_through(&open, [0.5, 11.5, 0.5], [1.0, 0.0, 0.0]);
        assert_eq!(at, Some([5, 11, 0]));
    }
}

// SPDX-License-Identifier: MIT
//! Glowstone frames lit with a water bucket, filled with end gateways, and
//! the travel between the overworld and the tuffer at 1:128.

use tuff_plugin_sdk::events::{BlockChange, Click, Inside};
use tuff_plugin_sdk::{
    block_get, block_set, teleport_entity, teleport_player,
};
use tuff_plugin_sdk::store::{get_bytes, set_bytes};
use tuff_plugin_sdk::world::{World, pos};

pub const TUFFER: &str = "tuffer:tuffer";
pub const OVERWORLD: &str = "minecraft:overworld";
pub const SCALE: i32 = 128;
const GATEWAY: &str = "minecraft:end_gateway";
const FRAME: &str = "minecraft:glowstone";
const REACH: i32 = 22;
const MAX_SIDE: i32 = 21;
const WORLD_LIMIT: i32 = 29_000_000;
const KEY: &str = "portals";
const MAX_PORTALS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Z,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    Frame,
    Empty,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Portal {
    pub dim: String,
    pub axis: Axis,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub w: i32,
    pub h: i32,
}

impl Portal {
    fn at(&self, i: i32, j: i32) -> [i32; 3] {
        match self.axis {
            Axis::X => [self.x + i, self.y + j, self.z],
            Axis::Z => [self.x, self.y + j, self.z + i],
        }
    }

    fn contains_frame(&self, p: [i32; 3]) -> bool {
        let (a, other, a0, o0) = match self.axis {
            Axis::X => (p[0], p[2], self.x, self.z),
            Axis::Z => (p[2], p[0], self.z, self.x),
        };
        other == o0
            && (a0 - 1..=a0 + self.w).contains(&a)
            && (self.y - 1..=self.y + self.h).contains(&p[1])
    }

    fn stand(&self) -> [f64; 3] {
        let along = f64::from(self.w) / 2.0;
        match self.axis {
            Axis::X => [
                f64::from(self.x) + along,
                f64::from(self.y),
                f64::from(self.z) + 0.5,
            ],
            Axis::Z => [
                f64::from(self.x) + 0.5,
                f64::from(self.y),
                f64::from(self.z) + along,
            ],
        }
    }

    fn encode(&self) -> String {
        let axis = if self.axis == Axis::X { 'x' } else { 'z' };
        format!(
            "{} {axis} {} {} {} {} {}",
            self.dim, self.x, self.y, self.z, self.w, self.h
        )
    }

    fn decode(line: &str) -> Option<Portal> {
        let mut parts = line.split(' ');
        let dim = parts.next()?.to_owned();
        let axis = match parts.next()? {
            "x" => Axis::X,
            "z" => Axis::Z,
            _ => return None,
        };
        let mut n = || parts.next()?.parse::<i32>().ok();
        Some(Portal {
            dim,
            axis,
            x: n()?,
            y: n()?,
            z: n()?,
            w: n()?,
            h: n()?,
        })
    }
}

fn load() -> Vec<Portal> {
    let Some(bytes) = get_bytes(KEY) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(Portal::decode)
        .collect()
}

fn save(portals: &[Portal]) {
    let text = portals
        .iter()
        .map(Portal::encode)
        .collect::<Vec<_>>()
        .join("\n");
    let _ = set_bytes(KEY, text.as_bytes());
}

fn classify(state: &str) -> Cell {
    let name = state.split('[').next().unwrap_or(state);
    match name {
        FRAME => Cell::Frame,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" | "minecraft:water"
        | GATEWAY => Cell::Empty,
        _ => Cell::Other,
    }
}

struct Plane {
    a0: i32,
    y0: i32,
    side: i32,
    cells: Vec<Cell>,
}

impl Plane {
    fn get(&self, a: i32, y: i32) -> Cell {
        let (i, j) = (a - self.a0, y - self.y0);
        if (0..self.side).contains(&i) && (0..self.side).contains(&j) {
            self.cells[(j * self.side + i) as usize]
        } else {
            Cell::Other
        }
    }
}

struct Palette(Vec<String>, Vec<u16>);

fn read_blocks(dim: &str, a: [i32; 3], b: [i32; 3]) -> Option<Palette> {
    let grid = World::new(dim)
        .blocks(pos(a[0], a[1], a[2]), pos(b[0], b[1], b[2]))
        .ok()?;
    Some(Palette(grid.palette().to_vec(), grid.indices().to_vec()))
}

fn plane(dim: &str, axis: Axis, p: [i32; 3]) -> Option<Plane> {
    let side = REACH * 2 + 1;
    let (a, b, a0) = match axis {
        Axis::X => (
            [p[0] - REACH, p[1] - REACH, p[2]],
            [p[0] + REACH, p[1] + REACH, p[2]],
            p[0] - REACH,
        ),
        Axis::Z => (
            [p[0], p[1] - REACH, p[2] - REACH],
            [p[0], p[1] + REACH, p[2] + REACH],
            p[2] - REACH,
        ),
    };
    let Palette(palette, indices) = read_blocks(dim, a, b)?;
    let cells = indices
        .iter()
        .map(|&i| palette.get(i as usize).map_or(Cell::Other, |s| classify(s)))
        .collect();
    Some(Plane {
        a0,
        y0: p[1] - REACH,
        side,
        cells,
    })
}

/// The lit area around the empty cell `(a, y)` as `(a0, y0, w, h)`.
fn shape(plane: &Plane, mut a: i32, mut y: i32) -> Option<(i32, i32, i32, i32)> {
    while plane.get(a, y - 1) == Cell::Empty {
        y -= 1;
    }
    if plane.get(a, y - 1) != Cell::Frame {
        return None;
    }
    while plane.get(a - 1, y) == Cell::Empty {
        a -= 1;
    }
    if plane.get(a - 1, y) != Cell::Frame {
        return None;
    }
    let mut w = 0;
    while plane.get(a + w, y) == Cell::Empty && w <= MAX_SIDE {
        w += 1;
    }
    if plane.get(a + w, y) != Cell::Frame || !(2..=MAX_SIDE).contains(&w) {
        return None;
    }
    for i in 0..w {
        if plane.get(a + i, y - 1) != Cell::Frame {
            return None;
        }
    }
    for h in 1..=MAX_SIDE + 1 {
        let row = y + h - 1;
        let cells: Vec<Cell> = (0..w).map(|i| plane.get(a + i, row)).collect();
        if h > 1 && cells.iter().all(|c| *c == Cell::Frame) {
            return ((3..=MAX_SIDE).contains(&(h - 1))).then_some((a, y, w, h - 1));
        }
        let sides = plane.get(a - 1, row) == Cell::Frame && plane.get(a + w, row) == Cell::Frame;
        if !sides || cells.iter().any(|c| *c != Cell::Empty) {
            return None;
        }
    }
    None
}

fn fill(portal: &Portal, state: &str) {
    for i in 0..portal.w {
        for j in 0..portal.h {
            let _ = block_set(&portal.dim, portal.at(i, j), state, false);
        }
    }
}

/// A water bucket right-clicked a glowstone block.
pub fn ignite(click: &Click) {
    let p = [click.pos.x, click.pos.y, click.pos.z];
    let dim = &click.dimension;
    for axis in [Axis::X, Axis::Z] {
        let Some(plane) = plane(dim, axis, p) else {
            continue;
        };
        let a = if axis == Axis::X { p[0] } else { p[2] };
        for (da, dy) in [(0, 1), (0, -1), (1, 0), (-1, 0)] {
            if plane.get(a + da, p[1] + dy) != Cell::Empty {
                continue;
            }
            let Some((a0, y0, w, h)) = shape(&plane, a + da, p[1] + dy) else {
                continue;
            };
            let portal = match axis {
                Axis::X => Portal {
                    dim: dim.clone(),
                    axis,
                    x: a0,
                    y: y0,
                    z: p[2],
                    w,
                    h,
                },
                Axis::Z => Portal {
                    dim: dim.clone(),
                    axis,
                    x: p[0],
                    y: y0,
                    z: a0,
                    w,
                    h,
                },
            };
            light(portal);
            return;
        }
    }
}

fn light(portal: Portal) {
    fill(&portal, GATEWAY);
    let mut portals = load();
    portals.retain(|p| *p != portal);
    portals.push(portal);
    if portals.len() > MAX_PORTALS {
        portals.remove(0);
    }
    save(&portals);
}

/// A player broke a block. A frame or gateway block of a known portal puts
/// the portal out.
pub fn broken(change: &BlockChange) {
    let p = [change.pos.x, change.pos.y, change.pos.z];
    let dim = &change.dimension;
    let mut portals = load();
    let before = portals.len();
    portals.retain(|portal| {
        let hit = portal.dim == *dim && portal.contains_frame(p);
        if hit {
            fill(portal, "minecraft:air");
        }
        !hit
    });
    if portals.len() != before {
        save(&portals);
    }
}

fn is_gateway(portal: &Portal) -> bool {
    let mut buf = [0u8; 128];
    block_get(&portal.dim, portal.at(0, 0), &mut buf).is_ok_and(|s| s.starts_with(GATEWAY))
}

fn find(dim: &str, x: i32, z: i32, radius: i32) -> Option<Portal> {
    let mut portals = load();
    let mut near: Vec<(i64, usize)> = portals
        .iter()
        .enumerate()
        .filter(|(_, p)| p.dim == dim && (p.x - x).abs() <= radius && (p.z - z).abs() <= radius)
        .map(|(i, p)| {
            let (dx, dz) = (i64::from(p.x - x), i64::from(p.z - z));
            (dx * dx + dz * dz, i)
        })
        .collect();
    near.sort();
    let mut stale = Vec::new();
    let mut found = None;
    for (_, i) in near {
        if is_gateway(&portals[i]) {
            found = Some(portals[i].clone());
            break;
        }
        stale.push(i);
    }
    if !stale.is_empty() {
        stale.sort_unstable_by(|a, b| b.cmp(a));
        for i in stale {
            portals.remove(i);
        }
        save(&portals);
    }
    found
}

fn height_range(dim: &str) -> (i32, i32) {
    if dim == TUFFER { (0, 255) } else { (-64, 319) }
}

fn ground(dim: &str, x: i32, z: i32) -> Option<(i32, i32, i32)> {
    let (min_y, max_y) = height_range(dim);
    let Palette(palette, indices) = read_blocks(dim, [x - 4, min_y, z - 4], [x + 4, max_y, z + 4])?;
    let solid: Vec<bool> = palette
        .iter()
        .map(|s| {
            let name = s.split('[').next().unwrap_or(s);
            !(name.ends_with("air") || name.ends_with("_leaves") || name.ends_with("_log"))
        })
        .collect();
    let mut best: Option<(i32, i32, i32, i32)> = None;
    for dz in 0..9 {
        for dx in 0..9 {
            let top = (min_y..=max_y).rev().find(|y| {
                let i = (((y - min_y) * 9 + dz) * 9 + dx) as usize;
                indices
                    .get(i)
                    .is_some_and(|&k| solid.get(k as usize).copied().unwrap_or(false))
            });
            let Some(top) = top else { continue };
            let score = (dx - 4) * (dx - 4) + (dz - 4) * (dz - 4);
            if best.is_none_or(|(s, ..)| score < s) {
                best = Some((score, x - 4 + dx, top, z - 4 + dz));
            }
        }
    }
    best.map(|(_, cx, top, cz)| (cx, top, cz))
}

fn build(dim: &str, x: i32, z: i32) -> Portal {
    let (min_y, max_y) = height_range(dim);
    let (cx, floor, cz) = ground(dim, x, z).unwrap_or(if dim == TUFFER {
        (x, 127, z)
    } else {
        (x, 63, z)
    });
    let y0 = (floor + 1).clamp(min_y + 2, max_y - 5);
    let x0 = cx - 1;
    let portal = Portal {
        dim: dim.into(),
        axis: Axis::X,
        x: x0,
        y: y0,
        z: cz,
        w: 2,
        h: 3,
    };
    for px in x0 - 1..=x0 + 2 {
        for pz in cz - 1..=cz + 1 {
            let _ = block_set(dim, [px, y0 - 1, pz], "minecraft:stone_bricks", false);
            for py in y0..=y0 + 3 {
                let _ = block_set(dim, [px, py, pz], "minecraft:air", false);
            }
        }
    }
    for j in -1..=portal.h {
        for i in -1..=portal.w {
            let edge = i == -1 || i == portal.w || j == -1 || j == portal.h;
            if edge {
                let _ = block_set(dim, portal.at(i, j), FRAME, false);
            }
        }
    }
    light(portal.clone());
    portal
}

fn destination(dim: &str, x: i32, z: i32) -> Option<(&'static str, i32, i32, i32)> {
    if dim == OVERWORLD || dim == "overworld" {
        Some((TUFFER, x.div_euclid(SCALE), z.div_euclid(SCALE), 16))
    } else if dim == TUFFER {
        let scale = |v: i32| {
            v.saturating_mul(SCALE)
                .saturating_add(SCALE / 2)
                .clamp(-WORLD_LIMIT, WORLD_LIMIT)
        };
        Some((OVERWORLD, scale(x), scale(z), SCALE))
    } else {
        None
    }
}

/// A body is inside a gateway.
pub fn travel(inside: &Inside) {
    let p = [inside.pos.x, inside.pos.y, inside.pos.z];
    let Some((target, x, z, radius)) = destination(&inside.dimension, p[0], p[2]) else {
        return;
    };
    let portal = find(target, x, z, radius).unwrap_or_else(|| build(target, x, z));
    let at = portal.stand();
    let uuid = inside.uuid.to_string();
    let _ = if inside.is_player {
        teleport_player(&uuid, target, at, None)
    } else {
        teleport_entity(&uuid, target, at, None)
    };
}

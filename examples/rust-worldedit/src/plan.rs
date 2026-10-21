// SPDX-License-Identifier: MIT
use std::collections::{HashMap, HashSet};

use crate::edit::{Clip, is_air};
use crate::pattern::{Env, Pattern};
use crate::region::{P, Region, WORLD_Y};

pub enum Item {
    Pat(Pattern),
    State(String),
}

#[derive(Default)]
pub struct Cells {
    items: Vec<Item>,
    at: HashMap<P, u32>,
}

impl Cells {
    pub fn item(&mut self, item: Item) -> u32 {
        self.items.push(item);
        (self.items.len() - 1) as u32
    }

    pub fn state(&mut self, state: &str) -> u32 {
        self.item(Item::State(state.to_owned()))
    }

    pub fn set(&mut self, pos: P, item: u32) {
        if (WORLD_Y[0]..=WORLD_Y[1]).contains(&pos[1]) {
            self.at.insert(pos, item);
        }
    }

    pub fn get(&self, pos: P) -> Option<&Item> {
        self.items.get(*self.at.get(&pos)? as usize)
    }

    pub fn take(&mut self, item: u32) -> Item {
        std::mem::replace(&mut self.items[item as usize], Item::State(String::new()))
    }

    pub fn replace_item(&mut self, at: u32, item: Item) {
        self.items[at as usize] = item;
    }

    pub fn len(&self) -> usize {
        self.at.len()
    }

    pub fn bounds(&self) -> Option<(P, P)> {
        let first = *self.at.keys().next()?;
        let mut lo = first;
        let mut hi = first;
        for p in self.at.keys() {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        Some((lo, hi))
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

fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: P, k: i32) -> P {
    a.map(|v| v * k)
}

pub fn walls(region: &Region, item: u32) -> Cells {
    shell(region, item, &DIRS[0..2], &DIRS[4..6])
}

pub fn faces(region: &Region, item: u32) -> Cells {
    shell(region, item, &DIRS, &[])
}

fn shell(region: &Region, item: u32, a: &[P], b: &[P]) -> Cells {
    let mut cells = Cells::default();
    cells.items.push(Item::State(String::new()));
    let mut out = Vec::new();
    region.each(|p| {
        if a.iter().chain(b).any(|d| !region.contains(add(p, *d))) {
            out.push(p);
        }
    });
    for p in out {
        cells.at.insert(p, item);
    }
    cells
}

pub fn center(region: &Region) -> Vec<P> {
    let (min, max) = region.bounds();
    let lo = [0, 1, 2].map(|a| (min[a] + max[a]).div_euclid(2));
    let hi = [0, 1, 2].map(|a| (min[a] + max[a] + 1).div_euclid(2));
    let mut out = Vec::new();
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                out.push([x, y, z]);
            }
        }
    }
    out
}

pub fn at(cells: &mut Cells, points: &[P], item: u32) {
    for p in points {
        cells.set(*p, item);
    }
}

fn top(clip: &Clip, region: &Region, x: i32, z: i32, ymin: i32, ymax: i32) -> Option<i32> {
    (ymin..=ymax)
        .rev()
        .find(|&y| region.contains([x, y, z]) && clip.at([x, y, z]).is_some_and(|s| !is_air(s)))
}

pub fn overlay(clip: &Clip, region: &Region, item: u32) -> Cells {
    let (min, max) = region.bounds();
    let mut cells = Cells::default();
    cells.items.push(Item::State(String::new()));
    for z in min[2]..=max[2] {
        for x in min[0]..=max[0] {
            if let Some(y) = top(clip, region, x, z, min[1], max[1]) {
                cells.set([x, y + 1, z], item);
            }
        }
    }
    cells
}

pub fn naturalize(clip: &Clip, region: &Region) -> Cells {
    let (min, max) = region.bounds();
    let mut cells = Cells::default();
    let grass = cells.state("minecraft:grass_block");
    let dirt = cells.state("minecraft:dirt");
    let stone = cells.state("minecraft:stone");
    for z in min[2]..=max[2] {
        for x in min[0]..=max[0] {
            let mut level = 0;
            for y in (min[1]..=max[1]).rev() {
                let p = [x, y, z];
                if !region.contains(p) || clip.at(p).is_none_or(is_air) {
                    continue;
                }
                let item = match level {
                    0 => grass,
                    1..=3 => dirt,
                    _ => stone,
                };
                cells.set(p, item);
                level += 1;
            }
        }
    }
    cells
}

pub fn hollow(region: &Region, thickness: i32, item: u32) -> Cells {
    let mut inside = HashSet::new();
    region.each(|p| {
        inside.insert(p);
    });
    let mut depth: HashMap<P, i32> = HashMap::new();
    let mut frontier: Vec<P> = inside
        .iter()
        .copied()
        .filter(|p| DIRS.iter().any(|d| !inside.contains(&add(*p, *d))))
        .collect();
    let mut level = 1;
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for p in &frontier {
            depth.insert(*p, level);
        }
        if level < thickness {
            for p in &frontier {
                for d in DIRS {
                    let q = add(*p, d);
                    if inside.contains(&q) && !depth.contains_key(&q) && !next.contains(&q) {
                        next.push(q);
                    }
                }
            }
        }
        frontier = next;
        level += 1;
    }
    let mut cells = Cells::default();
    cells.items.push(Item::State(String::new()));
    for p in inside {
        if !depth.contains_key(&p) {
            cells.at.insert(p, item);
        }
    }
    cells
}

pub fn smooth(
    clip: &Clip,
    region: &Region,
    iterations: u32,
    solid: &dyn Fn(&str, P) -> bool,
) -> Cells {
    let (min, max) = region.bounds();
    let mut heights: HashMap<(i32, i32), i32> = HashMap::new();
    for z in min[2]..=max[2] {
        for x in min[0]..=max[0] {
            let h = (min[1]..=max[1])
                .rev()
                .find(|&y| {
                    region.contains([x, y, z])
                        && clip.at([x, y, z]).is_some_and(|s| solid(s, [x, y, z]))
                })
                .unwrap_or(min[1] - 1);
            heights.insert((x, z), h);
        }
    }
    let original = heights.clone();
    for _ in 0..iterations {
        let mut next = heights.clone();
        for (&(x, z), h) in next.iter_mut() {
            let (mut sum, mut n) = (0, 0);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if let Some(v) = heights.get(&(x + dx, z + dz)) {
                        sum += v;
                        n += 1;
                    }
                }
            }
            *h = ((f64::from(sum) / f64::from(n)).round() as i32).clamp(min[1] - 1, max[1]);
        }
        heights = next;
    }
    let mut cells = Cells::default();
    let air = cells.state("minecraft:air");
    for (&(x, z), &new) in &heights {
        let old = original[&(x, z)];
        if new > old && old >= min[1] {
            if let Some(state) = clip.at([x, old, z]) {
                let item = cells.state(state);
                for y in old + 1..=new {
                    cells.set([x, y, z], item);
                }
            }
        } else if new < old {
            for y in new + 1..=old {
                cells.set([x, y, z], air);
            }
        }
    }
    cells
}

pub fn moved(clip: &Clip, region: &Region, by: P, clear: u32, cells: &mut Cells, skip_air: bool) {
    region.each(|p| cells.set(p, clear));
    region.each(|p| {
        let Some(state) = clip.at(p) else { return };
        if skip_air && is_air(state) {
            return;
        }
        let item = cells.state(state);
        cells.set(add(p, by), item);
    });
}

pub fn stack(clip: &Clip, region: &Region, dir: P, count: i32, skip_air: bool) -> Cells {
    let size = region.size();
    let step = (0..3).map(|a| dir[a].abs() * size[a]).sum::<i32>();
    let mut cells = Cells::default();
    let mut ids: HashMap<String, u32> = HashMap::new();
    for i in 1..=count {
        let by = scale(dir, step * i);
        region.each(|p| {
            let Some(state) = clip.at(p) else { return };
            if skip_air && is_air(state) {
                return;
            }
            let id = match ids.get(state) {
                Some(id) => *id,
                None => {
                    let id = cells.state(state);
                    ids.insert(state.to_owned(), id);
                    id
                }
            };
            cells.set(add(p, by), id);
        });
    }
    cells
}

pub fn line_points(a: P, b: P) -> Vec<P> {
    let d = [0, 1, 2].map(|i| b[i] - a[i]);
    let steps = d.iter().map(|v| v.abs()).max().unwrap_or(0).max(1);
    (0..=steps)
        .map(|i| {
            [0, 1, 2].map(|k| {
                (f64::from(a[k]) + f64::from(d[k]) * f64::from(i) / f64::from(steps)).round() as i32
            })
        })
        .collect()
}

pub fn spline_points(pts: &[P]) -> Vec<P> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let f = |p: P| p.map(f64::from);
    let at = |i: isize| f(pts[i.clamp(0, pts.len() as isize - 1) as usize]);
    let mut out = vec![pts[0]];
    for i in 0..pts.len() as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let len = (0..3).map(|k| (p2[k] - p1[k]).abs()).fold(0.0, f64::max);
        let n = (len * 2.0).ceil().max(1.0) as i32;
        for s in 1..=n {
            let t = f64::from(s) / f64::from(n);
            let v = [0usize, 1, 2].map(|k| {
                (0.5 * (2.0 * p1[k]
                    + (p2[k] - p0[k]) * t
                    + (2.0 * p0[k] - 5.0 * p1[k] + 4.0 * p2[k] - p3[k]) * t * t
                    + (3.0 * p1[k] - p0[k] - 3.0 * p2[k] + p3[k]) * t * t * t))
                    .round() as i32
            });
            out.push(v);
        }
    }
    out
}

pub fn draw(path: &[P], thickness: i32, hollow: bool, item: u32) -> Cells {
    let mut line: Vec<P> = Vec::new();
    for w in path.windows(2) {
        line.extend(line_points(w[0], w[1]));
    }
    if path.len() == 1 {
        line.push(path[0]);
    }
    let r = f64::from(thickness.max(0)) + 0.5;
    let reach = thickness.max(0);
    let mut set: HashSet<P> = HashSet::new();
    for c in line {
        for dy in -reach..=reach {
            for dz in -reach..=reach {
                for dx in -reach..=reach {
                    let d2 = f64::from(dx * dx + dy * dy + dz * dz);
                    if d2 <= r * r {
                        set.insert(add(c, [dx, dy, dz]));
                    }
                }
            }
        }
    }
    let mut cells = Cells::default();
    cells.items.push(Item::State(String::new()));
    for p in &set {
        if !hollow || DIRS.iter().any(|d| !set.contains(&add(*p, *d))) {
            cells.at.insert(*p, item);
        }
    }
    cells
        .at
        .retain(|p, _| (WORLD_Y[0]..=WORLD_Y[1]).contains(&p[1]));
    cells
}

pub const TREES: [&str; 6] = ["oak", "birch", "spruce", "jungle", "acacia", "dark_oak"];

fn is_ground(state: &str) -> bool {
    ["grass_block", "dirt", "podzol", "coarse_dirt"]
        .iter()
        .any(|n| {
            state.strip_prefix("minecraft:") == Some(n)
                || state.starts_with(&format!("minecraft:{n}["))
        })
}

fn column_ground(clip: &Clip, region: &Region, x: i32, z: i32, min: i32, max: i32) -> Option<i32> {
    let y = top(clip, region, x, z, min, max)?;
    let ground = clip.at([x, y, z]).is_some_and(is_ground);
    let above = clip.at([x, y + 1, z]).is_some_and(is_air);
    (ground && above).then_some(y)
}

pub fn forest(clip: &Clip, region: &Region, kind: &str, density: f64, env: &dyn Env) -> Cells {
    let (min, max) = region.bounds();
    let mut cells = Cells::default();
    let log = cells.state(&format!("minecraft:{kind}_log"));
    let leaf = cells.state(&format!("minecraft:{kind}_leaves[persistent=true]"));
    let mut trunks: Vec<(i32, i32)> = Vec::new();
    for z in min[2]..=max[2] {
        for x in min[0]..=max[0] {
            let Some(y) = column_ground(clip, region, x, z, min[1], max[1]) else {
                continue;
            };
            if env.random([x, y, z], 1) >= density
                || trunks
                    .iter()
                    .any(|t| (t.0 - x).abs() < 3 && (t.1 - z).abs() < 3)
            {
                continue;
            }
            trunks.push((x, z));
            let h = 4 + (env.random([x, y, z], 2) * 3.0) as i32;
            for (dy, radius) in [(h - 2, 2i32), (h - 1, 2), (h, 1), (h + 1, 1)] {
                for dz in -radius..=radius {
                    for dx in -radius..=radius {
                        let corner = dx.abs() == radius && dz.abs() == radius;
                        let p = [x + dx, y + dy, z + dz];
                        if corner && (radius == 2 || dy == h + 1) {
                            continue;
                        }
                        if clip.at(p).is_some_and(is_air) {
                            cells.set(p, leaf);
                        }
                    }
                }
            }
            for dy in 1..=h {
                cells.set([x, y + dy, z], log);
            }
        }
    }
    cells
}

pub fn flora(clip: &Clip, region: &Region, density: f64, env: &dyn Env) -> Cells {
    let (min, max) = region.bounds();
    let mut cells = Cells::default();
    let grass = cells.state("minecraft:short_grass");
    let dandelion = cells.state("minecraft:dandelion");
    let poppy = cells.state("minecraft:poppy");
    for z in min[2]..=max[2] {
        for x in min[0]..=max[0] {
            let Some(y) = top(clip, region, x, z, min[1], max[1]) else {
                continue;
            };
            let grassy = clip
                .at([x, y, z])
                .is_some_and(|s| s.starts_with("minecraft:grass_block"));
            let free = clip.at([x, y + 1, z]).is_some_and(is_air);
            if !grassy || !free || env.random([x, y, z], 3) >= density {
                continue;
            }
            let item = match env.random([x, y, z], 4) {
                r if r < 0.7 => grass,
                r if r < 0.85 => dandelion,
                _ => poppy,
            };
            cells.set([x, y + 1, z], item);
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(size: P, f: impl Fn(P) -> &'static str) -> Clip {
        let palette: Vec<String> = ["minecraft:air", "minecraft:stone", "minecraft:grass_block"]
            .map(str::to_owned)
            .to_vec();
        let mut idx = Vec::new();
        for y in 0..size[1] {
            for z in 0..size[2] {
                for x in 0..size[0] {
                    let s = f([x, y, z]);
                    idx.push(palette.iter().position(|p| p == s).unwrap() as u16);
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

    fn state(cells: &Cells, p: P) -> Option<&str> {
        match cells.get(p)? {
            Item::State(s) => Some(s),
            Item::Pat(_) => Some("pat"),
        }
    }

    #[test]
    fn walls_and_faces_select_the_shell() {
        let r = Region::cuboid([0, 0, 0], [4, 2, 4]);
        let w = walls(&r, 0);
        assert_eq!(w.len(), 3 * 16);
        let f = faces(&r, 0);
        assert_eq!(f.len(), 5 * 3 * 5 - 3 * 3);
    }

    #[test]
    fn hollow_keeps_the_requested_thickness() {
        let r = Region::cuboid([0, 0, 0], [6, 6, 6]);
        assert_eq!(hollow(&r, 1, 0).len(), 125);
        assert_eq!(hollow(&r, 2, 0).len(), 27);
    }

    #[test]
    fn center_has_two_blocks_on_even_axes() {
        assert_eq!(
            center(&Region::cuboid([0, 0, 0], [4, 4, 4])),
            vec![[2, 2, 2]]
        );
        assert_eq!(center(&Region::cuboid([0, 0, 0], [3, 4, 4])).len(), 2);
    }

    #[test]
    fn naturalize_layers_grass_dirt_stone() {
        let clip = world([1, 6, 1], |p| {
            if p[1] <= 4 {
                "minecraft:stone"
            } else {
                "minecraft:air"
            }
        });
        let cells = naturalize(&clip, &Region::cuboid([0, 0, 0], [0, 5, 0]));
        assert_eq!(state(&cells, [0, 4, 0]), Some("minecraft:grass_block"));
        assert_eq!(state(&cells, [0, 2, 0]), Some("minecraft:dirt"));
        assert_eq!(state(&cells, [0, 0, 0]), Some("minecraft:stone"));
        assert_eq!(state(&cells, [0, 5, 0]), None);
    }

    #[test]
    fn overlay_sits_on_the_top_block() {
        let clip = world([2, 4, 1], |p| {
            if p[1] <= p[0] + 1 {
                "minecraft:stone"
            } else {
                "minecraft:air"
            }
        });
        let cells = overlay(&clip, &Region::cuboid([0, 0, 0], [1, 3, 0]), 0);
        assert!(cells.get([0, 2, 0]).is_some());
        assert!(cells.get([1, 3, 0]).is_some());
    }

    #[test]
    fn smooth_lowers_a_spike() {
        let clip = world([3, 6, 3], |p| {
            if p[1] <= 1 || (p[0] == 1 && p[2] == 1 && p[1] <= 4) {
                "minecraft:stone"
            } else {
                "minecraft:air"
            }
        });
        let solid = |s: &str, _| !is_air(s);
        let cells = smooth(&clip, &Region::cuboid([0, 0, 0], [2, 5, 2]), 1, &solid);
        assert_eq!(state(&cells, [1, 4, 1]), Some("minecraft:air"));
    }

    #[test]
    fn lines_cover_both_ends_and_thickness_grows() {
        let l = line_points([0, 0, 0], [5, 2, 0]);
        assert_eq!(l.first(), Some(&[0, 0, 0]));
        assert_eq!(l.last(), Some(&[5, 2, 0]));
        assert_eq!(draw(&[[0, 0, 0], [4, 0, 0]], 0, false, 0).len(), 5);
        assert!(draw(&[[0, 0, 0], [4, 0, 0]], 1, false, 0).len() > 5);
        assert!(
            draw(&[[0, 0, 0], [8, 0, 0]], 2, true, 0).len()
                < draw(&[[0, 0, 0], [8, 0, 0]], 2, false, 0).len()
        );
    }

    #[test]
    fn spline_passes_through_control_points() {
        let pts = [[0, 0, 0], [10, 5, 0], [20, 0, 0]];
        let s = spline_points(&pts);
        assert!(s.contains(&[10, 5, 0]));
        assert_eq!(s.last(), Some(&[20, 0, 0]));
    }

    #[test]
    fn stack_repeats_by_region_size() {
        let clip = world([2, 1, 1], |_| "minecraft:stone");
        let cells = stack(
            &clip,
            &Region::cuboid([0, 0, 0], [1, 0, 0]),
            [1, 0, 0],
            2,
            false,
        );
        assert_eq!(cells.bounds(), Some(([2, 0, 0], [5, 0, 0])));
    }
}

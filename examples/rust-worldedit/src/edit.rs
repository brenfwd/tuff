// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use crate::expr::{Deform, Gen};
use crate::mask::Mask;
use crate::pattern::{Env, Pattern};
use crate::plan::{Cells, Item};
use crate::region::Region;
use tuff_plugin_sdk::world::{World, pos as block_pos};
use tuff_plugin_sdk::{WorldError, block_set};

const SLICE: u64 = 4096;
const EXPR_SLICE: u64 = 64;
const BIOME_SLICE: u64 = 256;
const EXPR_SLICE_STEPS: u64 = 20_000;
const EXPR_CALL_STEPS: u64 = 5_000;
pub const UNAVAILABLE: u16 = u16::MAX;

pub fn is_air(state: &str) -> bool {
    matches!(
        state,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

pub fn normal(state: &str) -> String {
    let name_end = state.find('[').unwrap_or(state.len());
    match state[..name_end].contains(':') {
        true => state.to_owned(),
        false => format!("minecraft:{state}"),
    }
}

fn split(state: &str) -> (&str, &str) {
    match state.find('[') {
        Some(at) => (&state[..at], state[at + 1..].trim_end_matches(']')),
        None => (state, ""),
    }
}

pub fn matches(old: &str, from: &str) -> bool {
    let (old_name, old_props) = split(old);
    let (name, props) = split(from);
    old_name == name
        && props
            .split(',')
            .filter(|p| !p.is_empty())
            .all(|p| old_props.split(',').any(|o| o == p))
}

pub fn order(a: [i32; 3], b: [i32; 3]) -> ([i32; 3], [i32; 3]) {
    (
        [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
        [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
    )
}

fn slices(min: [i32; 3], max: [i32; 3], limit: u64) -> Vec<([i32; 3], [i32; 3])> {
    let size = [
        (max[0] - min[0] + 1) as u64,
        (max[1] - min[1] + 1) as u64,
        (max[2] - min[2] + 1) as u64,
    ];
    let sx = size[0].min(limit);
    let sz = size[2].min(limit / sx);
    let sy = size[1].min(limit / (sx * sz));
    let mut out = Vec::new();
    let mut y = 0;
    while y < size[1] {
        let mut z = 0;
        while z < size[2] {
            let mut x = 0;
            while x < size[0] {
                out.push((
                    [min[0] + x as i32, min[1] + y as i32, min[2] + z as i32],
                    [
                        (sx).min(size[0] - x) as i32,
                        (sy).min(size[1] - y) as i32,
                        (sz).min(size[2] - z) as i32,
                    ],
                ));
                x += sx;
            }
            z += sz;
        }
        y += sy;
    }
    out
}

fn read_box(dim: &str, min: [i32; 3], size: [i32; 3]) -> Option<(Vec<String>, Vec<u16>)> {
    read_blocks(dim, min, size, None)
}

fn read_blocks(
    dim: &str,
    min: [i32; 3],
    size: [i32; 3],
    generated: Option<(Option<i64>, Option<&str>)>,
) -> Option<(Vec<String>, Vec<u16>)> {
    let max = [
        min[0] + size[0] - 1,
        min[1] + size[1] - 1,
        min[2] + size[2] - 1,
    ];
    let (a, b) = (
        block_pos(min[0], min[1], min[2]),
        block_pos(max[0], max[1], max[2]),
    );
    let world = World::new(dim);
    let grid = match generated {
        Some((seed, biome)) => world.generated_blocks_with(a, b, seed, biome),
        None => world.blocks(a, b),
    }
    .ok()?;
    Some((grid.palette().to_vec(), grid.indices().to_vec()))
}

#[derive(Clone)]
pub struct Snap {
    pub min: [i32; 3],
    pub size: [i32; 3],
    pub palette: Vec<String>,
    pub idx: Vec<u16>,
    pub biome: bool,
}

impl Snap {
    fn state(&self, i: usize) -> Option<&str> {
        self.palette
            .get(*self.idx.get(i)? as usize)
            .map(String::as_str)
    }
}

pub struct Entry {
    pub dim: String,
    pub snaps: Vec<Snap>,
    pub blocks: u64,
}

#[derive(Clone)]
pub struct Clip {
    pub palette: Vec<String>,
    pub size: [i32; 3],
    pub idx: Vec<u16>,
    pub offset: [i32; 3],
    pub origin: [i32; 3],
    pub biomes: Option<ClipBiomes>,
}

#[derive(Clone)]
pub struct ClipBiomes {
    pub palette: Vec<String>,
    pub idx: Vec<u16>,
}

impl Clip {
    pub fn capture_biomes(&mut self, dim: &str) -> bool {
        let world = World::new(dim);
        let mut cells: BTreeMap<[i32; 3], Option<String>> = BTreeMap::new();
        let mut palette: Vec<String> = Vec::new();
        let mut known: BTreeMap<String, u16> = BTreeMap::new();
        let mut idx = Vec::with_capacity(self.idx.len());
        let min = [0, 1, 2].map(|a| self.origin[a] - self.offset[a]);
        for y in 0..self.size[1] {
            for z in 0..self.size[2] {
                for x in 0..self.size[0] {
                    let cell = [
                        (min[0] + x) >> 2,
                        (min[1] + y) >> 2,
                        (min[2] + z) >> 2,
                    ];
                    let name = cells
                        .entry(cell)
                        .or_insert_with(|| {
                            world.biome(block_pos(cell[0] << 2, cell[1] << 2, cell[2] << 2)).ok()
                        })
                        .clone();
                    let Some(name) = name else {
                        return false;
                    };
                    idx.push(*known.entry(name.clone()).or_insert_with(|| {
                        palette.push(name);
                        (palette.len() - 1) as u16
                    }));
                }
            }
        }
        self.biomes = Some(ClipBiomes { palette, idx });
        true
    }

    fn biome_for_cell(&self, at: [i32; 3], corner: [i32; 3]) -> Option<&str> {
        let layer = self.biomes.as_ref()?;
        (0..64).find_map(|i| {
            let d = [
                corner[0] + (i & 3) - at[0],
                corner[1] + ((i >> 2) & 3) - at[1],
                corner[2] + (i >> 4) - at[2],
            ];
            if (0..3).any(|a| d[a] < 0 || d[a] >= self.size[a]) {
                return None;
            }
            let n = ((d[1] as usize * self.size[2] as usize) + d[2] as usize)
                * self.size[0] as usize
                + d[0] as usize;
            if *self.idx.get(n)? == UNAVAILABLE {
                return None;
            }
            layer.palette.get(*layer.idx.get(n)? as usize).map(String::as_str)
        })
    }

    pub fn volume(&self) -> u64 {
        self.idx.len() as u64
    }

    pub fn capture(dim: &str, a: [i32; 3], b: [i32; 3]) -> Option<Clip> {
        let (min, max) = order(a, b);
        let size = [
            max[0] - min[0] + 1,
            max[1] - min[1] + 1,
            max[2] - min[2] + 1,
        ];
        let mut idx =
            vec![UNAVAILABLE; (size[0] as usize) * (size[1] as usize) * (size[2] as usize)];
        let mut palette: Vec<String> = Vec::new();
        let mut known: BTreeMap<String, u16> = BTreeMap::new();
        for (from, part) in slices(min, max, SLICE) {
            let (local, indices) = read_box(dim, from, part)?;
            let map: Vec<u16> = local
                .into_iter()
                .map(|state| {
                    *known.entry(state.clone()).or_insert_with(|| {
                        palette.push(state);
                        (palette.len() - 1) as u16
                    })
                })
                .collect();
            let mut i = 0;
            for dy in 0..part[1] {
                for dz in 0..part[2] {
                    for dx in 0..part[0] {
                        let at = (((from[1] - min[1] + dy) as usize * size[2] as usize
                            + (from[2] - min[2] + dz) as usize)
                            * size[0] as usize)
                            + (from[0] - min[0] + dx) as usize;
                        let local = indices[i];
                        idx[at] = map.get(local as usize).copied().unwrap_or(UNAVAILABLE);
                        i += 1;
                    }
                }
            }
        }
        Some(Clip {
            palette,
            size,
            idx,
            offset: [0; 3],
            origin: min,
            biomes: None,
        })
    }

    pub fn at(&self, pos: [i32; 3]) -> Option<&str> {
        let d = [
            pos[0] - self.origin[0],
            pos[1] - self.origin[1],
            pos[2] - self.origin[2],
        ];
        if (0..3).any(|a| d[a] < 0 || d[a] >= self.size[a]) {
            return None;
        }
        let i = ((d[1] as usize * self.size[2] as usize) + d[2] as usize) * self.size[0] as usize
            + d[0] as usize;
        self.palette
            .get(*self.idx.get(i)? as usize)
            .map(String::as_str)
    }

    pub fn tile(&self, rel: [i32; 3]) -> Option<&str> {
        let d = [
            rel[0].rem_euclid(self.size[0]),
            rel[1].rem_euclid(self.size[1]),
            rel[2].rem_euclid(self.size[2]),
        ];
        let i = ((d[1] as usize * self.size[2] as usize) + d[2] as usize) * self.size[0] as usize
            + d[0] as usize;
        self.palette
            .get(*self.idx.get(i)? as usize)
            .map(String::as_str)
    }

    fn state(&self, at: [i32; 3], pos: [i32; 3]) -> Option<&str> {
        let d = [pos[0] - at[0], pos[1] - at[1], pos[2] - at[2]];
        let i = ((d[1] as usize * self.size[2] as usize) + d[2] as usize) * self.size[0] as usize
            + d[0] as usize;
        self.palette
            .get(*self.idx.get(i)? as usize)
            .map(String::as_str)
    }
}

pub enum Op {
    Set(Pattern),
    Replace(Mask, Pattern),
    Paste { clip: Clip, at: [i32; 3], air: bool },
    Cells(Cells),
    Gen(Gen, Pattern),
    Deform(Deform),
    Biome(String, Option<Gen>),
    Regen {
        seed: Option<i64>,
        biome: Option<String>,
    },
    Restore,
}

pub struct Work {
    min: [i32; 3],
    size: [i32; 3],
    src: Option<Snap>,
    biome: bool,
}

pub enum Step {
    More,
    Done,
    Failed(String),
}

pub struct Job {
    max: [i32; 3],
    heavy: bool,
    pub owner: u128,
    pub restoring: Option<bool>,
    pub total: u64,
    pub done: u64,
    pub changed: u64,
    pub ticks: u32,
    dim: String,
    op: Op,
    work: Vec<Work>,
    origin: [i32; 3],
    snaps: Vec<Snap>,
    recorded: u64,
    mask: Option<Region>,
    gmask: Option<Mask>,
    seed: u64,
}

fn plan(min: [i32; 3], max: [i32; 3], heavy: bool) -> Vec<Work> {
    let mut work: Vec<Work> = slices(min, max, if heavy { EXPR_SLICE } else { SLICE })
        .into_iter()
        .map(|(min, size)| Work {
            min,
            size,
            src: None,
            biome: false,
        })
        .collect();
    work.reverse();
    work
}

impl Op {
    fn heavy(&self) -> bool {
        match self {
            Op::Gen(..) | Op::Deform(_) => true,
            Op::Replace(mask, _) => mask.has_expr(),
            _ => false,
        }
    }
}

impl Job {
    pub fn new(owner: u128, dim: String, op: Op, a: [i32; 3], b: [i32; 3], total: u64) -> Job {
        let (min, max) = order(a, b);
        let heavy = op.heavy();
        let work = plan(min, max, heavy);
        Job {
            max,
            heavy,
            owner,
            restoring: None,
            total,
            done: 0,
            changed: 0,
            ticks: 0,
            dim,
            op,
            work,
            origin: min,
            snaps: Vec::new(),
            recorded: 0,
            mask: None,
            gmask: None,
            seed: (min[0] as u32 as u64) << 32
                ^ (min[2] as u32 as u64)
                ^ ((min[1] as u32 as u64) << 16),
        }
    }

    pub fn gmask(mut self, mask: Option<Mask>) -> Job {
        if !self.heavy && mask.as_ref().is_some_and(Mask::has_expr) {
            self.heavy = true;
            self.work = plan(self.origin, self.max, true);
        }
        self.gmask = mask;
        self
    }

    pub fn within(mut self, region: Region) -> Job {
        self.mask = Some(region);
        self
    }

    pub fn restore(owner: u128, entry: Entry, redo: bool) -> Job {
        let work = entry
            .snaps
            .into_iter()
            .map(|snap| Work {
                min: snap.min,
                size: snap.size,
                src: Some(snap),
                biome: false,
            })
            .collect();
        Job {
            max: [0; 3],
            heavy: false,
            owner,
            restoring: Some(redo),
            total: entry.blocks,
            done: 0,
            changed: 0,
            ticks: 0,
            dim: entry.dim,
            op: Op::Restore,
            work,
            origin: [0; 3],
            snaps: Vec::new(),
            recorded: 0,
            mask: None,
            gmask: None,
            seed: 0,
        }
    }

    pub fn biome(
        owner: u128,
        dim: String,
        name: String,
        gen_: Option<Gen>,
        region: Region,
    ) -> Job {
        let (min, max) = region.bounds();
        let (lo, hi) = (min.map(|v| v >> 2), max.map(|v| v >> 2));
        let cells = (0..3).map(|a| (hi[a] - lo[a] + 1) as u64).product();
        let mut work: Vec<Work> = slices(lo, hi, BIOME_SLICE)
            .into_iter()
            .map(|(min, size)| Work {
                min,
                size,
                src: None,
                biome: false,
            })
            .collect();
        work.reverse();
        let mut job = Job::new(owner, dim, Op::Biome(name, gen_), min, max, cells);
        job.work = work;
        job.within(region)
    }

    pub fn with_biomes_if(self, on: bool) -> Job {
        match on {
            true => self.with_biomes(),
            false => self,
        }
    }

    fn with_biomes(mut self) -> Job {
        let Op::Paste { clip, at, .. } = &self.op else {
            return self;
        };
        if clip.biomes.is_none() {
            return self;
        }
        let lo = at.map(|v| v >> 2);
        let hi = [0, 1, 2].map(|a| (at[a] + clip.size[a] - 1) >> 2);
        let mut extra: Vec<Work> = slices(lo, hi, BIOME_SLICE)
            .into_iter()
            .map(|(min, size)| Work {
                min,
                size,
                src: None,
                biome: true,
            })
            .collect();
        extra.reverse();
        extra.append(&mut self.work);
        self.work = extra;
        self
    }

    pub fn kind_name(&self) -> &'static str {
        match (&self.op, self.restoring) {
            (Op::Set(_), _) => "set",
            (Op::Replace(..), _) => "replace",
            (Op::Paste { .. }, _) => "paste",
            (Op::Cells(_), _) => "region edit",
            (Op::Gen(..), _) => "generate",
            (Op::Deform(_), _) => "deform",
            (Op::Biome(..), _) => "setbiome",
            (Op::Regen { .. }, _) => "regen",
            (Op::Restore, Some(true)) => "redo",
            (Op::Restore, _) => "undo",
        }
    }

    pub fn into_entry(self) -> Entry {
        Entry {
            dim: self.dim,
            snaps: self.snaps,
            blocks: self.recorded,
        }
    }

    pub fn step(&mut self) -> Step {
        if !self.heavy {
            return self.step_slice();
        }
        crate::expr::reset_spent();
        loop {
            match self.step_slice() {
                Step::More if crate::expr::spent() < EXPR_CALL_STEPS => {}
                other => return other,
            }
        }
    }

    fn step_slice(&mut self) -> Step {
        let before = crate::expr::spent();
        let Some(work) = self.work.pop() else {
            return Step::Done;
        };
        if work.biome || matches!(self.op, Op::Biome(..)) || work.src.as_ref().is_some_and(|s| s.biome) {
            return self.step_biome(work);
        }
        let Some((palette, idx)) = read_box(&self.dim, work.min, work.size) else {
            return Step::Failed("the blocks could not be read".to_owned());
        };
        let fresh = match &self.op {
            Op::Regen { seed, biome } => match read_blocks(
                &self.dim,
                work.min,
                work.size,
                Some((*seed, biome.as_deref())),
            ) {
                Some((palette, idx)) => Some(Snap {
                    min: work.min,
                    size: work.size,
                    palette,
                    idx,
                    biome: false,
                }),
                None => return Step::Failed("the terrain could not be generated".to_owned()),
            },
            _ => None,
        };
        let mut changed = 0u64;
        let env = Live {
            dim: &self.dim,
            region: self.mask.as_ref(),
            origin: self.origin,
            seed: self.seed,
            min: work.min,
            size: work.size,
            palette: &palette,
            idx: &idx,
            biomes: Default::default(),
        };
        let mut i = 0;
        for dy in 0..work.size[1] {
            for dz in 0..work.size[2] {
                for dx in 0..work.size[0] {
                    let old = idx[i];
                    let pos = [work.min[0] + dx, work.min[1] + dy, work.min[2] + dz];
                    let new = match palette.get(old as usize).map(String::as_str) {
                        Some(old) if !old.is_empty() => {
                            self.target(&env, old, pos, fresh.as_ref().or(work.src.as_ref()), i)
                        }
                        _ => None,
                    };
                    i += 1;
                    if crate::expr::spent() - before > EXPR_SLICE_STEPS {
                        return Step::Failed(
                            "the expression is too expensive to evaluate".to_owned(),
                        );
                    }
                    let Some(new) = new else { continue };
                    let new = new.as_str();
                    if self.mask.as_ref().is_some_and(|r| !r.contains(pos)) {
                        continue;
                    }
                    match block_set(&self.dim, pos, new, true) {
                        Ok(()) => changed += 1,
                        Err(WorldError::Unavailable) if self.changed == 0 && changed == 0 => {
                            return Step::Failed(format!("{new} is not a valid block state"));
                        }
                        Err(_) => return Step::Failed("the world refused a write".to_owned()),
                    }
                }
            }
        }
        if changed > 0 {
            let volume = idx.len() as u64;
            self.recorded += volume;
            self.snaps.push(Snap {
                min: work.min,
                size: work.size,
                palette,
                idx,
                biome: false,
            });
        }
        self.changed += changed;
        self.done += (work.size[0] as u64) * (work.size[1] as u64) * (work.size[2] as u64);
        match self.work.is_empty() {
            true => Step::Done,
            false => Step::More,
        }
    }

    fn step_biome(&mut self, work: Work) -> Step {
        let world = World::new(&self.dim);
        let wanted = match &self.op {
            Op::Biome(name, gen_) => Some((name.as_str(), gen_.as_ref())),
            _ => None,
        };
        let pasted = match &self.op {
            Op::Paste { clip, at, .. } if work.biome => Some((clip, *at)),
            _ => None,
        };
        let seed = self.seed;
        let mut palette: Vec<String> = Vec::new();
        let mut known: BTreeMap<String, u16> = BTreeMap::new();
        let mut idx = Vec::new();
        let mut changed = 0u64;
        for dy in 0..work.size[1] {
            for dz in 0..work.size[2] {
                for dx in 0..work.size[0] {
                    let cell = [work.min[0] + dx, work.min[1] + dy, work.min[2] + dz];
                    let corner = cell.map(|v| v << 2);
                    let at = block_pos(corner[0], corner[1], corner[2]);
                    let Ok(old) = world.biome(at) else {
                        return Step::Failed("the biomes could not be read".to_owned());
                    };
                    let slot = *known.entry(old.clone()).or_insert_with(|| {
                        palette.push(old.clone());
                        (palette.len() - 1) as u16
                    });
                    idx.push(slot);
                    let new = match wanted {
                        Some((name, gen_)) => (self.cell_touched(corner)
                            && gen_.is_none_or(|g| g.wants(corner, seed)))
                        .then_some(name),
                        None if pasted.is_some() => pasted
                            .and_then(|(clip, at)| clip.biome_for_cell(at, corner)),
                        None => work.src.as_ref().and_then(|s| s.state(idx.len() - 1)),
                    };
                    let Some(new) = new.filter(|new| *new != old) else {
                        continue;
                    };
                    match tuff_plugin_sdk::biome_set(&self.dim, corner, corner, new) {
                        Ok(()) => changed += 1,
                        Err(WorldError::Unavailable) if self.changed == 0 && changed == 0 => {
                            return Step::Failed(format!("{new} is not a biome"));
                        }
                        Err(_) => return Step::Failed("the world refused a write".to_owned()),
                    }
                }
            }
        }
        if changed > 0 {
            self.recorded += idx.len() as u64;
            self.snaps.push(Snap {
                min: work.min,
                size: work.size,
                palette,
                idx,
                biome: true,
            });
        }
        self.changed += changed;
        self.done += (work.size[0] as u64) * (work.size[1] as u64) * (work.size[2] as u64);
        match self.work.is_empty() {
            true => Step::Done,
            false => Step::More,
        }
    }

    fn cell_touched(&self, corner: [i32; 3]) -> bool {
        let Some(region) = &self.mask else {
            return true;
        };
        (0..64).any(|i| {
            region.contains([
                corner[0] + (i & 3),
                corner[1] + ((i >> 2) & 3),
                corner[2] + (i >> 4),
            ])
        })
    }

    fn target(
        &self,
        env: &Live,
        old: &str,
        pos: [i32; 3],
        src: Option<&Snap>,
        i: usize,
    ) -> Option<String> {
        if !matches!(self.op, Op::Restore)
            && self.gmask.as_ref().is_some_and(|g| !g.test(env, pos, old))
        {
            return None;
        }
        let new = match &self.op {
            Op::Set(pattern) => pattern.apply(env, pos, old),
            Op::Replace(from, to) => from
                .test(env, pos, old)
                .then(|| to.apply(env, pos, old))
                .flatten(),
            Op::Paste { clip, at, air } => clip
                .state(*at, pos)
                .filter(|s| !*air || !is_air(s))
                .map(str::to_owned),
            Op::Cells(cells) => match cells.get(pos)? {
                Item::Pat(p) => p.apply(env, pos, old),
                Item::State(s) => Some(s.clone()),
            },
            Op::Gen(g, pattern) => g
                .wants(pos, env.seed())
                .then(|| pattern.apply(env, pos, old))
                .flatten(),
            Op::Deform(d) => d.source(pos, env.seed()),
            Op::Biome(..) => None,
            Op::Regen { .. } | Op::Restore => src.and_then(|snap| snap.state(i)).map(str::to_owned),
        };
        new.filter(|new| new != old)
    }
}

struct Live<'a> {
    dim: &'a str,
    region: Option<&'a Region>,
    origin: [i32; 3],
    seed: u64,
    min: [i32; 3],
    size: [i32; 3],
    palette: &'a [String],
    idx: &'a [u16],
    biomes: std::cell::RefCell<BTreeMap<[i32; 3], Option<String>>>,
}

thread_local! {
    static TRAITS: std::cell::RefCell<BTreeMap<String, i32>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

impl Env for Live<'_> {
    fn traits(&self, state: &str) -> i32 {
        if let Some(bits) = TRAITS.with(|t| t.borrow().get(state).copied()) {
            return bits;
        }
        let bits = traits_of(state);
        TRAITS.with(|t| t.borrow_mut().insert(state.to_owned(), bits));
        bits
    }

    fn properties(&self, state: &str) -> Vec<(String, Vec<String>)> {
        properties(state)
    }

    fn normalize(&self, state: &str) -> Option<String> {
        normalize_state(state)
    }

    fn tag(&self, tag: &str) -> Option<Vec<String>> {
        tag_members(tag)
    }

    fn block_at(&self, pos: [i32; 3]) -> Option<String> {
        let inside = (0..3).all(|a| pos[a] >= self.min[a] && pos[a] < self.min[a] + self.size[a]);
        if inside {
            let d = [
                pos[0] - self.min[0],
                pos[1] - self.min[1],
                pos[2] - self.min[2],
            ];
            let i = (d[1] as usize * self.size[2] as usize + d[2] as usize) * self.size[0] as usize
                + d[0] as usize;
            return self.palette.get(*self.idx.get(i)? as usize).cloned();
        }
        let (palette, idx) = read_box(self.dim, pos, [1, 1, 1])?;
        palette.get(*idx.first()? as usize).cloned()
    }

    fn in_region(&self, pos: [i32; 3]) -> bool {
        self.region.is_none_or(|r| r.contains(pos))
    }

    fn origin(&self) -> [i32; 3] {
        self.origin
    }

    fn seed(&self) -> u64 {
        self.seed
    }

    fn biome_at(&self, pos: [i32; 3]) -> Option<String> {
        let cell = pos.map(|v| v >> 2);
        if let Some(found) = self.biomes.borrow().get(&cell) {
            return found.clone();
        }
        let at = tuff_plugin_sdk::world::pos(pos[0], pos[1], pos[2]);
        let found = tuff_plugin_sdk::world::World::new(self.dim).biome(at).ok();
        self.biomes.borrow_mut().insert(cell, found.clone());
        found
    }
}

pub struct Registry;

impl Env for Registry {
    fn traits(&self, state: &str) -> i32 {
        traits_of(state)
    }
    fn properties(&self, state: &str) -> Vec<(String, Vec<String>)> {
        properties(state)
    }
    fn normalize(&self, state: &str) -> Option<String> {
        normalize_state(state)
    }
    fn tag(&self, tag: &str) -> Option<Vec<String>> {
        tag_members(tag)
    }
    fn block_at(&self, _: [i32; 3]) -> Option<String> {
        None
    }
    fn in_region(&self, _: [i32; 3]) -> bool {
        true
    }
    fn origin(&self) -> [i32; 3] {
        [0; 3]
    }
    fn seed(&self) -> u64 {
        0
    }
}

fn traits_of(state: &str) -> i32 {
    tuff_plugin_sdk::world::BlockState::new(state)
        .traits()
        .map_or(0, |t| t.bits() as i32)
}

fn properties(state: &str) -> Vec<(String, Vec<String>)> {
    tuff_plugin_sdk::world::BlockState::new(state)
        .properties()
        .unwrap_or_default()
}

pub fn normalize_state(state: &str) -> Option<String> {
    tuff_plugin_sdk::world::BlockState::new(state)
        .normalize()
        .ok()
        .map(|s| s.as_str().to_owned())
}

fn tag_members(tag: &str) -> Option<Vec<String>> {
    tuff_plugin_sdk::world::block_tag(tag).ok()
}

pub fn scan(dim: &str, region: &Region, mut f: impl FnMut(&str)) -> Option<()> {
    let (min, max) = region.bounds();
    for (from, size) in slices(min, max, SLICE) {
        let (palette, idx) = read_box(dim, from, size)?;
        let mut i = 0;
        for dy in 0..size[1] {
            for dz in 0..size[2] {
                for dx in 0..size[0] {
                    let pos = [from[0] + dx, from[1] + dy, from[2] + dz];
                    if region.contains(pos)
                        && let Some(state) = palette.get(idx[i] as usize)
                    {
                        f(state);
                    }
                    i += 1;
                }
            }
        }
    }
    Some(())
}

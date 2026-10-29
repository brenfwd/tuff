// SPDX-License-Identifier: MIT
//! The `build` process, free of host calls: order the blocks bottom-up,
//! clear wrong blocks, place each from the hotbar, scaffold what has no
//! support and take the scaffolding down again.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use tuff_bots_api::control::{BlockPos, BuildBlock, BuildSpec, Vec3};

use crate::exec::Face;
use crate::hands::{Breaker, Carried, Hands, Orient, Placer, prop, stand_works};
use crate::path::{Pos, Tools};
use crate::world::{Blocks, name_of};

/// How deep a scaffold column may go below an unsupported block.
const MAX_SCAFFOLD: i32 = 8;
/// How far from a target column a standing pillar may go up.
const PILLAR_RANGE: i32 = 2;
/// How far below a target a pillar's ground may be.
const PILLAR_DROP: i32 = 4;
/// The tallest standing staircase a build may raise.
const PILLAR_HEIGHT: i32 = 3;
const MAX_RETRIES: u32 = 20;

/// Targets bottom-up, a row at a time in alternating directions so the
/// walk between placements stays short. A later entry for the same cell
/// replaces an earlier one.
pub fn order(
    origin: &BlockPos,
    blocks: &[tuff_bots_api::control::BuildBlock],
) -> Vec<(Pos, String)> {
    let mut cells: HashMap<Pos, String> = HashMap::new();
    for b in blocks {
        cells.insert(
            (
                origin.x + b.offset.x,
                origin.y + b.offset.y,
                origin.z + b.offset.z,
            ),
            b.state.clone(),
        );
    }
    let mut out: Vec<(Pos, String)> = cells.into_iter().collect();
    out.sort_by_key(|((x, y, z), _)| (*y, *z, *x));
    let mut flip = false;
    let mut i = 0;
    while i < out.len() {
        let key = (out[i].0.1, out[i].0.2);
        let mut j = i;
        while j < out.len() && (out[j].0.1, out[j].0.2) == key {
            j += 1;
        }
        if flip {
            out[i..j].reverse();
        }
        flip = !flip;
        i = j;
    }
    out
}

/// Item name counts the plan still needs, by block name.
pub fn needs(targets: &[(Pos, String)]) -> Vec<(String, u32)> {
    let mut counts: Vec<(String, u32)> = Vec::new();
    for (_, state) in targets {
        let name = name_of(state);
        if name == "air" || name == "cave_air" || name == "void_air" {
            continue;
        }
        match counts.iter_mut().find(|(n, _)| n == name) {
            Some((_, c)) => *c += 1,
            None => counts.push((name.to_owned(), 1)),
        }
    }
    counts
}

/// Whether `actual` is the wanted block with every property the wanted
/// state lists; properties it leaves out (stair shape, waterlogging) may be
/// anything.
pub fn state_matches(wanted: &str, actual: &str) -> bool {
    if name_of(wanted) != name_of(actual) {
        return false;
    }
    let Some(inner) = wanted
        .split_once('[')
        .and_then(|(_, rest)| rest.strip_suffix(']'))
    else {
        return true;
    };
    inner
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .all(|(k, v)| prop(actual, k) == Some(v))
}

/// Largest shape a command may ask for.
pub const MAX_SHAPE_BLOCKS: usize = 4096;

fn shape_block(x: i32, y: i32, z: i32, state: &str) -> BuildBlock {
    BuildBlock {
        offset: BlockPos { x, y, z },
        state: state.to_owned(),
    }
}

/// A `dx` by `dy` by `dz` box with its corner at the origin.
pub fn box_blocks(
    dx: i32,
    dy: i32,
    dz: i32,
    state: &str,
    hollow: bool,
) -> Result<Vec<BuildBlock>, String> {
    if dx < 1 || dy < 1 || dz < 1 {
        return Err("a box needs a size of at least 1 in each direction".into());
    }
    let volume = i64::from(dx)
        .saturating_mul(i64::from(dy))
        .saturating_mul(i64::from(dz));
    if volume > MAX_SHAPE_BLOCKS as i64 {
        return Err(format!("a box is at most {MAX_SHAPE_BLOCKS} blocks"));
    }
    let mut out = Vec::new();
    for y in 0..dy {
        for z in 0..dz {
            for x in 0..dx {
                let edge = x == 0 || x == dx - 1 || y == 0 || y == dy - 1 || z == 0 || z == dz - 1;
                if !hollow || edge {
                    out.push(shape_block(x, y, z, state));
                }
            }
        }
    }
    Ok(out)
}

/// `length` blocks in a row along `axis` (x, y or z).
pub fn line_blocks(axis: &str, length: i32, state: &str) -> Result<Vec<BuildBlock>, String> {
    if length < 1 || length as usize > MAX_SHAPE_BLOCKS {
        return Err(format!("a line is 1 to {MAX_SHAPE_BLOCKS} blocks"));
    }
    let step = match axis {
        "x" => (1, 0, 0),
        "y" => (0, 1, 0),
        "z" => (0, 0, 1),
        _ => return Err("the axis is x, y or z".into()),
    };
    Ok((0..length)
        .map(|i| shape_block(step.0 * i, step.1 * i, step.2 * i, state))
        .collect())
}

fn is_air(state: &str) -> bool {
    matches!(name_of(state), "air" | "cave_air" | "void_air")
}

enum Stage {
    Seek,
    Clearing(Breaker),
    Placing(Placer, Pos, bool),
    Stripping(Breaker),
}

pub struct Builder {
    todo: VecDeque<(Pos, String)>,
    targets: Rc<HashSet<Pos>>,
    scaffold: VecDeque<Pos>,
    scaffolds: Vec<Pos>,
    stage: Stage,
    checked: bool,
    retries: u32,
}

impl Builder {
    pub fn new(spec: &BuildSpec) -> Self {
        let todo: VecDeque<(Pos, String)> = order(&spec.origin, &spec.blocks).into();
        Self {
            targets: Rc::new(todo.iter().map(|(p, _)| *p).collect()),
            todo,
            scaffold: VecDeque::new(),
            scaffolds: Vec::new(),
            stage: Stage::Seek,
            checked: false,
            retries: 0,
        }
    }

    pub fn remaining(&self) -> usize {
        self.todo.len()
    }

    fn missing(&self, carried: &[Carried]) -> Option<String> {
        let pending: Vec<(Pos, String)> = self.todo.iter().cloned().collect();
        let short: Vec<String> = needs(&pending)
            .into_iter()
            .filter_map(|(name, need)| {
                let have: u32 = carried
                    .iter()
                    .filter(|(_, held, _)| name_of(held) == name)
                    .map(|(_, _, c)| *c)
                    .sum();
                (have < need).then(|| format!("{} {name}", need - have))
            })
            .collect();
        (!short.is_empty()).then(|| format!("missing materials: {}", short.join(", ")))
    }

    fn supported(world: &mut dyn Blocks, pos: Pos) -> Result<bool, ()> {
        for face in Face::ALL {
            let (dx, dy, dz) = face.normal();
            match world.class_at(pos.0 + dx, pos.1 + dy, pos.2 + dz) {
                Err(_) => return Err(()),
                Ok(c) if c.standable() => return Ok(true),
                Ok(_) => {}
            }
        }
        Ok(false)
    }

    /// A throwaway block, or else a plain block of the build itself.
    fn scaffold_item(&self, throwaway: &[String], carried: &[Carried]) -> Option<String> {
        let stocked = carried.iter().filter(|(_, _, count)| *count > 0);
        stocked
            .clone()
            .find(|(_, held, _)| throwaway.iter().any(|t| name_of(t) == name_of(held)))
            .or_else(|| {
                stocked.clone().find(|(_, held, _)| {
                    self.todo
                        .iter()
                        .any(|(_, state)| !state.contains('[') && name_of(state) == name_of(held))
                })
            })
            .map(|(_, held, _)| held.clone())
    }

    pub fn tick(
        &mut self,
        me: &Vec3,
        world: &mut dyn Blocks,
        carried: &[Carried],
        tools: Option<&Rc<RefCell<dyn Tools>>>,
        allow_break: bool,
        throwaway: &[String],
    ) -> Hands {
        if !self.checked {
            self.checked = true;
            if let Some(reason) = self.missing(carried) {
                return Hands::Failed(reason);
            }
        }
        match &mut self.stage {
            Stage::Clearing(breaker) => {
                match breaker.tick(me, world, tools) {
                    Hands::Done => self.stage = Stage::Seek,
                    other => return other,
                }
                Hands::Wait
            }
            Stage::Stripping(breaker) => breaker.tick(me, world, tools),
            Stage::Placing(placer, pos, scaffolding) => {
                let (pos, scaffolding) = (*pos, *scaffolding);
                match placer.tick(me, world, carried) {
                    Hands::Done => {
                        if scaffolding {
                            self.scaffolds.push(pos);
                        } else {
                            self.todo.retain(|(p, _)| *p != pos);
                        }
                        self.stage = Stage::Seek;
                        Hands::Wait
                    }
                    Hands::Failed(reason)
                        if reason == "in the way" && self.retries < MAX_RETRIES =>
                    {
                        self.retries += 1;
                        if scaffolding {
                            self.scaffold.push_back(pos);
                        } else if let Some(i) = self.todo.iter().position(|(p, _)| *p == pos) {
                            let item = self.todo.remove(i).unwrap();
                            self.todo.push_back(item);
                        }
                        self.stage = Stage::Seek;
                        Hands::Wait
                    }
                    Hands::Failed(reason) if reason == "no stand" && self.retries < MAX_RETRIES => {
                        self.retries += 1;
                        let state = self
                            .todo
                            .iter()
                            .find(|(p, _)| *p == pos)
                            .map(|(_, s)| s.clone())
                            .unwrap_or_default();
                        match self.plan_pillar(me, world, pos, &state) {
                            Err(()) => Hands::Wait,
                            Ok(Some(plan)) => {
                                if scaffolding {
                                    self.scaffold.push_front(pos);
                                }
                                for cell in plan.into_iter().rev() {
                                    self.scaffold.push_front(cell);
                                }
                                self.stage = Stage::Seek;
                                Hands::Wait
                            }
                            Ok(None) => Hands::Failed(reason),
                        }
                    }
                    other => other,
                }
            }
            Stage::Seek => self.seek(world, carried, allow_break, throwaway),
        }
    }

    fn seek(
        &mut self,
        world: &mut dyn Blocks,
        carried: &[Carried],
        allow_break: bool,
        throwaway: &[String],
    ) -> Hands {
        if let Some(&pos) = self.scaffold.front() {
            let Some(item) = self.scaffold_item(throwaway, carried) else {
                return Hands::Failed("missing materials: a scaffolding block".into());
            };
            self.scaffold.pop_front();
            self.stage = Stage::Placing(
                Placer::new(
                    &BlockPos {
                        x: pos.0,
                        y: pos.1,
                        z: pos.2,
                    },
                    &item,
                ),
                pos,
                true,
            );
            return Hands::Wait;
        }
        let mut deferred = None;
        let mut i = 0;
        while i < self.todo.len() {
            let (pos, state) = self.todo[i].clone();
            let Ok(actual) = world.state_at(pos.0, pos.1, pos.2) else {
                return Hands::Wait;
            };
            let Ok(class) = world.class_at(pos.0, pos.1, pos.2) else {
                return Hands::Wait;
            };
            let wanted = name_of(&state);
            let right = match &actual {
                Some(a) if is_air(&state) => is_air(a) || class.passable(),
                Some(a) => state_matches(&state, a),
                None => false,
            };
            if right {
                self.todo.remove(i);
                continue;
            }
            let occupied = !class.passable() || class.climbable();
            if occupied {
                if !allow_break {
                    return Hands::Failed("occupied".into());
                }
                self.stage = Stage::Clearing(Breaker::new(&[BlockPos {
                    x: pos.0,
                    y: pos.1,
                    z: pos.2,
                }]));
                return Hands::Wait;
            }
            if is_air(&state) {
                self.todo.remove(i);
                continue;
            }
            match Self::supported(world, pos) {
                Err(()) => return Hands::Wait,
                Ok(true) => {
                    self.stage = Stage::Placing(
                        Placer::oriented(
                            &BlockPos {
                                x: pos.0,
                                y: pos.1,
                                z: pos.2,
                            },
                            wanted,
                            &state,
                        )
                        .avoiding(&self.targets),
                        pos,
                        false,
                    );
                    return Hands::Wait;
                }
                Ok(false) => {
                    deferred.get_or_insert(pos);
                    i += 1;
                }
            }
        }
        if let Some(pos) = deferred {
            return self.plan_scaffold(world, pos);
        }
        if self.scaffolds.is_empty() {
            return Hands::Done;
        }
        let strip: Vec<BlockPos> = self
            .scaffolds
            .drain(..)
            .rev()
            .map(|p| BlockPos {
                x: p.0,
                y: p.1,
                z: p.2,
            })
            .collect();
        self.stage = Stage::Stripping(Breaker::new(&strip));
        Hands::Wait
    }

    /// Queues a throwaway staircase beside the build to stand on when no
    /// ground stand reaches `target`; the cells to fill, bottom first. Column `i`
    /// of a staircase of height `h` is `i + 1` blocks tall and the bot
    /// stands on the last one.
    fn plan_pillar(
        &mut self,
        me: &Vec3,
        world: &mut dyn Blocks,
        target: Pos,
        state: &str,
    ) -> Result<Option<Vec<Pos>>, ()> {
        let orient = Orient::from_state(state);
        let here = (me.x.floor() as i32, me.z.floor() as i32);
        let mut starts: Vec<(i32, i32)> = Vec::new();
        for dx in -PILLAR_RANGE..=PILLAR_RANGE {
            for dz in -PILLAR_RANGE..=PILLAR_RANGE {
                if (dx, dz) != (0, 0) {
                    starts.push((target.0 + dx, target.2 + dz));
                }
            }
        }
        starts.sort_by_key(|c| (c.0 - here.0).abs() + (c.1 - here.1).abs());
        for h in 1..=PILLAR_HEIGHT {
            for &(x, z) in &starts {
                for (sx, sz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    if let Some(plan) =
                        self.staircase(world, here, (x, z), (sx, sz), h, target, &orient)?
                    {
                        return Ok(Some(plan));
                    }
                }
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn staircase(
        &self,
        world: &mut dyn Blocks,
        here: (i32, i32),
        start: (i32, i32),
        step: (i32, i32),
        h: i32,
        target: Pos,
        orient: &Orient,
    ) -> Result<Option<Vec<Pos>>, ()> {
        let column = |i: i32| (start.0 + step.0 * i, start.1 + step.1 * i);
        let mut base = None;
        for y in (target.1 - PILLAR_DROP..=target.1).rev() {
            let (x, z) = start;
            let ours = self.scaffolds.contains(&(x, y - 1, z));
            if !ours && world.class_at(x, y - 1, z).map_err(|_| ())?.standable() {
                base = Some(y);
                break;
            }
        }
        let Some(y) = base else {
            return Ok(None);
        };
        for i in 0..h {
            let (x, z) = column(i);
            if (x, z) == here || (x, z) == (target.0, target.2) {
                return Ok(None);
            }
            let ours = self.scaffolds.contains(&(x, y - 1, z));
            if ours || !world.class_at(x, y - 1, z).map_err(|_| ())?.standable() {
                return Ok(None);
            }
            let top = if i == h - 1 { y + i + 4 } else { y + i + 1 };
            for cy in y..top {
                let class = world.class_at(x, cy, z).map_err(|_| ())?;
                let building =
                    self.targets.contains(&(x, cy, z)) || self.scaffold.contains(&(x, cy, z));
                let free = class.passable() && !class.climbable();
                if building || !free {
                    return Ok(None);
                }
            }
        }
        let (lx, lz) = column(h - 1);
        if !stand_works(world, (lx, y + h, lz), target, orient)? {
            return Ok(None);
        }
        let mut plan = Vec::new();
        for j in 0..h {
            for i in j..h {
                let (x, z) = column(i);
                plan.push((x, y + j, z));
            }
        }
        Ok(Some(plan))
    }

    fn plan_scaffold(&mut self, world: &mut dyn Blocks, target: Pos) -> Hands {
        for d in 1..=MAX_SCAFFOLD {
            let cell = (target.0, target.1 - d, target.2);
            let Ok(class) = world.class_at(cell.0, cell.1, cell.2) else {
                return Hands::Wait;
            };
            if class.standable() {
                self.scaffold = (1..d)
                    .rev()
                    .map(|k| (target.0, target.1 - k, target.2))
                    .collect();
                return if self.scaffold.iter().any(|c| self.targets.contains(c)) {
                    self.scaffold.clear();
                    Hands::Failed("nothing to place against".into())
                } else {
                    Hands::Wait
                };
            }
        }
        Hands::Failed("nothing to place against".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(x: i32, y: i32, z: i32, state: &str) -> BuildBlock {
        BuildBlock {
            offset: BlockPos { x, y, z },
            state: state.into(),
        }
    }

    #[test]
    fn order_is_bottom_up_and_the_last_duplicate_wins() {
        let origin = BlockPos { x: 10, y: 0, z: 0 };
        let blocks = [
            block(0, 2, 0, "minecraft:stone"),
            block(0, 0, 0, "minecraft:dirt"),
            block(1, 0, 0, "minecraft:dirt"),
            block(0, 1, 0, "minecraft:stone"),
            block(0, 0, 0, "minecraft:glass"),
        ];
        let out = order(&origin, &blocks);
        let ys: Vec<i32> = out.iter().map(|((_, y, _), _)| *y).collect();
        assert_eq!(ys, [0, 0, 1, 2]);
        assert_eq!(out.len(), 4);
        assert!(
            out.iter()
                .any(|(p, s)| *p == (10, 0, 0) && s == "minecraft:glass")
        );
    }

    #[test]
    fn needs_counts_by_name_and_skips_air() {
        let t = vec![
            ((0, 0, 0), "minecraft:oak_stairs[facing=north]".to_owned()),
            ((1, 0, 0), "minecraft:oak_stairs[facing=east]".to_owned()),
            ((2, 0, 0), "minecraft:air".to_owned()),
        ];
        assert_eq!(needs(&t), [("oak_stairs".to_owned(), 2)]);
    }

    #[test]
    fn a_wanted_state_matches_when_its_listed_properties_agree() {
        let wanted = "minecraft:oak_stairs[facing=east,half=top]";
        assert!(state_matches(
            wanted,
            "minecraft:oak_stairs[facing=east,half=top,shape=straight,waterlogged=false]"
        ));
        assert!(!state_matches(
            wanted,
            "minecraft:oak_stairs[facing=north,half=top,shape=straight]"
        ));
        assert!(!state_matches(wanted, "minecraft:oak_stairs"));
        assert!(!state_matches(wanted, "minecraft:stone"));
        assert!(state_matches("minecraft:stone", "minecraft:stone"));
        assert!(state_matches("stone", "minecraft:stone"));
    }

    #[test]
    fn a_hollow_box_is_its_shell_and_a_solid_box_is_full() {
        let hollow = box_blocks(3, 3, 3, "minecraft:cobblestone", true).unwrap();
        assert_eq!(hollow.len(), 26);
        assert!(
            hollow
                .iter()
                .all(|b| b.offset != BlockPos { x: 1, y: 1, z: 1 })
        );
        assert_eq!(
            box_blocks(3, 3, 3, "minecraft:stone", false).unwrap().len(),
            27
        );
        assert_eq!(
            box_blocks(1, 1, 1, "minecraft:stone", true).unwrap().len(),
            1
        );
        assert_eq!(
            box_blocks(2, 2, 2, "minecraft:stone", true).unwrap().len(),
            8
        );
    }

    #[test]
    fn shapes_reject_empty_oversize_and_bad_axes() {
        assert!(box_blocks(0, 3, 3, "minecraft:stone", true).is_err());
        assert!(box_blocks(-1, 3, 3, "minecraft:stone", true).is_err());
        assert!(box_blocks(17, 16, 16, "minecraft:stone", false).is_err());
        assert!(box_blocks(i32::MAX, i32::MAX, i32::MAX, "minecraft:stone", false).is_err());
        assert!(line_blocks("w", 3, "minecraft:stone").is_err());
        assert!(line_blocks("x", 0, "minecraft:stone").is_err());
        let line = line_blocks("z", 3, "minecraft:stone").unwrap();
        assert_eq!(line[2].offset, BlockPos { x: 0, y: 0, z: 2 });
    }
}

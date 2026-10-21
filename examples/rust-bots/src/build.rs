// SPDX-License-Identifier: MIT
//! The `build` process, free of host calls: order the blocks bottom-up,
//! clear wrong blocks, place each from the hotbar, scaffold what has no
//! support and take the scaffolding down again.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use tuff_bots_api::control::{BlockPos, BuildSpec, Vec3};

use crate::exec::Face;
use crate::hands::{Breaker, Carried, Hands, Placer};
use crate::path::{Pos, Tools};
use crate::world::{Blocks, name_of};

/// How deep a scaffold column may go below an unsupported block.
const MAX_SCAFFOLD: i32 = 8;
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
    targets: HashSet<Pos>,
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
            targets: todo.iter().map(|(p, _)| *p).collect(),
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

    fn scaffold_item(throwaway: &[String], carried: &[Carried]) -> Option<String> {
        carried
            .iter()
            .filter(|(_, _, count)| *count > 0)
            .find(|(_, held, _)| throwaway.iter().any(|t| name_of(t) == name_of(held)))
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
                        if !scaffolding
                            && let Some(i) = self.todo.iter().position(|(p, _)| *p == pos)
                        {
                            let item = self.todo.remove(i).unwrap();
                            self.todo.push_back(item);
                        }
                        self.stage = Stage::Seek;
                        Hands::Wait
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
            let Some(item) = Self::scaffold_item(throwaway, carried) else {
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
                Some(a) => name_of(a) == wanted,
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
                        ),
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
    use tuff_bots_api::control::BuildBlock;

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
}

// SPDX-License-Identifier: MIT
//! The `mine` and `get-to-block` processes, free of host calls: find
//! blocks by name in a bounded radius (budgeted section scans), pick the
//! nearest safe one, get within reach, dig it and collect what it drops.

use std::collections::{BTreeSet, HashSet};

use tuff_bots_api::control::{BlockPos, Goal, LeafGoal, NearGoal, Vec3};

use crate::core::goal_reached;
use crate::world::{Blocks, SectionKey, name_of};

pub type Pos = (i32, i32, i32);

/// How far from the eyes a block may be dug.
pub const REACH: f64 = 4.0;
/// How far an exhausted search wanders looking for more blocks.
pub const EXPLORE_RADIUS: u32 = 128;
const EYE: f64 = 1.62;
const DIG_TIMEOUT: u32 = 400;
const COLLECT_TICKS: u32 = 60;
/// Drops cannot be picked up for this many ticks after they spawn.
const PICKUP_DELAY: u32 = 15;
const COLLECT_RADIUS: f64 = 0.5;
const RECHECK_TICKS: u32 = 10;
/// Longest branch tunnel dug when no matching block is known.
const TUNNEL_MAX: i32 = 48;

#[derive(Debug, PartialEq)]
pub enum Mining {
    Done,
    Goal(Goal),
    Dig(Pos),
    Wait,
    /// Nothing known within the radius: walk somewhere new.
    Explore,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Seek,
    Digging { pos: Pos, ticks: u32 },
    Collect { pos: Pos, ticks: u32 },
}

pub struct Miner {
    wanted: Vec<String>,
    count: u32,
    mined: u32,
    radius: u32,
    dig: bool,
    origin: BlockPos,
    scanned: HashSet<SectionKey>,
    found: BTreeSet<Pos>,
    bad: HashSet<Pos>,
    current: Option<Pos>,
    phase: Phase,
    tunnelling: bool,
}

/// Blocks that drop when the support below them is dug away.
fn falls(state: &str) -> bool {
    let name = name_of(state);
    name == "gravel"
        || name == "sand"
        || name == "red_sand"
        || name == "suspicious_sand"
        || name == "suspicious_gravel"
        || name == "anvil"
        || name == "chipped_anvil"
        || name == "damaged_anvil"
        || name == "dragon_egg"
        || name.ends_with("concrete_powder")
}

/// A section that failed to load reads as empty space, so a block only
/// counts as gone once its state was actually read.
fn confirmed_gone(world: &mut dyn Blocks, pos: Pos) -> bool {
    matches!(world.state_at(pos.0, pos.1, pos.2), Ok(Some(_)))
        && world
            .class_at(pos.0, pos.1, pos.2)
            .is_ok_and(|c| !c.standable())
}

fn centre((x, y, z): Pos) -> Vec3 {
    Vec3 {
        x: f64::from(x) + 0.5,
        y: f64::from(y) + 0.5,
        z: f64::from(z) + 0.5,
    }
}

fn dist2(a: &Vec3, b: &Vec3) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)
}

impl Miner {
    pub fn new(blocks: &[String], count: u32, radius: u32, dig: bool, at: &Vec3) -> Self {
        Self {
            wanted: blocks.iter().map(|b| name_of(b).to_owned()).collect(),
            count,
            mined: 0,
            radius: radius.max(1),
            dig,
            origin: BlockPos {
                x: at.x.floor() as i32,
                y: at.y.floor() as i32,
                z: at.z.floor() as i32,
            },
            scanned: HashSet::new(),
            found: BTreeSet::new(),
            bad: HashSet::new(),
            current: None,
            phase: Phase::Seek,
            tunnelling: false,
        }
    }

    pub fn mined(&self) -> u32 {
        self.mined
    }

    pub fn origin(&self) -> &BlockPos {
        &self.origin
    }

    /// Gives up on the block being worked on (unreachable or stuck).
    pub fn skip_current(&mut self) {
        if let Some(pos) = self.current.take() {
            self.found.remove(&pos);
            self.bad.insert(pos);
        }
        self.phase = Phase::Seek;
    }

    /// Reads sections near `me` until the budget runs out; true once
    /// every section within the radius has been read.
    fn scan(&mut self, me: &Vec3, world: &mut dyn Blocks) -> bool {
        let r = f64::from(self.radius);
        let lo = |v: f64| ((v - r).floor() as i32) >> 4;
        let hi = |v: f64| ((v + r).floor() as i32) >> 4;
        let mut todo = Vec::new();
        for sx in lo(me.x)..=hi(me.x) {
            for sy in lo(me.y)..=hi(me.y) {
                for sz in lo(me.z)..=hi(me.z) {
                    let key = (sx, sy, sz);
                    if self.scanned.contains(&key) {
                        continue;
                    }
                    let c = centre((sx * 16 + 8, sy * 16 + 8, sz * 16 + 8));
                    let d = dist2(&c, me);
                    if d.sqrt() <= r + 14.0 {
                        todo.push((d, key));
                    }
                }
            }
        }
        todo.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, key) in todo {
            match world.find(key, &self.wanted) {
                Ok(hits) => {
                    self.scanned.insert(key);
                    self.found.extend(hits);
                }
                Err(_) => return false,
            }
        }
        true
    }

    fn hazardous(&self, pos: Pos, world: &mut dyn Blocks) -> bool {
        let (x, y, z) = pos;
        if world
            .state_at(x, y + 1, z)
            .ok()
            .flatten()
            .is_some_and(|s| falls(&s))
        {
            return true;
        }
        [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ]
        .iter()
        .any(|(dx, dy, dz)| {
            world
                .class_at(x + dx, y + dy, z + dz)
                .is_ok_and(|c| c.danger() || c.water())
        })
    }

    fn pick(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Option<Pos> {
        let mut order: Vec<Pos> = self
            .found
            .iter()
            .copied()
            .filter(|p| !self.bad.contains(p))
            .collect();
        order.sort_by(|a, b| dist2(&centre(*a), me).total_cmp(&dist2(&centre(*b), me)));
        for pos in order {
            if confirmed_gone(world, pos) {
                self.found.remove(&pos);
                continue;
            }
            if self.hazardous(pos, world) {
                self.bad.insert(pos);
                continue;
            }
            return Some(pos);
        }
        None
    }

    pub fn tick(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Mining {
        if self.dig && self.mined >= self.count && matches!(self.phase, Phase::Seek) {
            return Mining::Done;
        }
        match self.phase {
            Phase::Collect { pos, ticks } => {
                let c = centre(pos);
                let near = (c.x - me.x).hypot(c.z - me.z) <= COLLECT_RADIUS + 0.1
                    && (c.y - me.y).abs() <= 1.5;
                if ticks == 0 || (near && ticks <= COLLECT_TICKS - PICKUP_DELAY) {
                    self.phase = Phase::Seek;
                    return self.tick(me, world);
                }
                self.phase = Phase::Collect {
                    pos,
                    ticks: ticks - 1,
                };
                Mining::Goal(Goal::Leaf(LeafGoal::Near(NearGoal {
                    pos: BlockPos {
                        x: pos.0,
                        y: pos.1,
                        z: pos.2,
                    },
                    radius: COLLECT_RADIUS,
                })))
            }
            Phase::Digging { pos, ticks } => {
                if ticks.is_multiple_of(RECHECK_TICKS) {
                    world.refresh(&[pos]);
                    if confirmed_gone(world, pos) {
                        if !self.tunnelling {
                            self.mined += 1;
                        }
                        self.found.remove(&pos);
                        self.current = None;
                        self.phase = Phase::Collect {
                            pos,
                            ticks: COLLECT_TICKS,
                        };
                        return self.tick(me, world);
                    }
                }
                if ticks >= DIG_TIMEOUT {
                    self.skip_current();
                    return Mining::Wait;
                }
                self.phase = Phase::Digging {
                    pos,
                    ticks: ticks + 1,
                };
                Mining::Wait
            }
            Phase::Seek => self.seek(me, world),
        }
    }

    fn seek(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Mining {
        let scanned_all = self.scan(me, world);
        self.tunnelling = false;
        let Some(pos) = self.pick(me, world) else {
            if !scanned_all {
                return Mining::Wait;
            }
            return self.tunnel(me, world).unwrap_or(Mining::Explore);
        };
        self.current = Some(pos);
        let eyes = Vec3 {
            y: me.y + EYE,
            ..me.clone()
        };
        if self.dig && dist2(&centre(pos), &eyes) <= REACH * REACH {
            self.phase = Phase::Digging { pos, ticks: 0 };
            return Mining::Dig(pos);
        }
        let goal = Goal::Leaf(LeafGoal::GetToBlock(BlockPos {
            x: pos.0,
            y: pos.1,
            z: pos.2,
        }));
        if !self.dig && goal_reached(&goal, me) {
            return Mining::Done;
        }
        Mining::Goal(goal)
    }

    /// Branch mining: with nothing known, dig a two-high tunnel along +x
    /// at the starting level, stopping at fluid or falling blocks.
    fn tunnel(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Option<Mining> {
        if !self.dig {
            return None;
        }
        let ahead = me.x.floor() as i32 + 1;
        if ahead - self.origin.x > TUNNEL_MAX {
            return None;
        }
        let (y, z) = (self.origin.y, self.origin.z);
        for dy in 0..2 {
            let pos = (ahead, y + dy, z);
            let class = world.class_at(pos.0, pos.1, pos.2).ok()?;
            if class.danger() || class.water() {
                return None;
            }
            if !class.standable() {
                continue;
            }
            if self.hazardous(pos, world) {
                return None;
            }
            let eyes = Vec3 {
                y: me.y + EYE,
                ..me.clone()
            };
            if dist2(&centre(pos), &eyes) > REACH * REACH {
                return None;
            }
            self.tunnelling = true;
            self.current = Some(pos);
            self.phase = Phase::Digging { pos, ticks: 0 };
            return Some(Mining::Dig(pos));
        }
        Some(Mining::Goal(Goal::Leaf(LeafGoal::Near(NearGoal {
            pos: BlockPos { x: ahead, y, z },
            radius: 0.5,
        }))))
    }
}

// SPDX-License-Identifier: MIT
//! The `mine` and `get-to-block` processes, free of host calls: find
//! blocks by name in a bounded radius (budgeted section scans), pick the
//! nearest safe one, get within reach, dig it and collect what it drops.

use std::collections::{BTreeSet, HashSet};

use tuff_bots_api::control::{BlockPos, BranchSpec, Goal, Heading, LeafGoal, NearGoal, Vec3};

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
    Failed(String),
}

/// Verified breaks in a row that gave nothing before collecting gives up.
pub const COLLECT_STALL: u32 = 6;
/// Ticks to let the inventory view catch up before the baseline is read.
const BASELINE_SETTLE: u32 = 10;

/// Progress of a collect-mode miner: the wanted item is counted as an
/// increase over what was held on the first tick.
struct Collect {
    item: String,
    baseline: Option<u32>,
    settled: u32,
    last_held: u32,
    breaks_without_gain: u32,
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
    collect: Option<Collect>,
    radius: u32,
    dig: bool,
    origin: BlockPos,
    scanned: HashSet<SectionKey>,
    found: BTreeSet<Pos>,
    bad: HashSet<Pos>,
    retried: HashSet<Pos>,
    current: Option<Pos>,
    phase: Phase,
    tunnelling: bool,
    level: i32,
    dx: i32,
    dz: i32,
    level_at: Option<i32>,
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

/// Whether a face of the block at `pos` borders open space. A buried block
/// is reached by digging toward it instead: struck through other blocks it
/// is out of sight, and its drop would be sealed in.
fn exposed(world: &mut dyn Blocks, (x, y, z): Pos) -> bool {
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
            .is_ok_and(|c| !c.standable())
    })
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
    pub fn new(
        blocks: &[String],
        count: u32,
        radius: u32,
        dig: bool,
        branch: Option<&BranchSpec>,
        collect: Option<&str>,
        at: &Vec3,
    ) -> Self {
        let y = at.y.floor() as i32;
        let (dx, dz) = match branch.map_or(Heading::East, |b| b.heading) {
            Heading::North => (0, -1),
            Heading::South => (0, 1),
            Heading::East => (1, 0),
            Heading::West => (-1, 0),
        };
        Self {
            level: branch.and_then(|b| b.level).unwrap_or(y),
            dx,
            dz,
            level_at: None,
            wanted: blocks.iter().map(|b| name_of(b).to_owned()).collect(),
            count,
            mined: 0,
            collect: collect.map(|item| Collect {
                item: name_of(item).to_owned(),
                baseline: None,
                settled: 0,
                last_held: 0,
                breaks_without_gain: 0,
            }),
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
            retried: HashSet::new(),
            current: None,
            phase: Phase::Seek,
            tunnelling: false,
        }
    }

    /// The item being collected, when in collect mode.
    pub fn collecting(&self) -> Option<&str> {
        self.collect.as_ref().map(|c| c.item.as_str())
    }

    /// Why the process fails when the whole search area has been walked.
    pub fn exhausted(&self) -> String {
        match self.collecting() {
            Some(item) => format!("nothing nearby drops {item}"),
            None => "none found".into(),
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
        if let Some(pos) = self.current.take()
            && !self.tunnelling
        {
            self.found.remove(&pos);
            self.bad.insert(pos);
        }
        self.tunnelling = false;
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

    /// A wanted block the path could not reach but a tunnel has now come
    /// within reach of is dug once.
    fn bad_in_reach(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Option<Pos> {
        if !self.dig {
            return None;
        }
        let eyes = Vec3 {
            y: me.y + EYE,
            ..me.clone()
        };
        let mut near: Vec<Pos> = self
            .bad
            .iter()
            .copied()
            .filter(|p| !self.retried.contains(p))
            .filter(|p| dist2(&centre(*p), &eyes) <= REACH * REACH)
            .collect();
        near.sort();
        for pos in near {
            if confirmed_gone(world, pos) {
                self.bad.remove(&pos);
            } else if !self.hazardous(pos, world) {
                self.retried.insert(pos);
                return Some(pos);
            }
        }
        None
    }

    /// `held` is how many of the collected item the bot carries now.
    pub fn tick(&mut self, me: &Vec3, world: &mut dyn Blocks, held: u32) -> Mining {
        let mut stalled = false;
        let done = match self.collect.as_mut() {
            Some(c) => {
                if c.baseline.is_none() && c.settled < BASELINE_SETTLE {
                    c.settled += 1;
                    return Mining::Wait;
                }
                let baseline = *c.baseline.get_or_insert(held);
                if held > c.last_held.max(baseline) {
                    c.breaks_without_gain = 0;
                }
                c.last_held = held;
                stalled = c.breaks_without_gain >= COLLECT_STALL;
                held >= baseline.saturating_add(self.count)
            }
            None => self.mined >= self.count,
        };
        if self.dig && matches!(self.phase, Phase::Seek) {
            if done {
                return Mining::Done;
            }
            if stalled {
                let item = self.collect.as_ref().map(|c| c.item.clone());
                return Mining::Failed(format!(
                    "{COLLECT_STALL} breaks gave no {}",
                    item.unwrap_or_default()
                ));
            }
        }
        match self.phase {
            Phase::Collect { pos, ticks } => {
                let c = centre(pos);
                let near = (c.x - me.x).hypot(c.z - me.z) <= COLLECT_RADIUS + 0.1
                    && (c.y - me.y).abs() <= 1.5;
                if ticks == 0 || (near && ticks <= COLLECT_TICKS - PICKUP_DELAY) {
                    self.phase = Phase::Seek;
                    return self.tick(me, world, held);
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
                            if let Some(c) = self.collect.as_mut() {
                                c.breaks_without_gain += 1;
                            }
                        }
                        self.found.remove(&pos);
                        self.current = None;
                        self.phase = if self.tunnelling {
                            Phase::Seek
                        } else {
                            Phase::Collect {
                                pos,
                                ticks: COLLECT_TICKS,
                            }
                        };
                        return self.tick(me, world, held);
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
            if let Some(pos) = self.bad_in_reach(me, world) {
                self.current = Some(pos);
                self.phase = Phase::Digging { pos, ticks: 0 };
                return Mining::Dig(pos);
            }
            return self.tunnel(me, world).unwrap_or(Mining::Explore);
        };
        self.current = Some(pos);
        let eyes = Vec3 {
            y: me.y + EYE,
            ..me.clone()
        };
        if self.dig && dist2(&centre(pos), &eyes) <= REACH * REACH && exposed(world, pos) {
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

    /// Branch mining: with nothing known, dig a two-high tunnel along the
    /// heading at the planned level (a stair first when the bot is not
    /// there yet), stopping at fluid or falling blocks.
    fn tunnel(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Option<Mining> {
        if !self.dig {
            return None;
        }
        let (fx, fy, fz) = (
            me.x.floor() as i32,
            me.y.floor() as i32,
            me.z.floor() as i32,
        );
        let step = (fy - self.level).signum();
        let progress =
            |x: i32, z: i32| (x - self.origin.x) * self.dx + (z - self.origin.z) * self.dz;
        if step == 0 {
            let here = progress(fx, fz);
            if progress(fx, fz) - *self.level_at.get_or_insert(here) >= TUNNEL_MAX {
                return None;
            }
        }
        let (ax, az) = (fx + self.dx, fz + self.dz);
        let cells: Vec<Pos> = match step {
            0 => vec![(ax, fy, az), (ax, fy + 1, az)],
            s if s > 0 => vec![(ax, fy + 1, az), (ax, fy, az), (ax, fy - 1, az)],
            _ => vec![(ax, fy + 1, az), (ax, fy + 2, az), (fx, fy + 2, fz)],
        };
        let target_y = fy - step;
        let floor = (ax, target_y - 1, az);
        if world
            .class_at(floor.0, floor.1, floor.2)
            .ok()
            .is_none_or(|c| c.danger() || c.water() || !c.standable())
        {
            return None;
        }
        for pos in cells {
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
            pos: BlockPos {
                x: ax,
                y: target_y,
                z: az,
            },
            radius: 0.5,
        }))))
    }
}

#[cfg(test)]
mod tunnel_tests {
    use std::rc::Rc;

    use super::*;
    use crate::world::{Class, Pending};

    struct Air;

    impl Blocks for Air {
        fn class_at(&mut self, _: i32, _: i32, _: i32) -> Result<Class, Pending> {
            Ok(Class::PASSABLE)
        }

        fn state_at(&mut self, _: i32, _: i32, _: i32) -> Result<Option<Rc<str>>, Pending> {
            Ok(None)
        }

        fn find(&mut self, _: SectionKey, _: &[String]) -> Result<Vec<Pos>, Pending> {
            Ok(Vec::new())
        }
    }

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    #[test]
    fn a_tunnel_cell_given_up_on_is_never_dug_later_as_a_wanted_block() {
        let me = v(0.5, 64.0, 0.5);
        let mut miner = Miner::new(&["minecraft:iron_ore".into()], 1, 6, true, None, None, &me);
        miner.tunnelling = true;
        miner.current = Some((0, 64, -1));
        miner.phase = Phase::Digging {
            pos: (0, 64, -1),
            ticks: 3,
        };
        miner.skip_current();
        assert!(miner.bad.is_empty());
        assert!(!miner.tunnelling);
        assert_eq!(miner.bad_in_reach(&me, &mut Air), None);
        assert_eq!(miner.mined, 0);
    }
}

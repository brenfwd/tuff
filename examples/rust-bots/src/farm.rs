// SPDX-License-Identifier: MIT
//! The `farm` process, free of host calls: find mature crops in a box,
//! dig them, pick the drops up and plant seeds again where a crop stood.

use std::collections::HashSet;

use tuff_bots_api::control::{BlockPos, FarmSpec, Goal, LeafGoal, NearGoal, Vec3};

use crate::core::goal_reached;
use crate::exec::{Action, Face};
use crate::hands::{Carried, hotbar_slot};
use crate::world::{Blocks, Class, name_of};

pub type Pos = (i32, i32, i32);

/// The most blocks a farm box may hold.
pub const MAX_VOLUME: i64 = 32768;
const REACH: f64 = 4.0;
const APPROACH_RADIUS: f64 = 2.5;
const EYE: f64 = 1.62;
const DIG_TIMEOUT: u32 = 100;
const RECHECK_TICKS: u32 = 5;
const COLLECT_TICKS: u32 = 40;
const PICKUP_DELAY: u32 = 10;
const COLLECT_RADIUS: f64 = 0.5;
const PLACE_TICKS: u32 = 20;
const MAX_PLANT_TRIES: u8 = 4;
const SCAN_INTERVAL: u32 = 10;
const SEARCH: i32 = 3;
const STAND_RADIUS: f64 = 0.6;
const COLLECT_REACH: f64 = 1.3;
const MAX_FERTILIZE_TRIES: u8 = 4;

#[derive(Debug, PartialEq)]
pub enum Farming {
    Done,
    Failed(String),
    Goal(Goal),
    Dig(Pos),
    Act(Vec<Action>),
    Wait,
}

/// The seed item that plants `crop`, if it is planted at all.
pub fn seed_for(crop: &str) -> Option<&'static str> {
    Some(match crop {
        "wheat" => "wheat_seeds",
        "carrots" => "carrot",
        "potatoes" => "potato",
        "beetroots" => "beetroot_seeds",
        "nether_wart" => "nether_wart",
        _ => return None,
    })
}

fn soil_for(crop: &str) -> &'static str {
    if crop == "nether_wart" {
        "soul_sand"
    } else {
        "farmland"
    }
}

fn age(state: &str) -> Option<u32> {
    let props = state.split_once('[')?.1;
    props
        .trim_end_matches(']')
        .split(',')
        .find_map(|p| p.strip_prefix("age=")?.parse().ok())
}

/// Whether the block is a crop ready to harvest; `below` is the state
/// under it (sugar cane is only cut above its base).
pub fn is_mature(state: &str, below: Option<&str>) -> bool {
    match name_of(state) {
        "wheat" | "carrots" | "potatoes" => age(state).is_some_and(|a| a >= 7),
        "beetroots" | "nether_wart" => age(state).is_some_and(|a| a >= 3),
        "melon" | "pumpkin" => true,
        "sugar_cane" => below.is_some_and(|b| name_of(b) == "sugar_cane"),
        _ => false,
    }
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

fn near(pos: Pos, radius: f64) -> Goal {
    Goal::Leaf(LeafGoal::Near(NearGoal {
        pos: BlockPos {
            x: pos.0,
            y: pos.1,
            z: pos.2,
        },
        radius,
    }))
}

fn in_reach(me: &Vec3, pos: Pos) -> bool {
    let eyes = Vec3 {
        y: me.y + EYE,
        ..me.clone()
    };
    dist2(&centre(pos), &eyes) <= REACH * REACH
}

#[derive(Clone, Debug)]
enum Phase {
    Seek,
    Digging { pos: Pos, crop: String, ticks: u32 },
    Collect { pos: Pos, ticks: u32 },
    Planting { pos: Pos, crop: String, ticks: u32 },
    Fertilizing { pos: Pos, age: Option<u32>, ticks: u32 },
}

pub struct Farmer {
    min: Pos,
    max: Pos,
    replant: bool,
    phase: Phase,
    replants: Vec<(Pos, String)>,
    bad: HashSet<Pos>,
    tries: std::collections::HashMap<Pos, u8>,
    fertilize_tries: std::collections::HashMap<Pos, u8>,
    sown: HashSet<Pos>,
    current: Option<Pos>,
    cooldown: u32,
    harvested: u32,
    rescanned: bool,
}

impl Farmer {
    pub fn new(spec: &FarmSpec) -> Self {
        let (a, b) = (&spec.min, &spec.max);
        Self {
            min: (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: (a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
            replant: spec.replant,
            phase: Phase::Seek,
            replants: Vec::new(),
            bad: HashSet::new(),
            tries: std::collections::HashMap::new(),
            fertilize_tries: std::collections::HashMap::new(),
            sown: HashSet::new(),
            current: None,
            cooldown: 0,
            harvested: 0,
            rescanned: false,
        }
    }

    pub fn harvested(&self) -> u32 {
        self.harvested
    }

    /// Gives up on the block being worked on.
    pub fn skip_current(&mut self) {
        if let Some(pos) = self.current.take() {
            self.bad.insert(pos);
        }
        self.phase = Phase::Seek;
    }

    fn volume(&self) -> i64 {
        (i64::from(self.max.0 - self.min.0) + 1)
            * (i64::from(self.max.1 - self.min.1) + 1)
            * (i64::from(self.max.2 - self.min.2) + 1)
    }

    fn state(world: &mut dyn Blocks, p: Pos) -> Result<Option<String>, ()> {
        world
            .state_at(p.0, p.1, p.2)
            .map(|s| s.map(|s| s.to_string()))
            .map_err(|_| ())
    }

    fn mature_at(world: &mut dyn Blocks, p: Pos) -> Result<bool, ()> {
        let Some(state) = Self::state(world, p)? else {
            return Ok(false);
        };
        if !matches!(
            name_of(&state),
            "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "nether_wart"
                | "melon"
                | "pumpkin"
                | "sugar_cane"
        ) {
            return Ok(false);
        }
        let below = Self::state(world, (p.0, p.1 - 1, p.2))?;
        Ok(is_mature(&state, below.as_deref()))
    }

    fn nearest_mature(&mut self, me: &Vec3, world: &mut dyn Blocks) -> Result<Option<Pos>, ()> {
        let mut best: Option<(f64, Pos)> = None;
        for x in self.min.0..=self.max.0 {
            for y in self.min.1..=self.max.1 {
                for z in self.min.2..=self.max.2 {
                    let p = (x, y, z);
                    if self.bad.contains(&p) || !Self::mature_at(world, p)? {
                        continue;
                    }
                    let d = dist2(&centre(p), me);
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, p));
                    }
                }
            }
        }
        Ok(best.map(|(_, p)| p))
    }

    pub fn tick(&mut self, me: &Vec3, world: &mut dyn Blocks, carried: &[Carried]) -> Farming {
        if self.volume() > MAX_VOLUME {
            return Farming::Failed("area too large".into());
        }
        match self.phase.clone() {
            Phase::Digging { pos, crop, ticks } => {
                if ticks.is_multiple_of(RECHECK_TICKS) {
                    world.refresh(&[pos]);
                    if Self::mature_at(world, pos) == Ok(false) {
                        self.harvested += 1;
                        self.current = None;
                        if self.replant && seed_for(&crop).is_some() {
                            self.replants.push((pos, crop));
                        }
                        self.phase = Phase::Collect {
                            pos,
                            ticks: COLLECT_TICKS,
                        };
                        return Farming::Wait;
                    }
                }
                if ticks >= DIG_TIMEOUT {
                    self.skip_current();
                    return Farming::Wait;
                }
                self.phase = Phase::Digging {
                    pos,
                    crop,
                    ticks: ticks + 1,
                };
                Farming::Wait
            }
            Phase::Collect { pos, ticks } => {
                let Ok(stand) = self.standing_cell(me, world, pos, true) else {
                    self.phase = Phase::Collect {
                        pos,
                        ticks: ticks.saturating_sub(1),
                    };
                    return Farming::Wait;
                };
                let c = centre(pos);
                let reach = if stand.is_some() {
                    COLLECT_REACH
                } else {
                    COLLECT_RADIUS + 0.1
                };
                let close = (c.x - me.x).hypot(c.z - me.z) <= reach && (c.y - me.y).abs() <= 1.5;
                if ticks == 0 || (close && ticks <= COLLECT_TICKS - PICKUP_DELAY) {
                    self.phase = Phase::Seek;
                    return Farming::Wait;
                }
                self.phase = Phase::Collect {
                    pos,
                    ticks: ticks - 1,
                };
                Farming::Goal(match stand {
                    Some(cell) => near(cell, STAND_RADIUS),
                    None => near(pos, COLLECT_RADIUS),
                })
            }
            Phase::Fertilizing { pos, age: before, ticks } => {
                if ticks.is_multiple_of(2) {
                    world.refresh(&[pos]);
                }
                let now = Self::state(world, pos)
                    .ok()
                    .flatten()
                    .and_then(|s| age(&s));
                if now != before {
                    self.phase = Phase::Seek;
                    return Farming::Wait;
                }
                if ticks >= PLACE_TICKS {
                    *self.fertilize_tries.entry(pos).or_insert(0) += 1;
                    self.phase = Phase::Seek;
                    return Farming::Wait;
                }
                self.phase = Phase::Fertilizing {
                    pos,
                    age: before,
                    ticks: ticks + 1,
                };
                Farming::Wait
            }
            Phase::Planting { pos, crop, ticks } => {
                if ticks.is_multiple_of(2) {
                    world.refresh(&[pos]);
                }
                if let Ok(Some(state)) = Self::state(world, pos)
                    && name_of(&state) == crop
                {
                    self.phase = Phase::Seek;
                    return Farming::Wait;
                }
                if ticks >= PLACE_TICKS {
                    let tried = self.tries.entry(pos).or_insert(0);
                    *tried += 1;
                    if *tried < MAX_PLANT_TRIES {
                        self.replants.push((pos, crop));
                    }
                    self.phase = Phase::Seek;
                    return Farming::Wait;
                }
                self.phase = Phase::Planting {
                    pos,
                    crop,
                    ticks: ticks + 1,
                };
                Farming::Wait
            }
            Phase::Seek => self.seek(me, world, carried),
        }
    }

    fn plant(&mut self, me: &Vec3, world: &mut dyn Blocks, carried: &[Carried]) -> Option<Farming> {
        let i = (0..self.replants.len()).min_by(|a, b| {
            dist2(&centre(self.replants[*a].0), me)
                .total_cmp(&dist2(&centre(self.replants[*b].0), me))
        })?;
        {
            let (pos, crop) = self.replants[i].clone();
            let seed = seed_for(&crop)?;
            let Some(slot) = hotbar_slot(carried, seed) else {
                self.replants.remove(i);
                return Some(Farming::Wait);
            };
            let here = Self::state(world, pos);
            let soil = Self::state(world, (pos.0, pos.1 - 1, pos.2));
            let (Ok(here), Ok(soil)) = (here, soil) else {
                return Some(Farming::Wait);
            };
            let free = here.as_deref().is_none_or(|s| name_of(s) == "air");
            let planted = soil
                .as_deref()
                .is_some_and(|s| name_of(s) == soil_for(&crop));
            if !free || !planted {
                self.replants.remove(i);
                return Some(Farming::Wait);
            }
            if !in_reach(me, pos) {
                let Ok(goal) = self.approach(me, world, pos) else {
                    return Some(Farming::Wait);
                };
                if goal_reached(&goal, me) {
                    self.replants.remove(i);
                    return Some(Farming::Wait);
                }
                return Some(Farming::Goal(goal));
            }
            self.replants.remove(i);
            self.sown.insert(pos);
            self.phase = Phase::Planting {
                pos,
                crop,
                ticks: 0,
            };
            Some(Farming::Act(vec![
                Action::Select(slot),
                Action::Place {
                    against: (pos.0, pos.1 - 1, pos.2),
                    face: Face::Up,
                },
            ]))
        }
    }

    /// A cell to stand on within reach of `target` whose floor is not
    /// farmland, so walking there never tramples the field.
    fn standing_cell(
        &self,
        me: &Vec3,
        world: &mut dyn Blocks,
        target: Pos,
        closest: bool,
    ) -> Result<Option<Pos>, ()> {
        let tc = centre(target);
        let mut best: Option<((f64, f64), Pos)> = None;
        for dx in -SEARCH..=SEARCH {
            for dy in -1..=1 {
                for dz in -SEARCH..=SEARCH {
                    let cell = (target.0 + dx, target.1 + dy, target.2 + dz);
                    let eyes = Vec3 {
                        x: f64::from(cell.0) + 0.5,
                        y: f64::from(cell.1) + EYE,
                        z: f64::from(cell.2) + 0.5,
                    };
                    if dist2(&tc, &eyes) > (REACH - 0.4).powi(2) {
                        continue;
                    }
                    let floor = Self::state(world, (cell.0, cell.1 - 1, cell.2))?;
                    let feet = Self::state(world, cell)?;
                    let head = Self::state(world, (cell.0, cell.1 + 1, cell.2))?;
                    let Some(floor) = floor else {
                        continue;
                    };
                    let open = |s: Option<String>| s.is_none_or(|s| name_of(&s) == "air");
                    let solid = world
                        .class_at(cell.0, cell.1 - 1, cell.2)
                        .map_err(|_| ())?
                        .has(Class::STANDABLE);
                    if solid
                        && name_of(&floor) != "farmland"
                        && open(feet)
                        && open(head)
                    {
                        let c = centre(cell);
                        let key = if closest {
                            ((c.x - tc.x).hypot(c.z - tc.z), dist2(&c, me))
                        } else {
                            (dist2(&c, me), 0.0)
                        };
                        if best.is_none_or(|(b, _)| key < b) {
                            best = Some((key, cell));
                        }
                    }
                }
            }
        }
        Ok(best.map(|(_, c)| c))
    }

    fn approach(&self, me: &Vec3, world: &mut dyn Blocks, pos: Pos) -> Result<Goal, ()> {
        Ok(match self.standing_cell(me, world, pos, false)? {
            Some(cell) => near(cell, STAND_RADIUS),
            None => near(pos, APPROACH_RADIUS),
        })
    }

    fn fertilize(
        &mut self,
        me: &Vec3,
        world: &mut dyn Blocks,
        carried: &[Carried],
    ) -> Result<Option<Farming>, ()> {
        let Some(slot) = hotbar_slot(carried, "bone_meal") else {
            return Ok(None);
        };
        let mut best: Option<(f64, Pos, u32)> = None;
        for x in self.min.0..=self.max.0 {
            for y in self.min.1..=self.max.1 {
                for z in self.min.2..=self.max.2 {
                    let p = (x, y, z);
                    if self.bad.contains(&p)
                        || self.sown.contains(&p)
                        || self.fertilize_tries.get(&p).copied().unwrap_or(0) >= MAX_FERTILIZE_TRIES
                    {
                        continue;
                    }
                    let Some(state) = Self::state(world, p)? else {
                        continue;
                    };
                    let max = match name_of(&state) {
                        "wheat" | "carrots" | "potatoes" => 7,
                        "beetroots" => 3,
                        _ => continue,
                    };
                    let Some(a) = age(&state).filter(|a| *a < max) else {
                        continue;
                    };
                    let d = dist2(&centre(p), me);
                    if best.is_none_or(|(bd, _, _)| d < bd) {
                        best = Some((d, p, a));
                    }
                }
            }
        }
        let Some((_, pos, a)) = best else {
            return Ok(None);
        };
        if !in_reach(me, pos) {
            let goal = self.approach(me, world, pos)?;
            if goal_reached(&goal, me) {
                self.fertilize_tries.insert(pos, MAX_FERTILIZE_TRIES);
                return Ok(Some(Farming::Wait));
            }
            return Ok(Some(Farming::Goal(goal)));
        }
        self.phase = Phase::Fertilizing {
            pos,
            age: Some(a),
            ticks: 0,
        };
        Ok(Some(Farming::Act(vec![
            Action::Select(slot),
            Action::Place {
                against: pos,
                face: Face::Up,
            },
        ])))
    }

    fn seek(&mut self, me: &Vec3, world: &mut dyn Blocks, carried: &[Carried]) -> Farming {
        if let Some(step) = self.plant(me, world, carried) {
            return step;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return Farming::Wait;
        }
        match self.fertilize(me, world, carried) {
            Err(()) => return Farming::Wait,
            Ok(Some(step)) => return step,
            Ok(None) => {}
        }
        let pos = match self.nearest_mature(me, world) {
            Err(()) => return Farming::Wait,
            Ok(None) => {
                if !self.rescanned {
                    self.rescanned = true;
                    let cells: Vec<Pos> = (self.min.0..=self.max.0)
                        .flat_map(|x| (self.min.1..=self.max.1).map(move |y| (x, y)))
                        .flat_map(|(x, y)| (self.min.2..=self.max.2).map(move |z| (x, y, z)))
                        .collect();
                    world.refresh(&cells);
                    return Farming::Wait;
                }
                self.cooldown = SCAN_INTERVAL;
                return Farming::Done;
            }
            Ok(Some(pos)) => pos,
        };
        self.current = Some(pos);
        if in_reach(me, pos) {
            let crop = match Self::state(world, pos) {
                Ok(Some(s)) => name_of(&s).to_owned(),
                _ => return Farming::Wait,
            };
            self.phase = Phase::Digging {
                pos,
                crop,
                ticks: 0,
            };
            return Farming::Dig(pos);
        }
        let Ok(goal) = self.approach(me, world, pos) else {
            return Farming::Wait;
        };
        if goal_reached(&goal, me) {
            self.skip_current();
            return Farming::Wait;
        }
        Farming::Goal(goal)
    }
}

#[cfg(test)]
mod tests;

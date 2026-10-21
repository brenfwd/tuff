// SPDX-License-Identifier: MIT
//! A goal selector in the style of mob AI: goals with a priority and the
//! controls they need (move, look, target). Each tick, running goals that
//! can no longer continue stop, then idle goals start when their controls
//! are free or held only by interruptable goals of a lower priority (a
//! larger number), and every running goal writes its part of the intent.

use super::{BLOCK_REACH, EYE, Intent, Role, Seen, Shot, View, block_center, dist};
use crate::game::Id;
use crate::map::Pos;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    pub const MOVE: Flags = Flags(1);
    pub const LOOK: Flags = Flags(2);
    pub const TARGET: Flags = Flags(4);

    pub const fn with(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }

    pub fn overlaps(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }
}

pub trait Goal: std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn flags(&self) -> Flags;
    fn can_use(&mut self, view: &View) -> bool;
    fn can_continue(&mut self, view: &View) -> bool {
        self.can_use(view)
    }
    fn interruptable(&self) -> bool {
        true
    }
    fn start(&mut self, _view: &View) {}
    fn stop(&mut self) {}
    fn tick(&mut self, view: &View, out: &mut Intent);
}

#[derive(Debug)]
struct Slot {
    priority: u8,
    goal: Box<dyn Goal>,
    running: bool,
}

#[derive(Debug, Default)]
pub struct GoalSelector {
    slots: Vec<Slot>,
}

impl GoalSelector {
    pub fn add(&mut self, priority: u8, goal: impl Goal + 'static) {
        self.slots.push(Slot {
            priority,
            goal: Box::new(goal),
            running: false,
        });
        self.slots.sort_by_key(|s| s.priority);
    }

    pub fn running(&self) -> Vec<&'static str> {
        self.slots
            .iter()
            .filter(|s| s.running)
            .map(|s| s.goal.name())
            .collect()
    }

    pub fn tick(&mut self, view: &View) -> Intent {
        for slot in &mut self.slots {
            if slot.running && !slot.goal.can_continue(view) {
                slot.goal.stop();
                slot.running = false;
            }
        }
        for i in 0..self.slots.len() {
            if self.slots[i].running {
                continue;
            }
            let (priority, flags) = (self.slots[i].priority, self.slots[i].goal.flags());
            let blocked = self.slots.iter().any(|other| {
                other.running
                    && other.goal.flags().overlaps(flags)
                    && (other.priority <= priority || !other.goal.interruptable())
            });
            if blocked || !self.slots[i].goal.can_use(view) {
                continue;
            }
            for other in &mut self.slots {
                if other.running && other.goal.flags().overlaps(flags) {
                    other.goal.stop();
                    other.running = false;
                }
            }
            self.slots[i].goal.start(view);
            self.slots[i].running = true;
        }
        let mut intent = Intent::default();
        for slot in &mut self.slots {
            if slot.running {
                slot.goal.tick(view, &mut intent);
            }
        }
        intent
    }
}

/// The Bed Wars brain: fight what comes close, defend the bed, go for
/// enemy beds once the bot is bold enough, and otherwise hold home.
pub fn bedwars() -> GoalSelector {
    let mut brain = GoalSelector::default();
    brain.add(1, Shoot::default());
    brain.add(1, Fight::default());
    brain.add(2, DefendBed);
    brain.add(3, RushBed::default());
    brain.add(4, Follow);
    brain.add(4, Collect);
    brain.add(5, Guard);
    brain.add(6, Watch);
    brain
}

fn eye_of(feet: [f64; 3]) -> [f64; 3] {
    [feet[0], feet[1] + EYE, feet[2]]
}

/// How close a fighting bot walks up to its target.
const CLOSE_IN: f64 = 2.2;

/// Chases and strikes the nearest enemy in aggro range.
#[derive(Debug, Default)]
pub struct Fight {
    target: Option<Id>,
}

impl Fight {
    fn current(&self, view: &View) -> Option<Seen> {
        let id = self.target?;
        view.enemies
            .iter()
            .find(|e| e.id == id && dist(e.pos, view.me) <= view.skill.aggro * 1.5)
            .copied()
    }
}

impl Goal for Fight {
    fn name(&self) -> &'static str {
        "fight"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE.with(Flags::LOOK).with(Flags::TARGET)
    }

    fn can_use(&mut self, view: &View) -> bool {
        view.nearest_enemy(view.skill.aggro).is_some()
    }

    fn can_continue(&mut self, view: &View) -> bool {
        self.current(view).is_some()
    }

    fn start(&mut self, view: &View) {
        self.target = view.nearest_enemy(view.skill.aggro).map(|e| e.id);
    }

    fn stop(&mut self) {
        self.target = None;
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        let Some(enemy) = self.current(view) else {
            return;
        };
        if dist(view.me, enemy.pos) > CLOSE_IN {
            out.goto = Some(enemy.pos);
        }
        out.fight = Some(enemy.id);
        out.look = Some([enemy.pos[0], enemy.pos[1] + 1.3, enemy.pos[2]]);
        out.sprint = true;
    }
}

/// Runs back to the bed while an enemy is near it.
#[derive(Debug)]
pub struct DefendBed;

const BED_ALARM: f64 = 10.0;

impl DefendBed {
    fn threat(view: &View) -> Option<(Pos, Seen)> {
        let bed = view.bed?;
        let center = block_center(bed);
        let enemy = view
            .enemies
            .iter()
            .filter(|e| dist(e.pos, center) <= BED_ALARM)
            .min_by(|a, b| dist(a.pos, center).total_cmp(&dist(b.pos, center)))?;
        Some((bed, *enemy))
    }
}

impl Goal for DefendBed {
    fn name(&self) -> &'static str {
        "defend_bed"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE.with(Flags::LOOK)
    }

    fn can_use(&mut self, view: &View) -> bool {
        Self::threat(view).is_some()
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        let Some((bed, enemy)) = Self::threat(view) else {
            return;
        };
        let [x, y, z] = block_center(bed);
        out.goto = Some([x, y + 0.5, z]);
        out.look = Some([enemy.pos[0], enemy.pos[1] + 1.3, enemy.pos[2]]);
        out.sprint = true;
    }
}

/// Walks to the nearest enemy bed and breaks it.
#[derive(Debug, Default)]
pub struct RushBed {
    bed: Option<Pos>,
}

impl RushBed {
    fn pick(view: &View) -> Option<Pos> {
        view.enemy_beds
            .iter()
            .min_by(|a, b| {
                dist(block_center(**a), view.me).total_cmp(&dist(block_center(**b), view.me))
            })
            .copied()
    }
}

impl Goal for RushBed {
    fn name(&self) -> &'static str {
        "rush_bed"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE.with(Flags::LOOK)
    }

    fn can_use(&mut self, view: &View) -> bool {
        let after = match view.role {
            Role::Defender | Role::Follower => return false,
            Role::Rusher => view.skill.rush_after / 3,
            Role::Solo => view.skill.rush_after,
        };
        view.elapsed >= after && Self::pick(view).is_some()
    }

    fn can_continue(&mut self, view: &View) -> bool {
        self.bed.is_some_and(|b| view.enemy_beds.contains(&b))
    }

    fn start(&mut self, view: &View) {
        self.bed = Self::pick(view);
    }

    fn stop(&mut self) {
        self.bed = None;
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        let Some(bed) = self.bed else {
            return;
        };
        let center = block_center(bed);
        out.look = Some(center);
        out.sprint = true;
        if dist(eye_of(view.me), center) <= BLOCK_REACH - 0.5 {
            out.dig = Some(bed);
        } else {
            out.goto = Some([center[0], center[1] - 0.5, center[2]]);
        }
    }
}

/// Stays close to a teammate.
#[derive(Debug)]
pub struct Follow;

const FOLLOW_CLOSE: f64 = 4.0;

impl Goal for Follow {
    fn name(&self) -> &'static str {
        "follow"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, view: &View) -> bool {
        view.role == Role::Follower && view.ally.is_some_and(|a| dist(a, view.me) > FOLLOW_CLOSE)
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        out.goto = view.ally;
    }
}

/// Waits at the team's forge while saving up for gear.
#[derive(Debug)]
pub struct Collect;

impl Goal for Collect {
    fn name(&self) -> &'static str {
        "collect"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, view: &View) -> bool {
        view.saving
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        if dist(view.me, view.forge) > 0.8 {
            out.goto = Some(view.forge);
        }
    }
}

/// Holds the home island near the spawn.
#[derive(Debug)]
pub struct Guard;

impl Goal for Guard {
    fn name(&self) -> &'static str {
        "guard"
    }

    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, view: &View) -> bool {
        dist(view.me, view.home) > 3.0
    }

    fn can_continue(&mut self, view: &View) -> bool {
        dist(view.me, view.home) > 1.0
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        out.goto = Some(view.home);
    }
}

/// Keeps an eye on the nearest enemy it knows of.
#[derive(Debug)]
pub struct Watch;

impl Goal for Watch {
    fn name(&self) -> &'static str {
        "watch"
    }

    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, view: &View) -> bool {
        view.nearest_enemy(f64::INFINITY).is_some()
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        if let Some(enemy) = view.nearest_enemy(f64::INFINITY) {
            out.look = Some([enemy.pos[0], enemy.pos[1] + 1.3, enemy.pos[2]]);
        }
    }
}

/// Ticks a bow needs to be fully drawn.
pub const DRAW_TICKS: u32 = 20;
pub const ARROW_SPEED: f64 = 3.0;
const ARROW_GRAVITY: f64 = 0.05;
const BOW_RANGE: [f64; 2] = [7.0, 36.0];
const FIREBALL_RANGE: [f64; 2] = [3.5, 14.0];
const FIREBALL_PAUSE: u64 = 40;

/// Where to aim an arrow so it meets a target moving at `velocity`
/// (blocks per tick), leading the target and lifting for gravity.
pub fn arrow_aim(eye: [f64; 3], target: [f64; 3], velocity: [f64; 3]) -> [f64; 3] {
    let mut at = target;
    let mut ticks = 0.0;
    for _ in 0..3 {
        ticks = dist(eye, at) / ARROW_SPEED;
        at = [0, 1, 2].map(|a| target[a] + velocity[a] * ticks);
    }
    at[1] += ARROW_GRAVITY * ticks * ticks / 2.0;
    at
}

/// Draws the bow at an enemy out of melee reach, or throws a fire charge
/// at one that is closer.
#[derive(Debug, Default)]
pub struct Shoot {
    target: Option<Id>,
    last: Option<[f64; 3]>,
    drawn: u32,
    thrown: Option<u64>,
}

impl Shoot {
    fn shot_at(view: &View, enemy: &Seen) -> Option<Shot> {
        let d = dist(view.me, enemy.pos);
        if view.fireballs > 0 && (FIREBALL_RANGE[0]..=FIREBALL_RANGE[1]).contains(&d) {
            Some(Shot::Fireball)
        } else if view.bow && view.arrows > 0 && (BOW_RANGE[0]..=BOW_RANGE[1]).contains(&d) {
            Some(Shot::Arrow)
        } else {
            None
        }
    }

    fn pick(&self, view: &View) -> Option<(Seen, Shot)> {
        view.enemies
            .iter()
            .filter(|e| dist(e.pos, view.me) <= view.skill.aggro * 2.0)
            .filter_map(|e| Some((*e, Self::shot_at(view, e)?)))
            .min_by(|a, b| dist(a.0.pos, view.me).total_cmp(&dist(b.0.pos, view.me)))
    }

    fn current(&self, view: &View) -> Option<(Seen, Shot)> {
        let id = self.target?;
        let enemy = view.enemies.iter().find(|e| e.id == id)?;
        Some((*enemy, Self::shot_at(view, enemy)?))
    }
}

impl Goal for Shoot {
    fn name(&self) -> &'static str {
        "shoot"
    }

    fn flags(&self) -> Flags {
        Flags::LOOK.with(Flags::TARGET)
    }

    fn can_use(&mut self, view: &View) -> bool {
        self.pick(view).is_some()
    }

    fn can_continue(&mut self, view: &View) -> bool {
        self.current(view).is_some()
    }

    fn start(&mut self, view: &View) {
        self.target = self.pick(view).map(|(e, _)| e.id);
        self.last = None;
        self.drawn = 0;
    }

    fn stop(&mut self) {
        self.target = None;
        self.last = None;
        self.drawn = 0;
    }

    fn tick(&mut self, view: &View, out: &mut Intent) {
        let Some((enemy, shot)) = self.current(view) else {
            return;
        };
        let velocity = match self.last {
            Some(before) => [0, 1, 2].map(|a| enemy.pos[a] - before[a]),
            None => [0.0; 3],
        };
        self.last = Some(enemy.pos);
        out.aim = Some((enemy.id, shot));
        let chest = [enemy.pos[0], enemy.pos[1] + 1.0, enemy.pos[2]];
        match shot {
            Shot::Arrow => {
                out.look = Some(arrow_aim(eye_of(view.me), chest, velocity));
                self.drawn += 1;
                if self.drawn >= DRAW_TICKS {
                    self.drawn = 0;
                    out.shoot = Some(Shot::Arrow);
                }
            }
            Shot::Fireball => {
                self.drawn = 0;
                out.look = Some(chest);
                if self.thrown.is_none_or(|t| view.now >= t + FIREBALL_PAUSE) {
                    self.thrown = Some(view.now);
                    out.shoot = Some(Shot::Fireball);
                }
            }
        }
    }
}

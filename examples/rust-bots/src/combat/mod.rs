// SPDX-License-Identifier: MIT
//! Melee against one target, free of host calls: perception through a
//! delay line, capped turning with aim error, attack-charge timing, crit
//! jumps, w-taps, strafing, chasing and retreat. Hits go through the
//! server's attack path, so damage, reach and knockback are the server's.

#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use tuff_bots_api::control::{
    AttackSpec, AttackUntil, BlockPos, Goal, LeafGoal, RunAwayGoal, Skill, Vec3,
};

use crate::exec::Drive;
use crate::follow::{Follower, Tracking, distance};
use crate::world::Blocks;

pub const REACH: f64 = 3.0;
pub const EYE: f64 = 1.62;
const CHASE_RANGE: f64 = 2.5;
const DIRECT_RANGE: f64 = 6.0;
const DIRECT_MAX_DY: f64 = 1.0;
const CLOSE_IN: f64 = 2.0;
const STRAFE_RANGE: f64 = 3.5;
const RETREAT_BLOCKS: f64 = 16.0;
const WTAP_TICKS: u32 = 2;
const FIST_SPEED: f64 = 4.0;
const SIGHT_STEP: f64 = 0.2;
const SPRINT_FOOD: i32 = 6;
const CRIT_TICKS: u32 = 14;

/// What the bot knows about its target this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub pos: Vec3,
    pub alive: bool,
    pub width: f64,
    pub height: f64,
}

/// What the bot knows about itself this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Me {
    pub pos: Vec3,
    pub yaw: f32,
    pub on_ground: bool,
    pub health: f32,
    pub food: i32,
    pub allow_sprint: bool,
    pub hotbar: Vec<Option<String>>,
    pub selected: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swing {
    /// The crosshair is on the target: an attack.
    Hit,
    /// A swing at the air.
    Miss,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Combat {
    /// Path toward this goal (chase or retreat).
    Goal(Goal),
    /// Hold these keys, maybe swing and switch hotbar slot.
    Act {
        drive: Drive,
        swing: Option<Swing>,
        select: Option<u8>,
    },
    Done,
    Failed(String),
}

/// Attack damage and attack speed of a melee weapon.
pub fn weapon(item: &str) -> Option<(f64, f64)> {
    let item = item.strip_prefix("minecraft:").unwrap_or(item);
    let (material, kind) = item.rsplit_once('_')?;
    let tier = match material {
        "wooden" | "golden" => 0,
        "stone" | "copper" => 1,
        "iron" => 2,
        "diamond" => 3,
        "netherite" => 4,
        _ => return None,
    };
    match kind {
        "sword" => Some(([4.0, 5.0, 6.0, 7.0, 8.0][tier], 1.6)),
        "axe" => Some(match material {
            "wooden" => (7.0, 0.8),
            "golden" => (7.0, 1.0),
            "stone" | "copper" => (9.0, 0.8),
            "iron" => (9.0, 0.9),
            "diamond" => (9.0, 1.0),
            _ => (10.0, 1.0),
        }),
        _ => None,
    }
}

/// The attack speed of what is held: a fist's for anything not a weapon.
pub fn attack_speed(item: Option<&str>) -> f64 {
    item.and_then(weapon).map_or(FIST_SPEED, |(_, speed)| speed)
}

/// The hotbar slot with the most damage per second, `None` with no weapon.
pub fn best_weapon(hotbar: &[Option<String>]) -> Option<u8> {
    hotbar
        .iter()
        .enumerate()
        .filter_map(|(slot, item)| {
            let (damage, speed) = weapon(item.as_deref()?)?;
            Some((slot as u8, damage * speed))
        })
        .fold(None, |best: Option<(u8, f64)>, (slot, dps)| match best {
            Some((_, top)) if top >= dps => best,
            _ => Some((slot, dps)),
        })
        .map(|(slot, _)| slot)
}

/// Whether nothing solid lies between the eyes and the target's eyes.
pub fn line_of_sight(world: &mut dyn Blocks, me: &Vec3, target: &Target) -> bool {
    let from = (me.x, me.y + EYE, me.z);
    let to = (
        target.pos.x,
        target.pos.y + target.height * 0.85,
        target.pos.z,
    );
    let (dx, dy, dz) = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let length = (dx * dx + dy * dy + dz * dz).sqrt();
    let steps = (length / SIGHT_STEP).ceil() as u32;
    (1..steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let (x, y, z) = (from.0 + dx * t, from.1 + dy * t, from.2 + dz * t);
        !matches!(
            world.class_at(x.floor() as i32, y.floor() as i32, z.floor() as i32),
            Ok(class) if !class.passable()
        )
    })
}

/// The distance from the eyes to the nearest point of the target's box,
/// as the server measures reach.
pub fn reach_distance(me: &Vec3, target: &Vec3, width: f64, height: f64) -> f64 {
    let half = width / 2.0;
    let eye = me.y + EYE;
    let clamp = |v: f64, lo: f64, hi: f64| v.clamp(lo, hi);
    let nx = clamp(me.x, target.x - half, target.x + half);
    let ny = clamp(eye, target.y, target.y + height);
    let nz = clamp(me.z, target.z - half, target.z + half);
    ((me.x - nx).powi(2) + (eye - ny).powi(2) + (me.z - nz).powi(2)).sqrt()
}

fn wrap(degrees: f32) -> f32 {
    let d = degrees.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

fn block_of(pos: &Vec3) -> BlockPos {
    BlockPos {
        x: pos.x.floor() as i32,
        y: pos.y.floor() as i32,
        z: pos.z.floor() as i32,
    }
}

/// A small deterministic generator, so a seed replays a fight exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

pub struct Fighter {
    rng: Rng,
    seen: VecDeque<Option<Vec3>>,
    last_seen: Option<Vec3>,
    aim: Option<f32>,
    jitter: f32,
    since_swing: u32,
    since_click: u32,
    elapsed: u32,
    wtap: u32,
    crit: u32,
    left_ground: bool,
    last_y: f64,
    strafe: i8,
    strafe_left: u32,
    follower: Follower,
    found: bool,
    pub hits: u32,
    pub swings: u32,
}

impl Fighter {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            seen: VecDeque::new(),
            last_seen: None,
            aim: None,
            jitter: 0.0,
            since_swing: u32::MAX / 2,
            since_click: u32::MAX / 2,
            elapsed: 0,
            wtap: 0,
            crit: 0,
            left_ground: false,
            last_y: f64::NAN,
            strafe: 1,
            strafe_left: 0,
            follower: Follower::default(),
            found: false,
            hits: 0,
            swings: 0,
        }
    }

    /// The target as the bot perceives it after its reaction delay; only
    /// what it saw, never through walls.
    fn perceive(&mut self, target: &Target, visible: bool, skill: &Skill) -> Option<Vec3> {
        self.seen.push_back(visible.then(|| target.pos.clone()));
        while self.seen.len() > skill.reaction_ticks as usize {
            if let Some(Some(pos)) = self.seen.pop_front() {
                self.last_seen = Some(pos);
            }
        }
        self.last_seen.clone()
    }

    fn charge(&self, held: Option<&str>) -> f64 {
        let delay = 20.0 / attack_speed(held);
        ((f64::from(self.since_swing) + 0.5) / delay).clamp(0.0, 1.0)
    }

    fn turn(&mut self, me: &Me, want: f32, skill: &Skill) -> f32 {
        let aim = *self.aim.get_or_insert(me.yaw);
        let diff = wrap(want - aim);
        let step = diff.clamp(-skill.turn_rate, skill.turn_rate);
        let aim = wrap(aim + step);
        self.aim = Some(aim);
        aim
    }

    pub fn tick(
        &mut self,
        me: &Me,
        target: Option<&Target>,
        visible: bool,
        skill: &Skill,
        spec: &AttackSpec,
    ) -> Combat {
        self.elapsed = self.elapsed.saturating_add(1);
        self.since_swing = self.since_swing.saturating_add(1);
        self.since_click = self.since_click.saturating_add(1);
        let falling = !me.on_ground && me.pos.y < self.last_y;
        self.last_y = me.pos.y;
        if let AttackUntil::Ticks(n) = spec.until
            && self.elapsed > n
        {
            return Combat::Done;
        }
        let Some(target) = target else {
            return match spec.until {
                AttackUntil::Death if self.found => Combat::Done,
                _ => Combat::Failed("target gone".into()),
            };
        };
        self.found = true;
        if !target.alive {
            return Combat::Done;
        }
        if let AttackUntil::Distance(r) = spec.until
            && distance(&me.pos, &target.pos) > r
        {
            return Combat::Done;
        }
        let Some(seen) = self.perceive(target, visible, skill) else {
            return Combat::Act {
                drive: Drive {
                    yaw: self.aim.unwrap_or(me.yaw),
                    ..Drive::default()
                },
                swing: None,
                select: None,
            };
        };
        if spec.retreat_health > 0.0 && me.health < spec.retreat_health {
            self.crit = 0;
            return Combat::Goal(Goal::Leaf(LeafGoal::RunAway(RunAwayGoal {
                origin: block_of(&seen),
                distance: RETREAT_BLOCKS,
            })));
        }
        let (dx, dz) = (seen.x - me.pos.x, seen.z - me.pos.z);
        let flat = dx.hypot(dz);
        let direct = visible && flat <= DIRECT_RANGE && (seen.y - me.pos.y).abs() <= DIRECT_MAX_DY;
        if !direct && let Tracking::Chase(goal) = self.follower.track(&me.pos, &seen, CHASE_RANGE) {
            self.crit = 0;
            return Combat::Goal(goal);
        }
        self.act(me, &seen, target, skill, flat, falling)
    }

    fn act(
        &mut self,
        me: &Me,
        seen: &Vec3,
        target: &Target,
        skill: &Skill,
        flat: f64,
        falling: bool,
    ) -> Combat {
        let mut select = None;
        let mut selected = me.selected;
        if let Some(best) = best_weapon(&me.hotbar)
            && best != me.selected
        {
            select = Some(best);
            selected = best;
            self.since_swing = 0;
        }
        let held = me
            .hotbar
            .get(usize::from(selected))
            .and_then(|i| i.as_deref());
        let want = (-(seen.x - me.pos.x)).atan2(seen.z - me.pos.z).to_degrees() as f32;
        let aim = self.turn(me, want, skill);
        let reach = reach_distance(&me.pos, seen, target.width, target.height);
        let charged = self.charge(held) >= f64::from(skill.attack_charge);
        let clicks = skill.click_cps_cap.max(0.1);
        let click_ready = f64::from(self.since_click) >= 20.0 / f64::from(clicks);
        let in_reach = reach < REACH;
        if self.crit > 0 {
            self.crit -= 1;
            self.left_ground |= !me.on_ground;
            if me.on_ground && self.left_ground {
                self.crit = 0;
            }
        }
        let critting = self.crit > 0;
        let mut jump = false;
        if in_reach && charged && click_ready && me.on_ground && !critting && self.wtap == 0 {
            let crit_chance = (1.0 - f64::from(skill.aim_error_degrees) / 10.0).clamp(0.0, 0.8);
            if self.rng.unit() < crit_chance {
                self.crit = CRIT_TICKS;
                self.left_ground = false;
                jump = true;
            }
        }
        let mut swing = None;
        let swing_now = in_reach && charged && click_ready && !jump && (!critting || falling);
        if swing_now {
            let crosshair = aim + self.jitter;
            let half_angle = (target.width / 2.0).atan2(flat.max(0.1)).to_degrees() as f32;
            let on_target = wrap(want - crosshair).abs() <= half_angle;
            swing = Some(if on_target { Swing::Hit } else { Swing::Miss });
            self.swings += 1;
            if on_target {
                self.hits += 1;
            }
            self.since_swing = 0;
            self.since_click = 0;
            self.crit = 0;
            let error = f64::from(skill.aim_error_degrees);
            self.jitter = self.rng.between(-error, error) as f32;
            if on_target && me.allow_sprint {
                self.wtap = WTAP_TICKS;
            }
        }
        if self.strafe_left == 0 {
            self.strafe = if self.rng.unit() < 0.5 { -1 } else { 1 };
            self.strafe_left = self.rng.between(10.0, 30.0) as u32;
        }
        self.strafe_left -= 1;
        let tapping = self.wtap > 0;
        self.wtap = self.wtap.saturating_sub(1);
        let forward = i8::from(flat > CLOSE_IN && !tapping);
        let sprint = me.allow_sprint
            && me.food > SPRINT_FOOD
            && forward == 1
            && self.crit == 0
            && !jump
            && wrap(want - aim).abs() < 30.0;
        let strafe = if flat < STRAFE_RANGE { self.strafe } else { 0 };
        Combat::Act {
            drive: Drive {
                forward,
                strafe,
                jump,
                jump_at_edge: false,
                sprint,
                sneak: false,
                yaw: aim,
                pitch: 0.0,
            },
            swing,
            select,
        }
    }
}

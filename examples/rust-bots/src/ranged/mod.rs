// SPDX-License-Identifier: MIT
//! Ranged combat and item use, free of host calls: projectile ballistics
//! with target lead, a bow archer that draws, aims and releases, and a
//! one-shot item user for throwables and chargeable items.

#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use tuff_bots_api::control::{AttackUntil, LookTarget, ShootSpec, Skill, UseItemSpec, Vec3};

use crate::combat::{EYE, Me, Target};
use crate::follow::distance;
use crate::world::name_of;

/// How a launched projectile flies: launch speed, then per tick drag
/// and gravity applied after the move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ballistics {
    pub speed: f64,
    pub gravity: f64,
    pub drag: f64,
}

pub const ARROW: Ballistics = Ballistics {
    speed: 3.0,
    gravity: 0.05,
    drag: 0.99,
};
pub const THROWN: Ballistics = Ballistics {
    speed: 1.5,
    gravity: 0.03,
    drag: 0.99,
};
pub const POTION: Ballistics = Ballistics {
    speed: 0.5,
    gravity: 0.05,
    drag: 0.99,
};
pub const STRAIGHT: Ballistics = Ballistics {
    speed: 3.0,
    gravity: 0.0,
    drag: 1.0,
};

const FULL_DRAW: u32 = 20;
const MAX_FLIGHT: u32 = 400;
const MAX_ARC_DEGREES: f64 = 45.0;
const AIM_POINT: f64 = 0.6;
const LEAD_ITERATIONS: usize = 5;
const AIM_TOLERANCE: f32 = 1.2;
const RELEASE_COOLDOWN: u32 = 5;
const MAX_HOLD: u32 = 60;
const USE_AIM_TOLERANCE: f32 = 2.0;

/// The projectile an item throws, `None` when it is not a throwable.
pub fn ballistics_of(item: &str) -> Ballistics {
    match name_of(item) {
        "snowball" | "egg" | "ender_pearl" | "experience_bottle" => THROWN,
        "splash_potion" | "lingering_potion" => POTION,
        "bow" | "crossbow" | "arrow" => ARROW,
        _ => STRAIGHT,
    }
}

/// Where a projectile launched at `angle` degrees above the horizontal is
/// when it passes `dist` blocks out: its height and the ticks it took.
pub fn flight(b: &Ballistics, angle: f64, dist: f64) -> Option<(f64, f64)> {
    let rad = angle.to_radians();
    let (mut vx, mut vy) = (b.speed * rad.cos(), b.speed * rad.sin());
    let (mut x, mut y) = (0.0, 0.0);
    for tick in 0..MAX_FLIGHT {
        let (nx, ny) = (x + vx, y + vy);
        if nx >= dist {
            let part = (dist - x) / (nx - x);
            return Some((y + (ny - y) * part, f64::from(tick) + part));
        }
        x = nx;
        y = ny;
        vx *= b.drag;
        vy = vy * b.drag - b.gravity;
        if vx < 1e-6 {
            return None;
        }
    }
    None
}

/// The lowest launch angle that lands `height` above the launcher at
/// `dist` blocks out, with the ticks of flight; `None` when out of range.
pub fn solve_angle(b: &Ballistics, dist: f64, height: f64) -> Option<(f64, f64)> {
    let direct = height.atan2(dist.max(1e-6)).to_degrees();
    let mut lo = direct.clamp(-89.0, 89.0);
    let mut hi = (lo + 30.0).clamp(MAX_ARC_DEGREES, 89.0);
    if let Some((y, t)) = flight(b, lo, dist)
        && y >= height
    {
        return Some((lo, t));
    }
    let (top, _) = flight(b, hi, dist)?;
    if top < height {
        return None;
    }
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        match flight(b, mid, dist) {
            Some((y, _)) if y < height => lo = mid,
            _ => hi = mid,
        }
    }
    flight(b, hi, dist).map(|(_, t)| (hi, t))
}

/// Yaw and pitch (degrees, pitch positive looking down) that put a
/// projectile on a target moving at `velocity` per tick, and the ticks of
/// flight. `None` when it cannot reach.
pub fn lead(b: &Ballistics, eye: &Vec3, point: &Vec3, velocity: &Vec3) -> Option<(f32, f32, f64)> {
    let mut ticks = 0.0;
    let mut result = None;
    for _ in 0..LEAD_ITERATIONS {
        let aim = Vec3 {
            x: point.x + velocity.x * ticks,
            y: point.y + velocity.y * ticks,
            z: point.z + velocity.z * ticks,
        };
        let (dx, dz) = (aim.x - eye.x, aim.z - eye.z);
        let flat = dx.hypot(dz);
        let (angle, t) = solve_angle(b, flat, aim.y - eye.y)?;
        ticks = t;
        let yaw = (-dx).atan2(dz).to_degrees() as f32;
        result = Some((yaw, -angle as f32, t));
    }
    result
}

fn wrap(degrees: f32) -> f32 {
    let d = degrees.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

fn eye_of(pos: &Vec3) -> Vec3 {
    Vec3 {
        x: pos.x,
        y: pos.y + EYE - 0.1,
        z: pos.z,
    }
}

fn aim_point(target: &Vec3, height: f64) -> Vec3 {
    Vec3 {
        x: target.x,
        y: target.y + height * AIM_POINT,
        z: target.z,
    }
}

fn turn(current: f32, want: f32, rate: f32) -> f32 {
    wrap(current + wrap(want - current).clamp(-rate, rate))
}

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draw {
    None,
    /// Press use now.
    Start,
    /// Let go of use now, loosing the arrow.
    Release,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shot {
    Done,
    Failed(String),
    /// Inside melee range: the caller fights instead (and lets go of any
    /// draw when `release` is set).
    Melee {
        release: bool,
    },
    Act {
        yaw: f32,
        pitch: f32,
        draw: Draw,
        select: Option<u8>,
    },
}

pub fn bow_slot(hotbar: &[Option<String>]) -> Option<u8> {
    hotbar
        .iter()
        .position(|i| i.as_deref().is_some_and(|i| name_of(i) == "bow"))
        .map(|slot| slot as u8)
}

/// A bow archer against one target.
pub struct Archer {
    rng: Rng,
    seen: VecDeque<Option<Vec3>>,
    last_seen: Option<Vec3>,
    velocity: Vec3,
    yaw: Option<f32>,
    pitch: f32,
    jitter: (f32, f32),
    drawn: Option<u32>,
    cooldown: u32,
    elapsed: u32,
    found: bool,
    pub shots: u32,
}

impl Archer {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            seen: VecDeque::new(),
            last_seen: None,
            velocity: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            yaw: None,
            pitch: 0.0,
            jitter: (0.0, 0.0),
            drawn: None,
            cooldown: 0,
            elapsed: 0,
            found: false,
            shots: 0,
        }
    }

    fn perceive(&mut self, target: &Target, visible: bool, skill: &Skill) -> Option<Vec3> {
        self.seen.push_back(visible.then(|| target.pos.clone()));
        while self.seen.len() > skill.reaction_ticks as usize {
            if let Some(Some(pos)) = self.seen.pop_front() {
                if let Some(last) = &self.last_seen {
                    let blend = |old: f64, new: f64, was: f64| 0.5 * old + 0.5 * (new - was);
                    self.velocity = Vec3 {
                        x: blend(self.velocity.x, pos.x, last.x),
                        y: blend(self.velocity.y, pos.y, last.y),
                        z: blend(self.velocity.z, pos.z, last.z),
                    };
                }
                self.last_seen = Some(pos);
            }
        }
        self.last_seen.clone()
    }

    pub fn drawing(&self) -> bool {
        self.drawn.is_some()
    }

    pub fn tick(
        &mut self,
        me: &Me,
        target: Option<&Target>,
        visible: bool,
        skill: &Skill,
        spec: &ShootSpec,
        has_ammo: bool,
    ) -> Shot {
        let (until, melee_range) = (&spec.until, spec.melee_range);
        self.elapsed = self.elapsed.saturating_add(1);
        self.cooldown = self.cooldown.saturating_sub(1);
        if let AttackUntil::Ticks(n) = until
            && self.elapsed > *n
        {
            return Shot::Done;
        }
        let Some(target) = target else {
            return match until {
                AttackUntil::Death if self.found => Shot::Done,
                _ => Shot::Failed("target gone".into()),
            };
        };
        self.found = true;
        if !target.alive {
            return Shot::Done;
        }
        if let AttackUntil::Distance(r) = until
            && distance(&me.pos, &target.pos) > *r
        {
            return Shot::Done;
        }
        let Some(bow) = bow_slot(&me.hotbar) else {
            return Shot::Failed("no bow".into());
        };
        let seen = self.perceive(target, visible, skill);
        let yaw_now = *self.yaw.get_or_insert(me.yaw);
        let hold = |yaw: f32, pitch: f32, draw: Draw, select: Option<u8>| Shot::Act {
            yaw,
            pitch,
            draw,
            select,
        };
        if me.selected != bow {
            self.drawn = None;
            return hold(yaw_now, self.pitch, Draw::None, Some(bow));
        }
        let Some(seen) = seen else {
            return hold(yaw_now, self.pitch, Draw::None, None);
        };
        let flat = (seen.x - me.pos.x).hypot(seen.z - me.pos.z);
        if flat < melee_range {
            let release = self.drawn.take().is_some();
            return Shot::Melee { release };
        }
        if self.drawn.is_none() && !has_ammo {
            return Shot::Failed("no arrows".into());
        }
        let eye = eye_of(&me.pos);
        let point = aim_point(&seen, target.height);
        let velocity = self.velocity.clone();
        let Some((want_yaw, want_pitch, _)) = lead(&ARROW, &eye, &point, &velocity) else {
            self.drawn = None;
            return hold(yaw_now, self.pitch, Draw::None, None);
        };
        let (want_yaw, want_pitch) = (want_yaw + self.jitter.0, want_pitch + self.jitter.1);
        let yaw = turn(yaw_now, want_yaw, skill.turn_rate);
        let pitch = turn(self.pitch, want_pitch, skill.turn_rate);
        self.yaw = Some(yaw);
        self.pitch = pitch;
        let aimed = wrap(want_yaw - yaw).abs() <= AIM_TOLERANCE
            && wrap(want_pitch - pitch).abs() <= AIM_TOLERANCE;
        match self.drawn {
            None if self.cooldown == 0 && visible => {
                self.drawn = Some(0);
                let error = f64::from(skill.aim_error_degrees);
                self.jitter = (
                    ((self.rng.unit() * 2.0 - 1.0) * error) as f32,
                    ((self.rng.unit() * 2.0 - 1.0) * error) as f32,
                );
                hold(yaw, pitch, Draw::Start, None)
            }
            None => hold(yaw, pitch, Draw::None, None),
            Some(held) => {
                let held = held + 1;
                if held >= FULL_DRAW && aimed && visible || held >= MAX_HOLD {
                    self.drawn = None;
                    self.cooldown = RELEASE_COOLDOWN;
                    self.shots += 1;
                    hold(yaw, pitch, Draw::Release, None)
                } else {
                    self.drawn = Some(held);
                    hold(yaw, pitch, Draw::None, None)
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseAction {
    None,
    /// Press use (and keep holding it when the spec holds).
    Press,
    Release,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Use {
    Done,
    Failed(String),
    Act {
        look: Option<(f32, f32)>,
        action: UseAction,
        select: Option<u8>,
    },
}

/// Uses one item once: select, look, press, hold, release.
pub struct ItemUser {
    yaw: Option<f32>,
    pitch: f32,
    pressed: Option<u32>,
    released: bool,
}

impl Default for ItemUser {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemUser {
    pub fn new() -> Self {
        Self {
            yaw: None,
            pitch: 0.0,
            pressed: None,
            released: false,
        }
    }

    /// `aim` is where `look-at` resolved to this tick: a point and its
    /// velocity per tick.
    pub fn tick(
        &mut self,
        me: &Me,
        spec: &UseItemSpec,
        aim: Option<(Vec3, Vec3)>,
        skill: &Skill,
    ) -> Use {
        if let Some(held) = self.pressed {
            let held = held + 1;
            self.pressed = Some(held);
            return if held > spec.hold_ticks {
                self.pressed = None;
                self.released = true;
                if spec.hold_ticks == 0 {
                    Use::Done
                } else {
                    Use::Act {
                        look: self.look(),
                        action: UseAction::Release,
                        select: None,
                    }
                }
            } else {
                Use::Act {
                    look: self.look(),
                    action: UseAction::None,
                    select: None,
                }
            };
        }
        if self.released {
            return Use::Done;
        }
        let wanted = name_of(&spec.item);
        let Some(slot) = me
            .hotbar
            .iter()
            .position(|i| i.as_deref().is_some_and(|i| name_of(i) == wanted))
        else {
            return Use::Failed(format!("no {wanted} in the hotbar"));
        };
        let slot = slot as u8;
        if me.selected != slot {
            return Use::Act {
                look: None,
                action: UseAction::None,
                select: Some(slot),
            };
        }
        let yaw_now = *self.yaw.get_or_insert(me.yaw);
        let mut look = None;
        let mut aimed = true;
        if let Some((point, velocity)) = &aim {
            let eye = eye_of(&me.pos);
            let (want_yaw, want_pitch) = lead(&ballistics_of(&spec.item), &eye, point, velocity)
                .map_or_else(
                    || {
                        let (dx, dz) = (point.x - eye.x, point.z - eye.z);
                        (
                            (-dx).atan2(dz).to_degrees() as f32,
                            -(point.y - eye.y).atan2(dx.hypot(dz)).to_degrees() as f32,
                        )
                    },
                    |(yaw, pitch, _)| (yaw, pitch),
                );
            let yaw = turn(yaw_now, want_yaw, skill.turn_rate);
            let pitch = turn(self.pitch, want_pitch, skill.turn_rate);
            self.yaw = Some(yaw);
            self.pitch = pitch;
            aimed = wrap(want_yaw - yaw).abs() <= USE_AIM_TOLERANCE
                && wrap(want_pitch - pitch).abs() <= USE_AIM_TOLERANCE;
            look = Some((yaw, pitch));
        }
        if !aimed {
            return Use::Act {
                look,
                action: UseAction::None,
                select: None,
            };
        }
        self.pressed = Some(0);
        Use::Act {
            look,
            action: UseAction::Press,
            select: None,
        }
    }

    fn look(&self) -> Option<(f32, f32)> {
        self.yaw.map(|yaw| (yaw, self.pitch))
    }
}

/// The point a `look-at` position names, `None` for an entity (the
/// caller resolves it).
pub fn fixed_point(look: &LookTarget) -> Option<Vec3> {
    match look {
        LookTarget::Position(p) => Some(p.clone()),
        LookTarget::Entity(_) => None,
    }
}

// SPDX-License-Identifier: MIT
//! The `fly` process, free of host calls: a coarse 3D route over sampled
//! terrain, pitch and yaw steering for an elytra glide, fireworks spent
//! within a budget, and a descent that ends near the goal for `goto`.

/// Blocks between terrain samples along the route.
pub const SAMPLE_STEP: f64 = 8.0;
/// Blocks kept between the bot and the highest terrain ahead.
pub const CLEARANCE: f64 = 12.0;
const LOOKAHEAD: f64 = 96.0;
const MAX_UP: f32 = -45.0;
const MAX_DOWN: f32 = 40.0;
const MIN_SPEED: f64 = 0.9;
const BOOST_COOLDOWN: u32 = 32;
const HOVER_TICKS: u32 = 3;
const GLIDE_RATIO: f64 = 6.0;
const HANDOVER_HEIGHT: f64 = 2.0;
/// Upward speed (blocks per tick) above which the nose stays up, so a
/// climb never shows a level or downward look.
const RISING: f64 = 0.02;
const STALL_TICKS: u32 = 400;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    pub pos: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
    pub gliding: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Flying {
    /// Jump, then start gliding once in the air.
    Takeoff {
        jump: bool,
    },
    /// Hold this look, optionally using a firework this tick.
    Steer {
        yaw: f32,
        pitch: f32,
        boost: bool,
    },
    /// Close enough to the goal and low: walk the rest with `goto`.
    Land,
    Failed(String),
}

pub struct Flight {
    goal: (i32, i32),
    fireworks: u32,
    cooldown: u32,
    ticks: u32,
    airborne: u32,
}

/// The yaw (degrees) that faces from `from` toward `to` on the xz plane.
pub fn yaw_toward(from: [f64; 3], to: (f64, f64)) -> f32 {
    let (dx, dz) = (to.0 - from[0], to.1 - from[2]);
    (-dx.atan2(dz)).to_degrees() as f32
}

/// The highest terrain on the straight line from `from` toward `goal`
/// within `reach` blocks, sampled every [`SAMPLE_STEP`]; unknown columns
/// count as `fallback`.
pub fn highest_ahead(
    from: [f64; 3],
    goal: (f64, f64),
    reach: f64,
    fallback: f64,
    ground: &mut dyn FnMut(i32, i32) -> Option<i32>,
) -> f64 {
    let (dx, dz) = (goal.0 - from[0], goal.1 - from[2]);
    let far = dx.hypot(dz).max(1e-6);
    let end = reach.min(far);
    let mut top = f64::MIN;
    let mut d = 0.0;
    loop {
        let (x, z) = (from[0] + dx / far * d, from[2] + dz / far * d);
        let h = ground(x.floor() as i32, z.floor() as i32).map_or(fallback, f64::from);
        top = top.max(h);
        if d >= end {
            return top;
        }
        d = (d + SAMPLE_STEP).min(end);
    }
}

impl Flight {
    pub fn new(goal: (i32, i32), fireworks: u32) -> Self {
        Self {
            goal,
            fireworks,
            cooldown: 0,
            ticks: 0,
            airborne: 0,
        }
    }

    fn yaw_from(&self, pos: [f64; 3]) -> f32 {
        yaw_toward(
            pos,
            (f64::from(self.goal.0) + 0.5, f64::from(self.goal.1) + 0.5),
        )
    }

    pub fn fireworks_left(&self) -> u32 {
        self.fireworks
    }

    pub fn tick(
        &mut self,
        body: &Body,
        glider: bool,
        ground: &mut dyn FnMut(i32, i32) -> Option<i32>,
    ) -> Flying {
        self.ticks += 1;
        self.cooldown = self.cooldown.saturating_sub(1);
        let goal = (f64::from(self.goal.0) + 0.5, f64::from(self.goal.1) + 0.5);
        let far = (goal.0 - body.pos[0]).hypot(goal.1 - body.pos[2]);
        let goal_ground = ground(self.goal.0, self.goal.1).map_or(body.pos[1], f64::from);
        if !glider && !body.on_ground {
            return Flying::Failed("no elytra to glide with".into());
        }
        if body.on_ground {
            self.airborne = 0;
            if far < 6.0 && self.ticks > 1 {
                return Flying::Land;
            }
            if !glider {
                return Flying::Failed("no elytra to glide with".into());
            }
            if self.ticks > STALL_TICKS {
                return Flying::Failed("could not take off".into());
            }
            return Flying::Takeoff { jump: true };
        }
        self.airborne += 1;
        if !body.gliding {
            return Flying::Takeoff { jump: false };
        }
        if body.pos[1] - goal_ground <= HANDOVER_HEIGHT && far < 12.0 {
            return Flying::Land;
        }
        let yaw = yaw_toward(body.pos, goal);
        let fallback = body.pos[1] - CLEARANCE;
        let top = highest_ahead(body.pos, goal, LOOKAHEAD, fallback, ground);
        let descend_from = goal_ground + 1.0 + far / GLIDE_RATIO;
        let target = if far < LOOKAHEAD {
            (top + CLEARANCE).min(descend_from.max(top + 3.0))
        } else {
            top + CLEARANCE
        };
        let speed = body.velocity[0].hypot(body.velocity[2]);
        let raw = (body.pos[1] - target) * 1.2 + body.velocity[1] * 30.0;
        let mut pitch = (raw as f32).clamp(MAX_UP, MAX_DOWN);
        let mut boost = false;
        let low = speed < MIN_SPEED || (body.pos[1] < target - 4.0 && far > 24.0);
        if low && self.fireworks > 0 && self.cooldown == 0 && self.airborne > HOVER_TICKS {
            boost = true;
            self.fireworks -= 1;
            self.cooldown = BOOST_COOLDOWN;
            pitch = pitch.min(-10.0);
        }
        if body.velocity[1] > RISING {
            pitch = pitch.min(-1.0);
        }
        Flying::Steer { yaw, pitch, boost }
    }
}

/// What the executor does this tick for a flight.
#[derive(Clone, Debug, PartialEq)]
pub enum Fly {
    Act {
        yaw: f32,
        pitch: f32,
        jump: bool,
        /// Hotbar slot to select before anything else.
        select: Option<u8>,
        /// Use the held firework.
        fire: bool,
    },
    Land,
    Failed(String),
}

/// A flight as the core runs it: the planner plus the key presses that
/// start a glide (release jump, then press it again in the air).
pub struct Run {
    flight: Flight,
    glider: bool,
    rocket: Option<u8>,
    gliding: bool,
    jump_held: bool,
    last: Option<[f64; 3]>,
}

impl Run {
    pub fn new(goal: (i32, i32), fireworks: u32, glider: bool, rocket: Option<u8>) -> Self {
        Self {
            flight: Flight::new(goal, if rocket.is_some() { fireworks } else { 0 }),
            glider,
            rocket,
            gliding: false,
            jump_held: false,
            last: None,
        }
    }

    pub fn tick(
        &mut self,
        body: &Body,
        selected: u8,
        ground: &mut dyn FnMut(i32, i32) -> Option<i32>,
    ) -> Fly {
        let velocity = self
            .last
            .map_or([0.0; 3], |p| std::array::from_fn(|i| body.pos[i] - p[i]));
        self.last = Some(body.pos);
        let seen = Body {
            gliding: self.gliding && !body.on_ground,
            velocity,
            ..*body
        };
        if body.on_ground {
            self.gliding = false;
            self.jump_held = false;
        }
        let goal_yaw = |flight: &Flight| flight.yaw_from(body.pos);
        match self.flight.tick(&seen, self.glider, ground) {
            Flying::Land => Fly::Land,
            Flying::Failed(reason) => Fly::Failed(reason),
            Flying::Takeoff { jump: true } => {
                self.jump_held = true;
                Fly::Act {
                    yaw: goal_yaw(&self.flight),
                    pitch: -10.0,
                    jump: true,
                    select: self.rocket,
                    fire: false,
                }
            }
            Flying::Takeoff { jump: false } => {
                let press = !self.jump_held;
                self.jump_held = press;
                self.gliding = press;
                Fly::Act {
                    yaw: goal_yaw(&self.flight),
                    pitch: -10.0,
                    jump: press,
                    select: self.rocket,
                    fire: false,
                }
            }
            Flying::Steer { yaw, pitch, boost } => {
                self.jump_held = false;
                let ready = self.rocket.is_some_and(|slot| slot == selected);
                Fly::Act {
                    yaw,
                    pitch,
                    jump: false,
                    select: self.rocket.filter(|_| !ready),
                    fire: boost && ready,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(yaw: f32, pitch: f32) -> [f64; 3] {
        let (y, p) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
        [-y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()]
    }

    fn glide(v: [f64; 3], yaw: f32, pitch: f32) -> [f64; 3] {
        let l = look(yaw, pitch);
        let hlook = l[0].hypot(l[2]);
        let hspeed = v[0].hypot(v[2]);
        let p = f64::from(pitch).to_radians();
        let lift = p.cos().powi(2);
        let mut v = v;
        v[1] += 0.08 * (-1.0 + lift * 0.75);
        if v[1] < 0.0 && hlook > 0.0 {
            let d = v[1] * -0.1 * lift;
            v = [v[0] + l[0] * d / hlook, v[1] + d, v[2] + l[2] * d / hlook];
        }
        if p < 0.0 && hlook > 0.0 {
            let d = hspeed * -p.sin() * 0.04;
            v = [
                v[0] - l[0] * d / hlook,
                v[1] + d * 3.2,
                v[2] - l[2] * d / hlook,
            ];
        }
        if hlook > 0.0 {
            v[0] += (l[0] / hlook * hspeed - v[0]) * 0.1;
            v[2] += (l[2] / hlook * hspeed - v[2]) * 0.1;
        }
        [v[0] * 0.99, v[1] * 0.98, v[2] * 0.99]
    }

    fn rocket(v: [f64; 3], l: [f64; 3]) -> [f64; 3] {
        let mut o = v;
        for i in 0..3 {
            o[i] += l[i] * 0.1 + (l[i] * 1.5 - v[i]) * 0.5;
        }
        o
    }

    struct Sim {
        body: Body,
        steps: u32,
        used: u32,
        burn: u32,
    }

    fn fly(
        start: [f64; 3],
        goal: (i32, i32),
        fireworks: u32,
        ground: &mut dyn FnMut(i32, i32) -> Option<i32>,
    ) -> (Sim, Flying) {
        let mut flight = Flight::new(goal, fireworks);
        let mut sim = Sim {
            body: Body {
                pos: start,
                velocity: [0.0; 3],
                on_ground: false,
                gliding: false,
            },
            steps: 0,
            used: 0,
            burn: 0,
        };
        let mut last = Flying::Land;
        for _ in 0..3000 {
            sim.steps += 1;
            last = flight.tick(&sim.body, true, ground);
            match &last {
                Flying::Takeoff { .. } => sim.body.gliding = true,
                Flying::Steer { yaw, pitch, boost } => {
                    if *boost {
                        sim.used += 1;
                        sim.burn = 25;
                    }
                    if sim.burn > 0 {
                        sim.burn -= 1;
                        sim.body.velocity = rocket(sim.body.velocity, look(*yaw, *pitch));
                    }
                    sim.body.velocity = glide(sim.body.velocity, *yaw, *pitch);
                    for i in 0..3 {
                        sim.body.pos[i] += sim.body.velocity[i];
                    }
                    let g = ground(
                        sim.body.pos[0].floor() as i32,
                        sim.body.pos[2].floor() as i32,
                    )
                    .map_or(0.0, f64::from);
                    if sim.body.pos[1] <= g {
                        return (sim, Flying::Failed("crashed".into()));
                    }
                }
                _ => return (sim, last),
            }
        }
        (sim, last)
    }

    #[test]
    fn yaw_faces_the_goal() {
        assert!((yaw_toward([0.0; 3], (0.0, 10.0)) - 0.0).abs() < 1e-3);
        assert!((yaw_toward([0.0; 3], (-10.0, 0.0)) - 90.0).abs() < 1e-3);
        assert!((yaw_toward([0.0; 3], (10.0, 0.0)) + 90.0).abs() < 1e-3);
    }

    #[test]
    fn highest_ahead_sees_a_ridge_and_unknown_columns() {
        let mut g = |x: i32, _z: i32| (x == 40).then_some(90);
        let top = highest_ahead([0.0, 70.0, 0.0], (100.0, 0.0), 96.0, 60.0, &mut g);
        assert_eq!(top, 90.0);
        let mut none = |_: i32, _: i32| None;
        assert_eq!(
            highest_ahead([0.0; 3], (50.0, 0.0), 96.0, 33.0, &mut none),
            33.0
        );
    }

    #[test]
    fn grounded_without_elytra_fails_and_with_one_jumps() {
        let mut g = |_: i32, _: i32| Some(64);
        let body = Body {
            pos: [0.0, 65.0, 0.0],
            velocity: [0.0; 3],
            on_ground: true,
            gliding: false,
        };
        assert!(matches!(
            Flight::new((200, 0), 3).tick(&body, false, &mut g),
            Flying::Failed(_)
        ));
        assert_eq!(
            Flight::new((200, 0), 3).tick(&body, true, &mut g),
            Flying::Takeoff { jump: true }
        );
    }

    #[test]
    fn flies_200_blocks_over_flat_terrain_and_hands_over_low_near_the_goal() {
        let mut g = |_: i32, _: i32| Some(64);
        let (sim, end) = fly([0.5, 110.0, 0.5], (0, 200), 6, &mut g);
        assert_eq!(end, Flying::Land, "pos {:?}", sim.body.pos);
        assert!((sim.body.pos[2] - 200.0).abs() < 14.0, "{:?}", sim.body.pos);
        assert!(sim.body.pos[1] - 64.0 <= 3.0, "{:?}", sim.body.pos);
        assert!(sim.used <= 6);
    }

    #[test]
    fn clears_a_ridge_on_the_way() {
        let mut g = |_x: i32, z: i32| Some(if (90..110).contains(&z) { 100 } else { 64 });
        let (sim, end) = fly([0.5, 130.0, 0.5], (0, 220), 8, &mut g);
        assert_eq!(end, Flying::Land, "pos {:?}", sim.body.pos);
    }

    #[test]
    fn never_spends_more_fireworks_than_the_budget() {
        let mut g = |_: i32, _: i32| Some(64);
        let (sim, _) = fly([0.5, 90.0, 0.5], (0, 400), 2, &mut g);
        assert!(sim.used <= 2);
    }

    #[test]
    fn leaves_the_ground_with_rockets_and_lands_at_the_goal() {
        let mut g = |_: i32, _: i32| Some(64);
        let (sim, end) = fly([0.5, 65.0, 0.5], (0, 200), 16, &mut g);
        assert_eq!(
            end,
            Flying::Land,
            "pos {:?} used {}",
            sim.body.pos,
            sim.used
        );
        assert!((sim.body.pos[2] - 200.0).abs() < 14.0, "{:?}", sim.body.pos);
    }
}

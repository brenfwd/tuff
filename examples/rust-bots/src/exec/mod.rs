// SPDX-License-Identifier: MIT
//! Turns a planned path into held inputs for one bot: steering toward the
//! next waypoint, sprint and jump timing, progress tracking and replans.

#[cfg(test)]
pub(crate) mod double;
#[cfg(test)]
mod tests;
mod work;

use std::cell::RefCell;
use std::rc::Rc;

use tuff_bots_api::control::{Aabb, Goal, Policy, Skill, Vec3};

pub use work::{Action, Face, Kit, Need, need};

use crate::path::{Advance, Params, Planner, Pos, Tools, parkour_gap};
use crate::world::Blocks;

pub const STUCK_TICKS: u32 = 30;
pub const MAX_REPLANS: u32 = 3;
const PROGRESS_BLOCKS: f64 = 0.25;
const REPLAN_RESET_BLOCKS: f64 = 3.0;
const SPRINT_FOOD: i32 = 6;
const YAW_DEADBAND: f32 = 2.0;
const TURN_IN_PLACE: f32 = 45.0;
const SPRINT_ALIGNED: f32 = 12.0;
const JUMP_REACH: f64 = 1.3;
const DIG_RECHECK_TICKS: u32 = 5;
const DIG_SLACK_TICKS: u32 = 40;
const DIG_UNKNOWN_TICKS: u32 = 200;
const PLACE_RECHECK_TICKS: u32 = 2;
const PLACE_VERIFY_TICKS: u32 = 20;
/// How far past the edge of its block a bridging bot's centre gets before
/// it places: far enough that its eyes see the side of the block.
const BRIDGE_OVERHANG: f64 = 0.6;
const BRIDGE_APPROACH_TICKS: u32 = 60;
const PILLAR_JUMPS: u32 = 3;
const PILLAR_JUMP_TICKS: u32 = 14;
const TAKE_OFF: f64 = 0.1;
const TAKE_OFF_REACH: f64 = 0.2806;

/// The keys and look a bot holds. The host is called only when it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drive {
    pub forward: i8,
    pub strafe: i8,
    pub jump: bool,
    pub sprint: bool,
    pub sneak: bool,
    pub yaw: f32,
    pub pitch: f32,
}

impl Drive {
    pub fn released(self) -> Drive {
        Drive {
            yaw: self.yaw,
            pitch: self.pitch,
            ..Drive::default()
        }
    }
}

/// What the executor reads from the body each tick.
#[derive(Clone, Debug)]
pub struct View {
    pub pos: Vec3,
    pub yaw: f32,
    pub on_ground: bool,
    pub in_fluid: bool,
    pub food: i32,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Drive(Drive),
    Done,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Approach,
    Placed,
}

/// Breaking or placing that holds the bot before it walks on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Work {
    Dig {
        pos: Pos,
        next: Pos,
        ticks: u32,
        limit: u32,
    },
    Bridge {
        at: Pos,
        next: Pos,
        stage: Stage,
        ticks: u32,
    },
    Pillar {
        at: Pos,
        stage: Stage,
        ticks: u32,
        jumps: u32,
    },
}

pub struct Executor {
    planner: Planner,
    tools: Option<Rc<RefCell<dyn Tools>>>,
    kit: Kit,
    work: Option<Work>,
    actions: Vec<Action>,
    allow_sprint: bool,
    aim: Option<f32>,
    last: Drive,
    stuck: u32,
    replans: u32,
    anchor: (f64, f64),
    moved_since_replan: f64,
    target: Option<Pos>,
    last_pos: Option<(f64, f64)>,
    last_at: Option<Pos>,
}

fn wrap(degrees: f32) -> f32 {
    let d = degrees.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

fn block_of(pos: &Vec3) -> Pos {
    (
        pos.x.floor() as i32,
        (pos.y + 1e-3).floor() as i32,
        pos.z.floor() as i32,
    )
}

fn yaw_toward(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

impl Executor {
    pub fn new(
        goal: Goal,
        policy: &Policy,
        tools: Option<Rc<RefCell<dyn Tools>>>,
        kit: Kit,
    ) -> Self {
        let params = Params::from_policy(policy).equipped(policy, tools.clone(), kit.throwaway);
        Self {
            planner: Planner::new(goal, params),
            tools,
            kit,
            work: None,
            actions: Vec::new(),
            allow_sprint: policy.allow_sprint,
            aim: None,
            last: Drive::default(),
            stuck: 0,
            replans: 0,
            anchor: (0.0, 0.0),
            moved_since_replan: 0.0,
            target: None,
            last_pos: None,
            last_at: None,
        }
    }

    /// A block event touched the world: the path ahead is checked.
    pub fn mark_dirty(&mut self) {
        self.planner.mark_dirty();
    }

    /// What the bot carries now.
    pub fn set_kit(&mut self, kit: Kit) {
        self.kit = kit;
        self.planner.set_throwaway(kit.throwaway);
    }

    /// The hand actions this tick asked for, in order.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    fn hold(&mut self, drive: Drive) -> Step {
        self.last = drive;
        Step::Drive(drive)
    }

    fn give_up(&mut self, view: &View, next: Pos) -> Step {
        self.work = None;
        self.replans += 1;
        if self.replans > MAX_REPLANS {
            return Step::Failed("stuck".into());
        }
        self.restart_progress(view);
        self.planner.penalise(next);
        let held = self.last.released();
        self.hold(held)
    }

    fn restart_progress(&mut self, view: &View) {
        self.anchor = (view.pos.x, view.pos.z);
        self.stuck = 0;
    }

    fn start(&mut self, view: &View, world: &mut dyn Blocks, at: Pos, next: Pos, need: Need) {
        self.restart_progress(view);
        self.work = Some(match need {
            Need::Clear => return,
            Need::Dig(pos) => {
                let tool = world
                    .state_at(pos.0, pos.1, pos.2)
                    .ok()
                    .flatten()
                    .zip(self.tools.as_ref())
                    .and_then(|(state, tools)| tools.borrow_mut().break_tool(&state));
                if let Some(tool) = tool {
                    self.actions.push(Action::Select(tool.slot));
                }
                self.actions.push(Action::Dig(pos));
                Work::Dig {
                    pos,
                    next,
                    ticks: 0,
                    limit: tool.map_or(DIG_UNKNOWN_TICKS, |t| t.ticks * 2 + DIG_SLACK_TICKS),
                }
            }
            Need::Bridge => Work::Bridge {
                at,
                next,
                stage: Stage::Approach,
                ticks: 0,
            },
            Need::Pillar => Work::Pillar {
                at,
                stage: Stage::Approach,
                ticks: 0,
                jumps: 0,
            },
        });
    }

    fn place(&mut self, against: Pos, face: Face) -> bool {
        let Some(slot) = self.kit.throwaway_slot else {
            return false;
        };
        self.actions.push(Action::Select(slot));
        self.actions.push(Action::Place { against, face });
        true
    }

    /// Advances breaking or placing; `None` once it is done and the bot
    /// may walk on this tick.
    fn work_tick(&mut self, view: &View, world: &mut dyn Blocks) -> Option<Step> {
        let work = self.work?;
        match work {
            Work::Dig {
                pos,
                next,
                ticks,
                limit,
            } => {
                let ticks = ticks + 1;
                if ticks.is_multiple_of(DIG_RECHECK_TICKS) {
                    world.refresh(&[pos]);
                }
                if world
                    .class_at(pos.0, pos.1, pos.2)
                    .is_ok_and(|c| c.passable())
                {
                    self.work = None;
                    self.restart_progress(view);
                    return None;
                }
                if ticks > limit {
                    return Some(self.give_up(view, next));
                }
                self.work = Some(Work::Dig {
                    pos,
                    next,
                    ticks,
                    limit,
                });
                let held = self.last.released();
                Some(self.hold(held))
            }
            Work::Bridge {
                at,
                next,
                stage,
                ticks,
            } => self.bridge_tick(view, world, at, next, stage, ticks + 1),
            Work::Pillar {
                at,
                stage,
                ticks,
                jumps,
            } => self.pillar_tick(view, world, at, stage, ticks + 1, jumps),
        }
    }

    fn bridge_tick(
        &mut self,
        view: &View,
        world: &mut dyn Blocks,
        at: Pos,
        next: Pos,
        stage: Stage,
        ticks: u32,
    ) -> Option<Step> {
        let (dx, dz) = (next.0 - at.0, next.2 - at.2);
        let below = (next.0, next.1 - 1, next.2);
        let sneaking = Drive {
            sneak: true,
            ..self.last.released()
        };
        match stage {
            Stage::Approach => {
                if !view.on_ground || block_of(&view.pos).1 != at.1 {
                    self.work = None;
                    return None;
                }
                let along = (view.pos.x - f64::from(at.0) - 0.5) * f64::from(dx)
                    + (view.pos.z - f64::from(at.2) - 0.5) * f64::from(dz);
                if along >= BRIDGE_OVERHANG || (ticks > 20 && along > 0.5) {
                    let against = (at.0, at.1 - 1, at.2);
                    let face = Face::toward(dx, dz)?;
                    if !self.place(against, face) {
                        return Some(self.give_up(view, next));
                    }
                    self.work = Some(Work::Bridge {
                        at,
                        next,
                        stage: Stage::Placed,
                        ticks: 0,
                    });
                    return Some(self.hold(sneaking));
                }
                if ticks > BRIDGE_APPROACH_TICKS {
                    return Some(self.give_up(view, next));
                }
                self.work = Some(Work::Bridge {
                    at,
                    next,
                    stage,
                    ticks,
                });
                let backward = Drive {
                    forward: -1,
                    sneak: true,
                    yaw: yaw_toward(-f64::from(dx), -f64::from(dz)),
                    ..Drive::default()
                };
                self.aim = Some(backward.yaw);
                Some(self.hold(backward))
            }
            Stage::Placed => {
                if ticks.is_multiple_of(PLACE_RECHECK_TICKS) {
                    world.refresh(&[below]);
                }
                if world
                    .class_at(below.0, below.1, below.2)
                    .is_ok_and(|c| c.standable())
                {
                    self.work = None;
                    self.restart_progress(view);
                    return None;
                }
                if ticks > PLACE_VERIFY_TICKS {
                    return Some(self.give_up(view, next));
                }
                self.work = Some(Work::Bridge {
                    at,
                    next,
                    stage,
                    ticks,
                });
                Some(self.hold(sneaking))
            }
        }
    }

    fn pillar_tick(
        &mut self,
        view: &View,
        world: &mut dyn Blocks,
        at: Pos,
        stage: Stage,
        ticks: u32,
        jumps: u32,
    ) -> Option<Step> {
        let next = (at.0, at.1 + 1, at.2);
        let still = self.last.released();
        match stage {
            Stage::Approach => {
                if view.pos.y >= f64::from(at.1) + 1.0 {
                    if !self.place((at.0, at.1 - 1, at.2), Face::Up) {
                        return Some(self.give_up(view, next));
                    }
                    self.work = Some(Work::Pillar {
                        at,
                        stage: Stage::Placed,
                        ticks: 0,
                        jumps,
                    });
                    return Some(self.hold(still));
                }
                let (ticks, jumps) = if view.on_ground && ticks > PILLAR_JUMP_TICKS {
                    (0, jumps + 1)
                } else {
                    (ticks, jumps)
                };
                if jumps >= PILLAR_JUMPS {
                    return Some(self.give_up(view, next));
                }
                self.work = Some(Work::Pillar {
                    at,
                    stage,
                    ticks,
                    jumps,
                });
                let jump = Drive {
                    jump: true,
                    ..still
                };
                Some(self.hold(jump))
            }
            Stage::Placed => {
                if ticks.is_multiple_of(PLACE_RECHECK_TICKS) {
                    world.refresh(&[at]);
                }
                if world
                    .class_at(at.0, at.1, at.2)
                    .is_ok_and(|c| c.standable())
                {
                    self.work = None;
                    self.restart_progress(view);
                    return None;
                }
                if ticks > PLACE_VERIFY_TICKS {
                    return Some(self.give_up(view, next));
                }
                self.work = Some(Work::Pillar {
                    at,
                    stage,
                    ticks,
                    jumps,
                });
                Some(self.hold(still))
            }
        }
    }

    fn steer(&mut self, view: &View, skill: &Skill, dx: f64, dz: f64) -> f32 {
        let aim = *self.aim.get_or_insert(view.yaw);
        let want = (-dx).atan2(dz).to_degrees() as f32;
        let diff = wrap(want - aim);
        if diff.abs() > YAW_DEADBAND {
            let step = diff.clamp(-skill.turn_rate, skill.turn_rate);
            self.aim = Some(aim + step);
        }
        wrap(want - self.aim.unwrap_or(aim)).abs()
    }

    fn track_progress(&mut self, view: &View, waypoint: Pos) -> Result<(), String> {
        let here = (view.pos.x, view.pos.z);
        let moved = (here.0 - self.anchor.0).hypot(here.1 - self.anchor.1);
        if self.target != Some(waypoint) {
            self.target = Some(waypoint);
            self.stuck = 0;
        }
        if moved >= PROGRESS_BLOCKS {
            self.anchor = here;
            self.stuck = 0;
            self.moved_since_replan += moved;
            if self.moved_since_replan >= REPLAN_RESET_BLOCKS {
                self.replans = 0;
            }
            return Ok(());
        }
        self.stuck += 1;
        if self.stuck < STUCK_TICKS {
            return Ok(());
        }
        self.stuck = 0;
        self.replans += 1;
        if self.replans > MAX_REPLANS {
            return Err("stuck".into());
        }
        self.moved_since_replan = 0.0;
        self.planner.penalise(waypoint);
        Ok(())
    }

    pub fn tick(&mut self, view: &View, skill: &Skill, world: &mut dyn Blocks) -> Step {
        let mut at = block_of(&view.pos);
        let overhang = view.on_ground
            && !view.in_fluid
            && world
                .class_at(at.0, at.1 - 1, at.2)
                .is_ok_and(|c| !c.standable());
        if overhang && let Some(last) = self.last_at {
            at = last;
        }
        self.last_at = Some(at);
        let climbing = world
            .class_at(at.0, at.1, at.2)
            .is_ok_and(|c| c.climbable());
        let speed = self
            .last_pos
            .replace((view.pos.x, view.pos.z))
            .map_or(0.0, |(x, z)| (view.pos.x - x).hypot(view.pos.z - z));
        if let Some(step) = self.work_tick(view, world) {
            return step;
        }
        let airborne = !view.on_ground && !view.in_fluid && !climbing;
        if airborne {
            let held = Drive {
                jump: false,
                sprint: self.last.sprint && self.last.forward != 0,
                ..self.last
            };
            self.last = held;
            return Step::Drive(held);
        }
        let waypoint = match self.planner.tick(at, world) {
            Advance::Arrived => return Step::Done,
            Advance::Failed(reason) => return Step::Failed(reason),
            Advance::Searching => {
                let held = Drive {
                    sneak: climbing,
                    ..self.last.released()
                };
                self.last = held;
                return Step::Drive(held);
            }
            Advance::Waypoint(next) => next,
        };
        match need(world, at, waypoint) {
            Ok(Need::Clear) => {}
            Ok(needed) => {
                self.start(view, world, at, waypoint, needed);
                if let Some(step) = self.work_tick(view, world) {
                    return step;
                }
            }
            Err(_) => {
                let held = Drive {
                    sneak: climbing,
                    ..self.last.released()
                };
                return self.hold(held);
            }
        }
        if let Err(reason) = self.track_progress(view, waypoint) {
            return Step::Failed(reason);
        }
        let dx = f64::from(waypoint.0) + 0.5 - view.pos.x;
        let dz = f64::from(waypoint.2) + 0.5 - view.pos.z;
        let flat = dx.hypot(dz);
        let misaligned = if flat > 0.05 {
            self.steer(view, skill, dx, dz)
        } else {
            0.0
        };
        let yaw = self.aim.unwrap_or(view.yaw);
        let vertical = waypoint.1 - at.1;
        let after = {
            let path = self.planner.path();
            path.iter()
                .position(|p| *p == at)
                .and_then(|i| path.get(i + 2).copied())
        };
        let straight_on = after.is_some_and(|n| n.1 == waypoint.1);
        let forward = i8::from(flat > 0.05 && misaligned < TURN_IN_PLACE);
        let parkour = parkour_gap(at, waypoint);
        let can_sprint = self.allow_sprint
            && view.food > SPRINT_FOOD
            && misaligned < SPRINT_ALIGNED
            && forward != 0;
        let sprint =
            can_sprint && ((vertical == 0 && straight_on) || parkour.is_some_and(|g| g > 1));
        let jump = match parkour {
            Some(_) => {
                view.on_ground
                    && edge_distance(&view.pos, at, waypoint)
                        <= TAKE_OFF.max(speed.min(TAKE_OFF_REACH))
            }
            None => {
                (vertical > 0 && flat <= JUMP_REACH && view.on_ground)
                    || ((view.in_fluid || climbing) && vertical > 0)
                    || (view.in_fluid && vertical == 0)
            }
        };
        let drive = Drive {
            forward,
            strafe: 0,
            jump,
            sprint,
            sneak: false,
            yaw,
            pitch: 0.0,
        };
        self.last = drive;
        Step::Drive(drive)
    }
}

/// How far the body centre is from the side of block `at` that faces `to`.
fn edge_distance(pos: &Vec3, at: Pos, to: Pos) -> f64 {
    let (dx, dz) = ((to.0 - at.0).signum(), (to.2 - at.2).signum());
    let along = |p: f64, b: i32, d: i32| match d {
        1 => f64::from(b) + 1.0 - p,
        -1 => p - f64::from(b),
        _ => f64::INFINITY,
    };
    along(pos.x, at.0, dx).min(along(pos.z, at.2, dz))
}

pub fn unit_box((x, y, z): Pos) -> Aabb {
    let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
    Aabb {
        min: Vec3 { x, y, z },
        max: Vec3 {
            x: x + 1.0,
            y: y + 1.0,
            z: z + 1.0,
        },
    }
}

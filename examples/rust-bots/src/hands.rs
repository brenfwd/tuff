//! The `break` and `place` processes, free of host calls: walk into reach
//! of a block, then dig it or place against a neighbour, and check the
//! world for the change before calling it done.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tuff_bots_api::control::{BlockPos, Goal, LeafGoal, NearGoal, Vec3};

use crate::core::goal_reached;
use crate::exec::{Action, Face};
use crate::path::{Pos, Tools};
use crate::world::{Blocks, name_of};

/// How far from the eyes a block may be broken or placed against.
pub const REACH: f64 = 4.5;
/// How close the feet get to a block that is out of reach.
const APPROACH_RADIUS: f64 = 2.5;
const EYE: f64 = 1.62;
const RECHECK_TICKS: u32 = 5;
const DIG_SLACK_TICKS: u32 = 40;
const DIG_UNKNOWN_TICKS: u32 = 200;
const PLACE_VERIFY_TICKS: u32 = 20;
pub const HOTBAR: std::ops::RangeInclusive<u8> = 36..=44;

/// One occupied inventory slot: menu index, item and count.
pub type Carried = (u8, String, u32);

#[derive(Debug, PartialEq)]
pub enum Hands {
    Done,
    Failed(String),
    Goal(Goal),
    Act(Vec<Action>),
    Wait,
}

fn centre((x, y, z): Pos) -> Vec3 {
    Vec3 {
        x: f64::from(x) + 0.5,
        y: f64::from(y) + 0.5,
        z: f64::from(z) + 0.5,
    }
}

fn in_reach(me: &Vec3, pos: Pos) -> bool {
    let c = centre(pos);
    let (dx, dy, dz) = (c.x - me.x, c.y - me.y - EYE, c.z - me.z);
    dx * dx + dy * dy + dz * dz <= REACH * REACH
}

/// Walks closer, or fails when the walk already ended and it is still out
/// of reach.
fn approach(me: &Vec3, pos: Pos) -> Hands {
    let goal = Goal::Leaf(LeafGoal::Near(NearGoal {
        pos: BlockPos {
            x: pos.0,
            y: pos.1,
            z: pos.2,
        },
        radius: APPROACH_RADIUS,
    }));
    if goal_reached(&goal, me) {
        Hands::Failed("out of reach".into())
    } else {
        Hands::Goal(goal)
    }
}

/// The hotbar slot (0 to 8) holding `item`.
pub fn hotbar_slot(carried: &[Carried], item: &str) -> Option<u8> {
    let wanted = name_of(item);
    carried
        .iter()
        .find(|(slot, held, count)| HOTBAR.contains(slot) && *count > 0 && name_of(held) == wanted)
        .map(|(slot, _, _)| slot - HOTBAR.start())
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Seek,
    Working { pos: Pos, ticks: u32, limit: u32 },
}

pub struct Breaker {
    targets: VecDeque<Pos>,
    phase: Phase,
}

impl Breaker {
    pub fn new(targets: &[BlockPos]) -> Self {
        Self {
            targets: targets.iter().map(|p| (p.x, p.y, p.z)).collect(),
            phase: Phase::Seek,
        }
    }

    pub fn tick(
        &mut self,
        me: &Vec3,
        world: &mut dyn Blocks,
        tools: Option<&Rc<RefCell<dyn Tools>>>,
    ) -> Hands {
        match self.phase {
            Phase::Working { pos, ticks, limit } => {
                let ticks = ticks + 1;
                if ticks.is_multiple_of(RECHECK_TICKS) {
                    world.refresh(&[pos]);
                }
                if world
                    .class_at(pos.0, pos.1, pos.2)
                    .is_ok_and(|c| c.passable())
                {
                    self.targets.pop_front();
                    self.phase = Phase::Seek;
                    return Hands::Wait;
                }
                if ticks > limit {
                    return Hands::Failed("could not break".into());
                }
                self.phase = Phase::Working { pos, ticks, limit };
                Hands::Wait
            }
            Phase::Seek => loop {
                let Some(&pos) = self.targets.front() else {
                    return Hands::Done;
                };
                let Ok(class) = world.class_at(pos.0, pos.1, pos.2) else {
                    return Hands::Wait;
                };
                if class.passable() {
                    self.targets.pop_front();
                    continue;
                }
                if !in_reach(me, pos) {
                    return approach(me, pos);
                }
                let Ok(state) = world.state_at(pos.0, pos.1, pos.2) else {
                    return Hands::Wait;
                };
                let tool = match (state, tools) {
                    (Some(state), Some(tools)) => match tools.borrow_mut().break_tool(&state) {
                        None => return Hands::Failed("unbreakable".into()),
                        tool => tool,
                    },
                    _ => None,
                };
                let mut actions = Vec::new();
                if let Some(tool) = tool {
                    actions.push(Action::Select(tool.slot));
                }
                actions.push(Action::Dig(pos));
                self.phase = Phase::Working {
                    pos,
                    ticks: 0,
                    limit: tool.map_or(DIG_UNKNOWN_TICKS, |t| t.ticks * 2 + DIG_SLACK_TICKS),
                };
                return Hands::Act(actions);
            },
        }
    }
}

/// How a placed block must face, read from the state's properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Orient {
    /// Look direction (yaw, pitch) that makes the block face as wanted.
    pub look: Option<(f32, f32)>,
    /// Faces of the support block that give the wanted orientation;
    /// empty means any.
    pub clicked: Vec<Face>,
}

fn prop<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    let inner = state.split_once('[')?.1.strip_suffix(']')?;
    inner
        .split(',')
        .find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

impl Orient {
    pub fn from_state(state: &str) -> Self {
        let mut out = Orient::default();
        if let Some(facing) = prop(state, "facing") {
            out.look = match facing {
                "south" => Some((180.0, 0.0)),
                "north" => Some((0.0, 0.0)),
                "west" => Some((270.0, 0.0)),
                "east" => Some((90.0, 0.0)),
                "up" => Some((0.0, 90.0)),
                "down" => Some((0.0, -90.0)),
                _ => None,
            };
        }
        if let Some(axis) = prop(state, "axis") {
            out.clicked = match axis {
                "x" => vec![Face::East, Face::West],
                "z" => vec![Face::North, Face::South],
                "y" => vec![Face::Up, Face::Down],
                _ => vec![],
            };
        } else if let Some(half) = prop(state, "half").or_else(|| prop(state, "type")) {
            out.clicked = match half {
                "top" => vec![Face::Down],
                "bottom" => vec![Face::Up],
                _ => vec![],
            };
        }
        out
    }
}

pub struct Placer {
    pos: Pos,
    item: String,
    orient: Orient,
    phase: Phase,
}

/// Whether a player's box at `me` reaches into the cell.
fn overlaps_body(me: &Vec3, (x, y, z): Pos) -> bool {
    const HALF: f64 = 0.3;
    const HEIGHT: f64 = 1.8;
    let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
    me.x - HALF < x + 1.0
        && me.x + HALF > x
        && me.y < y + 1.0
        && me.y + HEIGHT > y
        && me.z - HALF < z + 1.0
        && me.z + HALF > z
}

fn opposite(face: Face) -> Face {
    match face {
        Face::Down => Face::Up,
        Face::Up => Face::Down,
        Face::North => Face::South,
        Face::South => Face::North,
        Face::West => Face::East,
        Face::East => Face::West,
    }
}

impl Placer {
    pub fn new(pos: &BlockPos, item: &str) -> Self {
        Self {
            pos: (pos.x, pos.y, pos.z),
            item: item.to_owned(),
            orient: Orient::default(),
            phase: Phase::Seek,
        }
    }

    pub fn oriented(pos: &BlockPos, item: &str, state: &str) -> Self {
        let mut placer = Self::new(pos, item);
        placer.orient = Orient::from_state(state);
        placer
    }

    pub fn tick(&mut self, me: &Vec3, world: &mut dyn Blocks, carried: &[Carried]) -> Hands {
        let pos = self.pos;
        match self.phase {
            Phase::Working { ticks, limit, .. } => {
                let ticks = ticks + 1;
                if ticks.is_multiple_of(2) {
                    world.refresh(&[pos]);
                }
                if world
                    .class_at(pos.0, pos.1, pos.2)
                    .is_ok_and(|c| !c.passable())
                {
                    return Hands::Done;
                }
                if ticks > limit {
                    return Hands::Failed("not placed".into());
                }
                self.phase = Phase::Working { pos, ticks, limit };
                Hands::Wait
            }
            Phase::Seek => {
                let Ok(class) = world.class_at(pos.0, pos.1, pos.2) else {
                    return Hands::Wait;
                };
                if !class.passable() || class.climbable() {
                    return Hands::Failed("occupied".into());
                }
                let Some(slot) = hotbar_slot(carried, &self.item) else {
                    return Hands::Failed("no item".into());
                };
                let mut support = None;
                let mut faces: Vec<Face> = Face::ALL.to_vec();
                faces.sort_by_key(|f| !self.orient.clicked.contains(&opposite(*f)));
                for face in faces {
                    let (dx, dy, dz) = face.normal();
                    let n = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
                    match world.class_at(n.0, n.1, n.2) {
                        Err(_) => return Hands::Wait,
                        Ok(c) if c.standable() => {
                            support = Some((n, opposite(face)));
                            break;
                        }
                        Ok(_) => {}
                    }
                }
                let Some((against, face)) = support else {
                    return Hands::Failed("nothing to place against".into());
                };
                if overlaps_body(me, pos) {
                    return Hands::Failed("in the way".into());
                }
                if !in_reach(me, pos) {
                    return approach(me, pos);
                }
                self.phase = Phase::Working {
                    pos,
                    ticks: 0,
                    limit: PLACE_VERIFY_TICKS,
                };
                let mut acts = vec![Action::Select(slot)];
                if let Some((yaw, pitch)) = self.orient.look {
                    acts.push(Action::Look { yaw, pitch });
                }
                acts.push(Action::Place { against, face });
                Hands::Act(acts)
            }
        }
    }
}

#[cfg(test)]
mod orient_tests {
    use super::*;

    #[test]
    fn facing_axis_and_half_are_read_from_the_state() {
        let o = Orient::from_state("minecraft:oak_stairs[facing=north,half=top]");
        assert_eq!(o.look, Some((0.0, 0.0)));
        assert_eq!(o.clicked, [Face::Down]);
        let o = Orient::from_state("minecraft:oak_log[axis=x]");
        assert_eq!(o.look, None);
        assert_eq!(o.clicked, [Face::East, Face::West]);
        assert_eq!(Orient::from_state("minecraft:stone"), Orient::default());
    }
}

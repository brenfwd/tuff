//! The `break` and `place` processes, free of host calls: walk into reach
//! of a block, then dig it or place against a neighbour, and check the
//! world for the change before calling it done.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
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
/// How far the eyes may drift between the decision and the click.
const VIEW_SLACK: f64 = 0.3;
const SETTLE_TICKS: u32 = 1;
const STAND_RADIUS: f64 = 0.6;
const STAND_SLACKS: [f64; 3] = [STAND_RADIUS, 0.45, VIEW_SLACK];
const CHASE_TICKS: u32 = 200;
const STAND_SEARCH: i32 = 4;
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
    stand: Option<(Pos, Pos)>,
    chase: u32,
}

impl Breaker {
    pub fn new(targets: &[BlockPos]) -> Self {
        Self {
            targets: targets.iter().map(|p| (p.x, p.y, p.z)).collect(),
            phase: Phase::Seek,
            stand: None,
            chase: 0,
        }
    }

    /// A goal that brings a face of `pos` into clear view, `None` when it
    /// is visible already or no stand shows it.
    fn sighted(&mut self, me: &Vec3, world: &mut dyn Blocks, pos: Pos) -> Result<Option<Goal>, ()> {
        let eye = (me.x, me.y + EYE, me.z);
        let mut faces = Vec::new();
        for face in Face::ALL {
            let (dx, dy, dz) = face.normal();
            if world
                .class_at(pos.0 + dx, pos.1 + dy, pos.2 + dz)
                .map_err(|_| ())?
                .passable()
            {
                faces.push((pos, face));
            }
        }
        let dug = dug_face(eye, pos);
        for &(against, face) in faces.iter().filter(|(_, face)| *face == dug) {
            if clear_view(world, eye, against, face)? {
                return Ok(None);
            }
        }
        let stand = match self.stand {
            Some((target, stand)) if target == pos => Some(stand),
            _ => {
                let found = find_stand(
                    world,
                    me,
                    false,
                    pos,
                    &HashSet::new(),
                    &faces,
                    true,
                    STAND_RADIUS,
                )?;
                self.stand = found.map(|s| (pos, s));
                found
            }
        };
        Ok(stand.map(|s| {
            Goal::Leaf(LeafGoal::Near(NearGoal {
                pos: BlockPos {
                    x: s.0,
                    y: s.1,
                    z: s.2,
                },
                radius: STAND_RADIUS,
            }))
        }))
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
                    self.chase = 0;
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
                match self.sighted(me, world, pos) {
                    Err(()) => return Hands::Wait,
                    Ok(Some(goal)) if !goal_reached(&goal, me) && self.chase < CHASE_TICKS => {
                        self.chase += 1;
                        return Hands::Goal(goal);
                    }
                    Ok(_) => {}
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
    /// An upper-half block: also placeable by clicking a side face high.
    pub high: bool,
}

/// Cursor height of a side click that makes a half block sit high.
const HIGH_CURSOR: f32 = 0.9;

pub(crate) fn prop<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    let inner = state.split_once('[')?.1.strip_suffix(']')?;
    inner.split(',').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
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
            out.high = half == "top";
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
    stand: Option<Pos>,
    settled: Option<(Vec3, u32)>,
    avoid: Rc<HashSet<Pos>>,
    rejected: HashSet<Pos>,
    slack: f64,
    chase: u32,
    pub grounded: bool,
}

fn stand_goal(stand: Pos, radius: f64) -> Goal {
    Goal::Leaf(LeafGoal::Near(NearGoal {
        pos: BlockPos {
            x: stand.0,
            y: stand.1,
            z: stand.2,
        },
        radius,
    }))
}

/// Whether nothing solid stands between the eyes and the middle of the
/// clicked face, the clicked block itself aside.
fn dug_face(eye: (f64, f64, f64), pos: Pos) -> Face {
    Face::dug_from(
        eye.0 - (f64::from(pos.0) + 0.5),
        eye.1 - (f64::from(pos.1) + 0.5),
        eye.2 - (f64::from(pos.2) + 0.5),
    )
}

fn clear_view(
    world: &mut dyn Blocks,
    eye: (f64, f64, f64),
    against: Pos,
    face: Face,
) -> Result<bool, ()> {
    clear_view_within(world, eye, against, face, VIEW_SLACK)
}

/// As `clear_view`, for a body that may stand anywhere within `slack` of
/// `eye` on both horizontal axes.
fn clear_view_within(
    world: &mut dyn Blocks,
    eye: (f64, f64, f64),
    against: Pos,
    face: Face,
    slack: f64,
) -> Result<bool, ()> {
    for (ox, oz) in [
        (0.0, 0.0),
        (slack, 0.0),
        (-slack, 0.0),
        (0.0, slack),
        (0.0, -slack),
        (slack, slack),
        (slack, -slack),
        (-slack, slack),
        (-slack, -slack),
    ] {
        if !clear_ray(world, (eye.0 + ox, eye.1, eye.2 + oz), against, face)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn clear_ray(
    world: &mut dyn Blocks,
    eye: (f64, f64, f64),
    against: Pos,
    face: Face,
) -> Result<bool, ()> {
    let (nx, ny, nz) = face.normal();
    let to = (
        f64::from(against.0) + 0.5 + 0.5 * f64::from(nx),
        f64::from(against.1) + 0.5 + 0.5 * f64::from(ny),
        f64::from(against.2) + 0.5 + 0.5 * f64::from(nz),
    );
    let (dx, dy, dz) = (to.0 - eye.0, to.1 - eye.1, to.2 - eye.2);
    if dx * f64::from(nx) + dy * f64::from(ny) + dz * f64::from(nz) > -0.05 {
        return Ok(false);
    }
    let steps = ((dx * dx + dy * dy + dz * dz).sqrt() / 0.02)
        .ceil()
        .max(1.0) as u32;
    for i in 0..steps {
        let t = f64::from(i) / f64::from(steps);
        let cell = (
            (eye.0 + dx * t).floor() as i32,
            (eye.1 + dy * t).floor() as i32,
            (eye.2 + dz * t).floor() as i32,
        );
        if cell == against {
            continue;
        }
        match world.class_at(cell.0, cell.1, cell.2) {
            Err(_) => return Err(()),
            Ok(c) if c.standable() => return Ok(false),
            Ok(_) => {}
        }
    }
    Ok(true)
}

/// The support faces to click for a block at `pos`, preferred ones first.
fn candidates(world: &mut dyn Blocks, pos: Pos, orient: &Orient) -> Result<Vec<(Pos, Face)>, ()> {
    let preferred =
        |f: &Face| orient.clicked.contains(&opposite(*f)) || (orient.high && f.normal().1 == 0);
    let mut faces: Vec<Face> = Face::ALL.to_vec();
    faces.sort_by_key(|f| (!preferred(f), orient.high && f.normal().1 != 0));
    let want_preferred = faces.iter().any(preferred);
    let mut standable = Vec::new();
    let mut preferred_only = Vec::new();
    for face in faces {
        let (dx, dy, dz) = face.normal();
        let n = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
        if world.class_at(n.0, n.1, n.2).map_err(|_| ())?.standable() {
            standable.push((n, opposite(face)));
            if preferred(&face) {
                preferred_only.push((n, opposite(face)));
            }
        }
    }
    Ok(if want_preferred && !preferred_only.is_empty() {
        preferred_only
    } else {
        standable
    })
}

/// Whether a body standing in `stand` can click a face for the block at
/// `target`.
pub(crate) fn stand_works(
    world: &mut dyn Blocks,
    stand: Pos,
    target: Pos,
    orient: &Orient,
) -> Result<bool, ()> {
    let candidates = candidates(world, target, orient)?;
    let eye = (
        f64::from(stand.0) + 0.5,
        f64::from(stand.1) + EYE,
        f64::from(stand.2) + 0.5,
    );
    let feet = Vec3 {
        x: eye.0,
        y: f64::from(stand.1),
        z: eye.2,
    };
    for (against, face) in candidates {
        if in_reach(&feet, against) && clear_view_within(world, eye, against, face, STAND_RADIUS)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A cell the bot can walk to from which one of the faces can be clicked,
/// the nearest first.
fn find_stand(
    world: &mut dyn Blocks,
    me: &Vec3,
    leave_here: bool,
    target: Pos,
    avoid: &HashSet<Pos>,
    candidates: &[(Pos, Face)],
    dig: bool,
    slack: f64,
) -> Result<Option<Pos>, ()> {
    let here = (
        me.x.floor() as i32,
        me.y.floor() as i32,
        me.z.floor() as i32,
    );
    let mut seen: HashSet<Pos> = HashSet::from([here]);
    let mut queue: VecDeque<Pos> = VecDeque::from([here]);
    while let Some(cell) = queue.pop_front() {
        let in_target = cell.0 == target.0 && cell.2 == target.2 && (target.1 - cell.1).abs() <= 1;
        let eye = (
            f64::from(cell.0) + 0.5,
            f64::from(cell.1) + EYE,
            f64::from(cell.2) + 0.5,
        );
        let in_build = avoid.contains(&cell) || avoid.contains(&(cell.0, cell.1 + 1, cell.2));
        if !in_target && !in_build && !(leave_here && cell == here) {
            let feet = Vec3 {
                x: eye.0,
                y: f64::from(cell.1),
                z: eye.2,
            };
            for &(against, face) in candidates {
                if dig && dug_face(eye, target) != face {
                    continue;
                }
                if in_reach(&feet, against) && clear_view_within(world, eye, against, face, slack)?
                {
                    return Ok(Some(cell));
                }
            }
        }
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            'steps: for dy in [0, 1, -1, -2, -3] {
                let next = (cell.0 + dx, cell.1 + dy, cell.2 + dz);
                if dy < -1 {
                    for up in [0, 1] {
                        let open = world
                            .class_at(next.0, cell.1 + up, next.2)
                            .map_err(|_| ())?
                            .passable();
                        if !open {
                            continue 'steps;
                        }
                    }
                }
                if (next.0 - here.0).abs() > STAND_SEARCH
                    || (next.2 - here.2).abs() > STAND_SEARCH
                    || !seen.insert(next)
                {
                    continue;
                }
                if walkable(world, next)? {
                    queue.push_back(next);
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn walkable(world: &mut dyn Blocks, cell: Pos) -> Result<bool, ()> {
    let mut ok = true;
    for (dy, floor) in [(-1, true), (0, false), (1, false)] {
        let class = world
            .class_at(cell.0, cell.1 + dy, cell.2)
            .map_err(|_| ())?;
        ok &= if floor {
            class.standable()
        } else {
            class.passable() && !class.climbable()
        };
    }
    Ok(ok)
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
            stand: None,
            settled: None,
            grounded: true,
            avoid: Rc::default(),
            rejected: HashSet::new(),
            slack: STAND_RADIUS,
            chase: 0,
        }
    }

    pub fn oriented(pos: &BlockPos, item: &str, state: &str) -> Self {
        let mut placer = Self::new(pos, item);
        placer.orient = Orient::from_state(state);
        placer
    }

    /// Cells the bot must not choose to stand in.
    pub fn avoiding(mut self, cells: &Rc<HashSet<Pos>>) -> Self {
        self.avoid = Rc::clone(cells);
        self
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
                let Ok(candidates) = candidates(world, pos, &self.orient) else {
                    return Hands::Wait;
                };
                if candidates.is_empty() {
                    return Hands::Failed("nothing to place against".into());
                }
                let eye = (me.x, me.y + EYE, me.z);
                let mut support = None;
                let blocked = overlaps_body(me, pos);
                for &(n, face) in candidates.iter().filter(|_| !blocked) {
                    match clear_view(world, eye, n, face) {
                        Err(()) => return Hands::Wait,
                        Ok(true) => {
                            support = Some((n, face));
                            break;
                        }
                        Ok(false) => {}
                    }
                }
                if support.is_none() {
                    if self.stand.is_none() {
                        let mut avoid = (*self.avoid).clone();
                        avoid.extend(self.rejected.iter().copied());
                        match find_stand(
                            world,
                            me,
                            blocked,
                            pos,
                            &avoid,
                            &candidates,
                            false,
                            self.slack,
                        ) {
                            Err(()) => return Hands::Wait,
                            Ok(None) if self.slack > VIEW_SLACK => {
                                self.slack = STAND_SLACKS
                                    .into_iter()
                                    .find(|s| *s < self.slack)
                                    .unwrap_or(VIEW_SLACK);
                                self.rejected.clear();
                                return Hands::Wait;
                            }
                            Ok(stand) => self.stand = stand,
                        }
                    }
                    let Some(stand) = self.stand else {
                        return Hands::Failed("no stand".into());
                    };
                    if !goal_reached(&stand_goal(stand, self.slack), me) {
                        self.chase += 1;
                        if self.chase > CHASE_TICKS {
                            self.chase = 0;
                            self.rejected.insert(stand);
                            self.stand = None;
                            return Hands::Wait;
                        }
                        return Hands::Goal(stand_goal(stand, self.slack));
                    }
                    let eye = (
                        f64::from(stand.0) + 0.5,
                        f64::from(stand.1) + EYE,
                        f64::from(stand.2) + 0.5,
                    );
                    for &(n, face) in &candidates {
                        match clear_view_within(world, eye, n, face, self.slack) {
                            Err(()) => return Hands::Wait,
                            Ok(true) => {
                                support = Some((n, face));
                                break;
                            }
                            Ok(false) => {}
                        }
                    }
                    if support.is_none() {
                        self.rejected.insert(stand);
                        self.stand = None;
                        self.settled = None;
                        return Hands::Wait;
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
                if !self.grounded {
                    self.settled = None;
                    return match self.stand {
                        Some(stand) => Hands::Goal(stand_goal(stand, self.slack)),
                        None => Hands::Wait,
                    };
                }
                let held = match &self.settled {
                    Some((last, n))
                        if (last.x - me.x).abs() < 0.02
                            && (last.y - me.y).abs() < 0.02
                            && (last.z - me.z).abs() < 0.02 =>
                    {
                        n + 1
                    }
                    _ => 0,
                };
                self.settled = Some((me.clone(), held));
                if held < SETTLE_TICKS {
                    return Hands::Wait;
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
                if self.orient.high && face.normal().1 == 0 {
                    acts.push(Action::PlaceAt {
                        against,
                        face,
                        cursor_y: HIGH_CURSOR,
                    });
                } else {
                    acts.push(Action::Place { against, face });
                }
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

    #[test]
    fn upper_halves_are_high_and_lower_halves_are_not() {
        assert!(Orient::from_state("minecraft:oak_stairs[half=top]").high);
        assert!(Orient::from_state("minecraft:oak_slab[type=top]").high);
        assert!(!Orient::from_state("minecraft:oak_stairs[half=bottom]").high);
        assert!(!Orient::from_state("minecraft:oak_log[axis=y]").high);
    }

    #[test]
    fn facing_and_half_combine_and_double_slabs_have_no_preference() {
        let o = Orient::from_state("minecraft:oak_stairs[facing=east,half=top,shape=straight]");
        assert_eq!(o.look, Some((90.0, 0.0)));
        assert_eq!(o.clicked, [Face::Down]);
        let o = Orient::from_state("minecraft:oak_stairs[half=bottom,facing=west]");
        assert_eq!(o.look, Some((270.0, 0.0)));
        assert_eq!(o.clicked, [Face::Up]);
        let o = Orient::from_state("minecraft:oak_slab[type=double]");
        assert_eq!(o, Orient::default());
    }
}

#[cfg(test)]
mod stand_tests {
    use super::*;
    use crate::world::{Class, Pending, SectionKey};

    struct Field {
        solid: HashSet<Pos>,
    }

    impl Blocks for Field {
        fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, Pending> {
            Ok(if y < 64 || self.solid.contains(&(x, y, z)) {
                Class::STANDABLE
            } else {
                Class::PASSABLE
            })
        }

        fn state_at(&mut self, _: i32, _: i32, _: i32) -> Result<Option<Rc<str>>, Pending> {
            Ok(None)
        }

        fn find(&mut self, _: SectionKey, _: &[String]) -> Result<Vec<Pos>, Pending> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_stand_whose_view_was_blocked_since_it_was_chosen_is_rejected() {
        let mut world = Field {
            solid: HashSet::from([(3, 65, 5)]),
        };
        let target = BlockPos { x: 5, y: 64, z: 5 };
        let mut placer = Placer::new(&target, "minecraft:stone");
        placer.stand = Some((2, 64, 5));
        let me = Vec3 {
            x: 2.5,
            y: 64.0,
            z: 5.5,
        };
        let carried = vec![(36u8, "minecraft:stone".to_string(), 8u32)];
        assert_eq!(placer.tick(&me, &mut world, &carried), Hands::Wait);
        assert_eq!(placer.stand, None);
        assert!(placer.rejected.contains(&(2, 64, 5)));
    }
}

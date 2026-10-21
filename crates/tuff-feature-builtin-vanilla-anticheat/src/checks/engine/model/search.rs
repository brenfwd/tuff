use super::*;

/// Every combination of movement keys.
pub(crate) const KEYS: [Keys; 9] = [
    Keys(0),
    Keys(Keys::FORWARD),
    Keys(Keys::BACKWARD),
    Keys(Keys::LEFT),
    Keys(Keys::RIGHT),
    Keys(Keys::FORWARD | Keys::LEFT),
    Keys(Keys::FORWARD | Keys::RIGHT),
    Keys(Keys::BACKWARD | Keys::LEFT),
    Keys(Keys::BACKWARD | Keys::RIGHT),
];

pub(crate) const JUMP: u8 = 0x10;
pub(crate) const MOVEMENT_KEYS: u8 = 0x0f;

/// The best replay of a move found so far.
pub(crate) struct Search {
    pub(crate) found: Option<(State, Marks, u32)>,
    /// The state and input of the matched replay's last tick.
    pub(crate) last: Option<(State, Input)>,
    pub(crate) closest: Option<(f64, State)>,
    /// The reported rise.
    pub(crate) rise: f64,
    /// Horizontal end offsets of the replays that rose as the move did,
    /// and of those that did not jump.
    pub(crate) level: Vec<[f64; 2]>,
    pub(crate) walked: Vec<[f64; 2]>,
    pub(crate) highest: f64,
    pub(crate) tried: u32,
    /// Every one-tick replay's last tick (its start, input and end), kept
    /// when entities nearby may have pushed the player.
    pub(crate) ends: Option<Vec<(State, Input, State)>>,
}

impl Search {
    pub(crate) fn new(rise: f64, keep_ends: bool) -> Self {
        Search {
            found: None,
            last: None,
            closest: None,
            rise,
            level: Vec::new(),
            walked: Vec::new(),
            highest: f64::NEG_INFINITY,
            tried: 0,
            ends: keep_ends.then(Vec::new),
        }
    }

    /// Where the move may end: anywhere a replay that rose as it did
    /// could reach with less input, else anywhere one that did not jump
    /// could (a jump's push comes with its rise).
    pub(crate) fn reach(&self) -> &[[f64; 2]] {
        if self.level.is_empty() {
            &self.walked
        } else {
            &self.level
        }
    }

    pub(crate) fn farthest(&self) -> f64 {
        self.reach()
            .iter()
            .map(|p| p[0].hypot(p[1]))
            .fold(0.0, f64::max)
    }

    /// How far `point` lies outside the convex hull of the reachable
    /// offsets and standing still (the client may fall short of a replay when
    /// its world holds a block this one lacks): a move short of a replay in its own direction is slower
    /// than the client could have gone, never an advantage.
    pub(crate) fn outside(&self, point: [f64; 2]) -> f64 {
        let mut reach = self.reach().to_vec();
        if !reach.is_empty() {
            reach.push([0.0, 0.0]);
        }
        let hull = convex_hull(&reach);
        match hull.len() {
            0 => point[0].hypot(point[1]),
            1 => (point[0] - hull[0][0]).hypot(point[1] - hull[0][1]),
            _ => {
                let cross = |a: [f64; 2], b: [f64; 2], p: [f64; 2]| {
                    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
                };
                let edges = || (0..hull.len()).map(|i| (hull[i], hull[(i + 1) % hull.len()]));
                if hull.len() > 2 && edges().all(|(a, b)| cross(a, b, point) >= 0.0) {
                    return 0.0;
                }
                edges()
                    .map(|(a, b)| segment_distance(point, a, b))
                    .fold(f64::INFINITY, f64::min)
            }
        }
    }
}

pub(crate) fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len > 0.0 {
        (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - t * ab[0]).hypot(p[1] - a[1] - t * ab[1])
}

/// The convex hull of `points`, counter-clockwise.
pub(crate) fn convex_hull(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    sorted.dedup();
    if sorted.len() < 3 {
        return sorted;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(sorted.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let ordered: Box<dyn Iterator<Item = &[f64; 2]>> = if pass == 0 {
            Box::new(sorted.iter())
        } else {
            Box::new(sorted.iter().rev())
        };
        for &p in ordered {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// What held during a tick the client ended.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ended {
    pub(crate) flags: u8,
    pub(crate) rotation: Option<(f32, f32)>,
    pub(crate) sprint: bool,
    pub(crate) item: f32,
}

pub(crate) fn item_scale(facts: &Facts) -> f32 {
    facts
        .item_use
        .as_ref()
        .and_then(|used| super::super::super::movement::use_input_scale(used.item))
        .map_or(1.0, |scale| scale as f32)
}

/// A velocity change the client applied between two ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Change {
    pub(crate) set: Option<[f64; 3]>,
    pub(crate) add: [f64; 3],
}

impl Change {
    /// Server velocities received together, in the order sent: a motion
    /// replaces what came before it, a blast adds to it.
    pub(crate) fn of(impulses: &[Impulse]) -> Change {
        impulses.iter().fold(Change::default(), |change, impulse| {
            if impulse.added {
                Change {
                    add: [0, 1, 2].map(|a| change.add[a] + impulse.velocity[a]),
                    ..change
                }
            } else {
                Change {
                    set: Some(impulse.velocity),
                    add: [0.0; 3],
                }
            }
        })
    }

    pub(crate) fn apply(&self, velocity: [f64; 3]) -> [f64; 3] {
        let base = self.set.unwrap_or(velocity);
        [0, 1, 2].map(|a| base[a] + self.add[a])
    }
}

/// What moved the client between ticks during one replay. Tick `t`
/// counts the replayed ticks from the first unreported one.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Plan {
    /// Server velocity the client applied just before tick `t`.
    pub(crate) impulse: Option<(u32, Change)>,
    /// An attached firework pulled before each tick in `from..to`.
    pub(crate) boost: Option<(u32, u32)>,
    /// A displacement the client applied to itself just before tick `t`.
    pub(crate) shove: Option<(u32, ShoveKind)>,
}

/// One way to replay a move: the rules, the client's blocks, the plan and
/// the look the client had at the end of each tick before tick `t`
/// (`looks[0]` is the previous move's).
pub(crate) struct Replay<'a> {
    pub(crate) rules: Rules,
    pub(crate) world: &'a World<'a>,
    pub(crate) plan: Plan,
    pub(crate) looks: &'a [[f32; 2]],
}

impl Replay<'_> {
    /// The state as tick `t` starts, after what moved it in between.
    pub(crate) fn prepare(&self, mut s: State, t: u32) -> State {
        if let Some((from, to)) = self.plan.boost
            && (from..to).contains(&t)
        {
            let look = self.looks[(t as usize).min(self.looks.len() - 1)];
            s.velocity = firework_boost(s.velocity, view(look[0], look[1]));
        }
        if let Some((at, change)) = self.plan.impulse
            && at == t
        {
            s.velocity = change.apply(s.velocity);
        }
        if let Some((at, kind)) = self.plan.shove
            && at == t
        {
            self.shove(&mut s, kind);
        }
        s
    }

    pub(crate) fn shove(&self, s: &mut State, kind: ShoveKind) {
        match kind {
            ShoveKind::Moved(delta) => {
                for axis in 0..3 {
                    s.position[axis] += delta[axis];
                }
            }
            ShoveKind::Riptide {
                strength,
                yaw,
                pitch,
            } => {
                let look = view(yaw, pitch).map(|c| c as f32);
                let length = (look[0] * look[0] + look[1] * look[1] + look[2] * look[2]).sqrt();
                for axis in 0..3 {
                    s.velocity[axis] += f64::from(look[axis] * (strength / length));
                }
                if s.on_ground {
                    let input = Input {
                        keys: Keys(0),
                        jump: false,
                        sprint: false,
                        slow: false,
                        shift: false,
                        item: 1.0,
                        yaw,
                        pitch,
                    };
                    let moved = self.world.collide(
                        body(s.position, s.pose),
                        [0.0, RIPTIDE_HOP, 0.0],
                        self.rules.body.step_height,
                        true,
                        context_of(s, &input, &self.rules),
                    );
                    s.position[1] += moved[1];
                    s.on_ground = false;
                    s.supporting = None;
                }
            }
        }
    }

    pub(crate) fn step(&self, s: State, t: u32, input: &Input, marks: &mut Marks) -> State {
        tick(self.prepare(s, t), input, &self.rules, self.world, marks)
    }
}

//! Horizontal walking prediction by the client's own rules: movement keys,
//! the input scaling of item use and sneaking, acceleration on the ground
//! or in the air and friction carried as internal momentum.
//!
//! A reported move that one tick from the momentum does not explain is
//! retried over every key combination and over the ticks the client did
//! not report: it sends no position while a tick moves under 2e-4 blocks,
//! so one move can hold up to three ticks of motion. While sneaking, a
//! move may also be the back-off at a block edge, which shortens the
//! move but keeps the momentum.

use tuff_core::protocol::play::Serverbound;

use crate::trace::Tags;

/// The client reports no position for a tick moving less than this
/// (squared) distance.
const SILENT_SQ: f64 = 2.0e-4 * 2.0e-4;
/// How close a predicted path must end to the reported position, per axis.
pub const MATCH: f64 = 1.0e-3;
/// Unreported ticks searched over every input.
pub const MAX_HIDDEN: u32 = 2;
/// Further unreported ticks, replayed without input before the searched ones.
const MAX_DECAY: u32 = 40;
const CARRY_SLACK: f64 = 0.01;
const EDGE_STEP: f64 = 0.05;
/// The longest tick the edge back-off explains.
const EDGE_LIMIT: f64 = 0.3;
const INPUT_DAMPING: f32 = 0.98;
const SPRINT_JUMP: f32 = 0.2;

/// The movement keys of a Player Input packet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys(pub u8);

impl Keys {
    pub const FORWARD: u8 = 0x01;
    pub const BACKWARD: u8 = 0x02;
    pub const LEFT: u8 = 0x04;
    pub const RIGHT: u8 = 0x08;
    pub const SNEAK: u8 = 0x20;

    fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    pub fn sneaking(self) -> bool {
        self.has(Self::SNEAK)
    }

    /// Strafe (left positive) and forward impulse as a unit vector.
    fn impulse(self) -> [f32; 2] {
        let axis = |pos: u8, neg: u8| {
            f32::from(u8::from(self.has(pos))) - f32::from(u8::from(self.has(neg)))
        };
        let v = [
            axis(Self::LEFT, Self::RIGHT),
            axis(Self::FORWARD, Self::BACKWARD),
        ];
        let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
        if len < 1.0e-4 {
            [0.0, 0.0]
        } else {
            [v[0] / len, v[1] / len]
        }
    }
}

/// Every combination of movement keys, searched when the reported keys
/// do not explain a move.
const INPUTS: [Keys; 9] = [
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

pub(crate) fn table_sin(index: f32) -> f64 {
    let i = (index as i32) & 0xffff;
    (f64::from(i) * std::f64::consts::TAU / 65536.0).sin() as f32 as f64
}

/// Sine and cosine of a yaw in degrees, quantised like the client's table.
pub(crate) fn sin_cos(yaw: f32) -> (f64, f64) {
    let radians = yaw * (std::f64::consts::PI / 180.0) as f32;
    let index = radians * 10430.378;
    (table_sin(index), table_sin(index + 16384.0))
}

/// How one tick of walking moves: friction, acceleration and the input
/// scaling that hold at the start of the tick.
#[derive(Clone, Copy, Debug)]
pub struct Regime {
    /// Applied to this tick's motion for the next: block friction x 0.91
    /// on the ground, 0.91 in the air.
    pub friction: f32,
    /// Movement speed on the ground, flying speed in the air.
    pub speed: f32,
    /// Item use input scale, 1.0 when no slowing item is in use.
    pub item: f32,
    /// Sneaking input scale, 1.0 when not sneaking.
    pub sneak: f32,
    pub yaw: f32,
    /// This tick starts a sprint jump.
    pub sprint_jump: bool,
    /// Sneaking on the ground, where the client backs off from edges.
    pub edge: bool,
}

impl Regime {
    fn input(&self, keys: Keys) -> [f32; 2] {
        let raw = keys.impulse();
        if raw == [0.0, 0.0] {
            return raw;
        }
        let scale = INPUT_DAMPING * self.item * self.sneak;
        let v = [raw[0] * scale, raw[1] * scale];
        let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
        if len <= 0.0 {
            return v;
        }
        let unit = [v[0] / len, v[1] / len];
        let (x, y) = (unit[0].abs(), unit[1].abs());
        let tan = if y > x { x / y } else { y / x };
        let stretched = (len * (1.0 + tan * tan).sqrt()).min(1.0);
        [unit[0] * stretched, unit[1] * stretched]
    }

    /// The motion one tick of these keys adds.
    pub fn accel(&self, keys: Keys) -> [f64; 2] {
        let [strafe, forward] = self.input(keys);
        let (mut x, mut z) = (f64::from(strafe), f64::from(forward));
        let len2 = x * x + z * z;
        if len2 < 1.0e-7 {
            (x, z) = (0.0, 0.0);
        } else if len2 > 1.0 {
            let len = len2.sqrt();
            x /= len;
            z /= len;
        }
        let speed = f64::from(self.speed);
        x *= speed;
        z *= speed;
        let (sin, cos) = sin_cos(self.yaw);
        let mut accel = [x * cos - z * sin, z * cos + x * sin];
        if self.sprint_jump {
            accel[0] -= sin * f64::from(SPRINT_JUMP);
            accel[1] += cos * f64::from(SPRINT_JUMP);
        }
        accel
    }

    /// The longest acceleration any keys give, whatever the yaw.
    pub fn max_accel(&self) -> f64 {
        let flat = Regime {
            sprint_jump: false,
            ..*self
        };
        INPUTS
            .iter()
            .map(|keys| {
                let [x, z] = flat.accel(*keys);
                x.hypot(z)
            })
            .fold(0.0, f64::max)
    }
}

/// How a move was explained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// One tick of the reported keys.
    Keys,
    /// One tick of other keys.
    Inputs,
    /// This many unreported ticks before the reported one.
    Hidden(u32),
    /// The client backed the move off at a block edge.
    Edge,
    /// No path matched; the move only has to stay under the bound.
    Unexplained,
}

impl Path {
    pub fn tags(self) -> Tags {
        match self {
            Path::Keys | Path::Inputs => Tags::PREDICTED,
            Path::Hidden(_) => Tags::PREDICTED.with(Tags::HIDDEN_TICKS, true),
            Path::Edge => Tags::EDGE_BACKOFF,
            Path::Unexplained => Tags::empty(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Last {
    /// The last tick's own motion, before an edge back-off shortened it.
    motion: [f64; 2],
    friction: f32,
}

/// Internal momentum and the keys and ticks the client reported since
/// its last position.
#[derive(Default)]
pub struct Momentum {
    last: Option<Last>,
    keys: Keys,
    prior: Keys,
    ticks: u32,
    ticking: bool,
    tried: u32,
    predicted: Option<f64>,
    strict: bool,
}

fn close(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() <= MATCH && (a[1] - b[1]).abs() <= MATCH
}

fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn scale(a: [f64; 2], by: f64) -> [f64; 2] {
    [a[0] * by, a[1] * by]
}

/// A reported axis is the predicted one shortened toward zero in
/// back-off steps, or stopped.
fn backed_off(predicted: f64, reported: f64) -> bool {
    if (predicted - reported).abs() <= MATCH || reported.abs() <= MATCH {
        return true;
    }
    if predicted.signum() != reported.signum() || reported.abs() > predicted.abs() {
        return false;
    }
    let steps = ((predicted.abs() - reported.abs()) / EDGE_STEP).round();
    (predicted.abs() - steps * EDGE_STEP - reported.abs()).abs() <= MATCH
}

/// Replays unreported ticks of these keys; each must have stayed under
/// the client's reporting threshold.
fn silent(
    mut velocity: [f64; 2],
    ticks: u32,
    accel: [f64; 2],
    friction: f64,
) -> Option<([f64; 2], [f64; 2])> {
    let mut offset = [0.0, 0.0];
    for _ in 0..ticks {
        let motion = add(velocity, accel);
        if motion[0] * motion[0] + motion[1] * motion[1] >= SILENT_SQ {
            return None;
        }
        offset = add(offset, motion);
        velocity = scale(motion, friction);
    }
    Some((velocity, offset))
}

impl Momentum {
    /// Tracks the reported keys and the client's tick ends.
    pub fn observe(&mut self, packet: &Serverbound) {
        match packet {
            Serverbound::PlayerInput(flags) => {
                let keys = Keys(*flags);
                if keys != self.keys {
                    self.prior = self.keys;
                    self.keys = keys;
                }
            }
            Serverbound::ClientTickEnd => {
                self.ticks = self.ticks.saturating_add(1);
                self.ticking = true;
            }
            _ => {}
        }
    }

    pub fn keys(&self) -> Keys {
        self.keys
    }

    /// Forgets the momentum after anything the prediction does not follow.
    pub fn reset(&mut self) {
        self.last = None;
        self.ticks = 0;
    }

    /// The length of the last tick's own motion.
    pub fn last_motion(&self) -> Option<f64> {
        self.last.map(|last| last.motion[0].hypot(last.motion[1]))
    }

    /// Candidate paths the last judged move tried.
    pub fn tried(&self) -> u32 {
        self.tried
    }

    /// Whether an unexplained move carries on only the predicted momentum
    /// (plus a little) instead of the observed one.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Length of the reported-keys prediction for the last judged move.
    pub fn predicted(&self) -> Option<f64> {
        self.predicted
    }

    fn commit(&mut self, motion: [f64; 2], regime: &Regime) {
        self.last = Some(Last {
            motion,
            friction: regime.friction,
        });
    }

    /// Explains a reported horizontal move. A move no path explains
    /// carries its own motion on, at most `bound` long.
    pub fn judge(&mut self, delta: [f64; 2], regime: &Regime, bound: f64) -> Path {
        let ticks = std::mem::take(&mut self.ticks);
        self.tried = 0;
        self.predicted = None;
        let Some(last) = self.last else {
            self.commit(delta, regime);
            return Path::Unexplained;
        };
        let carried = scale(last.motion, f64::from(last.friction));
        let keyed = add(carried, regime.accel(Keys(self.keys.0 & 0x0f)));
        self.predicted = Some(keyed[0].hypot(keyed[1]));
        let unreported = if self.ticking {
            ticks.saturating_sub(1)
        } else {
            0
        };
        let found = self
            .replay(carried, delta, regime, unreported)
            .or_else(|| self.back_off(carried, delta, regime, unreported));
        match found {
            Some((path, motion)) => {
                self.commit(motion, regime);
                path
            }
            None => self.unexplained(delta, regime, bound, keyed),
        }
    }

    /// The reported keys first, then every input over the unreported
    /// ticks: at most 81 paths for a client that sends tick ends.
    fn replay(
        &mut self,
        carried: [f64; 2],
        delta: [f64; 2],
        regime: &Regime,
        unreported: u32,
    ) -> Option<(Path, [f64; 2])> {
        let friction = f64::from(regime.friction);
        let extra = unreported.saturating_sub(MAX_HIDDEN).min(MAX_DECAY);
        let hidden = unreported.min(MAX_HIDDEN);
        let (start, decayed) = silent(carried, extra, [0.0, 0.0], friction)?;
        let target = [delta[0] - decayed[0], delta[1] - decayed[1]];
        let path = |count: u32, searched: bool| match count {
            0 if searched => Path::Inputs,
            0 => Path::Keys,
            n => Path::Hidden(n),
        };
        let keys = Keys(self.keys.0 & 0x0f);
        self.tried = 1;
        if let Some((velocity, offset)) = silent(start, hidden, regime.accel(keys), friction) {
            let motion = add(velocity, regime.accel(keys));
            if close(add(offset, motion), target) {
                return Some((path(hidden, false), motion));
            }
        }
        let counts: &[u32] = if self.ticking {
            std::slice::from_ref(&hidden)
        } else {
            &[0, 1, 2]
        };
        for &count in counts {
            let quiet: &[Keys] = if count == 0 { &INPUTS[..1] } else { &INPUTS };
            for unseen in quiet {
                let Some((velocity, offset)) =
                    silent(start, count, regime.accel(*unseen), friction)
                else {
                    continue;
                };
                for keys in INPUTS {
                    self.tried += 1;
                    let motion = add(velocity, regime.accel(keys));
                    if close(add(offset, motion), target) {
                        return Some((path(count, true), motion));
                    }
                }
            }
        }
        None
    }

    /// A sneaking client held at an edge: the momentum ran on through the
    /// unreported ticks, keys held or released, while the back-off kept
    /// the player in place, and the reported tick is backed off per axis.
    fn back_off(
        &mut self,
        carried: [f64; 2],
        delta: [f64; 2],
        regime: &Regime,
        unreported: u32,
    ) -> Option<(Path, [f64; 2])> {
        if !regime.edge {
            return None;
        }
        let friction = f64::from(regime.friction);
        let keys = Keys(self.keys.0 & 0x0f);
        let prior = Keys(self.prior.0 & 0x0f);
        let choices = [keys, prior, Keys(0)];
        for (n, held) in choices.iter().enumerate() {
            if choices[..n].contains(held) {
                continue;
            }
            let mut velocity = carried;
            for _ in 0..unreported.min(MAX_DECAY + MAX_HIDDEN) {
                velocity = scale(add(velocity, regime.accel(*held)), friction);
            }
            for keys in INPUTS {
                self.tried += 1;
                let motion = add(velocity, regime.accel(keys));
                if motion[0].hypot(motion[1]) <= EDGE_LIMIT
                    && backed_off(motion[0], delta[0])
                    && backed_off(motion[1], delta[1])
                {
                    return Some((Path::Edge, motion));
                }
            }
        }
        None
    }

    fn unexplained(
        &mut self,
        delta: [f64; 2],
        regime: &Regime,
        bound: f64,
        keyed: [f64; 2],
    ) -> Path {
        let h = delta[0].hypot(delta[1]);
        let cap = if self.strict {
            bound.min(keyed[0].hypot(keyed[1]) + CARRY_SLACK)
        } else {
            bound
        };
        let motion = if h > cap && h > 0.0 {
            scale(delta, cap.max(0.0) / h)
        } else {
            delta
        };
        self.commit(motion, regime);
        Path::Unexplained
    }
}

#[cfg(test)]
pub(crate) mod sim {
    use super::*;

    const GROUND: f32 = 0.6 * 0.91;

    pub(crate) fn walking(item: f32, sneak: f32, yaw: f32) -> Regime {
        Regime {
            friction: GROUND,
            speed: 0.1,
            item,
            sneak,
            yaw,
            sprint_jump: false,
            edge: sneak < 1.0,
        }
    }

    /// An independent model of the client's walking tick, with plain
    /// trigonometry, its position reporting threshold and an edge at
    /// `edge` on +x that a sneaking client backs off from.
    pub(crate) struct Client {
        velocity: [f64; 2],
        pub(crate) x: f64,
        pub(crate) z: f64,
        pub(crate) edge: Option<f64>,
    }

    impl Client {
        pub(crate) fn new() -> Self {
            Client {
                velocity: [0.0, 0.0],
                x: 0.0,
                z: 0.0,
                edge: None,
            }
        }

        fn input(keys: Keys, regime: &Regime) -> [f64; 2] {
            let strafe = f64::from(u8::from(keys.has(Keys::LEFT)))
                - f64::from(u8::from(keys.has(Keys::RIGHT)));
            let forward = f64::from(u8::from(keys.has(Keys::FORWARD)))
                - f64::from(u8::from(keys.has(Keys::BACKWARD)));
            let len = strafe.hypot(forward);
            if len == 0.0 {
                return [0.0, 0.0];
            }
            let s = 0.98 * f64::from(regime.item) * f64::from(regime.sneak);
            let (ux, uz) = (strafe / len, forward / len);
            let tan = ux.abs().min(uz.abs()) / ux.abs().max(uz.abs());
            let m = (s * (1.0 + tan * tan).sqrt()).min(1.0) * f64::from(regime.speed);
            let yaw = f64::from(regime.yaw).to_radians();
            let (x, z) = (ux * m, uz * m);
            [x * yaw.cos() - z * yaw.sin(), z * yaw.cos() + x * yaw.sin()]
        }

        /// One client tick; returns the reported move, if any.
        pub(crate) fn tick(&mut self, keys: Keys, regime: &Regime) -> Option<[f64; 2]> {
            let a = Self::input(keys, regime);
            let motion = [self.velocity[0] + a[0], self.velocity[1] + a[1]];
            let mut dx = motion[0];
            if let Some(edge) = self.edge.filter(|_| regime.edge) {
                while dx > 0.0 && self.x + dx > edge {
                    dx = if dx < EDGE_STEP { 0.0 } else { dx - EDGE_STEP };
                }
            }
            self.x += dx;
            self.z += motion[1];
            self.velocity = [
                motion[0] * f64::from(regime.friction),
                motion[1] * f64::from(regime.friction),
            ];
            let moved_sq = dx * dx + motion[1] * motion[1];
            (moved_sq > SILENT_SQ).then_some([dx, motion[1]])
        }
    }

    /// Drives a client through key presses and returns the paths of
    /// every reported move, the client sending tick ends like vanilla.
    pub(crate) fn drive(
        client: &mut Client,
        momentum: &mut Momentum,
        plan: &[(Keys, u32)],
        regime: &Regime,
    ) -> Vec<Path> {
        let mut paths = Vec::new();
        let mut pending = [0.0, 0.0];
        for &(keys, ticks) in plan {
            momentum.observe(&Serverbound::PlayerInput(keys.0));
            for _ in 0..ticks {
                let before = [client.x, client.z];
                let reported = client.tick(keys, regime);
                pending[0] += client.x - before[0];
                pending[1] += client.z - before[1];
                if reported.is_some() {
                    paths.push(momentum.judge(pending, regime, 1.0));
                    pending = [0.0, 0.0];
                }
                momentum.observe(&Serverbound::ClientTickEnd);
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::sim::{Client, drive, walking};
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn slow_walking_stopping_and_starting_are_all_predicted() {
        for (item, yaw) in [(0.2, 0.0), (1.0, 37.5), (0.2, -123.0)] {
            let regime = walking(item, 1.0, yaw);
            let mut client = Client::new();
            let mut momentum = Momentum::default();
            let forward = Keys(Keys::FORWARD);
            let diagonal = Keys(Keys::FORWARD | Keys::RIGHT);
            let plan = [
                (forward, 20),
                (Keys(0), 30),
                (diagonal, 12),
                (Keys(Keys::BACKWARD), 1),
                (Keys(0), 25),
                (Keys(Keys::LEFT), 9),
            ];
            let paths = drive(&mut client, &mut momentum, &plan, &regime);
            assert!(
                paths[1..].iter().all(|p| *p != Path::Unexplained),
                "{item} {yaw}: {paths:?}"
            );
            assert!(
                paths.iter().any(|p| matches!(p, Path::Hidden(_))),
                "the stop ends in unreported ticks: {paths:?}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reported_keys_that_match_are_not_searched_against_other_inputs() {
        let regime = walking(1.0, 1.0, 0.0);
        let mut client = Client::new();
        let mut momentum = Momentum::default();
        let plan = [(Keys(Keys::FORWARD), 20)];
        let paths = drive(&mut client, &mut momentum, &plan, &regime);
        assert!(paths[1..].iter().all(|p| *p == Path::Keys), "{paths:?}");
        assert_eq!(momentum.tried(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_steady_cheat_does_not_ratchet_the_carried_momentum() {
        let regime = walking(0.2, 1.0, 270.0);
        let mut client = Client::new();
        let mut momentum = Momentum::default();
        momentum.set_strict(true);
        momentum.observe(&Serverbound::PlayerInput(Keys::FORWARD));
        let mut honest = 0.0;
        for _ in 0..40 {
            let before = client.x;
            client.tick(Keys(Keys::FORWARD), &regime);
            honest = client.x - before;
            momentum.judge([honest, 0.0], &regime, 1.0);
        }
        let mut step = honest;
        for _ in 0..40 {
            step = step * 0.546 + 0.0196 * 1.06 / 1.0;
            momentum.judge([honest * 1.06, 0.0], &regime, 1.0);
        }
        let carried = momentum.last_motion().unwrap();
        assert!(
            carried < honest * 1.06 + 0.012,
            "{carried} vs {honest} {step}"
        );
        assert!(momentum.predicted().unwrap() < honest * 1.1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn keys_reported_a_tick_late_are_found_by_the_search() {
        let regime = walking(0.2, 1.0, 90.0);
        let mut client = Client::new();
        let mut momentum = Momentum::default();
        drive(
            &mut client,
            &mut momentum,
            &[(Keys(Keys::FORWARD), 10)],
            &regime,
        );
        let reported = client.tick(Keys(Keys::LEFT), &regime).unwrap();
        assert_eq!(momentum.judge(reported, &regime, 1.0), Path::Inputs);
        assert!(momentum.tried() <= 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn sneaking_at_an_edge_keeps_the_momentum_the_back_off_hides() {
        let regime = walking(1.0, 0.3, -90.0);
        let mut client = Client::new();
        client.edge = Some(0.4);
        let mut momentum = Momentum::default();
        let paths = drive(
            &mut client,
            &mut momentum,
            &[(Keys(Keys::FORWARD), 30)],
            &regime,
        );
        assert!(paths.contains(&Path::Edge), "{paths:?}");
        assert!(
            paths[1..].iter().all(|p| *p != Path::Unexplained),
            "{paths:?}"
        );
        client.edge = None;
        let paths = drive(&mut client, &mut momentum, &[(Keys(0), 3)], &regime);
        assert_eq!(
            paths.first(),
            Some(&Path::Edge),
            "the momentum held at the edge carries on"
        );
        assert!(client.x > 0.4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_search_runs_only_after_a_miss_and_is_bounded() {
        let regime = walking(0.2, 1.0, 10.0);
        let mut client = Client::new();
        let mut momentum = Momentum::default();
        drive(
            &mut client,
            &mut momentum,
            &[(Keys(Keys::FORWARD), 10)],
            &regime,
        );
        let reported = client.tick(Keys(Keys::FORWARD), &regime).unwrap();
        assert_eq!(momentum.judge(reported, &regime, 1.0), Path::Keys);
        assert_eq!(momentum.tried(), 1);
        momentum.observe(&Serverbound::ClientTickEnd);
        momentum.observe(&Serverbound::ClientTickEnd);
        assert_eq!(momentum.judge([0.3, 0.0], &regime, 0.1), Path::Unexplained);
        assert!(momentum.tried() <= 1 + 81 + 27, "{}", momentum.tried());
        assert!(
            momentum.last_motion().unwrap() <= 0.1 + 1e-9,
            "only the bound carries on"
        );
        let mut legacy = Momentum::default();
        legacy.judge([0.0, 0.0], &regime, 1.0);
        legacy.judge([0.3, 0.0], &regime, 1.0);
        assert!(legacy.tried() <= 1 + 9 + 81 + 81 + 27, "{}", legacy.tried());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_move_faster_than_the_scaled_input_is_unexplained() {
        let slowed = walking(0.2, 1.0, 0.0);
        let full = walking(1.0, 1.0, 0.0);
        let mut client = Client::new();
        let mut momentum = Momentum::default();
        let mut paths = Vec::new();
        for _ in 0..20 {
            let reported = client.tick(Keys(Keys::FORWARD), &full).unwrap();
            momentum.observe(&Serverbound::ClientTickEnd);
            paths.push(momentum.judge(reported, &slowed, 1.0));
        }
        assert!(
            paths[3..].iter().all(|p| *p == Path::Unexplained),
            "{paths:?}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_diagonal_is_stretched_toward_the_square_and_the_jump_pushes_forward() {
        let regime = walking(0.2, 1.0, 0.0);
        let straight = regime.accel(Keys(Keys::FORWARD));
        let diagonal = regime.accel(Keys(Keys::FORWARD | Keys::LEFT));
        assert!((straight[1] - 0.0196).abs() < 1e-6, "{straight:?}");
        assert!((diagonal[0].hypot(diagonal[1]) - 0.0196 * 2f64.sqrt()).abs() < 1e-5);
        assert!((regime.max_accel() - 0.0196 * 2f64.sqrt()).abs() < 1e-5);
        let full = walking(1.0, 1.0, 0.0);
        assert!((full.max_accel() - 0.1).abs() < 1e-6);
        let jump = Regime {
            sprint_jump: true,
            ..walking(1.0, 1.0, 90.0)
        };
        let push = jump.accel(Keys(0));
        assert!(
            (push[0] + 0.2).abs() < 1e-4 && push[1].abs() < 1e-4,
            "{push:?}"
        );
        assert!(backed_off(0.12, 0.07) && backed_off(0.12, 0.0) && !backed_off(0.12, 0.09));
        assert!(!backed_off(0.12, -0.02) && backed_off(-0.12, -0.02));
    }
}

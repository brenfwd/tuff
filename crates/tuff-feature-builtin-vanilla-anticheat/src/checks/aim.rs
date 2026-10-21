//! Attack aim. The client only attacks what its crosshair picked, so the
//! look ray must pass the target's hitbox as the client showed it; the
//! rotation used is the one the client sends after the attack within the
//! same tick, so a pending attack is judged at the tick's end against
//! every rotation between the last accepted one and that one, every
//! recent eye position and every recent target position. Kill-aura that
//! hits several entities around itself without turning snaps between
//! distant directions faster than a hand turns; those snaps are counted
//! per short window.

use std::collections::VecDeque;

use tuff_core::config::{ConfigStore, Key, Value};
use tuff_core::protocol::play::{GameMode, Serverbound};
use tuff_core::world::physics::Aabb;
use tuff_core::world::session::PlayerSession;
use tuff_vanilla_data::registry::EntityType;

use super::{Check, Facts, Fix, Flag};
use crate::config::{number_key, read_number};
use crate::history::{self, PositionHistory};

pub const MAX_ANGLE: &str = "tuff.anticheat.aim.maxAngle";
pub const SNAP_ANGLE: &str = "tuff.anticheat.aim.snapAngle";
pub const SNAPS: &str = "tuff.anticheat.aim.snaps";
pub const MAX_LATENCY: &str = "tuff.anticheat.aim.maxLatency";

const BOX_MARGIN: f64 = 0.1;
const ROTATION_STEPS: usize = 6;
const SEARCH_STEPS: usize = 40;
const SEARCH_FAR: f64 = 8.0;
const SNAP_TICKS: u64 = 2;
const SNAP_WINDOW: u64 = 40;
const KEPT_ATTACKS: usize = 16;
/// A pending attack with no tick end this long is judged on any packet.
const STALE_TICKS: u64 = 2;

pub fn keys() -> Vec<Key> {
    vec![
        number_key(
            MAX_ANGLE,
            AimSettings::DEFAULT.max_angle,
            "Degrees an attack's look ray may miss the target's hitbox before the aim check flags it",
        ),
        number_key(
            SNAP_ANGLE,
            AimSettings::DEFAULT.snap_angle,
            "Degrees between two different attack targets, hit within two ticks, that count as a snap for the multi_aura check",
        ),
        Key::new(
            SNAPS,
            Value::Int(i64::from(AimSettings::DEFAULT.snaps)),
            "Snaps between different targets within two seconds above which multi_aura flags",
        )
        .validated(|value| match value {
            Value::Int(n) if *n >= 1 => Ok(()),
            _ => Err("must be at least 1".to_owned()),
        }),
        Key::new(
            MAX_LATENCY,
            Value::Int(i64::from(AimSettings::DEFAULT.max_latency_ms)),
            "Ping in milliseconds above which the aim check abstains, since the trace no longer covers what the client saw",
        )
        .validated(|value| match value {
            Value::Int(n) if *n >= 0 => Ok(()),
            _ => Err("must not be negative".to_owned()),
        }),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AimSettings {
    pub max_angle: f64,
    pub snap_angle: f64,
    pub snaps: u32,
    pub max_latency_ms: i32,
}

impl AimSettings {
    pub const DEFAULT: AimSettings = AimSettings {
        max_angle: 10.0,
        snap_angle: 60.0,
        snaps: 2,
        max_latency_ms: 600,
    };

    pub fn read(store: &ConfigStore) -> AimSettings {
        let d = Self::DEFAULT;
        AimSettings {
            max_angle: read_number(store, MAX_ANGLE).unwrap_or(d.max_angle),
            snap_angle: read_number(store, SNAP_ANGLE).unwrap_or(d.snap_angle),
            snaps: store
                .get_int(SNAPS)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(d.snaps),
            max_latency_ms: store
                .get_int(MAX_LATENCY)
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(d.max_latency_ms),
        }
    }
}

impl Default for AimSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where an attacked entity was and where the eyes were, over the
/// latency window.
#[derive(Clone, Debug, PartialEq)]
pub struct AimTarget {
    pub entity: i32,
    pub boxes: Vec<Aabb>,
    pub eyes: Vec<[f64; 3]>,
    pub settings: AimSettings,
}

impl AimTarget {
    pub fn probe(
        session: &PlayerSession,
        packet: &Serverbound,
        history: &PositionHistory,
        settings: AimSettings,
    ) -> Option<AimTarget> {
        let Serverbound::Attack(entity) = packet else {
            return None;
        };
        let (kind, position) = match session.view.entities.visible_player(*entity) {
            Some((position, _)) => (EntityType::Player, position),
            None => {
                let (kind, _, position) = session.view.entities.visible_entity(*entity)?;
                (kind, position)
            }
        };
        if kind == EntityType::EnderDragon {
            return None;
        }
        let size = kind.size();
        let half = f64::from(size.width) / 2.0 + BOX_MARGIN;
        let height = f64::from(size.height) + BOX_MARGIN;
        let window = history::window(session.net.latency_ms);
        let boxes = std::iter::once(position)
            .chain(history.target(*entity, session.tick, window))
            .map(|p| {
                [
                    p[0] - half,
                    p[1] - BOX_MARGIN,
                    p[2] - half,
                    p[0] + half,
                    p[1] + height,
                    p[2] + half,
                ]
            })
            .collect();
        let eyes = std::iter::once(session.eye())
            .chain(history.eyes(session.tick, window))
            .collect();
        Some(AimTarget {
            entity: *entity,
            boxes,
            eyes,
            settings,
        })
    }
}

pub fn look(rotation: [f32; 2]) -> [f64; 3] {
    let yaw = f64::from(rotation[0]).to_radians();
    let pitch = f64::from(rotation[1]).to_radians();
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

fn angle_between(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let la = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let lb = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
    if la < 1e-9 || lb < 1e-9 {
        return 0.0;
    }
    (dot / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn nearest(p: [f64; 3], b: Aabb) -> [f64; 3] {
    [
        p[0].clamp(b[0], b[3]),
        p[1].clamp(b[1], b[4]),
        p[2].clamp(b[2], b[5]),
    ]
}

fn ray_hits(eye: [f64; 3], dir: [f64; 3], b: Aabb) -> bool {
    let mut near = 0.0f64;
    let mut far = f64::INFINITY;
    for axis in 0..3 {
        let (lo, hi) = (b[axis], b[axis + 3]);
        if dir[axis].abs() < 1e-12 {
            if eye[axis] < lo || eye[axis] > hi {
                return false;
            }
            continue;
        }
        let a = (lo - eye[axis]) / dir[axis];
        let c = (hi - eye[axis]) / dir[axis];
        near = near.max(a.min(c));
        far = far.min(a.max(c));
        if near > far {
            return false;
        }
    }
    true
}

/// Degrees the ray from `eye` along `dir` misses box `b` by.
pub fn miss_angle(eye: [f64; 3], dir: [f64; 3], b: Aabb) -> f64 {
    if ray_hits(eye, dir, b) {
        return 0.0;
    }
    let angle_at = |t: f64| {
        let along = [
            eye[0] + dir[0] * t,
            eye[1] + dir[1] * t,
            eye[2] + dir[2] * t,
        ];
        angle_between(dir, sub(nearest(along, b), eye))
    };
    let (mut lo, mut hi) = (0.0f64, SEARCH_FAR);
    for _ in 0..SEARCH_STEPS {
        let a = lo + (hi - lo) / 3.0;
        let c = hi - (hi - lo) / 3.0;
        if angle_at(a) <= angle_at(c) {
            hi = c;
        } else {
            lo = a;
        }
    }
    let centre = [
        (b[0] + b[3]) / 2.0,
        (b[1] + b[4]) / 2.0,
        (b[2] + b[5]) / 2.0,
    ];
    angle_at((lo + hi) / 2.0).min(angle_between(dir, sub(centre, eye)))
}

fn yaw_step(from: f32, to: f32) -> f32 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

/// The rotations the client may have picked with, from the last accepted
/// one to the one sent with the tick.
fn rotations(from: [f32; 2], to: [f32; 2]) -> impl Iterator<Item = [f32; 2]> {
    let dyaw = yaw_step(from[0], to[0]);
    let dpitch = to[1] - from[1];
    (0..=ROTATION_STEPS).map(move |i| {
        let f = i as f32 / ROTATION_STEPS as f32;
        [from[0] + dyaw * f, from[1] + dpitch * f]
    })
}

struct Pending {
    target: AimTarget,
    rotation: [f32; 2],
    tick: u64,
}

impl Pending {
    fn miss(&self, now: [f32; 2]) -> f64 {
        let mut best = f64::INFINITY;
        for rotation in rotations(self.rotation, now) {
            let dir = look(rotation);
            for eye in &self.target.eyes {
                for b in &self.target.boxes {
                    best = best.min(miss_angle(*eye, dir, *b));
                    if best <= 0.0 {
                        return 0.0;
                    }
                }
            }
        }
        best
    }
}

/// The look ray against the attacked hitbox (`aim`, counted only) and
/// snapping between distant targets (`multi_aura`, cancelled).
#[derive(Default)]
pub struct AimCheck {
    pending: Option<Pending>,
    attacks: VecDeque<(u64, i32, [f64; 3])>,
    snaps: VecDeque<u64>,
    weak: f64,
}

impl AimCheck {
    fn judge(&mut self, rotation: [f32; 2]) -> Option<Flag> {
        let pending = self.pending.take()?;
        let miss = pending.miss(rotation);
        if miss > pending.target.settings.max_angle {
            return Some(Flag {
                check: "aim",
                weight: (miss / 30.0).clamp(0.5, 2.0),
                fix: Fix::Pass,
            });
        }
        self.weak = self.weak.max(crate::improbable::weak_weight(
            miss,
            pending.target.settings.max_angle,
            0.5,
        ));
        None
    }

    fn snap(&mut self, facts: &Facts, target: &AimTarget) -> Option<Flag> {
        let b = target.boxes[0];
        let centre = [
            (b[0] + b[3]) / 2.0,
            (b[1] + b[4]) / 2.0,
            (b[2] + b[5]) / 2.0,
        ];
        let dir = sub(centre, facts.eye);
        let tick = facts.tick;
        if let Some((at, previous, last)) = self.attacks.back().copied()
            && previous != target.entity
            && tick.saturating_sub(at) <= SNAP_TICKS
            && angle_between(last, dir) > target.settings.snap_angle
        {
            self.snaps.push_back(tick);
        }
        if self.attacks.len() >= KEPT_ATTACKS {
            self.attacks.pop_front();
        }
        self.attacks.push_back((tick, target.entity, dir));
        while self
            .snaps
            .front()
            .is_some_and(|at| tick.saturating_sub(*at) > SNAP_WINDOW)
        {
            self.snaps.pop_front();
        }
        let distinct = {
            let mut ids: Vec<i32> = self
                .attacks
                .iter()
                .filter(|(at, _, _)| tick.saturating_sub(*at) <= SNAP_WINDOW)
                .map(|(_, id, _)| *id)
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len()
        };
        if self.snaps.len() > target.settings.snaps as usize && distinct >= 3 {
            return Flag::cancel("multi_aura", 1.0);
        }
        None
    }
}

impl Check for AimCheck {
    fn weak(&mut self) -> f64 {
        std::mem::take(&mut self.weak)
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if facts.teleport_pending || facts.game_mode == GameMode::Spectator {
            self.pending = None;
            return None;
        }
        match packet {
            Serverbound::Attack(_) => {
                let earlier = self.judge(facts.rotation);
                let target = facts.aim.as_ref()?;
                if facts.latency_ms <= target.settings.max_latency_ms {
                    self.pending = Some(Pending {
                        target: target.clone(),
                        rotation: facts.rotation,
                        tick: facts.tick,
                    });
                }
                self.snap(facts, target).or(earlier)
            }
            Serverbound::ClientTickEnd => self.judge(facts.rotation),
            _ if self
                .pending
                .as_ref()
                .is_some_and(|p| facts.tick.saturating_sub(p.tick) > STALE_TICKS) =>
            {
                self.judge(facts.rotation)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn target(entity: i32, x: f64, z: f64) -> AimTarget {
        AimTarget {
            entity,
            boxes: vec![[x - 0.4, 64.0, z - 0.4, x + 0.4, 66.0, z + 0.4]],
            eyes: vec![[0.0, 65.62, 0.0]],
            settings: AimSettings::DEFAULT,
        }
    }

    fn attack(check: &mut AimCheck, f: &mut Facts, t: AimTarget) -> Option<Flag> {
        f.aim = Some(t);
        let flag = check.examine(f, &Serverbound::Attack(1));
        f.aim = None;
        flag
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_miss_angle_is_zero_on_the_box_and_grows_away() {
        let b = [-0.4, 64.0, 2.6, 0.4, 66.0, 3.4];
        let eye = [0.0, 65.62, 0.0];
        assert_eq!(miss_angle(eye, look([0.0, 0.0]), b), 0.0);
        let side = miss_angle(eye, look([45.0, 0.0]), b);
        assert!((30.0..45.0).contains(&side), "{side}");
        let behind = miss_angle(eye, look([180.0, 0.0]), b);
        assert!(behind > 160.0, "{behind}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dead_centre_look_is_not_weak() {
        let mut check = AimCheck::default();
        let mut f = facts();
        f.rotation = [0.0, 0.0];
        attack(&mut check, &mut f, target(1, 0.0, 3.0));
        check.examine(&f, &Serverbound::ClientTickEnd);
        assert_eq!(check.weak(), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn looking_at_the_target_passes_and_a_turned_back_flags_at_tick_end() {
        let mut check = AimCheck::default();
        let mut f = facts();
        f.rotation = [0.0, 0.0];
        assert_eq!(attack(&mut check, &mut f, target(1, 0.0, 3.0)), None);
        assert_eq!(check.examine(&f, &Serverbound::ClientTickEnd), None);

        f.rotation = [180.0, 0.0];
        assert_eq!(attack(&mut check, &mut f, target(1, 0.0, 3.0)), None);
        let flag = check.examine(&f, &Serverbound::ClientTickEnd).unwrap();
        assert_eq!(flag.check, "aim");
        assert_eq!(flag.fix, Fix::Pass);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_rotation_sent_after_the_attack_counts() {
        let mut check = AimCheck::default();
        let mut f = facts();
        f.rotation = [90.0, 0.0];
        attack(&mut check, &mut f, target(1, 0.0, 3.0));
        f.rotation = [0.0, 0.0];
        assert_eq!(check.examine(&f, &Serverbound::ClientTickEnd), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn snapping_between_targets_around_the_player_is_a_multi_aura() {
        let mut check = AimCheck::default();
        let mut f = facts();
        let spots: [(f64, f64); 4] = [(0.0, 3.0), (3.0, 0.0), (0.0, -3.0), (-3.0, 0.0)];
        let mut flagged = false;
        for round in 0..3u64 {
            for (i, (x, z)) in spots.iter().enumerate() {
                f.tick = round * 4 + i as u64;
                let flag = attack(&mut check, &mut f, target(i as i32 + 1, *x, *z));
                flagged |= flag.is_some_and(|flag| flag.check == "multi_aura");
            }
        }
        assert!(flagged);
    }

    #[tuff_core::watchdog]
    #[test]
    fn switching_targets_at_a_human_pace_is_fine() {
        let mut check = AimCheck::default();
        let mut f = facts();
        let spots: [(f64, f64); 4] = [(0.0, 3.0), (3.0, 0.0), (0.0, -3.0), (-3.0, 0.0)];
        for round in 0..5u64 {
            for (i, (x, z)) in spots.iter().enumerate() {
                f.tick = (round * 4 + i as u64) * 12;
                f.rotation = [(-x).atan2(*z).to_degrees() as f32, 0.0];
                let flag = attack(&mut check, &mut f, target(i as i32 + 1, *x, *z));
                assert_eq!(flag, None);
            }
        }
        let mut check = AimCheck::default();
        f.rotation = [0.0, 0.0];
        for n in 0..20u64 {
            f.tick = n;
            let (x, z) = if n % 2 == 0 { (0.3, 3.0) } else { (-0.3, 3.0) };
            assert_eq!(
                attack(&mut check, &mut f, target((n % 2) as i32 + 1, x, z)),
                None
            );
        }
    }
}

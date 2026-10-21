//! When a region resends an entity's movement to the players tracking it,
//! and in what form. The game paces this per entity type: clients simulate
//! falling items and TNT themselves, so movement goes out only every few
//! ticks, on a periodic forced resync, or right after something shoved the
//! entity. Positions go out as short deltas from the last position sent,
//! falling back to an absolute sync when a delta cannot carry the move.

use tuff_vanilla_data::entity::{FORCED_POSITION_PERIOD, POSITION_TOLERANCE, Tracking};
use tuff_vanilla_data::registry::EntityType;

use crate::protocol::play::{PositionStep, TrackStep};
use crate::world::distance_sq;

const VELOCITY_TOLERANCE: f64 = 1.0e-7;

/// Due updates after which the position goes out absolute again, so
/// rounding in deltas cannot drift for long.
const ABSOLUTE_PERIOD: u32 = 400;

/// Steps per block of a position delta.
const DELTA_STEPS: f64 = 4096.0;

/// What a movement update is computed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    /// A passenger: clients place it on its vehicle themselves.
    pub riding: bool,
}

/// How a movement update carries the position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    #[default]
    Unchanged,
    /// Steps of 1/4096 block from the last position sent.
    Delta([i16; 3]),
    Absolute,
    /// The steps it took along a track (a minecart in a world with the
    /// minecart improvements feature), position and velocity included.
    Track,
}

/// What changed since the last update, as far as viewers are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Movement {
    pub placement: Placement,
    /// The new packed yaw and pitch.
    pub rotation: Option<[i8; 2]>,
    pub velocity: bool,
}

impl Movement {
    pub fn is_empty(&self) -> bool {
        self.placement == Placement::Unchanged && self.rotation.is_none() && !self.velocity
    }
}

/// The stops a living entity made when its position was flagged for sync
/// (a bounce), reported to viewers as one stepped path.
#[derive(Debug, Clone)]
struct SteppedTracker {
    steps: Vec<PositionStep>,
    ticks_since_step: i32,
}

impl SteppedTracker {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            ticks_since_step: -1,
        }
    }

    fn add_step(&mut self, position: [f64; 3]) {
        if self.ticks_since_step > 0 {
            self.steps.push(PositionStep {
                position,
                tick_offset: self.ticks_since_step,
            });
            self.ticks_since_step = 0;
        }
    }

    fn update(&mut self, position: [f64; 3], flagged: bool) {
        self.ticks_since_step += 1;
        if flagged {
            self.add_step(position);
        }
    }

    fn take_path(&mut self, current: [f64; 3]) -> Vec<PositionStep> {
        self.add_step(current);
        self.ticks_since_step = 0;
        std::mem::take(&mut self.steps)
    }
}

#[derive(Debug, Clone)]
pub struct MovementSync {
    stepped: SteppedTracker,
    path: Vec<PositionStep>,
    tracking: Tracking,
    age: u64,
    base: [f64; 3],
    last_velocity: [f64; 3],
    last_rotation: [i8; 2],
    on_ground: bool,
    was_riding: bool,
    since_absolute: u32,
    impulse: bool,
    due: bool,
}

/// An angle as the protocol packs it into a byte.
pub fn pack_degrees(degrees: f32) -> i8 {
    (degrees * 256.0 / 360.0).floor() as i32 as i8
}

fn steps(coordinate: f64) -> i64 {
    (coordinate * DELTA_STEPS + 0.5).floor() as i64
}

/// The delta that moves a client from `base` to `position`, if it fits.
pub fn encode_delta(base: [f64; 3], position: [f64; 3]) -> Option<[i16; 3]> {
    let mut delta = [0; 3];
    for axis in 0..3 {
        delta[axis] = i16::try_from(steps(position[axis]) - steps(base[axis])).ok()?;
    }
    Some(delta)
}

/// Where a client that knew `base` puts the entity after `delta`.
pub fn decode_delta(base: [f64; 3], delta: [i16; 3]) -> [f64; 3] {
    let mut position = base;
    for axis in 0..3 {
        if delta[axis] != 0 {
            position[axis] = (steps(base[axis]) + i64::from(delta[axis])) as f64 / DELTA_STEPS;
        }
    }
    position
}

impl MovementSync {
    /// Starts from the state the entity was spawned with, which the spawn
    /// packets already told clients.
    pub fn new(kind: EntityType, pose: &Pose) -> Self {
        let mut sync = Self {
            stepped: SteppedTracker::new(),
            path: Vec::new(),
            tracking: kind.tracking(),
            age: 0,
            base: pose.position,
            last_velocity: pose.velocity,
            last_rotation: [0; 2],
            on_ground: pose.on_ground,
            was_riding: pose.riding,
            since_absolute: 0,
            impulse: false,
            due: false,
        };
        sync.resync(pose);
        sync
    }

    /// Every viewer was just sent the entity's full state.
    pub fn resync(&mut self, pose: &Pose) {
        self.base = pose.position;
        self.last_rotation = [pack_degrees(pose.yaw), pack_degrees(pose.pitch)];
        self.on_ground = pose.on_ground;
        self.was_riding = pose.riding;
        self.since_absolute = 0;
        if self.tracking.track_deltas {
            self.last_velocity = pose.velocity;
        }
    }

    /// The position viewers were last sent, which deltas start from.
    pub fn base(&self) -> [f64; 3] {
        self.base
    }

    /// Something pushed the entity; its movement goes out on the next tick.
    pub fn impulse(&mut self) {
        self.impulse = true;
    }

    /// Notes the entity's position for this tick before [`Self::tick`]; a
    /// `flagged` position (it bounced) is recorded as a stop on the path
    /// and goes out on this tick.
    pub fn track_position(&mut self, position: [f64; 3], flagged: bool) {
        self.stepped.update(position, flagged);
        if flagged {
            self.impulse = true;
        }
    }

    /// The stepped path of the last due tick, empty when the entity made no
    /// recorded stops (a plain sync then goes straight to its position).
    pub fn take_path(&mut self) -> Vec<PositionStep> {
        std::mem::take(&mut self.path)
    }

    /// Whether the last tick was one viewers are due updates on.
    pub fn was_due(&self) -> bool {
        self.due
    }

    pub fn has_impulse(&self) -> bool {
        self.impulse
    }

    /// Advances one tick after the entity moved and says what of its
    /// movement viewers are due, if anything.
    pub fn tick(&mut self, pose: &Pose) -> Option<Movement> {
        let age = self.age;
        self.age += 1;
        let forced = std::mem::take(&mut self.impulse);
        self.due = forced || self.tracking.is_due(age);
        if !self.due {
            return None;
        }
        self.path = self.stepped.take_path(pose.position);
        let change = distance_sq(pose.velocity, self.last_velocity);
        let velocity = (self.tracking.track_deltas || forced)
            && (change > VELOCITY_TOLERANCE
                || (change > 0.0 && distance_sq(pose.velocity, [0.0; 3]) == 0.0));
        if velocity {
            self.last_velocity = pose.velocity;
        }
        let rotation = [pack_degrees(pose.yaw), pack_degrees(pose.pitch)];
        let turned = rotation != self.last_rotation;
        let placement = if pose.riding {
            self.base = pose.position;
            self.was_riding = true;
            Placement::Unchanged
        } else {
            self.since_absolute += 1;
            let placement = if self.since_absolute > ABSOLUTE_PERIOD
                || self.was_riding
                || self.on_ground != pose.on_ground
            {
                Placement::Absolute
            } else if distance_sq(pose.position, self.base) >= POSITION_TOLERANCE
                || age.is_multiple_of(u64::from(FORCED_POSITION_PERIOD))
            {
                encode_delta(self.base, pose.position).map_or(Placement::Absolute, Placement::Delta)
            } else {
                Placement::Unchanged
            };
            self.was_riding = false;
            if placement == Placement::Absolute {
                self.since_absolute = 0;
                self.on_ground = pose.on_ground;
            }
            if placement != Placement::Unchanged {
                self.base = pose.position;
            }
            placement
        };
        let rotation = (turned || placement == Placement::Absolute).then_some(rotation);
        if let Some(rotation) = rotation {
            self.last_rotation = rotation;
        }
        let movement = Movement {
            placement,
            rotation,
            velocity,
        };
        (!movement.is_empty()).then_some(movement)
    }
}

impl MovementSync {
    /// [`Self::tick`] for an entity that reports its movement as track
    /// steps: when an update is due, `steps` go out (or, with none since the
    /// last one, its current state once anything changed or every so often)
    /// and are drained into `sent`.
    pub fn tick_track(
        &mut self,
        pose: &Pose,
        steps: &mut Vec<TrackStep>,
        sent: &mut Vec<TrackStep>,
    ) -> Option<Movement> {
        let age = self.age;
        self.age += 1;
        let forced = std::mem::take(&mut self.impulse);
        self.due = forced || self.tracking.is_due(age);
        if !self.due {
            return None;
        }
        let rotation = [pack_degrees(pose.yaw), pack_degrees(pose.pitch)];
        sent.clear();
        if steps.is_empty() {
            let moved = distance_sq(pose.position, self.base) >= POSITION_TOLERANCE
                || age.is_multiple_of(u64::from(FORCED_POSITION_PERIOD));
            let turned = rotation != self.last_rotation;
            let pushed = distance_sq(pose.velocity, self.last_velocity) > VELOCITY_TOLERANCE;
            if moved || turned || pushed {
                sent.push(TrackStep {
                    position: pose.position,
                    velocity: pose.velocity,
                    yaw: pose.yaw,
                    pitch: pose.pitch,
                    weight: 1.0,
                });
            }
        } else {
            sent.append(steps);
        }
        self.last_rotation = rotation;
        self.base = pose.position;
        self.was_riding = false;
        (!sent.is_empty()).then_some(Movement {
            placement: Placement::Track,
            rotation: None,
            velocity: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(position: [f64; 3], velocity: [f64; 3]) -> Pose {
        Pose {
            position,
            velocity,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            riding: false,
        }
    }

    fn now(sync: &mut MovementSync, pose: &Pose) -> Option<Movement> {
        sync.impulse();
        sync.tick(pose)
    }

    fn due_ticks(sync: &mut MovementSync, ticks: u64, step: [f64; 3]) -> Vec<u64> {
        let mut position = [0.0; 3];
        (0..ticks)
            .filter(|_| {
                position = [0, 1, 2].map(|axis| position[axis] + step[axis]);
                sync.tick(&pose(position, step)).is_some()
            })
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cow_is_due_every_third_tick_only() {
        let mut sync = MovementSync::new(EntityType::Cow, &pose([0.0; 3], [0.0; 3]));
        let due: Vec<bool> = (0..7)
            .map(|_| {
                sync.tick(&pose([0.0; 3], [0.0; 3]));
                sync.was_due()
            })
            .collect();
        assert_eq!(due, [true, false, false, true, false, false, true]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn moving_tnt_is_resent_on_its_interval_only() {
        let mut sync = MovementSync::new(EntityType::Tnt, &pose([0.0; 3], [0.0, -0.04, 0.0]));
        assert_eq!(
            due_ticks(&mut sync, 45, [0.0, -0.04, 0.0]),
            vec![0, 10, 20, 30, 40]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn resting_entities_only_get_the_forced_resync() {
        let mut sync = MovementSync::new(EntityType::Item, &pose([0.0; 3], [0.0; 3]));
        assert_eq!(due_ticks(&mut sync, 130, [0.0; 3]), vec![0, 60, 120]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_impulse_sends_the_new_velocity_at_once() {
        let mut sync = MovementSync::new(EntityType::Item, &pose([0.0; 3], [0.0; 3]));
        assert!(sync.tick(&pose([0.0; 3], [0.0; 3])).is_some());
        assert!(sync.tick(&pose([0.0; 3], [0.0; 3])).is_none());
        sync.impulse();
        let movement = sync.tick(&pose([0.0; 3], [0.5, 0.3, 0.0])).unwrap();
        assert!(movement.velocity);
        assert_eq!(movement.placement, Placement::Unchanged);
        assert!(sync.tick(&pose([0.0; 3], [0.5, 0.3, 0.0])).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn walking_goes_out_as_deltas_and_velocity_only_when_it_changes() {
        let start = [10.5, 64.0, -3.25];
        let mut sync = MovementSync::new(EntityType::Zombie, &pose(start, [0.0; 3]));
        let step = [0.1, 0.0, -0.05];
        let mut position = start;
        let mut client = start;
        let mut velocity_updates = 0;
        for tick in 0..300 {
            position = [0, 1, 2].map(|axis| position[axis] + step[axis]);
            let Some(movement) = sync.tick(&pose(position, step)) else {
                continue;
            };
            velocity_updates += usize::from(movement.velocity);
            match movement.placement {
                Placement::Delta(delta) => client = decode_delta(client, delta),
                other => panic!("tick {tick}: {other:?}"),
            }
            assert!(distance_sq(client, position) < 1.0e-6, "tick {tick}");
        }
        assert_eq!(velocity_updates, 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_long_jump_or_landing_goes_out_absolute() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        let far = now(&mut sync, &pose([9.0, 0.0, 0.0], [0.0; 3])).unwrap();
        assert_eq!(far.placement, Placement::Absolute);
        assert_eq!(far.rotation, Some([0, 0]));
        let mut landed = pose([9.1, 0.0, 0.0], [0.0; 3]);
        landed.on_ground = true;
        assert_eq!(
            now(&mut sync, &landed).unwrap().placement,
            Placement::Absolute
        );
        landed.position[0] = 9.2;
        assert_eq!(
            now(&mut sync, &landed).unwrap().placement,
            Placement::Delta([409, 0, 0])
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn turning_in_place_sends_rotation_alone() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        assert!(now(&mut sync, &pose([0.0; 3], [0.0; 3])).is_some());
        let mut turned = pose([0.0; 3], [0.0; 3]);
        turned.yaw = 90.0;
        assert_eq!(
            now(&mut sync, &turned),
            Some(Movement {
                placement: Placement::Unchanged,
                rotation: Some([64, 0]),
                velocity: false,
            })
        );
        assert_eq!(now(&mut sync, &turned), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn passengers_send_no_position_and_resync_absolute_after_dismounting() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        let mut seated = pose([1.0, 0.0, 0.0], [0.0; 3]);
        seated.riding = true;
        assert_eq!(now(&mut sync, &seated), None);
        seated.yaw = 45.0;
        let movement = now(&mut sync, &seated).unwrap();
        assert_eq!(movement.placement, Placement::Unchanged);
        assert_eq!(movement.rotation, Some([32, 0]));
        seated.riding = false;
        assert_eq!(
            now(&mut sync, &seated).unwrap().placement,
            Placement::Absolute
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn deltas_give_way_to_a_periodic_absolute_sync() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        let mut position = [0.0; 3];
        let absolute: Vec<u64> = (0..1000)
            .filter(|_| {
                position[0] += 0.01;
                now(&mut sync, &pose(position, [0.0; 3]))
                    .is_some_and(|m| m.placement == Placement::Absolute)
            })
            .collect();
        assert_eq!(absolute, vec![400, 801]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn deltas_round_like_the_client() {
        assert_eq!(encode_delta([0.0; 3], [-0.5 / 4096.0, 8.0, -8.0]), None);
        assert_eq!(
            encode_delta([0.0; 3], [-0.5 / 4096.0, 7.9, -7.9]),
            Some([0, 32358, -32358])
        );
        assert_eq!(
            decode_delta([1.0, 2.0, 3.0], [4096, 0, -2048]),
            [2.0, 2.0, 2.5]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flagged_position_goes_out_as_a_stepped_path() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        sync.track_position([0.0; 3], false);
        sync.tick(&pose([0.0; 3], [0.0; 3]));
        sync.track_position([0.0, 1.0, 0.0], true);
        sync.tick(&pose([0.0, 1.0, 0.0], [0.0; 3]));
        let path = sync.take_path();
        assert_eq!(
            path,
            vec![PositionStep {
                position: [0.0, 1.0, 0.0],
                tick_offset: 1
            }]
        );
        assert!(sync.take_path().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unflagged_tick_has_no_path() {
        let mut sync = MovementSync::new(EntityType::Zombie, &pose([0.0; 3], [0.0; 3]));
        sync.track_position([0.0; 3], false);
        sync.tick(&pose([0.0; 3], [0.0; 3]));
        sync.track_position([1.0; 3], false);
        sync.tick(&pose([1.0; 3], [0.0; 3]));
        assert_eq!(sync.take_path(), Vec::new());
    }
}

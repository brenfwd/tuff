//! Driven vehicles: how far one Move Vehicle may go for the vehicle's
//! type and surface, running through blocks, how many arrive per second,
//! and Move Vehicle from a rider the game does not let steer.

use std::time::Instant;

use tuff_core::protocol::play::Serverbound;

use super::budget::{BudgetLimits, PacketBudget, TickClock};
use super::vehicle::{Vehicle, given_window};
use super::{Check, Facts, Fix, Flag};

const SPEED: &str = "vehicle_speed";
const PHASE: &str = "vehicle_phase";
const TIMER: &str = "vehicle_timer";
const CONTROL: &str = "vehicle_control";

/// Forward paddling adds this much speed a tick.
const PADDLE: f64 = 0.04;
const BOAT_SLACK: f64 = 0.01;
/// A water current pushes a floating boat this much a tick.
const FLOW: f64 = 0.015;
/// Above a boat's top speed, about 3.64 blocks a tick on blue ice.
const BOAT_CEILING: f64 = 4.0;
/// A mount's top ground speed is its movement speed times about 2.2
/// (2.23 on blue ice).
const GROUND: f64 = 2.3;
/// The most one move can cover without a leap: landing from the air,
/// where the air's drag still carries most of a fast ground speed while
/// the ground adds its full push.
const PEAK: f64 = 3.4;
/// Air keeps this much of a mount's horizontal speed a tick, so a leap's
/// push adds about this many times itself over the moves that follow.
const LEAP_CARRY: f64 = 1.0 / (1.0 - 0.91);
const MAX_LEAPS: f64 = 2.0;
const MOUNT_SLACK: f64 = 0.1;
/// Blocks a mount may run ahead of its ground speed in total, beyond
/// what its leaps explain.
const SURPLUS_LIMIT: f64 = 1.0;
const JUMP_WINDOW: u64 = 20;
const CREDIT_WINDOW: u64 = 60;
const HUGE: f64 = 10.0;
/// Moves after mounting or after the server moved the vehicle, when the
/// client's copy may still be catching up.
const SETTLE_MOVES: u32 = 10;
const MAX_LAG: f64 = 4.0;
const LAG_WINDOW: u64 = 20;
const CONTROL_GRACE: u64 = 20;
const CONTROL_INTERVAL: u64 = 20;

fn put_back(vehicle: &Vehicle, check: &'static str, weight: f64, rotation: (f32, f32)) -> Flag {
    Flag {
        check,
        weight,
        fix: Fix::MoveVehicle {
            position: vehicle.from,
            yaw: rotation.0,
            pitch: rotation.1,
        },
    }
}

fn driven<'a>(
    facts: &'a Facts,
    packet: &Serverbound,
) -> Option<(&'a Vehicle, [f64; 3], (f32, f32))> {
    let Serverbound::MoveVehicle {
        position,
        yaw,
        pitch,
        ..
    } = packet
    else {
        return None;
    };
    let vehicle = facts.vehicle.as_ref().filter(|vehicle| vehicle.controls)?;
    Some((vehicle, *position, (*yaw, *pitch)))
}

fn excess_weight(distance: f64, limit: f64) -> f64 {
    1.0 + ((distance - limit) / limit.max(0.1)).min(4.0)
}

/// Whether moves still continue the same ride: the same vehicle, from
/// where the last move left it or was put back. Anything else (a new
/// vehicle, a teleport, the server moving it) starts a new ride whose
/// first moves only get a ceiling.
#[derive(Default)]
struct Ride {
    vehicle: Option<i32>,
    moves: u32,
    from: Option<[f64; 3]>,
    to: Option<[f64; 3]>,
}

impl Ride {
    /// Notes this move; true when it starts a new ride.
    fn track(&mut self, facts: &Facts, vehicle: &Vehicle, to: [f64; 3]) -> bool {
        let same = self.vehicle == Some(vehicle.id)
            && !facts.teleport_pending
            && (self.from == Some(vehicle.from) || self.to == Some(vehicle.from));
        if !same {
            *self = Ride {
                vehicle: Some(vehicle.id),
                ..Ride::default()
            };
        }
        self.moves = self.moves.saturating_add(1);
        self.from = Some(vehicle.from);
        self.to = Some(to);
        !same
    }

    fn settling(&self) -> bool {
        self.moves <= SETTLE_MOVES
    }
}

/// Horizontal distance per Move Vehicle. A boat keeps at most its
/// surface's share of the last move's speed and paddles at most 0.04 on
/// top (top speed about 0.4 on water, 2 on ice, 3.6 on blue ice); the
/// tick it drops from the air onto water skips the drag. A mount covers
/// at most its ground speed, beyond which only a jump or dash the rider
/// announced may carry it.
#[derive(Default)]
pub struct VehicleSpeed {
    ride: Ride,
    speed: Option<f64>,
    was_wet: Option<bool>,
    given: Option<f64>,
    given_at: Option<u64>,
    surplus: f64,
    credit: f64,
    leap: f64,
    jumped_at: Option<u64>,
}

impl VehicleSpeed {
    fn restart(&mut self) {
        self.speed = None;
        self.was_wet = None;
        self.given = None;
        self.given_at = None;
        self.surplus = 0.0;
    }

    fn note_given(&mut self, launch: Option<(u64, [f64; 3])>) {
        match launch {
            Some((at, velocity)) if self.given_at != Some(at) => {
                let speed = velocity[0].hypot(velocity[2]);
                self.given_at = Some(at);
                self.given = Some(self.given.map_or(speed, |given| given.max(speed)));
            }
            Some(_) => {}
            None => self.given = None,
        }
    }

    fn jumped(&self, facts: &Facts, window: u64) -> bool {
        self.jumped_at.is_some_and(|at| {
            facts.tick.saturating_sub(at) <= given_window(window, facts.latency_ms)
        })
    }

    fn boat(&mut self, vehicle: &Vehicle, distance: f64) -> Option<f64> {
        self.note_given(vehicle.launch);
        let Some(friction) = vehicle.friction else {
            self.speed = Some(distance);
            self.was_wet = None;
            return None;
        };
        let landed = vehicle.wet && self.was_wet == Some(false);
        self.was_wet = Some(vehicle.wet);
        let keep = if landed { 1.0 } else { friction };
        let slack = BOAT_SLACK + if vehicle.wet { FLOW } else { 0.0 };
        let limit = match self.speed {
            Some(speed) if !self.ride.settling() => speed.mul_add(keep, PADDLE) + slack,
            _ => BOAT_CEILING,
        };
        if distance <= limit {
            self.speed = Some(distance);
            return None;
        }
        if self
            .given
            .is_some_and(|given| distance <= given + PADDLE + slack)
        {
            self.given = None;
            self.speed = Some(distance);
            return None;
        }
        self.speed = Some(limit.min(BOAT_CEILING));
        Some(limit)
    }

    fn mount(&mut self, facts: &Facts, vehicle: &Vehicle, distance: f64) -> Option<f64> {
        let ridden = vehicle.ridden?;
        let speed = ridden.speed.max(0.0);
        self.leap = ridden.leap.max(0.0);
        if !self.jumped(facts, CREDIT_WINDOW) {
            self.credit = 0.0;
        }
        let leaping = self.ride.settling() || self.jumped(facts, JUMP_WINDOW);
        let cap = speed.mul_add(PEAK, MOUNT_SLACK) + if leaping { self.leap } else { 0.0 };
        if distance > cap {
            return Some(cap);
        }
        if self.ride.settling() {
            return None;
        }
        let mut surplus = distance - speed.mul_add(GROUND, MOUNT_SLACK / 2.0);
        if surplus > 0.0 {
            let used = surplus.min(self.credit);
            self.credit -= used;
            surplus -= used;
        }
        self.surplus = (self.surplus + surplus).max(0.0);
        if self.surplus > SURPLUS_LIMIT {
            self.surplus = SURPLUS_LIMIT;
            return Some(speed * GROUND);
        }
        None
    }
}

impl Check for VehicleSpeed {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if let Serverbound::RidingJump(_) = packet {
            if self.leap > 0.0 {
                let push = self.leap * LEAP_CARRY;
                self.credit = (self.credit + push).min(push * MAX_LEAPS);
                self.jumped_at = Some(facts.tick);
            }
            return None;
        }
        let (vehicle, to, rotation) = driven(facts, packet)?;
        if self.ride.track(facts, vehicle, to) {
            self.restart();
        }
        let distance = (to[0] - vehicle.from[0]).hypot(to[2] - vehicle.from[2]);
        if distance > HUGE {
            return Some(put_back(vehicle, SPEED, 2.0, rotation));
        }
        if self.ride.moves == 1 || vehicle.pushed {
            self.speed = Some(distance);
            return None;
        }
        let limit = if vehicle.boat {
            self.boat(vehicle, distance)
        } else {
            self.mount(facts, vehicle, distance)
        }?;
        Some(put_back(
            vehicle,
            SPEED,
            excess_weight(distance, limit),
            rotation,
        ))
    }
}

/// A driven vehicle may not move through blocks: the core of its box,
/// above what it can step over, must slide from the old position to the
/// new one without meeting a block it was not already inside.
#[derive(Default)]
pub struct VehiclePhase {
    ride: Ride,
}

impl Check for VehiclePhase {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let (vehicle, to, rotation) = driven(facts, packet)?;
        let fresh = self.ride.track(facts, vehicle, to);
        (vehicle.clipped && !fresh && !vehicle.pushed)
            .then(|| put_back(vehicle, PHASE, 1.0, rotation))
    }
}

/// The driver's client sends one Move Vehicle per client tick, at most
/// 20 a second, so the shared packet budget limits them like player
/// moves. Time is the wall clock or the session's ticks, whichever ran
/// further, so a server ticking faster than real time never flags.
pub struct VehicleTimer {
    budget: PacketBudget,
    clock: TickClock,
    window: Option<(u64, Instant)>,
    lag: f64,
}

impl Default for VehicleTimer {
    fn default() -> Self {
        Self {
            budget: PacketBudget::new(BudgetLimits::MOVEMENT),
            clock: TickClock::default(),
            window: None,
            lag: 1.0,
        }
    }
}

impl VehicleTimer {
    fn measure_lag(&mut self, facts: &Facts) {
        let Some((tick, at)) = self.window else {
            self.window = Some((facts.tick, facts.now));
            return;
        };
        if facts.tick.saturating_sub(tick) < LAG_WINDOW {
            return;
        }
        let wall = facts.now.saturating_duration_since(at).as_secs_f64();
        let per_tick = wall / (facts.tick - tick) as f64;
        let expected = 1.0 / f64::from(facts.tick_rate.clamp(0.1, 20.0));
        self.lag = (per_tick / expected).clamp(1.0, MAX_LAG);
        self.window = Some((facts.tick, facts.now));
    }
}

impl Check for VehicleTimer {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let (vehicle, _, rotation) = driven(facts, packet)?;
        if facts.teleport_pending {
            return None;
        }
        self.measure_lag(facts);
        let now = self.clock.now(facts.now, facts.tick);
        self.budget
            .record_scaled(now, self.lag)
            .map(|_| put_back(vehicle, TIMER, 1.0, rotation))
    }
}

/// Move Vehicle from a rider the game does not let steer (a passenger,
/// a minecart rider, a pig rider without its lure) changes nothing on
/// the server; its client is put back where the server shows the
/// vehicle, at most once a second and not right after the riders
/// changed. One reset a second never outgrows the ladder's decay.
#[derive(Default)]
pub struct VehicleControl {
    state: Option<(i32, bool)>,
    changed_at: u64,
    reset_at: Option<u64>,
}

impl Check for VehicleControl {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::MoveVehicle { yaw, pitch, .. } = packet else {
            return None;
        };
        let Some(vehicle) = facts.vehicle else {
            self.state = None;
            return None;
        };
        if self.state != Some((vehicle.id, vehicle.controls)) {
            self.state = Some((vehicle.id, vehicle.controls));
            self.changed_at = facts.tick;
        }
        if vehicle.controls
            || facts.tick.saturating_sub(self.changed_at)
                <= given_window(CONTROL_GRACE, facts.latency_ms)
            || self
                .reset_at
                .is_some_and(|at| facts.tick.saturating_sub(at) < CONTROL_INTERVAL)
        {
            return None;
        }
        self.reset_at = Some(facts.tick);
        Some(Flag {
            check: CONTROL,
            weight: 1.0,
            fix: Fix::ResetVehicle {
                position: vehicle.shown,
                yaw: *yaw,
                pitch: *pitch,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_core::world::entity::RiddenSpeed;
    use tuff_vanilla_data::registry::EntityType;

    const TICK: std::time::Duration = std::time::Duration::from_millis(50);
    const WATER: f64 = 0.9;
    const ICE: f64 = 0.98;
    const BLUE_ICE: f64 = 0.989;
    const LAND: f64 = 0.6;
    const HORSE: RiddenSpeed = RiddenSpeed {
        speed: 0.3375,
        leap: 0.4,
        launch: 0.0,
        levitation: 0,
        slow_falling: false,
    };

    /// Drives one vehicle east, one Move Vehicle per tick, through the
    /// checks under test.
    struct Driver {
        checks: Vec<Box<dyn Check>>,
        facts: Facts,
        start: Instant,
        flags: Vec<(usize, &'static str)>,
        moves: usize,
    }

    impl Driver {
        fn new(vehicle: Vehicle) -> Driver {
            let mut facts = facts();
            facts.riding = true;
            facts.vehicle = Some(vehicle);
            Driver {
                checks: vec![
                    Box::new(VehicleSpeed::default()),
                    Box::new(VehiclePhase::default()),
                    Box::new(VehicleTimer::default()),
                    Box::new(VehicleControl::default()),
                ],
                start: facts.now,
                facts,
                flags: Vec::new(),
                moves: 0,
            }
        }

        fn boat(friction: f64) -> Driver {
            let mut vehicle = Vehicle::boat(7, [0.0, 64.0, 0.0]);
            vehicle.friction = Some(friction);
            vehicle.wet = friction == WATER;
            Driver::new(vehicle)
        }

        fn horse(ridden: RiddenSpeed) -> Driver {
            Driver::new(Vehicle {
                kind: EntityType::Horse,
                boat: false,
                ridden: Some(ridden),
                friction: None,
                ..Vehicle::boat(9, [0.0, 64.0, 0.0])
            })
        }

        fn vehicle(&mut self) -> &mut Vehicle {
            self.facts.vehicle.as_mut().expect("vehicle")
        }

        fn send(&mut self, packet: &Serverbound) -> Vec<Flag> {
            let flags: Vec<Flag> = self
                .checks
                .iter_mut()
                .filter_map(|check| check.examine(&self.facts, packet))
                .collect();
            for flag in &flags {
                self.flags.push((self.moves, flag.check));
            }
            flags
        }

        /// One tick: moves `dx` east (and `dy` up); a refused move leaves
        /// the vehicle where it was.
        fn drive(&mut self, dx: f64) -> Vec<Flag> {
            self.facts.tick += 1;
            self.facts.now = self.start + TICK * u32::try_from(self.facts.tick).unwrap();
            let from = self.vehicle().from;
            let to = [from[0] + dx, from[1], from[2]];
            let flags = self.send(&Serverbound::MoveVehicle {
                position: to,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            });
            if flags.is_empty() {
                self.vehicle().from = to;
            }
            self.moves += 1;
            flags
        }

        fn jump(&mut self) {
            self.send(&Serverbound::RidingJump(100));
        }

        fn speed_flags(&self) -> Vec<usize> {
            self.flags
                .iter()
                .filter(|(_, check)| *check == SPEED)
                .map(|(n, _)| *n)
                .collect()
        }
    }

    /// A boat paddled forward for `ticks` from rest on one surface: each
    /// tick keeps `friction` of its speed and adds 0.04.
    fn paddle(driver: &mut Driver, friction: f64, from: f64, ticks: usize) -> f64 {
        let mut v = from;
        for _ in 0..ticks {
            v = v.mul_add(friction, PADDLE);
            driver.drive(v);
        }
        v
    }

    #[tuff_core::watchdog]
    #[test]
    fn boats_paddled_to_top_speed_on_water_ice_and_blue_ice_are_never_flagged() {
        for (surface, top) in [(WATER, 0.4), (ICE, 2.0), (BLUE_ICE, 3.6), (LAND, 0.1)] {
            let mut driver = Driver::boat(surface);
            let v = paddle(&mut driver, surface, 0.0, 1200);
            assert!((v - top).abs() < 0.05, "{surface}: {v}");
            assert!(driver.flags.is_empty(), "{surface}: {:?}", driver.flags);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fast_ice_boat_coasting_onto_water_land_and_through_the_air_is_not_flagged() {
        let mut driver = Driver::boat(BLUE_ICE);
        let v = paddle(&mut driver, BLUE_ICE, 0.0, 800);
        driver.vehicle().friction = Some(WATER);
        driver.vehicle().wet = false;
        let v = paddle(&mut driver, WATER, v, 5);
        driver.vehicle().wet = true;
        let v = paddle(&mut driver, 1.0, v, 1);
        let v = paddle(&mut driver, WATER, v, 40);
        driver.vehicle().wet = false;
        driver.vehicle().friction = Some(LAND);
        paddle(&mut driver, LAND, v, 40);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_dropping_into_water_keeps_its_speed_for_one_tick_only() {
        let mut driver = Driver::boat(WATER);
        driver.vehicle().wet = false;
        let v = paddle(&mut driver, WATER, 0.0, 200);
        driver.vehicle().wet = true;
        driver.drive(v + PADDLE);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        driver.drive(v + 2.0 * PADDLE);
        assert_eq!(driver.speed_flags().len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_in_a_current_may_beat_still_water_top_speed() {
        let mut driver = Driver::boat(WATER);
        let mut v = 0.0f64;
        for _ in 0..400 {
            v = v.mul_add(WATER, PADDLE + 0.014);
            driver.drive(v);
        }
        assert!(v > 0.5, "{v}");
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sped_up_boat_is_flagged_on_every_surface() {
        for (surface, top) in [(WATER, 0.4), (ICE, 2.0), (BLUE_ICE, 3.64), (LAND, 0.1)] {
            let mut driver = Driver::boat(surface);
            paddle(&mut driver, surface, 0.0, 1200);
            for _ in 0..40 {
                driver.drive(top * 1.5 + 0.05);
            }
            assert!(
                driver.speed_flags().len() >= 30,
                "{surface}: {:?}",
                driver.flags
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blast_may_throw_a_boat_once() {
        let mut driver = Driver::boat(WATER);
        let v = paddle(&mut driver, WATER, 0.0, 100);
        driver.vehicle().launch = Some((driver.facts.tick, [1.5, 0.5, 0.0]));
        let mut thrown = 1.5f64;
        for _ in 0..30 {
            driver.drive(thrown);
            thrown = thrown.mul_add(WATER, PADDLE);
        }
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        driver.drive(v + 1.0);
        assert_eq!(driver.speed_flags().len(), 1, "the throw is used up");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_first_moves_of_a_ride_only_have_a_ceiling() {
        let mut driver = Driver::boat(WATER);
        driver.drive(8.0);
        driver.drive(3.5);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        driver.drive(4.5);
        assert_eq!(driver.speed_flags().len(), 1);
        for _ in 0..8 {
            driver.drive(0.3);
        }
        driver.drive(0.8);
        assert_eq!(driver.speed_flags().len(), 2, "settled: water limits again");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_server_move_or_a_new_vehicle_starts_a_new_ride() {
        let mut driver = Driver::boat(WATER);
        paddle(&mut driver, WATER, 0.0, 100);
        driver.vehicle().from[0] += 20.0;
        driver.drive(1.5);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        driver.vehicle().id = 8;
        driver.drive(2.0);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        driver.drive(11.0);
        assert_eq!(driver.speed_flags().len(), 1, "too far even to settle");
    }

    #[tuff_core::watchdog]
    #[test]
    fn piston_shoves_and_unknown_surfaces_are_not_speed_checked() {
        let mut driver = Driver::boat(WATER);
        paddle(&mut driver, WATER, 0.0, 100);
        driver.vehicle().pushed = true;
        driver.drive(0.9);
        driver.vehicle().pushed = false;
        driver.vehicle().friction = None;
        driver.drive(1.2);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
    }

    /// A horse's client: ground ticks keep 0.546 of the speed and add the
    /// movement speed times the 0.98 forward input; air ticks keep 0.91
    /// and add a tenth of that; a jump adds the leap on its tick.
    struct Gallop {
        v: f64,
        speed: f64,
    }

    impl Gallop {
        fn ground(&mut self, driver: &mut Driver, ticks: usize) {
            for _ in 0..ticks {
                let d = self.v + self.speed * 0.98;
                driver.drive(d);
                self.v = d * 0.546;
            }
        }

        /// Ground ticks on a block with this friction, steering
        /// diagonally (the input's full length of 1).
        fn slide(&mut self, driver: &mut Driver, ticks: usize, friction: f64) {
            let push = if friction > 0.6 {
                self.speed * 0.216 / friction.powi(3)
            } else {
                self.speed
            };
            for _ in 0..ticks {
                let d = self.v + push;
                driver.drive(d);
                self.v = d * friction * 0.91;
            }
        }

        fn leap(&mut self, driver: &mut Driver, air: usize) {
            driver.jump();
            let d = self.v + 0.4 + self.speed * 0.98;
            driver.drive(d);
            self.v = d * 0.91;
            for _ in 0..air {
                let d = self.v + self.speed * 0.098;
                driver.drive(d);
                self.v = d * 0.91;
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_galloping_and_jumping_horse_is_never_flagged() {
        let mut driver = Driver::horse(HORSE);
        let mut horse = Gallop {
            v: 0.0,
            speed: HORSE.speed,
        };
        horse.ground(&mut driver, 200);
        for _ in 0..30 {
            horse.leap(&mut driver, 12);
            horse.ground(&mut driver, 2);
        }
        horse.ground(&mut driver, 100);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_horse_jumping_across_blue_ice_ice_and_onto_grass_is_never_flagged() {
        let mut driver = Driver::horse(HORSE);
        let mut horse = Gallop {
            v: 0.0,
            speed: HORSE.speed,
        };
        horse.slide(&mut driver, 300, BLUE_ICE);
        for _ in 0..20 {
            horse.leap(&mut driver, 10);
            horse.slide(&mut driver, 1, BLUE_ICE);
        }
        horse.leap(&mut driver, 10);
        horse.slide(&mut driver, 40, LAND);
        horse.slide(&mut driver, 300, ICE);
        horse.leap(&mut driver, 10);
        horse.slide(&mut driver, 40, LAND);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sped_up_horse_is_flagged_and_jumps_cannot_be_banked() {
        let mut driver = Driver::horse(HORSE);
        let mut horse = Gallop {
            v: 0.0,
            speed: HORSE.speed,
        };
        horse.ground(&mut driver, 100);
        for _ in 0..40 {
            driver.drive(0.95);
        }
        assert!(driver.speed_flags().len() >= 30, "{:?}", driver.flags);

        let mut banked = Driver::horse(HORSE);
        horse.v = 0.0;
        horse.ground(&mut banked, 20);
        banked.jump();
        horse.ground(&mut banked, 100);
        for _ in 0..40 {
            banked.drive(0.95);
        }
        assert!(!banked.speed_flags().is_empty(), "{:?}", banked.flags);

        let mut teleport = Driver::horse(HORSE);
        horse.v = 0.0;
        horse.ground(&mut teleport, 20);
        teleport.drive(1.5);
        assert_eq!(teleport.speed_flags(), [20], "past the peak at once");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_faster_mount_gets_a_higher_limit_and_unknown_mounts_none() {
        let fast = RiddenSpeed {
            speed: 0.6,
            leap: 0.0,
            ..RiddenSpeed::default()
        };
        let mut driver = Driver::horse(fast);
        let mut horse = Gallop { v: 0.0, speed: 0.6 };
        horse.ground(&mut driver, 200);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);

        let mut unknown = Driver::horse(HORSE);
        unknown.vehicle().ridden = None;
        for _ in 0..40 {
            unknown.drive(2.0);
        }
        assert!(unknown.flags.is_empty(), "{:?}", unknown.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn running_through_a_block_is_flagged_after_the_first_move() {
        let mut driver = Driver::boat(WATER);
        driver.vehicle().clipped = true;
        driver.drive(0.2);
        assert!(driver.flags.is_empty(), "the first move of a ride");
        driver.vehicle().clipped = false;
        driver.drive(0.2);
        driver.vehicle().clipped = true;
        let flags = driver.drive(0.2);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].check, PHASE);
        assert_eq!(
            flags[0].fix,
            Fix::MoveVehicle {
                position: [0.4, 64.0, 0.0],
                yaw: 0.0,
                pitch: 0.0
            }
        );
        driver.vehicle().pushed = true;
        assert!(driver.drive(0.2).is_empty(), "a piston shoved it");
    }

    #[tuff_core::watchdog]
    #[test]
    fn vehicle_moves_faster_than_the_client_ticks_are_flagged() {
        let mut driver = Driver::boat(WATER);
        paddle(&mut driver, WATER, 0.0, 400);
        assert!(driver.flags.is_empty(), "{:?}", driver.flags);
        let mut flooded = 0;
        for _ in 0..200 {
            flooded += driver
                .send(&Serverbound::MoveVehicle {
                    position: driver.facts.vehicle.unwrap().from,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                })
                .iter()
                .filter(|flag| flag.check == TIMER)
                .count();
        }
        assert!(flooded > 100, "{flooded}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_rider_that_does_not_steer_is_reset_now_and_then() {
        let mut driver = Driver::boat(WATER);
        driver.vehicle().controls = false;
        driver.vehicle().shown = [5.0, 64.0, 5.0];
        let mut resets = Vec::new();
        for n in 0..100 {
            for flag in driver.drive(1.0) {
                assert_eq!(flag.check, CONTROL);
                assert_eq!(
                    flag.fix,
                    Fix::ResetVehicle {
                        position: [5.0, 64.0, 5.0],
                        yaw: 0.0,
                        pitch: 0.0
                    }
                );
                resets.push(n);
            }
        }
        assert_eq!(resets, [21, 41, 61, 81], "after the grace, once a second");
    }
}

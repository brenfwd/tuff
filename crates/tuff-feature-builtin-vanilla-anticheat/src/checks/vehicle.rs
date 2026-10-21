use tuff_core::protocol::play::Serverbound;
use tuff_core::world::entity::RiddenSpeed;
use tuff_core::world::messages::Riding;
use tuff_core::world::physics::{self, Aabb};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::{Block, BlockState, Item, tag};
use tuff_world_model::BlockPos;

use super::{Check, Facts, Fix, Flag};

const NAME: &str = "vehicle_fly";
const GRAVITY: f64 = 0.04;
const LAUNCH: f64 = 0.7;
const BOUNCE: f64 = 0.8;
const SLACK: f64 = 0.005;
const REACH: f64 = 0.0625;
const BELOW: f64 = 0.1;
const ENTITY_SLACK: f64 = 0.5;
const KNOWN_MARGIN: f64 = 1.0;
const GIVEN_WINDOW: u64 = 20;
const PISTON_WINDOW: u64 = 5;
const MAX_GIVEN_WINDOW: u64 = 100;
const FLOAT_DRAG: f64 = 0.9;
const LAND_MARGIN: f64 = 0.01;
const LAND_DEPTH: f64 = 0.01;
const BOAT_PROBE: f64 = 0.001;
const CORE_INSET: f64 = 0.2;
const CLIP_TOLERANCE: f64 = 0.05;
const MAX_SWEEP: f64 = 16.0;
const MOUNT_GRAVITY: f64 = 0.08;
const MOUNT_DRAG: f64 = 0.98;
/// The highest a ridden jump or dash launches: full-strength horse jump
/// plus a jump boost on the mount.
const MOUNT_LAUNCH: f64 = 1.2;
const MOUNT_LAUNCH_MARGIN: f64 = 1.05;
const LEVITATION_RISE: f64 = 0.05;
const MOUNT_SLACK: f64 = 0.03;
/// A ridden jump launches within this many ticks (plus a round trip) of
/// the rider's jump packet.
const MOUNT_JUMP_WINDOW: u64 = 10;

/// The vehicle a rider's Move Vehicle packet moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vehicle {
    pub id: i32,
    pub kind: EntityType,
    pub boat: bool,
    /// The rider steers it the way the game's client decides, so its
    /// client sends where it moved; any other rider's Move Vehicle is
    /// bogus.
    pub controls: bool,
    /// Where the server last accepted it.
    pub from: [f64; 3],
    /// Every chunk around both positions was sent to the client.
    pub known: bool,
    /// Water, a block or a solid entity touches it at either position,
    /// any of which can give it vertical speed gravity does not explain
    /// (for a mount, a climbable too).
    pub held: bool,
    pub no_gravity: bool,
    /// It flies where its rider steers it: no gravity bound applies.
    pub flying: bool,
    /// The session tick and velocity of the last velocity the server
    /// gave it, while the client may still be catching up on it.
    pub launch: Option<(u64, [f64; 3])>,
    /// A moving block shoved it recently.
    pub pushed: bool,
    /// How fast its rider can drive it, for a mount.
    pub ridden: Option<RiddenSpeed>,
    /// The most of its horizontal speed a boat keeps over the tick that
    /// starts at `from`, or `None` where that is unknown.
    pub friction: Option<f64>,
    /// Liquid touches it at `from`.
    pub wet: bool,
    /// The move ran it through a block it was not already inside.
    pub clipped: bool,
    /// Where the server shows it to a rider that does not steer it.
    pub shown: [f64; 3],
}

/// How long after the server moved a vehicle its client may still show
/// it: `base` ticks plus a round trip.
pub fn given_window(base: u64, latency_ms: i32) -> u64 {
    let round_trip = u64::try_from(latency_ms).unwrap_or(0) * 2;
    (base + round_trip.div_ceil(50)).min(MAX_GIVEN_WINDOW)
}

fn is_boat(kind: EntityType) -> bool {
    let name = kind.name();
    name.ends_with("_boat") || name.ends_with("_raft")
}

pub(crate) fn solid_entity(kind: EntityType) -> bool {
    is_boat(kind) || matches!(kind, EntityType::Shulker | EntityType::HappyGhast)
}

/// Ticks a client takes to interpolate a moved entity to its new spot.
pub(crate) const LERP_TICKS: u64 = 3;

pub(crate) fn bounds(kind: EntityType, at: [f64; 3]) -> Aabb {
    let size = kind.size();
    let half = f64::from(size.width) / 2.0;
    [
        at[0] - half,
        at[1],
        at[2] - half,
        at[0] + half,
        at[1] + f64::from(size.height),
        at[2] + half,
    ]
}

fn moves_vertically(state: BlockState) -> bool {
    state.is_liquid()
        || state.get("waterlogged") == Some("true")
        || matches!(
            state.block(),
            Block::BubbleColumn | Block::Cobweb | Block::PowderSnow
        )
}

fn any_block(world: &ChunkMirror, area: Aabb, test: impl Fn(BlockState) -> bool) -> bool {
    let cell = |v: f64| v.floor() as i32;
    (cell(area[0])..=cell(area[3])).any(|x| {
        (cell(area[1])..=cell(area[4])).any(|y| {
            (cell(area[2])..=cell(area[5]))
                .any(|z| world.block_at(BlockPos::new(x, y, z)).is_some_and(&test))
        })
    })
}

fn climbable(state: BlockState) -> bool {
    tuff_datapack::tags::is(tag::block::key::CLIMBABLE, state.block())
}

fn held(
    world: &ChunkMirror,
    solids: &[Aabb],
    kind: EntityType,
    at: [f64; 3],
    climbs: bool,
) -> bool {
    let side = if climbs { 0.0 } else { REACH };
    let near = physics::inflate(
        physics::expand_towards(bounds(kind, at), [0.0, -BELOW, 0.0]),
        [side, 0.0, side],
    );
    if !world.colliders(near).is_empty() {
        return true;
    }
    if solids
        .iter()
        .any(|solid| physics::intersects(physics::inflate(*solid, [ENTITY_SLACK; 3]), near))
    {
        return true;
    }
    any_block(world, near, |state| {
        moves_vertically(state) || (climbs && climbable(state))
    })
}

fn wet(state: BlockState) -> bool {
    state.is_liquid()
        || state.get("waterlogged") == Some("true")
        || state.block() == Block::BubbleColumn
}

/// Blocks whose collision the mirror cannot place exactly (random
/// offsets, shapes that depend on who touches them) or that boats break.
fn uncertain(state: BlockState) -> bool {
    matches!(
        state.block(),
        Block::LilyPad
            | Block::Bamboo
            | Block::PointedDripstone
            | Block::Scaffolding
            | Block::PowderSnow
            | Block::MovingPiston
    )
}

fn boxes(world: &ChunkMirror, area: Aabb) -> Vec<Aabb> {
    physics::colliders_shaped(area, &|pos| match world.block_at(pos) {
        Some(state) if !uncertain(state) => physics::Shape::collision(Some(state), pos),
        _ => physics::Shape::from(&[][..]),
    })
}

/// The most of its horizontal speed a boat at `at` keeps for a tick: the
/// grip of the slipperiest block it may rest on, or the drag of water and
/// air (0.9) wherever it may be floating or flying instead.
fn boat_friction(world: &ChunkMirror, kind: EntityType, at: [f64; 3]) -> (f64, bool) {
    let body = bounds(kind, at);
    let wet = any_block(world, physics::inflate(body, [REACH, BELOW, REACH]), wet);
    let under = |margin: f64, depth: f64| {
        [
            body[0] - margin,
            body[1] - depth,
            body[2] - margin,
            body[3] + margin,
            body[1],
            body[5] + margin,
        ]
    };
    let mut grip = 0.0f64;
    let wide = under(LAND_MARGIN, LAND_DEPTH);
    let cell = |v: f64| v.floor() as i32;
    for x in cell(wide[0])..=cell(wide[3]) {
        for y in cell(wide[1])..=cell(wide[4]) {
            for z in cell(wide[2])..=cell(wide[5]) {
                let pos = BlockPos::new(x, y, z);
                let Some(state) = world.block_at(pos) else {
                    continue;
                };
                if state.block() == Block::LilyPad {
                    continue;
                }
                if physics::block_boxes(Some(state), pos).any(|b| physics::intersects(b, wide)) {
                    grip = grip.max(f64::from(state.block().movement().friction));
                }
            }
        }
    }
    let narrow = under(-LAND_MARGIN, BOAT_PROBE);
    let airborne = boxes(world, narrow).is_empty();
    if wet || airborne {
        grip = grip.max(FLOAT_DRAG);
    }
    (grip, wet)
}

/// Whether moving `kind` from `from` to `to` runs the core of its box
/// (clear of what it can step over) through a block it was not already
/// inside, sliding one axis at a time like the game does.
fn clipped(world: &ChunkMirror, kind: EntityType, from: [f64; 3], to: [f64; 3]) -> bool {
    let size = kind.size();
    let step = kind.base_attribute(Attribute::StepHeight).unwrap_or(0.0);
    let half = f64::from(size.width) / 2.0 - CORE_INSET;
    let (bottom, top) = (
        step + CORE_INSET / 2.0,
        f64::from(size.height) - CORE_INSET / 2.0,
    );
    let movement = [0, 1, 2].map(|i| to[i] - from[i]);
    if half <= 0.0
        || bottom >= top
        || movement.iter().map(|v| v * v).sum::<f64>() > MAX_SWEEP * MAX_SWEEP
    {
        return false;
    }
    let core = [
        from[0] - half,
        from[1] + bottom,
        from[2] - half,
        from[0] + half,
        from[1] + top,
        from[2] + half,
    ];
    if !boxes(world, core).is_empty() {
        return false;
    }
    let shapes = boxes(world, physics::expand_towards(core, movement));
    let resolved = physics::collide_with_shapes(movement, core, &shapes);
    (0..3).any(|i| (movement[i] - resolved[i]).abs() > CLIP_TOLERANCE)
}

fn holds(session: &PlayerSession, item: Item) -> bool {
    [false, true]
        .into_iter()
        .any(|off| session.inventory.hand(off).is_some_and(|s| s.item == item))
}

/// Whether the game's client lets this rider steer: the first rider of
/// a vehicle that takes steering, holding the lure a pig or strider
/// needs.
fn controls(session: &PlayerSession, riding: &Riding) -> bool {
    riding.driver
        && match riding.kind {
            EntityType::Pig => holds(session, Item::CarrotOnAStick),
            EntityType::Strider => holds(session, Item::WarpedFungusOnAStick),
            _ => true,
        }
}

impl Vehicle {
    pub fn probe(session: &PlayerSession, packet: &Serverbound) -> Option<Vehicle> {
        let Serverbound::MoveVehicle { position, .. } = packet else {
            return None;
        };
        let riding = session.ride.riding.as_ref()?;
        if !position.iter().all(|c| c.is_finite()) {
            return None;
        }
        let (kind, from, to) = (riding.kind, riding.position, *position);
        let boat = is_boat(kind);
        let controls = controls(session, riding);
        let world = session.client_world();
        let margin = [KNOWN_MARGIN; 3];
        let known = controls
            && !world.touches_unknown(physics::inflate(bounds(kind, from), margin))
            && !world.touches_unknown(physics::inflate(bounds(kind, to), margin));
        let held = known && {
            let solids: Vec<Aabb> = session
                .view
                .entities
                .visible_entities()
                .filter(|(id, kind, _, _)| *id != riding.vehicle && solid_entity(*kind))
                .map(|(_, kind, _, at)| bounds(kind, at))
                .collect();
            held(world, &solids, kind, from, !boat) || held(world, &solids, kind, to, !boat)
        };
        let recent = |at: u64, base: u64| {
            session.tick.saturating_sub(at) <= given_window(base, session.net.latency_ms)
        };
        let (friction, wet) = if known && boat {
            let (friction, wet) = boat_friction(world, kind, from);
            (Some(friction), wet)
        } else {
            (None, false)
        };
        Some(Vehicle {
            id: riding.vehicle,
            kind,
            boat,
            controls,
            from,
            known,
            held,
            no_gravity: riding.no_gravity,
            flying: riding.flying,
            launch: riding
                .given
                .launch
                .filter(|(at, _)| recent(*at, GIVEN_WINDOW)),
            pushed: riding
                .given
                .piston
                .is_some_and(|at| recent(at, PISTON_WINDOW)),
            ridden: riding.ridden,
            friction,
            wet,
            clipped: known && clipped(world, kind, from, to),
            shown: if controls {
                from
            } else {
                shown(session, riding)
            },
        })
    }
}

/// Where the server shows the vehicle to a rider that does not steer it:
/// the last tracked position, else where it was when mounted.
fn shown(session: &PlayerSession, riding: &Riding) -> [f64; 3] {
    session
        .view
        .entities
        .visible_entities()
        .find(|(id, ..)| *id == riding.vehicle)
        .map_or(riding.position, |(.., at)| at)
}

/// A boat in the air falls: with nothing touching it, each move may rise
/// at most the previous move less boat gravity (boats have no vertical
/// air drag). Leaving water or a block allows one launch, covering
/// bubble columns and a slime bounce; velocity the server gave it allows
/// one more, and a moving block shoving it counts as touching it.
#[derive(Default)]
pub struct BoatGravity {
    vehicle: Option<i32>,
    velocity: Option<f64>,
    launch: f64,
    /// The vertical speed the server gave that no move has used yet.
    given: Option<f64>,
    given_at: Option<u64>,
}

impl BoatGravity {
    fn reset(&mut self, vehicle: Option<i32>) {
        let (given, given_at) = if vehicle.is_some() && vehicle == self.vehicle {
            (self.given, self.given_at)
        } else {
            (None, None)
        };
        *self = BoatGravity {
            vehicle,
            velocity: None,
            launch: LAUNCH,
            given,
            given_at,
        };
    }

    fn note_given(&mut self, launch: Option<(u64, f64)>) {
        match launch {
            Some((at, speed)) if self.given_at != Some(at) => {
                self.given_at = Some(at);
                self.given = Some(self.given.map_or(speed, |given| given.max(speed)));
            }
            Some(_) => {}
            None => self.given = None,
        }
    }
}

impl Check for BoatGravity {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::MoveVehicle {
            position,
            yaw,
            pitch,
            ..
        } = packet
        else {
            return None;
        };
        let Some(vehicle) = facts
            .vehicle
            .filter(|vehicle| vehicle.boat && vehicle.controls)
        else {
            self.reset(None);
            return None;
        };
        if self.vehicle != Some(vehicle.id) {
            self.reset(Some(vehicle.id));
        }
        if !vehicle.known || facts.teleport_pending {
            self.reset(Some(vehicle.id));
            return None;
        }
        self.note_given(vehicle.launch.map(|(at, velocity)| (at, velocity[1])));
        let gravity = if vehicle.no_gravity { 0.0 } else { GRAVITY };
        let dy = position[1] - vehicle.from[1];
        if vehicle.held || vehicle.pushed {
            if let Some(velocity) = self.velocity {
                self.launch = self.launch.max(BOUNCE * (gravity - velocity));
            }
            self.velocity = None;
            return None;
        }
        let limit = self.velocity.unwrap_or(self.launch) - gravity;
        self.launch = LAUNCH;
        if dy <= limit + SLACK {
            self.velocity = Some(dy);
            return None;
        }
        if self
            .given
            .is_some_and(|given| dy <= given - gravity + SLACK)
        {
            self.given = None;
            self.velocity = Some(dy);
            return None;
        }
        self.velocity = Some(limit.min(0.0));
        Some(Flag {
            check: NAME,
            weight: 1.0,
            fix: Fix::MoveVehicle {
                position: vehicle.from,
                yaw: *yaw,
                pitch: *pitch,
            },
        })
    }
}

/// A driven mount in the air falls like any living thing: each move may
/// rise at most the previous one less gravity, then drag. Touching a
/// block, liquid, climbable or solid entity lifts the bound (steps,
/// swimming, lava walking, ladders); from rest only a jump or dash the
/// rider announced, server-given velocity or a moving block launches it.
#[derive(Default)]
pub struct MountGravity {
    vehicle: Option<i32>,
    velocity: Option<f64>,
    jumped_at: Option<u64>,
    given: Option<f64>,
    given_at: Option<u64>,
}

impl MountGravity {
    fn reset(&mut self, vehicle: Option<i32>) {
        if vehicle != self.vehicle {
            *self = MountGravity {
                vehicle,
                jumped_at: self.jumped_at,
                ..MountGravity::default()
            };
        } else {
            self.velocity = None;
        }
    }

    fn note_given(&mut self, launch: Option<(u64, f64)>) {
        match launch {
            Some((at, speed)) if self.given_at != Some(at) => {
                self.given_at = Some(at);
                self.given = Some(self.given.map_or(speed, |given| given.max(speed)));
            }
            Some(_) => {}
            None => self.given = None,
        }
    }
}

impl Check for MountGravity {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if let Serverbound::RidingJump(_) = packet {
            self.jumped_at = Some(facts.tick);
            return None;
        }
        let Serverbound::MoveVehicle {
            position,
            yaw,
            pitch,
            ..
        } = packet
        else {
            return None;
        };
        let Some(vehicle) = facts
            .vehicle
            .filter(|vehicle| !vehicle.boat && !vehicle.flying && vehicle.controls)
        else {
            self.reset(None);
            return None;
        };
        if self.vehicle != Some(vehicle.id) {
            self.reset(Some(vehicle.id));
        }
        if !vehicle.known || facts.teleport_pending {
            self.velocity = None;
            return None;
        }
        self.note_given(vehicle.launch.map(|(at, velocity)| (at, velocity[1])));
        let gravity = if vehicle.no_gravity {
            0.0
        } else {
            MOUNT_GRAVITY
        };
        let dy = position[1] - vehicle.from[1];
        if vehicle.held || vehicle.pushed {
            self.velocity = None;
            return None;
        }
        let jumped = self.jumped_at.take().filter(|at| {
            facts.tick.saturating_sub(*at) <= given_window(MOUNT_JUMP_WINDOW, facts.latency_ms)
        });
        let ridden = vehicle.ridden.unwrap_or_default();
        let gravity = if ridden.slow_falling {
            gravity.min(0.01)
        } else {
            gravity
        };
        let launch = if ridden.launch > 0.0 {
            ridden.launch * MOUNT_LAUNCH_MARGIN
        } else {
            MOUNT_LAUNCH
        };
        let mut limit = match self.velocity {
            Some(velocity) => (velocity - gravity) * MOUNT_DRAG,
            None if jumped.is_some() => launch,
            None => 0.0,
        };
        if ridden.levitation > 0 {
            limit = limit.max(LEVITATION_RISE * f64::from(ridden.levitation));
        }
        if dy <= limit + MOUNT_SLACK {
            self.velocity = Some(dy);
            return None;
        }
        if self.given.is_some_and(|given| dy <= given + MOUNT_SLACK) {
            self.given = None;
            self.velocity = Some(dy);
            return None;
        }
        self.velocity = Some(limit.min(0.0));
        Some(Flag {
            check: NAME,
            weight: 1.0,
            fix: Fix::MoveVehicle {
                position: vehicle.from,
                yaw: *yaw,
                pitch: *pitch,
            },
        })
    }
}

#[cfg(test)]
impl Vehicle {
    /// A driven oak boat at `from` in known open air.
    pub(crate) fn boat(id: i32, from: [f64; 3]) -> Vehicle {
        Vehicle {
            id,
            kind: EntityType::OakBoat,
            boat: true,
            controls: true,
            from,
            known: true,
            held: false,
            no_gravity: false,
            flying: false,
            launch: None,
            pushed: false,
            ridden: None,
            friction: Some(FLOAT_DRAG),
            wet: false,
            clipped: false,
            shown: from,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    const GROUND: f64 = 63.0;

    struct Ride {
        check: BoatGravity,
        facts: Facts,
        flags: Vec<usize>,
        moves: usize,
    }

    impl Ride {
        fn at(y: f64) -> Ride {
            let mut facts = facts();
            facts.riding = true;
            facts.vehicle = Some(Vehicle {
                held: true,
                ..Vehicle::boat(7, [0.0, y, 0.0])
            });
            Ride {
                check: BoatGravity::default(),
                facts,
                flags: Vec::new(),
                moves: 0,
            }
        }

        fn from(&self) -> [f64; 3] {
            self.facts.vehicle.expect("vehicle").from
        }

        fn drive(&mut self, y: f64, held: bool) -> Option<Flag> {
            let vehicle = self.facts.vehicle.as_mut().expect("vehicle");
            vehicle.held = held || vehicle.from[1] < GROUND + BELOW || y < GROUND + BELOW;
            let packet = Serverbound::MoveVehicle {
                position: [0.0, y, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                on_ground: false,
            };
            let flag = self.check.examine(&self.facts, &packet);
            if flag.is_some() {
                self.flags.push(self.moves);
            } else {
                self.facts.vehicle.as_mut().expect("vehicle").from[1] = y;
            }
            self.moves += 1;
            flag
        }

        fn steps(&mut self, steps: &[f64]) {
            for dy in steps {
                let y = self.from()[1] + dy;
                self.drive(y, false);
            }
        }
    }

    fn repeat(dy: f64, n: usize) -> Vec<f64> {
        vec![dy; n]
    }

    fn falling(from: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| from - GRAVITY * i as f64).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_recorded_boatfly_climb_is_flagged_from_its_third_move() {
        let mut ride = Ride::at(GROUND);
        ride.steps(&repeat(0.26, 30));
        assert_eq!(ride.flags.first(), Some(&2), "{:?}", ride.flags);
        assert_eq!(ride.flags.len(), 28);
        assert!(ride.from()[1] < GROUND + 0.6, "{:?}", ride.from());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_recorded_glide_and_a_hover_are_flagged_after_one_move() {
        let mut glide = Ride::at(GROUND);
        glide.facts.vehicle.as_mut().unwrap().from[1] = 77.56;
        glide.check.vehicle = Some(7);
        glide.check.velocity = Some(0.26);
        glide.steps(&repeat(-0.04, 40));
        assert_eq!(glide.flags, (1..40).collect::<Vec<_>>());

        let mut hover = Ride::at(GROUND);
        hover.facts.vehicle.as_mut().unwrap().from[1] = 70.0;
        hover.steps(&repeat(0.0, 40));
        assert_eq!(hover.flags, (1..40).collect::<Vec<_>>());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_driven_off_a_ledge_falls_freely_and_lands() {
        let mut ride = Ride::at(GROUND + 20.0);
        ride.drive(GROUND + 20.0, true);
        ride.steps(&falling(-GRAVITY, 28));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
        let y = ride.from()[1];
        assert!(y < GROUND + 5.0, "{y}");
        ride.drive(GROUND, true);
        ride.drive(GROUND, false);
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_recorded_vanilla_parabola_after_boatfly_stops_is_not_flagged() {
        let mut ride = Ride::at(GROUND + 5.0);
        ride.check.vehicle = Some(7);
        ride.check.velocity = Some(0.26);
        ride.steps(&falling(0.22, 20));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bubble_column_launch_and_a_slime_bounce_are_allowed_once() {
        let mut ride = Ride::at(GROUND + 10.0);
        ride.drive(GROUND + 10.0, true);
        ride.steps(&falling(0.56, 20));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);

        let mut bounce = Ride::at(GROUND + 20.0);
        bounce.drive(GROUND + 20.0, true);
        bounce.steps(&falling(-GRAVITY, 30));
        let speed = GRAVITY * 31.0;
        bounce.facts.vehicle.as_mut().unwrap().from[1] = GROUND + 2.0;
        bounce.drive(GROUND + 1.0, true);
        bounce.steps(&falling(BOUNCE * speed - GRAVITY, 10));
        assert!(bounce.flags.is_empty(), "{:?}", bounce.flags);
        bounce.steps(&[0.6]);
        assert_eq!(bounce.flags.len(), 1, "gravity holds after the bounce");
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_and_unknown_chunks_never_flag_and_a_new_boat_starts_fresh() {
        let mut ride = Ride::at(GROUND);
        for _ in 0..20 {
            let y = ride.from()[1] + 0.26;
            assert!(ride.drive(y, true).is_none());
        }
        ride.facts.vehicle.as_mut().unwrap().known = false;
        ride.steps(&repeat(0.26, 5));
        assert!(ride.flags.is_empty());

        ride.facts.vehicle.as_mut().unwrap().known = true;
        ride.steps(&repeat(0.0, 3));
        assert_eq!(ride.flags, [26, 27]);

        let vehicle = ride.facts.vehicle.as_mut().unwrap();
        vehicle.id = 8;
        ride.steps(&[0.3]);
        assert_eq!(ride.flags.len(), 2, "a new vehicle gets a launch");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flag_puts_the_boat_back_where_the_server_has_it() {
        let mut ride = Ride::at(70.0);
        ride.check.vehicle = Some(7);
        ride.check.velocity = Some(0.0);
        let flag = ride.drive(70.5, false).expect("flagged");
        assert_eq!(
            flag.fix,
            Fix::MoveVehicle {
                position: [0.0, 70.0, 0.0],
                yaw: 0.0,
                pitch: 0.0
            }
        );
        assert_eq!(flag.check, NAME);
    }

    #[tuff_core::watchdog]
    #[test]
    fn other_vehicles_and_other_packets_are_ignored() {
        let mut ride = Ride::at(70.0);
        ride.facts.vehicle.as_mut().unwrap().boat = false;
        ride.steps(&repeat(0.5, 10));
        assert!(ride.flags.is_empty());
        assert!(
            ride.check
                .examine(&ride.facts, &Serverbound::Punch)
                .is_none()
        );
    }

    impl Ride {
        fn launch(&mut self, at: u64, speed: f64) {
            self.facts.vehicle.as_mut().unwrap().launch = Some((at, [0.0, speed, 0.0]));
        }

        fn expire(&mut self) {
            self.facts.vehicle.as_mut().unwrap().launch = None;
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blast_launch_is_allowed_once_and_only_while_it_is_fresh() {
        let mut ride = Ride::at(GROUND + 10.0);
        ride.drive(GROUND + 10.0, true);
        ride.steps(&falling(-GRAVITY, 5));
        ride.launch(1, 1.2);
        ride.steps(&falling(-0.24, 3));
        ride.steps(&falling(1.16, 30));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
        ride.steps(&[1.0]);
        assert_eq!(ride.flags.len(), 1, "a used launch is gone");

        let mut late = Ride::at(GROUND + 10.0);
        late.drive(GROUND + 10.0, true);
        late.launch(1, 1.2);
        late.steps(&falling(-GRAVITY, 5));
        late.expire();
        late.steps(&[1.16]);
        assert_eq!(late.flags.len(), 1, "an expired launch allows nothing");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_launch_in_water_carries_into_the_air_and_stacked_launches_keep_the_highest() {
        let mut ride = Ride::at(GROUND + 10.0);
        ride.launch(1, 0.9);
        ride.drive(GROUND + 10.0, true);
        ride.launch(2, 1.5);
        ride.drive(GROUND + 10.0, true);
        ride.launch(3, 1.0);
        ride.steps(&falling(1.46, 20));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_piston_shove_and_its_slime_bounce_are_allowed() {
        let mut ride = Ride::at(GROUND + 10.0);
        ride.check.vehicle = Some(7);
        ride.check.velocity = Some(0.0);
        let vehicle = ride.facts.vehicle.as_mut().unwrap();
        vehicle.pushed = true;
        vehicle.launch = Some((1, [0.0, 1.0, 0.0]));
        ride.steps(&[0.51, 0.51, 0.97]);
        ride.facts.vehicle.as_mut().unwrap().pushed = false;
        ride.steps(&falling(0.92, 20));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_without_gravity_may_hover_but_not_climb_from_rest() {
        let mut ride = Ride::at(GROUND + 10.0);
        ride.facts.vehicle.as_mut().unwrap().no_gravity = true;
        ride.check.vehicle = Some(7);
        ride.check.velocity = Some(0.0);
        ride.steps(&repeat(0.0, 40));
        assert!(ride.flags.is_empty(), "{:?}", ride.flags);
        ride.steps(&[0.3]);
        assert_eq!(ride.flags.len(), 1, "nothing pushed it up");

        ride.facts.vehicle.as_mut().unwrap().no_gravity = false;
        ride.steps(&repeat(0.0, 2));
        assert_eq!(ride.flags.len(), 3, "gravity is back");
    }

    struct Mount {
        check: MountGravity,
        facts: Facts,
        flags: Vec<usize>,
        moves: usize,
    }

    impl Mount {
        fn at(y: f64) -> Mount {
            let mut facts = facts();
            facts.riding = true;
            facts.vehicle = Some(Vehicle {
                kind: EntityType::Horse,
                boat: false,
                friction: None,
                ..Vehicle::boat(9, [0.0, y, 0.0])
            });
            Mount {
                check: MountGravity::default(),
                facts,
                flags: Vec::new(),
                moves: 0,
            }
        }

        fn y(&self) -> f64 {
            self.facts.vehicle.expect("vehicle").from[1]
        }

        fn drive(&mut self, dy: f64) {
            self.facts.tick += 1;
            let from = self.y();
            let to = from + dy;
            let vehicle = self.facts.vehicle.as_mut().expect("vehicle");
            vehicle.held = from < GROUND + BELOW || to < GROUND + BELOW;
            let packet = Serverbound::MoveVehicle {
                position: [0.0, to.max(GROUND), 0.0],
                yaw: 0.0,
                pitch: 0.0,
                on_ground: false,
            };
            if self.check.examine(&self.facts, &packet).is_some() {
                self.flags.push(self.moves);
            } else {
                self.facts.vehicle.as_mut().expect("vehicle").from[1] = to.max(GROUND);
            }
            self.moves += 1;
        }

        fn jump(&mut self) {
            self.check
                .examine(&self.facts, &Serverbound::RidingJump(100));
        }

        fn arc(&mut self, launch: f64) {
            let mut vy = launch;
            for _ in 0..60 {
                self.drive(vy);
                if self.y() <= GROUND {
                    break;
                }
                vy = (vy - MOUNT_GRAVITY) * MOUNT_DRAG;
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_jumping_horse_and_a_mount_walking_off_a_ledge_are_never_flagged() {
        let mut horse = Mount::at(GROUND);
        for launch in [0.5, 0.9, 1.15] {
            horse.drive(0.0);
            horse.jump();
            horse.arc(launch);
            assert_eq!(horse.y(), GROUND);
        }
        assert!(horse.flags.is_empty(), "{:?}", horse.flags);

        let mut pig = Mount::at(GROUND + 12.0);
        pig.facts.vehicle.as_mut().unwrap().kind = EntityType::Pig;
        pig.arc(0.0);
        assert!(pig.flags.is_empty(), "{:?}", pig.flags);
        assert_eq!(pig.y(), GROUND);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_happy_ghast_climbing_and_hovering_is_never_flagged() {
        let mut ghast = Mount::at(GROUND + 5.0);
        let vehicle = ghast.facts.vehicle.as_mut().unwrap();
        vehicle.kind = EntityType::HappyGhast;
        vehicle.flying = true;
        for _ in 0..20 {
            ghast.drive(0.3);
        }
        for _ in 0..20 {
            ghast.drive(0.0);
        }
        assert!(ghast.flags.is_empty(), "{:?}", ghast.flags);
        assert!((ghast.y() - (GROUND + 11.0)).abs() < 1.0e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flying_or_hovering_mount_is_flagged() {
        let mut climb = Mount::at(GROUND + 5.0);
        for _ in 0..20 {
            climb.drive(0.3);
        }
        assert_eq!(climb.flags.len(), 20, "{:?}", climb.flags);
        assert_eq!(climb.y(), GROUND + 5.0);

        let mut hover = Mount::at(GROUND + 5.0);
        for _ in 0..20 {
            hover.drive(0.0);
        }
        assert_eq!(hover.flags, (1..20).collect::<Vec<_>>());

        let mut unannounced = Mount::at(GROUND);
        unannounced.drive(0.0);
        unannounced.drive(0.1);
        unannounced.drive(0.9);
        assert_eq!(unannounced.flags, [2], "a jump needs the jump packet");

        let mut stale = Mount::at(GROUND);
        stale.jump();
        stale.facts.tick += 100;
        stale.drive(0.1);
        stale.drive(0.9);
        assert_eq!(stale.flags, [1], "an old jump packet launches nothing");
    }

    #[tuff_core::watchdog]
    #[test]
    fn contact_given_velocity_and_passengers_are_exempt_from_the_mount_bound() {
        let mut swim = Mount::at(GROUND + 5.0);
        swim.facts.vehicle.as_mut().unwrap().held = true;
        for _ in 0..10 {
            let packet = Serverbound::MoveVehicle {
                position: [0.0, swim.y() + 0.2, 0.0],
                yaw: 0.0,
                pitch: 0.0,
                on_ground: false,
            };
            assert!(swim.check.examine(&swim.facts, &packet).is_none());
            swim.facts.vehicle.as_mut().unwrap().from[1] += 0.2;
        }

        let mut blast = Mount::at(GROUND + 5.0);
        blast.facts.vehicle.as_mut().unwrap().launch = Some((1, [0.0, 1.5, 0.0]));
        blast.arc(1.5);
        assert!(blast.flags.is_empty(), "{:?}", blast.flags);

        let mut passenger = Mount::at(GROUND + 5.0);
        passenger.facts.vehicle.as_mut().unwrap().controls = false;
        for _ in 0..10 {
            passenger.drive(0.5);
        }
        assert!(passenger.flags.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_levitating_mount_may_rise_and_a_slow_falling_one_hangs() {
        let mut lev = Mount::at(GROUND + 5.0);
        lev.facts.vehicle.as_mut().unwrap().ridden = Some(RiddenSpeed {
            levitation: 2,
            ..RiddenSpeed::default()
        });
        for _ in 0..20 {
            lev.drive(0.1);
        }
        assert!(lev.flags.is_empty(), "{:?}", lev.flags);
        lev.drive(0.3);
        assert_eq!(lev.flags.len(), 1);

        let mut slow = Mount::at(GROUND + 20.0);
        slow.facts.vehicle.as_mut().unwrap().ridden = Some(RiddenSpeed {
            slow_falling: true,
            ..RiddenSpeed::default()
        });
        slow.drive(-0.01);
        for _ in 0..10 {
            slow.drive(-0.03);
        }
        assert!(slow.flags.is_empty(), "{:?}", slow.flags);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_jump_is_capped_by_the_mounts_own_strength() {
        let mut weak = Mount::at(GROUND);
        weak.facts.vehicle.as_mut().unwrap().ridden = Some(RiddenSpeed {
            launch: 0.5,
            ..RiddenSpeed::default()
        });
        weak.jump();
        weak.drive(0.1);
        weak.drive(0.9);
        assert_eq!(weak.flags, [1]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn wall_contact_holds_a_boat_but_not_a_mount() {
        use tuff_world_model::chunk::{Chunk, ChunkShape};
        use tuff_world_model::pos::ChunkPos;
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        for y in 60..80 {
            chunk.set(1, y, 0, Block::Stone.default_state());
        }
        chunk.set(0, 59, 0, Block::Stone.default_state());
        let mut world = ChunkMirror::default();
        world.insert(ChunkPos::new(0, 0), std::sync::Arc::new(chunk));
        let at = [0.27, 70.0, 0.5];
        assert!(held(&world, &[], EntityType::Horse, at, false));
        assert!(!held(&world, &[], EntityType::Horse, at, true));
        assert!(held(
            &world,
            &[],
            EntityType::Horse,
            [0.27, 60.0, 0.5],
            true
        ));
        assert!(!held(
            &world,
            &[],
            EntityType::Horse,
            [0.27, 65.0, 0.5],
            true
        ));
    }
}

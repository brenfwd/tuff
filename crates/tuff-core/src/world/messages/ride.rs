use tuff_vanilla_data::Item;

use crate::world::dimension::DimensionId;
use tuff_nbt::Compound;

use super::*;

/// Who `/ride` puts on or takes off a vehicle.
#[derive(Debug, Clone)]
pub enum RidePassenger {
    Entity(i32),
    Player { handle: PlayerHandle, uuid: u128 },
}

impl RidePassenger {
    pub fn id(&self) -> i32 {
        match self {
            Self::Entity(id) => *id,
            Self::Player { handle, .. } => handle.id,
        }
    }
}

/// One `/ride` step for a region.
#[derive(Debug)]
pub enum RideOp {
    /// What the entity or player `passenger` rides here.
    VehicleOf { passenger: i32 },
    /// Seats `passenger` on `vehicle` regardless of free seats.
    Mount {
        passenger: RidePassenger,
        vehicle: i32,
    },
    /// Seats an entity taken from another region (its saved form) on
    /// `vehicle`.
    Board { nbt: Compound, vehicle: i32 },
    /// Takes `passenger` off what it rides here.
    Dismount { passenger: RidePassenger },
}

/// A region's answer to a [`RideOp`].
#[derive(Debug)]
pub enum RideAnswer {
    NotHere,
    /// The vehicle the passenger rode (or now rides).
    Vehicle(Box<EntitySnapshot>),
    /// The vehicle is here but the entity passenger is not.
    PassengerElsewhere(Box<EntitySnapshot>),
    /// The vehicle already rides the passenger.
    Loop,
    Failed,
}

/// The vehicle a player rides, as its region last told it.
#[derive(Debug, Clone, PartialEq)]
pub struct Riding {
    pub vehicle: i32,
    pub kind: tuff_vanilla_data::registry::EntityType,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    /// Every rider, the driver first.
    pub riders: Vec<i32>,
    /// Where this player sits, from the vehicle's position.
    pub seat: [f64; 3],
    /// Whether this player steers it.
    pub driver: bool,
    /// Whether its held movement keys go to the vehicle each tick.
    pub steers: bool,
    /// It ignores gravity (its `NoGravity` tag).
    pub no_gravity: bool,
    /// It flies where its driver steers it (a harnessed happy ghast), so
    /// no gravity bounds its climb.
    pub flying: bool,
    /// How fast its driver can ride it, for a mount.
    pub ridden: Option<crate::world::entity::RiddenSpeed>,
    /// What the server did to it that its driver's client simulates; the
    /// region leaves it empty and the session keeps it.
    pub given: GivenMotion,
}

/// Movement the server gave a vehicle its driver's client moves, stamped
/// with the driver's session tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GivenMotion {
    /// The last velocity a blast, a shove or a slime block set.
    pub launch: Option<(u64, [f64; 3])>,
    /// The last tick a moving block shoved it.
    pub piston: Option<u64>,
}

/// How the server moved a vehicle its driver's client simulates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VehicleMotion {
    /// A blast or a shove gave it this velocity, which reaches the client
    /// only through the driver's session.
    Impulse([f64; 3]),
    /// A moving block shoved it, a slime block setting this velocity. The
    /// client runs the piston itself.
    Piston { bounce: Option<[f64; 3]> },
    /// How fast its driver can ride it now, sent each tick it steers.
    Ridden(crate::world::entity::RiddenSpeed),
    /// The region refused or held the driver's move: the vehicle is here.
    Corrected([f64; 3]),
}

/// What a rider does with its vehicle, sent to the region holding it.
#[derive(Debug, Clone)]
pub enum VehicleRequest {
    /// The driver's client moved the vehicle.
    Move {
        player: PlayerId,
        vehicle: i32,
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    /// The driver's paddle input.
    Steer {
        player: PlayerId,
        vehicle: i32,
        left: bool,
        right: bool,
    },
    /// The driver's movement keys and current facing, for a mount the
    /// server itself steers (see `RegionEntity::rideable_by_input`); sent
    /// once a tick while driving one, so its yaw keeps tracking the
    /// rider's even between key changes. Ignored by anything else.
    Drive {
        player: PlayerId,
        vehicle: i32,
        input: crate::world::entity::RiderInput,
    },
    /// The rider used a stick-held lure to spur its mount on, answered
    /// with [`PlayerEvent::VehicleBoosted`] when it worked.
    Boost { player: PlayerHandle, vehicle: i32 },
    /// The rider opens its vehicle's inventory (the inventory key while
    /// riding a chest boat).
    OpenInventory { player: PlayerHandle, vehicle: i32 },
    /// The rider gets off, answered with [`PlayerEvent::Dismounted`].
    Dismount {
        player: PlayerHandle,
        vehicle: i32,
        width: f64,
        yaw: f32,
    },
    /// The rider disconnected: it leaves its seat but the vehicle keeps its
    /// UUID as an offline rider so it remounts on rejoin (see
    /// [`RegionRequest::ReclaimVehicle`]) or survives a restart.
    LeaveOffline {
        player: PlayerId,
        vehicle: i32,
        uuid: u128,
    },
}

/// A boat item used on what the player looks at.
#[derive(Debug, Clone)]
pub struct BoatPlace {
    pub player: PlayerHandle,
    pub eye: [f64; 3],
    pub look: [f64; 3],
    pub reach: f64,
    pub yaw: f32,
    pub kind: tuff_vanilla_data::registry::EntityType,
    pub item: Item,
    /// The stack's custom name, which the boat takes.
    pub name: Option<tuff_nbt::Tag>,
    pub sequence: i32,
    pub off_hand: bool,
}

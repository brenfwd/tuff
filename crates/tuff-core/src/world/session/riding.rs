//! Riding vehicles: boats and other seated entities.

use crate::world::messages::{GivenMotion, VehicleMotion, VehicleRequest};

use tuff_vanilla_data::registry::EntityType;

use super::*;
use crate::protocol::play::GameMode;
use crate::world::messages::RegionRequest;
use crate::world::player::Fall;

/// Ticks before a player who followed its vehicle through a portal can
/// use one itself.
const PORTAL_RIDE_COOLDOWN: u32 = 10;

/// Ticks after leaving a vehicle before boarding another.
pub const BOARDING_COOLDOWN: u32 = 60;

impl PlayerSession {
    /// After joining, gets back on a vehicle this player was riding when it
    /// last disconnected (or that a save caught it riding), if one of the
    /// regions around its spawn point still has it as an offline rider.
    /// A miss (vehicle moved to another region, unloaded elsewhere, or
    /// never existed) is silent: the player simply stands.
    pub async fn reclaim_vehicle(&mut self) {
        if self.ride.riding.is_some() {
            return;
        }
        let home = self.chunk().region();
        if let Some(region) = self.region_in(self.dimension, home).await {
            region
                .request(RegionRequest::ReclaimVehicle {
                    player: self.conn.handle.clone(),
                    uuid: self.profile.id,
                })
                .await;
        }
    }

    /// Disconnecting while riding: the seat empties for viewers, but the
    /// vehicle keeps this player's UUID as an offline rider (persisted
    /// through its own save) instead of putting it back on solid ground,
    /// so it remounts on rejoin -- see `RegionRequest::ReclaimVehicle`.
    pub async fn leave_vehicle_offline(&mut self) {
        let Some(riding) = self.ride.riding.take() else {
            return;
        };
        self.ride.dismounting = false;
        let at = BlockPos::containing(riding.position);
        if let Some(region) = self.region_of(at) {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::LeaveOffline {
                    player: self.id,
                    vehicle: riding.vehicle,
                    uuid: self.profile.id,
                }))
                .await;
        }
    }

    /// The region put this player on a vehicle or changed its riders.
    pub async fn ride(&mut self, mut riding: Box<crate::world::messages::Riding>) {
        if riding.dimension != self.dimension || self.is_dead() {
            return;
        }
        let seat = self.seat_position(&riding);
        let chunk_before = self.chunk();
        if self
            .ride
            .riding
            .as_ref()
            .is_some_and(|current| current.vehicle == riding.vehicle)
        {
            let moved = [0, 1, 2].map(|i| seat[i] - self.pose.position[i]);
            self.sync.known_movement = moved;
            self.riding_stats(moved);
        }
        self.pose.position = seat;
        self.body.fall = Fall::default();
        let mounted = match &self.ride.riding {
            Some(current) if current.vehicle == riding.vehicle => {
                riding.given = current.given;
                false
            }
            _ => true,
        };
        self.ride.riding = Some(riding);
        if mounted {
            self.fire(crate::world::advancement::trigger::Event::StartedRiding);
        }
        if self.chunk() != chunk_before {
            self.update_view().await;
        }
    }

    /// The vehicle this player rides went through a portal: it follows,
    /// and the target region seats it on the vehicle once it arrives.
    pub async fn portal_ride(&mut self, dimension: DimensionId, position: [f64; 3], turn: f32) {
        if self.ride.riding.take().is_none() || self.is_dead() {
            return;
        }
        self.ride.dismounting = false;
        let (yaw, pitch) = (self.pose.yaw + turn, self.pose.pitch);
        self.relocate(
            dimension,
            position,
            yaw,
            pitch,
            crate::protocol::play::respawn::KEEP_ALL_DATA,
        )
        .await;
        self.send(&crate::protocol::play::LevelEvent {
            event: crate::protocol::play::level_event::SOUND_PORTAL_TRAVEL,
            position: BlockPos::default(),
            data: 0,
            global: false,
        });
        self.actions.portal_cooldown = PORTAL_RIDE_COOLDOWN;
    }

    /// Where this player is while riding: its seat less the point it sits
    /// on.
    pub fn seat_position(&self, riding: &crate::world::messages::Riding) -> [f64; 3] {
        let attachment = EntityType::Player.vehicle_attachment();
        [0, 1, 2].map(|i| riding.position[i] + riding.seat[i] - attachment[i])
    }

    /// The region took this player off its vehicle: it stands at
    /// `position` (not finite when the vehicle is gone).
    pub async fn dismounted(&mut self, vehicle: i32, position: [f64; 3]) {
        if self
            .ride
            .riding
            .as_ref()
            .is_none_or(|r| r.vehicle != vehicle)
        {
            return;
        }
        self.ride.riding = None;
        self.ride.dismounting = false;
        self.ride.boarding_cooldown = BOARDING_COOLDOWN;
        self.body.fall = Fall::default();
        if position.iter().all(|c| c.is_finite()) && !self.is_dead() {
            let width = f64::from(EntityType::Player.size().width);
            self.surroundings.contact.cramped =
                crate::world::block_contact::cramped(position, width, &|pos| {
                    self.view.mirror.block_at(pos)
                });
            self.pose.position = position;
            self.teleport(position);
            self.publish(true).await;
        }
    }

    /// Gets off the vehicle without waiting for the answer (dying, leaving,
    /// changing dimension).
    pub async fn leave_vehicle(&mut self) {
        let Some(riding) = self.ride.riding.take() else {
            return;
        };
        self.ride.dismounting = false;
        self.ride.boarding_cooldown = BOARDING_COOLDOWN;
        let others: Vec<RegionHandle> = self
            .view
            .regions
            .iter()
            .filter(|((dimension, _), _)| *dimension == self.dimension)
            .map(|(_, region)| region.clone())
            .collect();
        for region in others {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::Dismount {
                    player: self.conn.handle.clone(),
                    vehicle: riding.vehicle,
                    width: f64::from(EntityType::Player.size().width),
                    yaw: self.pose.yaw,
                }))
                .await;
        }
    }

    /// Sneaking gets the rider off.
    pub async fn tick_riding(&mut self) {
        let Some(riding) = &self.ride.riding else {
            return;
        };
        if !self.pose.sneaking || self.ride.dismounting {
            return;
        }
        self.ride.dismounting = true;
        let (vehicle, at) = (riding.vehicle, BlockPos::containing(riding.position));
        if let Some(region) = self.region_of(at) {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::Dismount {
                    player: self.conn.handle.clone(),
                    vehicle,
                    width: f64::from(EntityType::Player.size().width),
                    yaw: self.pose.yaw,
                }))
                .await;
        }
    }

    /// The server moved the vehicle this player drives. Its client runs
    /// the vehicle, so an impulse goes on to it; both kinds are noted for
    /// the anti-cheat.
    pub fn vehicle_motion(&mut self, vehicle: i32, motion: VehicleMotion) {
        let tick = self.tick;
        let Some(riding) = self
            .ride
            .riding
            .as_mut()
            .filter(|riding| riding.vehicle == vehicle && riding.driver)
        else {
            return;
        };
        let given = &mut riding.given;
        let launch = |given: &mut GivenMotion, velocity: [f64; 3]| {
            let higher = given
                .launch
                .is_none_or(|(at, before)| at != tick || velocity[1] >= before[1]);
            if higher {
                given.launch = Some((tick, velocity));
            }
        };
        match motion {
            VehicleMotion::Impulse(velocity) => {
                launch(given, velocity);
                self.send(&play::SetEntityMotion {
                    entity_id: vehicle,
                    velocity,
                });
            }
            VehicleMotion::Piston { bounce } => {
                if let Some(velocity) = bounce {
                    launch(given, velocity);
                }
                given.piston = Some(tick);
            }
            VehicleMotion::Ridden(speed) => riding.ridden = Some(speed),
        }
    }

    /// The driver's client moved the vehicle.
    pub async fn drive(&mut self, position: [f64; 3], yaw: f32, pitch: f32, on_ground: bool) {
        let dead = self.is_dead();
        let Some(riding) = self.ride.riding.as_mut() else {
            return;
        };
        if !riding.driver
            || dead
            || !position.iter().all(|c| c.is_finite())
            || !yaw.is_finite()
            || !pitch.is_finite()
        {
            return;
        }
        let (vehicle, from) = (riding.vehicle, BlockPos::containing(riding.position));
        riding.position = position;
        let seat = self.seat_position(self.ride.riding.as_ref().expect("riding"));
        let chunk_before = self.chunk();
        self.sync.known_movement = [0, 1, 2].map(|i| seat[i] - self.pose.position[i]);
        self.riding_stats(self.sync.known_movement);
        self.pose.position = seat;
        self.body.fall = Fall::default();
        if let Some(region) = self.region_of(from) {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::Move {
                    player: self.id,
                    vehicle,
                    position,
                    yaw,
                    pitch,
                    on_ground,
                }))
                .await;
        }
        if self.chunk() != chunk_before {
            self.update_view().await;
        }
    }

    /// While on a vehicle that takes its rider's keys (a saddled, tamed
    /// horse, a minecart -- see `RegionEntity::takes_rider_input`), reports
    /// this tick's held movement keys and facing to the region. Boats move
    /// from their own client-reported position instead (see `drive`
    /// above).
    pub async fn tick_driving(&mut self) {
        let Some(riding) = &self.ride.riding else {
            return;
        };
        if !riding.steers || self.is_dead() {
            return;
        }
        let (vehicle, at) = (riding.vehicle, BlockPos::containing(riding.position));
        let flags = self.sync.input_flags;
        let forward = match (flags & 0x01 != 0, flags & 0x02 != 0) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => 0.0,
        };
        let strafe = match (flags & 0x04 != 0, flags & 0x08 != 0) {
            (true, false) => -1.0,
            (false, true) => 1.0,
            _ => 0.0,
        };
        let jumping = flags & 0x10 != 0;
        let Some(region) = self.region_of(at) else {
            return;
        };
        region
            .request(RegionRequest::Vehicle(VehicleRequest::Drive {
                player: self.id,
                vehicle,
                input: crate::world::entity::RiderInput {
                    forward,
                    strafe,
                    jumping,
                    yaw: self.pose.yaw,
                    pitch: self.pose.pitch,
                    sprinting: self.pose.sprinting,
                    jump_charge: self.sync.pending_jump.take(),
                    main_hand: self.inventory.hand(false).map(|s| s.item),
                    off_hand: self.inventory.hand(true).map(|s| s.item),
                },
            }))
            .await;
    }

    /// The inventory key while riding: opens the vehicle's container, if
    /// it has one.
    pub async fn open_vehicle_inventory(&mut self) {
        let Some(riding) = &self.ride.riding else {
            return;
        };
        let (vehicle, at) = (riding.vehicle, BlockPos::containing(riding.position));
        if let Some(region) = self.region_of(at) {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::OpenInventory {
                    player: self.conn.handle.clone(),
                    vehicle,
                }))
                .await;
        }
    }

    pub async fn paddle(&mut self, left: bool, right: bool) {
        let Some(riding) = &self.ride.riding else {
            return;
        };
        let (vehicle, at) = (riding.vehicle, BlockPos::containing(riding.position));
        if let Some(region) = self.region_of(at) {
            region
                .request(RegionRequest::Vehicle(VehicleRequest::Steer {
                    player: self.id,
                    vehicle,
                    left,
                    right,
                }))
                .await;
        }
    }

    /// A boat item: the region looks for where it goes. False when the
    /// held item is no boat.
    pub async fn place_boat(&mut self, off_hand: bool, sequence: i32) -> bool {
        let Some(stack) = self.inventory.hand(off_hand) else {
            return false;
        };
        let Some(kind) = stack.item.boat() else {
            return false;
        };
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return false;
        }
        let (item, name) = (
            stack.item,
            stack
                .components
                .custom_name
                .as_ref()
                .map(|name| name.to_nbt()),
        );
        let eye = self.eye();
        let Some(region) = self.region_of(BlockPos::containing(eye)) else {
            return false;
        };
        region
            .request(RegionRequest::PlaceBoat(
                crate::world::messages::BoatPlace {
                    player: self.conn.handle.clone(),
                    eye,
                    look: crate::world::raycast::view_vector(self.pose.yaw, self.pose.pitch),
                    reach: BLOCK_INTERACTION_RANGE,
                    yaw: self.pose.yaw,
                    kind,
                    item,
                    name,
                    sequence,
                    off_hand,
                },
            ))
            .await;
        true
    }
}

impl PlayerSession {
    /// Using a carrot or warped fungus on a stick on the pig or strider
    /// being ridden spurs it on; true when the use was this.
    pub async fn try_boost(&mut self, off_hand: bool) -> bool {
        let Some(item) = self.inventory.hand(off_hand).map(|s| s.item) else {
            return false;
        };
        let Some(riding) = &self.ride.riding else {
            return false;
        };
        let matches = matches!(
            (item, riding.kind),
            (Item::CarrotOnAStick, EntityType::Pig)
                | (Item::WarpedFungusOnAStick, EntityType::Strider)
        );
        if !matches || !riding.steers {
            return false;
        }
        let (vehicle, at) = (riding.vehicle, BlockPos::containing(riding.position));
        if let Some(region) = self.region_of(at) {
            self.motion.boost_hand = Some(off_hand);
            region
                .request(RegionRequest::Vehicle(VehicleRequest::Boost {
                    player: self.conn.handle.clone(),
                    vehicle,
                }))
                .await;
        }
        true
    }

    /// The mount took the spur: the stick wears, turning into a fishing
    /// rod when it breaks.
    pub async fn boosted(&mut self) {
        let Some(off_hand) = self.motion.boost_hand.take() else {
            return;
        };
        let Some(item) = self.inventory.hand(off_hand).map(|s| s.item) else {
            return;
        };
        let wear = if item == Item::CarrotOnAStick { 7 } else { 1 };
        self.hurt_hand(off_hand, wear).await;
        if !self.mode.abilities.instabuild && self.inventory.hand(off_hand).is_none() {
            self.inventory
                .set_hand(off_hand, Some(ItemStack::new(Item::FishingRod, 1)));
            self.send_inventory();
        }
    }
}

/// The vehicle this player rides and any unanswered dismount.
#[derive(Default)]
pub struct RideState {
    /// The vehicle this player rides, as its region last said.
    pub riding: Option<Box<crate::world::messages::Riding>>,
    /// A dismount was asked for and not answered yet.
    pub dismounting: bool,
    /// Ticks after getting off a vehicle during which no seat can be taken.
    pub boarding_cooldown: u32,
}

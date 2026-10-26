//! Riding: players getting on and off region entities that have seats and
//! driving them from their clients. Any region entity with seats
//! (`RegionEntity::seats`) takes riders; the region keeps the rider list on
//! the entity, tells viewers with Set Passengers and tells each rider where
//! it sits.

use super::{RegionActor, block_at, framed, packets};
use crate::protocol::play;
use crate::world::entity::{self, Rider, TrackedEntity};
use crate::world::messages::{PlayerEvent, PlayerHandle, Riding, VehicleMotion, VehicleRequest};
use crate::world::physics;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const VEHICLE_MOVED_TOO_QUICKLY: f64 = 100.0;
const VEHICLE_MOVED_WRONGLY: f64 = 0.0625;
const VEHICLE_SHRINK: f64 = 1.0e-5;

impl RegionActor {
    /// Tells viewers and every rider who rides `tracked` now.
    pub(super) fn announce_riders(&mut self, tracked: &mut TrackedEntity) {
        tracked.refresh_spawn();
        let base = tracked.entity.base();
        let chunk = tracked.chunk();
        let packet = packets([entity::passengers_packet(base)]);
        let id = base.id;
        self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
            entity: id,
            packets: packet.clone(),
        });
        self.tell_riders(tracked, false);
    }

    /// A blast or a shove gave a vehicle its driver's client moves some
    /// velocity: only that client acts on it, so the driver hears it
    /// before the vehicle's own tick or the driver's next move drops it.
    pub(super) fn forward_impulse(&self, tracked: &TrackedEntity) {
        let base = tracked.entity.base();
        if !tracked.sync.has_impulse()
            || base.velocity == [0.0; 3]
            || !tracked.entity.driven_by_rider()
        {
            return;
        }
        if let Some(driver) = base.riders.first() {
            driver.handle.send(PlayerEvent::VehicleMotion {
                vehicle: base.id,
                motion: VehicleMotion::Impulse(base.velocity),
            });
        }
    }

    /// Tells the driver of a vehicle its client moves that a moving block
    /// shoved it.
    pub(super) fn tell_piston_driver(&self, vehicle: i32, bounce: Option<[f64; 3]>) {
        let Some(tracked) = self.ents.entities.get(&vehicle) else {
            return;
        };
        if !tracked.entity.driven_by_rider() {
            return;
        }
        if let Some(driver) = tracked.entity.base().riders.first() {
            driver.handle.send(PlayerEvent::VehicleMotion {
                vehicle,
                motion: VehicleMotion::Piston { bounce },
            });
        }
    }

    /// The region moved a vehicle: every rider whose client does not move
    /// it hears where it went, so its session follows the seat.
    pub(super) fn follow_vehicle(&self, tracked: &TrackedEntity) {
        if !tracked.entity.base().riders.is_empty() {
            self.tell_riders(tracked, tracked.entity.controls_movement());
        }
    }

    /// Tells each rider (but the driver when `skip_driver`) where the
    /// vehicle is and where it sits.
    fn tell_riders(&self, tracked: &TrackedEntity, skip_driver: bool) {
        let base = tracked.entity.base();
        let riders = entity::all_passengers(base);
        let count = riders.len();
        for (driver, rider) in base.riders.iter().enumerate() {
            if skip_driver && driver == 0 {
                continue;
            }
            let index = riders
                .iter()
                .position(|id| *id == rider.id)
                .unwrap_or(driver);
            rider.handle.send(PlayerEvent::Riding(Box::new(Riding {
                vehicle: base.id,
                kind: base.kind,
                dimension: self.dimension,
                position: base.position,
                riders: riders.clone(),
                seat: tracked.entity.seat(index, count),
                driver: driver == 0 && tracked.entity.controls_movement(),
                steers: driver == 0 && tracked.entity.takes_rider_input(),
                no_gravity: base.no_gravity,
                flying: tracked.entity.flying_vehicle(),
                ridden: tracked.entity.ridden_speed(),
                given: Default::default(),
            })));
        }
    }

    /// A player's click on an entity with a free seat: it gets on. False
    /// when the entity has no seat for it.
    pub(super) fn mount(
        &mut self,
        tracked: &mut TrackedEntity,
        player: &PlayerHandle,
        uuid: u128,
    ) -> bool {
        let entity = &tracked.entity;
        let base = entity.base();
        if base.riders.len() >= entity.seats()
            || !entity.accepts_rider()
            || base.riders.iter().any(|rider| rider.id == player.id)
        {
            return false;
        }
        self.ents.rider_vehicles = None;
        tracked.entity.base_mut().riders.push(Rider {
            id: player.id,
            uuid,
            handle: player.clone(),
        });
        tracked.entity.base_mut().note_boarded(player.id);
        tracked.entity.on_mounted();
        self.announce_riders(tracked);
        crate::world::region_api::RegionWorld::game_event(
            self,
            crate::world::game_event::GameEvent::EntityMount,
            tracked.entity.base().position,
            crate::world::game_event::Context::from(crate::world::game_event::Source::player(
                player.id,
            )),
        );
        true
    }

    /// Takes `player` off `vehicle`, answering where it stands.
    pub(super) fn dismount(&mut self, player: &PlayerHandle, vehicle: i32, width: f64, yaw: f32) {
        let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
            player.send(PlayerEvent::Dismounted {
                vehicle,
                position: [f64::NAN; 3],
            });
            return;
        };
        let base = tracked.entity.base_mut();
        let before = base.riders.len();
        base.riders.retain(|rider| rider.id != player.id);
        let position = {
            let (columns, halo) = (&self.columns, &self.border.halo);
            let block = |pos: BlockPos| block_at(columns, halo, pos);
            tracked.entity.dismount_location(width, yaw, &block)
        };
        let left = tracked.entity.base().riders.len() != before;
        if left {
            self.announce_riders(&mut tracked);
        }
        let vehicle_position = tracked.entity.base().position;
        self.ents.entities.insert(vehicle, tracked);
        if left {
            crate::world::region_api::RegionWorld::game_event(
                self,
                crate::world::game_event::GameEvent::EntityDismount,
                vehicle_position,
                crate::world::game_event::Context::from(crate::world::game_event::Source::player(
                    player.id,
                )),
            );
        }
        player.send(PlayerEvent::Dismounted { vehicle, position });
    }

    /// Everyone riding an entity that is going away gets off.
    pub(super) fn eject_riders(&mut self, tracked: &mut TrackedEntity) {
        if tracked.entity.base().riders.is_empty() {
            return;
        }
        let riders = std::mem::take(&mut tracked.entity.base_mut().riders);
        let id = tracked.entity.base().id;
        for rider in riders {
            let position = {
                let (columns, halo) = (&self.columns, &self.border.halo);
                let block = |pos: BlockPos| block_at(columns, halo, pos);
                tracked
                    .entity
                    .dismount_location(0.6, tracked.entity.base().yaw, &block)
            };
            rider.handle.send(PlayerEvent::Dismounted {
                vehicle: id,
                position,
            });
        }
        self.announce_riders(tracked);
    }

    pub(super) fn vehicle_request(&mut self, request: VehicleRequest) {
        match request {
            VehicleRequest::Move {
                player,
                vehicle,
                position,
                yaw,
                pitch,
                on_ground,
            } => self.drive(player, vehicle, position, yaw, pitch, on_ground),
            VehicleRequest::OpenInventory { player, vehicle } => {
                let riding = self.ents.entities.get(&vehicle).is_some_and(|tracked| {
                    tracked
                        .entity
                        .base()
                        .riders
                        .iter()
                        .any(|r| r.id == player.id)
                        && tracked.entity.container_opens_for(player.id, true)
                });
                if riding {
                    self.open_entity_container(&player, vehicle);
                }
            }
            VehicleRequest::Steer {
                player,
                vehicle,
                left,
                right,
            } => {
                let Some(tracked) = self.ents.entities.get_mut(&vehicle) else {
                    return;
                };
                if tracked.entity.base().riders.first().map(|r| r.id) != Some(player) {
                    return;
                }
                tracked.entity.steer(left, right);
                let changed = tracked.entity.take_changed_data();
                if let Some(data) = tracked.delta_packets(&changed) {
                    let chunk = tracked.chunk();
                    self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
                        entity: vehicle,
                        packets: data.clone(),
                    });
                }
            }
            VehicleRequest::Drive {
                player,
                vehicle,
                input,
            } => {
                let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
                    return;
                };
                if tracked.entity.base().riders.first().map(|r| r.id) != Some(player)
                    || !tracked.entity.takes_rider_input()
                {
                    self.ents.entities.insert(vehicle, tracked);
                    return;
                }
                let controlled = tracked.entity.controls_movement();
                tracked.entity.steer_with_input(input);
                if tracked.entity.controls_movement() != controlled {
                    self.tell_riders(&tracked, false);
                }
                let tracked = self.ents.entities.get_or_insert(vehicle, tracked);
                if let (Some(speed), Some(driver)) = (
                    tracked.entity.ridden_speed(),
                    tracked.entity.base().riders.first(),
                ) {
                    driver.handle.send(PlayerEvent::VehicleMotion {
                        vehicle,
                        motion: VehicleMotion::Ridden(speed),
                    });
                }
            }
            VehicleRequest::Boost { player, vehicle } => {
                let roll = rand::Rng::r#gen::<u32>(&mut self.rng);
                let Some(tracked) = self.ents.entities.get_mut(&vehicle) else {
                    return;
                };
                if tracked.entity.base().riders.first().map(|r| r.id) == Some(player.id)
                    && tracked.entity.boost(roll)
                {
                    player.send(PlayerEvent::VehicleBoosted { vehicle });
                }
            }
            VehicleRequest::Dismount {
                player,
                vehicle,
                width,
                yaw,
            } => self.dismount(&player, vehicle, width, yaw),
            VehicleRequest::LeaveOffline {
                player,
                vehicle,
                uuid,
            } => self.leave_offline(vehicle, player, uuid),
        }
    }

    /// A rider disconnected: it leaves its seat, but the vehicle remembers
    /// its UUID so a rejoin (or a reload after this saves) remounts it.
    fn leave_offline(&mut self, vehicle: i32, player: i32, uuid: u128) {
        let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
            return;
        };
        let base = tracked.entity.base_mut();
        let before = base.riders.len();
        base.riders.retain(|rider| rider.id != player);
        if base.riders.len() != before {
            base.offline_riders.push(uuid);
            self.announce_riders(&mut tracked);
        }
        self.ents.entities.insert(vehicle, tracked);
    }

    /// A rejoining player whose UUID is an offline rider of one of this
    /// region's entities gets back on it.
    pub(super) fn reclaim_offline_rider(&mut self, player: &PlayerHandle, uuid: u128) -> bool {
        let Some(id) = self.ents.entities.iter().find_map(|(id, tracked)| {
            tracked
                .entity
                .base()
                .offline_riders
                .contains(&uuid)
                .then_some(*id)
        }) else {
            return false;
        };
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        tracked
            .entity
            .base_mut()
            .offline_riders
            .retain(|rider| *rider != uuid);
        let mounted = self.mount(&mut tracked, player, uuid);
        self.ents.entities.insert(id, tracked);
        mounted
    }

    /// The baseline checks on a vehicle position its driver reports: more
    /// distance than one packet may cover, or a replay of the move against
    /// the world that ends elsewhere, or a body pushed into blocks it was
    /// not in.
    fn vehicle_move_refused(&self, tracked: &TrackedEntity, position: [f64; 3]) -> bool {
        let base = tracked.entity.base();
        let delta = [0, 1, 2].map(|i| position[i] - base.position[i]);
        let moved_sq: f64 = delta.iter().map(|v| v * v).sum();
        let velocity_sq: f64 = base.velocity.iter().map(|v| v * v).sum();
        if moved_sq - velocity_sq > VEHICLE_MOVED_TOO_QUICKLY {
            return true;
        }
        let (columns, halo) = (&self.columns, &self.border.halo);
        let lookup = |p: BlockPos| block_at(columns, halo, p);
        let old_box = tracked.entity.bounds();
        let reach = physics::expand_towards(old_box, delta);
        let chunk = |value: f64| (value.floor() as i32) >> 4;
        let loaded = (chunk(reach[0])..=chunk(reach[3])).all(|x| {
            (chunk(reach[2])..=chunk(reach[5])).all(|z| {
                let at = ChunkPos::new(x, z);
                self.columns.contains_key(&at) || halo.contains_key(&at)
            })
        });
        if !loaded {
            return false;
        }
        let step = base
            .kind
            .base_attribute(tuff_vanilla_data::registry::Attribute::StepHeight)
            .unwrap_or(0.0) as f32;
        let moved = physics::collide(old_box, delta, step, base.on_ground, &lookup);
        let off = [delta[0] - moved[0], delta[2] - moved[2]];
        let wrong = off[0] * off[0] + off[1] * off[1] > VEHICLE_MOVED_WRONGLY;
        let shrink = |bounds| physics::inflate(bounds, [-VEHICLE_SHRINK; 3]);
        let new_box = physics::offset(old_box, delta);
        (wrong && physics::no_collision(shrink(old_box), &lookup))
            || physics::colliders(shrink(new_box), &lookup)
                .into_iter()
                .any(|shape| !physics::intersects(shape, shrink(old_box)))
    }

    /// The driver's client moved the vehicle: it goes there if that is in
    /// a loaded chunk, else the driver is put back.
    fn drive(
        &mut self,
        player: i32,
        vehicle: i32,
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    ) {
        let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
            return;
        };
        let driver = tracked
            .entity
            .base()
            .riders
            .first()
            .filter(|rider| rider.id == player && tracked.entity.controls_movement())
            .map(|rider| rider.handle.clone());
        let Some(driver) = driver else {
            self.ents.entities.insert(vehicle, tracked);
            return;
        };
        self.forward_impulse(&tracked);
        let from = tracked.chunk();
        let to = ChunkPos::of_position(position);
        if !position.iter().all(|c| c.is_finite())
            || (to != from
                && !self.columns.contains_key(&to)
                && !self.border.halo.contains_key(&to))
            || self.vehicle_move_refused(&tracked, position)
        {
            let base = tracked.entity.base();
            driver.send(PlayerEvent::EntityPackets {
                entity: vehicle,
                packets: packets([framed(&play::MoveVehicle {
                    position: base.position,
                    yaw: base.yaw,
                    pitch: base.pitch,
                })]),
            });
            driver.send(PlayerEvent::VehicleMotion {
                vehicle,
                motion: VehicleMotion::Corrected(base.position),
            });
            self.ents.entities.insert(vehicle, tracked);
            return;
        }
        let reported = position;
        let position = tracked.entity.clamp_driven_move(position);
        if position != reported {
            driver.send(PlayerEvent::EntityPackets {
                entity: vehicle,
                packets: packets([framed(&play::MoveVehicle {
                    position,
                    yaw,
                    pitch,
                })]),
            });
            driver.send(PlayerEvent::VehicleMotion {
                vehicle,
                motion: VehicleMotion::Corrected(position),
            });
        }
        let to = ChunkPos::of_position(position);
        let base = tracked.entity.base_mut();
        let fall = base.position[1] - position[1];
        base.fall_distance = if on_ground {
            0.0
        } else {
            (base.fall_distance + fall).max(0.0)
        };
        base.position = position;
        base.yaw = yaw;
        base.pitch = pitch;
        base.on_ground = on_ground;
        if let Some(column) = self.columns.get_mut(&from) {
            column.entities_dirty = true;
        }
        self.tell_riders(&tracked, true);
        let movement = tracked.tick_movement();
        if movement.is_some() || to != from {
            let track = tracked.moved(self.dimension, to, movement.unwrap_or_default());
            self.send_to_viewers(&[from, to], || PlayerEvent::Track(track.clone()));
        }
        if self.pos.contains(to) {
            if let Some(column) = self.columns.get_mut(&to) {
                column.entities_dirty = true;
            }
            self.ents.entities.insert(vehicle, tracked);
            return;
        }
        let seq = tracked.seq;
        let handed = match self.border.neighbors.get(&to.region()).cloned() {
            Some(neighbor) => self
                .hand_entity(&neighbor, tracked.entity, seq)
                .map(|()| neighbor),
            None => Err(tracked.entity),
        };
        match handed {
            Err(entity) => {
                tracked.entity = entity;
                self.ents.entities.insert(vehicle, tracked);
            }
            Ok(neighbor) => self.hand_off_viewers(vehicle, &neighbor),
        }
    }
}

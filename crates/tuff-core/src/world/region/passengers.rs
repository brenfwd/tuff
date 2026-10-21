//! Entities riding other entities of the region: a skeleton on a spider, a
//! zombie on a chicken. Passengers are ordinary region entities linked to
//! their vehicle by id (`EntityBase::vehicle` and `entity_riders`). They
//! tick right after their vehicle, sit where it seats them, cross region
//! borders behind it, and are saved nested in its `Passengers`.

use std::collections::{HashMap, HashSet};

use super::{RegionActor, block_at};
use crate::world::entity::{self, Mount, RegionEntity, Rider, TrackedEntity};
use crate::world::messages::{PlayerEvent, RideAnswer, RideOp, RidePassenger};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::Block;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;

/// How deep vehicles may stack (a passenger's passenger ...).
pub(super) const MAX_STACK: usize = 16;

/// Where a passenger sits on `vehicle` in seat `index` of `count`.
pub(super) fn seat_position(
    vehicle: &dyn RegionEntity,
    passenger: &dyn RegionEntity,
    index: usize,
    count: usize,
) -> [f64; 3] {
    let base = vehicle.base().position;
    let seat = vehicle.seat(index, count);
    let attachment = passenger.vehicle_attachment();
    [0, 1, 2].map(|axis| base[axis] + seat[axis] - attachment[axis])
}

impl RegionActor {
    /// Loads the passengers saved nested in `id`'s `Passengers` and seats
    /// them; those that do not load stay saved as they were.
    pub(super) fn board_saved_passengers(&mut self, id: i32, announce: bool, depth: usize) {
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return;
        };
        if tracked.entity.base().passengers.is_empty() || depth >= MAX_STACK {
            return;
        }
        let saved = std::mem::take(&mut tracked.entity.base_mut().passengers);
        let shared = self.shared.clone();
        let mut kept = Vec::new();
        let mut boarded = Vec::new();
        for mut nbt in saved {
            let finalize = nbt.remove(entity::FINALIZE_SPAWN).is_some();
            if !nbt.contains_key("Pos") {
                let position = self.ents.entities[&id].entity.base().position;
                nbt.insert("Pos", Tag::List(position.map(Tag::Double).to_vec()));
            }
            let boardable = nbt
                .get_str("id")
                .is_some_and(|kind| shared.entity_types.knows(kind) || is_display_like(kind));
            match boardable
                .then(|| {
                    shared
                        .entity_types
                        .load_or_inert(&nbt, shared.next_entity_id())
                })
                .flatten()
            {
                Some(mut passenger) => {
                    if finalize {
                        let mut budget = 0;
                        let ((), effects) = self.with_context(&[], &mut budget, |cx| {
                            passenger.finalize_spawn(cx, 0);
                        });
                        let chunk = passenger.base().chunk();
                        self.apply_effects(passenger.base().id, chunk, effects);
                    }
                    boarded.push(passenger);
                }
                None => kept.push(nbt),
            }
        }
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return;
        };
        tracked.entity.base_mut().passengers = kept;
        if boarded.is_empty() {
            return;
        }
        for mut passenger in boarded {
            let passenger_id = passenger.base().id;
            let vehicle = &self.ents.entities[&id].entity;
            let index = entity::all_passengers(vehicle.base()).len();
            let position = seat_position(&**vehicle, &*passenger, index, index + 1);
            let base = passenger.base_mut();
            base.position = position;
            base.velocity = [0.0; 3];
            base.vehicle = Some(Mount {
                id,
                passengers: Vec::new(),
            });
            if let Some(tracked) = self.ents.entities.get_mut(&id) {
                tracked.entity.base_mut().entity_riders.push(passenger_id);
                tracked.entity.base_mut().note_boarded(passenger_id);
            }
            let tracked = TrackedEntity::new(passenger, 0);
            let chunk = tracked.chunk();
            if let Some(column) = self.columns.get_mut(&chunk) {
                column.entities_dirty = true;
            }
            if announce {
                let track = tracked.track(self.dimension, chunk);
                self.fanout_chunk(chunk, None, |_| PlayerEvent::Track(track.clone()));
            }
            self.ents.entities.insert(passenger_id, tracked);
            self.board_saved_passengers(passenger_id, announce, depth + 1);
        }
        self.announce_passengers(id);
    }

    /// Seats `passenger` on `vehicle`, both entities of this region; false
    /// if either is missing, it already rides, or it would ride itself.
    pub(super) fn start_riding(&mut self, passenger: i32, vehicle: i32) -> bool {
        if passenger == vehicle
            || !self.ents.entities.contains_key(&vehicle)
            || self
                .ents
                .entities
                .get(&passenger)
                .is_none_or(|p| p.entity.base().vehicle.is_some())
            || self.root_vehicle(vehicle) == passenger
        {
            return false;
        }
        if let Some(tracked) = self.ents.entities.get_mut(&passenger) {
            tracked.entity.base_mut().vehicle = Some(Mount {
                id: vehicle,
                passengers: Vec::new(),
            });
        }
        if let Some(tracked) = self.ents.entities.get_mut(&vehicle) {
            tracked.entity.base_mut().entity_riders.push(passenger);
            tracked.entity.base_mut().note_boarded(passenger);
        }
        self.seat_all(vehicle);
        self.announce_passengers(vehicle);
        self.vehicle_event(
            crate::world::game_event::GameEvent::EntityMount,
            passenger,
            vehicle,
        );
        true
    }

    fn vehicle_event(
        &mut self,
        event: crate::world::game_event::GameEvent,
        passenger: i32,
        vehicle: i32,
    ) {
        use crate::world::game_event::{Context, Source};
        let Some(position) = self
            .ents
            .entities
            .get(&vehicle)
            .map(|tracked| tracked.entity.base().position)
        else {
            return;
        };
        crate::world::region_api::RegionWorld::game_event(
            self,
            event,
            position,
            Context::from(Source::entity(passenger)),
        );
    }

    /// Takes `passenger` off whatever it rides, standing where its vehicle
    /// lets it off. False if it rides nothing.
    pub(super) fn stop_riding(&mut self, passenger: i32) -> bool {
        let Some(mount) = self
            .ents
            .entities
            .get_mut(&passenger)
            .and_then(|p| p.entity.base_mut().vehicle.take())
        else {
            return false;
        };
        let width = self.ents.entities.get(&passenger).map_or(0.6, |p| {
            let bounds = p.entity.bounds();
            bounds[3] - bounds[0]
        });
        let landing = self.ents.entities.get_mut(&mount.id).map(|vehicle| {
            vehicle
                .entity
                .base_mut()
                .entity_riders
                .retain(|rider| *rider != passenger);
            let yaw = vehicle.entity.base().yaw;
            let (columns, halo) = (&self.columns, &self.border.halo);
            let border = self.shared.dimension(self.dimension);
            let block = |pos: BlockPos| {
                if border.block_within_border(pos) {
                    block_at(columns, halo, pos)
                } else {
                    Some(Block::Barrier.default_state())
                }
            };
            vehicle.entity.dismount_location(width, yaw, &block)
        });
        if let Some(tracked) = self.ents.entities.get_mut(&passenger) {
            if let Some(position) = landing.filter(|p| p.iter().all(|c| c.is_finite())) {
                tracked.entity.base_mut().position = position;
            }
            tracked.refresh_spawn();
        }
        self.announce_passengers(mount.id);
        self.vehicle_event(
            crate::world::game_event::GameEvent::EntityDismount,
            passenger,
            mount.id,
        );
        true
    }

    /// Unlinks a vehicle that is going away from its passengers (they get
    /// off where it was), and a passenger from its vehicle.
    pub(super) fn unlink_removed(&mut self, tracked: &mut TrackedEntity) {
        let id = tracked.id();
        let riders = std::mem::take(&mut tracked.entity.base_mut().entity_riders);
        if !riders.is_empty() {
            let width_of = |t: &TrackedEntity| {
                let b = t.entity.bounds();
                b[3] - b[0]
            };
            for rider in riders {
                let landing = self.ents.entities.get(&rider).map(|p| {
                    let (columns, halo) = (&self.columns, &self.border.halo);
                    let border = self.shared.dimension(self.dimension);
                    let block = |pos: BlockPos| {
                        if border.block_within_border(pos) {
                            block_at(columns, halo, pos)
                        } else {
                            Some(Block::Barrier.default_state())
                        }
                    };
                    let yaw = tracked.entity.base().yaw;
                    tracked.entity.dismount_location(width_of(p), yaw, &block)
                });
                if let Some(passenger) = self.ents.entities.get_mut(&rider) {
                    let base = passenger.entity.base_mut();
                    base.vehicle = None;
                    if let Some(position) = landing.filter(|p| p.iter().all(|c| c.is_finite())) {
                        base.position = position;
                    }
                    passenger.refresh_spawn();
                }
            }
            tracked.refresh_spawn();
            let chunk = tracked.chunk();
            let packet = crate::world::packets([entity::passengers_packet(tracked.entity.base())]);
            self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
                entity: id,
                packets: packet.clone(),
            });
        }
        if let Some(mount) = tracked.entity.base_mut().vehicle.take()
            && let Some(vehicle) = self.ents.entities.get_mut(&mount.id)
        {
            vehicle.entity.base_mut().entity_riders.retain(|r| *r != id);
            self.announce_passengers(mount.id);
        }
    }

    /// The display-like parts riding custom entity `id`, which go away
    /// with it instead of dismounting.
    pub(super) fn custom_entity_parts(&self, id: i32) -> Vec<i32> {
        let Some(tracked) = self.ents.entities.get(&id) else {
            return Vec::new();
        };
        let base = tracked.entity.base();
        if crate::custom::entity_id(&base.tags).is_none() {
            return Vec::new();
        }
        base.entity_riders
            .iter()
            .copied()
            .filter(|r| {
                self.ents
                    .entities
                    .get(r)
                    .is_some_and(|p| is_display_like(p.entity.base().kind.name()))
            })
            .collect()
    }

    /// Tells viewers who rides `vehicle` now, and each passenger the list,
    /// so a viewer who sees a passenger first still seats it.
    pub(super) fn announce_passengers(&mut self, vehicle: i32) {
        let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
            return;
        };
        let everyone = entity::all_passengers(tracked.entity.base());
        for rider in tracked.entity.base().entity_riders.clone() {
            if let Some(passenger) = self.ents.entities.get_mut(&rider) {
                passenger.entity.base_mut().vehicle = Some(Mount {
                    id: vehicle,
                    passengers: everyone.clone(),
                });
                passenger.refresh_spawn();
            }
        }
        self.announce_riders(&mut tracked);
        self.ents.entities.insert(vehicle, tracked);
    }

    /// The entity at the bottom of `id`'s stack of vehicles.
    pub(super) fn root_vehicle(&self, mut id: i32) -> i32 {
        for _ in 0..MAX_STACK {
            match self
                .ents
                .entities
                .get(&id)
                .and_then(|t| t.entity.base().vehicle.as_ref())
            {
                Some(mount) if self.ents.entities.contains_key(&mount.id) => id = mount.id,
                _ => break,
            }
        }
        id
    }

    /// Moves every passenger of `vehicle` (and theirs) onto its seat.
    fn seat_all(&mut self, vehicle: i32) {
        let Some(tracked) = self.ents.entities.remove(&vehicle) else {
            return;
        };
        let seats = self.ents.seats_of(&tracked);
        self.ents.entities.insert(vehicle, tracked);
        for (rider, position) in seats {
            if let Some(passenger) = self.ents.entities.get_mut(&rider) {
                passenger.entity.base_mut().position = position;
            }
            self.seat_all(rider);
        }
    }

    /// Drops links to vehicles and passengers that are no longer here (one
    /// crossed into another region without the other), telling viewers.
    pub(super) fn prune_mounts(&mut self) {
        let mut stale_vehicles = Vec::new();
        let mut lost_riders = Vec::new();
        for (id, tracked) in &self.ents.entities {
            let base = tracked.entity.base();
            if let Some(mount) = &base.vehicle {
                let listed = self
                    .ents
                    .entities
                    .get(&mount.id)
                    .is_some_and(|v| v.entity.base().entity_riders.contains(id));
                if !listed {
                    stale_vehicles.push(*id);
                }
            }
            if base.entity_riders.iter().any(|rider| {
                self.ents
                    .entities
                    .get(rider)
                    .is_none_or(|p| p.entity.base().vehicle.as_ref().map(|m| m.id) != Some(*id))
            }) {
                lost_riders.push(*id);
            }
        }
        let found: HashSet<(i32, bool)> = stale_vehicles
            .iter()
            .map(|id| (*id, true))
            .chain(lost_riders.iter().map(|id| (*id, false)))
            .collect();
        let previous = std::mem::replace(&mut self.ents.broken_links, found);
        stale_vehicles.retain(|id| previous.contains(&(*id, true)));
        lost_riders.retain(|id| previous.contains(&(*id, false)));
        for id in stale_vehicles {
            if let Some(tracked) = self.ents.entities.get_mut(&id) {
                tracked.entity.base_mut().vehicle = None;
                tracked.refresh_spawn();
            }
        }
        for id in lost_riders {
            let Some(tracked) = self.ents.entities.get(&id) else {
                continue;
            };
            let keep: Vec<i32> = tracked
                .entity
                .base()
                .entity_riders
                .iter()
                .copied()
                .filter(|rider| {
                    self.ents
                        .entities
                        .get(rider)
                        .is_some_and(|p| p.entity.base().vehicle.as_ref().map(|m| m.id) == Some(id))
                })
                .collect();
            if let Some(tracked) = self.ents.entities.get_mut(&id) {
                tracked.entity.base_mut().entity_riders = keep;
            }
            self.announce_passengers(id);
        }
    }

    /// `ids` reordered so each passenger comes right after its vehicle,
    /// as passengers tick after what they ride.
    pub(super) fn ride_order(&self, ids: Vec<i32>) -> Vec<i32> {
        if !self
            .ents
            .entities
            .values()
            .any(|t| !t.entity.base().entity_riders.is_empty())
        {
            return ids;
        }
        let mut order = Vec::with_capacity(ids.len());
        let mut placed = HashSet::new();
        for id in ids {
            if self.rides_here(id) {
                continue;
            }
            self.push_stack(id, &mut order, &mut placed, 0);
        }
        order
    }

    fn rides_here(&self, id: i32) -> bool {
        self.ents
            .entities
            .get(&id)
            .and_then(|t| t.entity.base().vehicle.as_ref())
            .is_some_and(|mount| self.ents.entities.contains_key(&mount.id))
    }

    fn push_stack(&self, id: i32, order: &mut Vec<i32>, placed: &mut HashSet<i32>, depth: usize) {
        if depth > MAX_STACK || !placed.insert(id) {
            return;
        }
        order.push(id);
        if let Some(tracked) = self.ents.entities.get(&id) {
            for rider in &tracked.entity.base().entity_riders {
                self.push_stack(*rider, order, placed, depth + 1);
            }
        }
    }

    /// The saved form of `tracked` with its passengers nested in it.
    pub(super) fn save_with_passengers(&self, tracked: &TrackedEntity, depth: usize) -> Compound {
        let mut saved = entity::save(&*tracked.entity);
        let riders = &tracked.entity.base().entity_riders;
        if riders.is_empty() || depth >= MAX_STACK {
            return saved;
        }
        let mut nested: Vec<Tag> = riders
            .iter()
            .filter_map(|rider| self.ents.entities.get(rider))
            .map(|passenger| Tag::Compound(self.save_with_passengers(passenger, depth + 1)))
            .collect();
        if let Some(Tag::List(rest)) = saved.remove("Passengers") {
            nested.extend(rest);
        }
        if !nested.is_empty() {
            saved.insert("Passengers", Tag::List(nested));
        }
        saved
    }

    /// Whether `id` is saved nested in a vehicle of this region rather than
    /// on its own.
    pub(super) fn saved_nested(&self, id: i32) -> bool {
        self.rides_here(id)
    }

    /// Every entity riding (directly or not) one of `roots`.
    pub(super) fn stacks_on(&self, roots: &[i32]) -> Vec<i32> {
        let mut out = Vec::new();
        let mut placed = HashSet::new();
        for root in roots {
            self.push_stack(*root, &mut out, &mut placed, 0);
        }
        out.retain(|id| !roots.contains(id));
        out
    }

    /// Handles the get-on and get-off requests entities made this tick.
    pub(super) fn handle_mount_requests(&mut self) {
        for (passenger, vehicle) in std::mem::take(&mut self.ents.mount_requests) {
            match vehicle {
                Some(vehicle) => {
                    self.start_riding(passenger, vehicle);
                }
                None => {
                    self.stop_riding(passenger);
                }
            }
        }
    }

    /// Records where the passengers of `vehicle` sit after its tick, for
    /// their own turn this tick.
    pub(super) fn note_seats(&self, vehicle: &TrackedEntity, seats: &mut HashMap<i32, [f64; 3]>) {
        if vehicle.entity.base().entity_riders.is_empty() {
            return;
        }
        seats.extend(self.ents.seats_of(vehicle));
    }

    /// One `/ride` step, for the entities of this region.
    pub(super) fn ride(&mut self, op: RideOp) -> RideAnswer {
        match op {
            RideOp::VehicleOf { passenger } => match self.ents.vehicle_holding(passenger) {
                Some(vehicle) => self.vehicle_answer(vehicle),
                None => RideAnswer::NotHere,
            },
            RideOp::Mount { passenger, vehicle } => {
                let Some(tracked) = self.ents.entities.get(&vehicle) else {
                    return RideAnswer::NotHere;
                };
                if !takes_command_passengers(tracked.entity.base().kind) {
                    return RideAnswer::Failed;
                }
                match passenger {
                    RidePassenger::Entity(id) => {
                        if !self.ents.entities.contains_key(&id) {
                            let snapshot = self.entity_snapshot(tracked, false);
                            return RideAnswer::PassengerElsewhere(Box::new(snapshot));
                        }
                        if self.ents.carried_by(vehicle, id) {
                            return RideAnswer::Loop;
                        }
                        if self.start_riding(id, vehicle) {
                            self.vehicle_answer(vehicle)
                        } else {
                            RideAnswer::Failed
                        }
                    }
                    RidePassenger::Player { handle, uuid } => {
                        let Some(mut tracked) = self.ents.entities.remove(&vehicle) else {
                            return RideAnswer::NotHere;
                        };
                        let rider_id = handle.id;
                        let mut mounted_by = None;
                        if !tracked
                            .entity
                            .base()
                            .riders
                            .iter()
                            .any(|r| r.id == handle.id)
                        {
                            self.ents.rider_vehicles = None;
                            tracked.entity.base_mut().riders.push(Rider {
                                id: handle.id,
                                uuid,
                                handle,
                            });
                            tracked.entity.on_mounted();
                            self.announce_riders(&mut tracked);
                            mounted_by = Some(tracked.entity.base().position);
                        }
                        self.ents.entities.insert(vehicle, tracked);
                        if let Some(position) = mounted_by {
                            crate::world::region_api::RegionWorld::game_event(
                                self,
                                crate::world::game_event::GameEvent::EntityMount,
                                position,
                                crate::world::game_event::Context::from(
                                    crate::world::game_event::Source::player(rider_id),
                                ),
                            );
                        }
                        self.vehicle_answer(vehicle)
                    }
                }
            }
            RideOp::Board { nbt, vehicle } => {
                let Some(tracked) = self.ents.entities.get_mut(&vehicle) else {
                    return RideAnswer::NotHere;
                };
                let before = tracked.entity.base().entity_riders.len();
                tracked.entity.base_mut().passengers.push(nbt);
                self.board_saved_passengers(vehicle, true, 0);
                let boarded = self
                    .ents
                    .entities
                    .get(&vehicle)
                    .is_some_and(|t| t.entity.base().entity_riders.len() > before);
                if boarded {
                    self.vehicle_answer(vehicle)
                } else {
                    RideAnswer::Failed
                }
            }
            RideOp::Dismount { passenger } => {
                let Some(vehicle) = self.ents.vehicle_holding(passenger.id()) else {
                    return RideAnswer::NotHere;
                };
                let answer = self.vehicle_answer(vehicle);
                match passenger {
                    RidePassenger::Entity(id) => {
                        self.stop_riding(id);
                    }
                    RidePassenger::Player { handle, .. } => {
                        let yaw = self
                            .seen_players()
                            .iter()
                            .find(|p| p.handle.id == handle.id)
                            .map_or(0.0, |p| p.looks.yaw);
                        let width = f64::from(EntityType::Player.size().width);
                        self.dismount(&handle, vehicle, width, yaw);
                    }
                }
                answer
            }
        }
    }

    fn vehicle_answer(&self, vehicle: i32) -> RideAnswer {
        match self.ents.entities.get(&vehicle) {
            Some(tracked) => RideAnswer::Vehicle(Box::new(self.entity_snapshot(tracked, false))),
            None => RideAnswer::Failed,
        }
    }
}

/// Whether `/ride` may seat anything on an entity of `kind`: markers and
/// item spawners take no passengers, and kinds that are never saved
/// cannot carry one.
fn takes_command_passengers(kind: EntityType) -> bool {
    !matches!(
        kind,
        EntityType::Marker
            | EntityType::OminousItemSpawner
            | EntityType::LightningBolt
            | EntityType::FishingBobber
            | EntityType::Player
    )
}

/// Display, interaction and marker entities have no behaviour of their own
/// but ride their vehicle live, so custom entities can compose them.
fn is_display_like(id: &str) -> bool {
    matches!(
        EntityType::from_name(id),
        Some(
            EntityType::ItemDisplay
                | EntityType::BlockDisplay
                | EntityType::TextDisplay
                | EntityType::Interaction
                | EntityType::Marker
        )
    )
}

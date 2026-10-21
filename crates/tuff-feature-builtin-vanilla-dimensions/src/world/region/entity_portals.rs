//! Entities other than players passing through nether and end portals.
//! They go the moment they touch one, then wait out the entity portal
//! cooldown. A vehicle takes its passengers along, players included, who
//! follow through their session and are seated again on arrival; a
//! passenger never enters on its own.

use std::sync::Arc;

use tokio::sync::oneshot;

use crate::world::gateway::ENTITY_PORTAL_COOLDOWN;
use crate::world::portal::{self, PortalKind};
use crate::world::travel::{self, Borrowed, Regions};
use tuff_core::world::Shared;
use tuff_core::world::dimension::{DimensionId, THE_END};
use tuff_core::world::entity::{self, Mount, Rider, TrackedEntity};
use tuff_core::world::heightmap_kind::HeightmapKind;
use tuff_core::world::messages::{
    ManagerMessage, PlayerEvent, RegionHandle, RegionInternal, RegionRequest, RideAnswer, RideOp,
    RidePassenger, TimedTicket,
};
use tuff_core::world::region_api::{RegionHooks, RegionTask, RegionWorld};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Nether and end portals for entities other than players.
pub struct EntityPortalHooks;

impl RegionHooks for EntityPortalHooks {
    fn entity_moved(
        &self,
        world: &mut dyn RegionWorld,
        tracked: &mut TrackedEntity,
        _before: [f64; 3],
    ) {
        enter_portal(world, tracked);
    }
}

/// Where an entity comes out.
struct Arrival {
    dimension: DimensionId,
    position: [f64; 3],
    yaw: Turn,
    pitch: Option<f32>,
    /// Leaving the end for the world: a player who has not seen the credits
    /// keeps the stack where it is.
    leaves_end: bool,
}

enum Turn {
    To(f32),
    By(f32),
}

fn enter_portal(world: &mut dyn RegionWorld, tracked: &mut TrackedEntity) {
    let entity = &tracked.entity;
    let base = entity.base();
    if !entity.uses_portals() || base.vehicle.is_some() {
        return;
    }
    let bounds = entity.bounds();
    let Some(contact) = portal::contact_box(bounds, &|pos| world.block(pos)) else {
        return;
    };
    if contact.kind == PortalKind::Gateway {
        return;
    }
    if contact.kind == PortalKind::End
        && world.shared().dimension(world.dimension_id()).key == THE_END
        && unseen_credits(world, tracked)
    {
        return;
    }
    let base = tracked.entity.base_mut();
    let waiting = base.portal_cooldown > 0;
    base.portal_cooldown = cooldown_of(base);
    if waiting {
        return;
    }
    let size = [bounds[3] - bounds[0], bounds[4] - bounds[1]];
    let (shared, from, origin, id, position) = (
        world.shared().clone(),
        world.dimension_id(),
        world.handle(),
        base.id,
        base.position,
    );
    tokio::spawn(async move {
        let mut regions = Borrowed::new(shared.clone());
        let arrival = match contact.kind {
            PortalKind::Nether => travel::nether_exit(
                &mut regions,
                &shared,
                from,
                contact.pos,
                position,
                size,
                true,
            )
            .await
            .map(|exit| Arrival {
                dimension: exit.dimension,
                position: exit.position,
                yaw: Turn::By(exit.rotation),
                pitch: None,
                leaves_end: false,
            }),
            PortalKind::End => end_arrival(&mut regions, &shared, from).await,
            PortalKind::Gateway => None,
        };
        if let Some(arrival) = arrival {
            let _ = shared
                .manager
                .send(ManagerMessage::Hold {
                    dimension: arrival.dimension,
                    center: ChunkPos::of_position(arrival.position),
                    ticket: TimedTicket::Portal,
                })
                .await;
            let target = regions
                .region_in(
                    arrival.dimension,
                    ChunkPos::of_position(arrival.position).region(),
                )
                .await;
            if let Some(target) = target {
                origin.internal(RegionInternal::Run(RegionTask::new(move |world| {
                    transfer(world, id, arrival, target);
                })));
            }
        }
        regions.release().await;
    });
}

/// Whether a player aboard `root` or anything riding it has not seen the
/// credits.
fn unseen_credits(world: &dyn RegionWorld, root: &TrackedEntity) -> bool {
    let unseen =
        |base: &entity::EntityBase| base.riders.iter().any(|rider| !rider.handle.seen_credits());
    let base = root.entity.base();
    unseen(base)
        || base
            .entity_riders
            .iter()
            .filter_map(|member| world.entity(*member))
            .any(|tracked| unseen(tracked.entity.base()))
}

/// The wait before the entity can use a portal again: short when a player is
/// the first passenger.
fn cooldown_of(base: &entity::EntityBase) -> i32 {
    let first = entity::all_passengers(base).first().copied();
    if first.is_some_and(|id| base.riders.iter().any(|rider| rider.id == id)) {
        i32::try_from(portal::PLAYER_PORTAL_COOLDOWN).unwrap_or(10)
    } else {
        ENTITY_PORTAL_COOLDOWN
    }
}

/// From the end to the world spawn, else onto the end's platform.
async fn end_arrival(
    regions: &mut Borrowed,
    shared: &Arc<Shared>,
    from: DimensionId,
) -> Option<Arrival> {
    if shared.dimension(from).key != THE_END {
        let end = travel::end_platform(regions, shared).await?;
        let spawn = portal::END_SPAWN;
        return Some(Arrival {
            dimension: end,
            position: [
                f64::from(spawn.x) + 0.5,
                f64::from(spawn.y),
                f64::from(spawn.z) + 0.5,
            ],
            yaw: Turn::To(90.0),
            pitch: None,
            leaves_end: false,
        });
    }
    let spawn = shared.spawn();
    let dimension = shared.spawn_dimension();
    let center = [
        f64::from(spawn.pos.x) + 0.5,
        f64::from(spawn.pos.y),
        f64::from(spawn.pos.z) + 0.5,
    ];
    let region = regions
        .region_in(dimension, ChunkPos::of_block(spawn.pos).region())
        .await?;
    let (reply, answer) = oneshot::channel();
    let column = spawn.pos;
    region
        .request(RegionRequest::WhenLoaded {
            min: column,
            max: column,
            run: RegionTask::new(move |world| {
                let _ = reply.send(standing_above_column(world, column));
            }),
        })
        .await;
    Some(Arrival {
        dimension,
        position: answer.await.unwrap_or(center),
        yaw: Turn::By(spawn.yaw),
        pitch: Some(spawn.pitch),
        leaves_end: true,
    })
}

/// One block above the column's top that is not a leaf, bottom centre.
fn standing_above_column(world: &dyn RegionWorld, column: BlockPos) -> [f64; 3] {
    let shape = world.dimension_info().shape;
    let bottom = shape.min_y;
    let mut y = bottom + shape.height - 1;
    while y >= bottom {
        let state = world.block(BlockPos::new(column.x, y, column.z));
        if state.is_some_and(|state| HeightmapKind::MotionBlockingNoLeaves.is_opaque(state)) {
            break;
        }
        y -= 1;
    }
    [
        f64::from(column.x) + 0.5,
        f64::from(y + 2),
        f64::from(column.z) + 0.5,
    ]
}

/// Takes entity `id` and everything riding it out of this region and gives
/// them to `target` at the arrival, or puts them back if `target` is gone.
/// Players aboard follow through their sessions and are seated again by the
/// target region.
fn transfer(world: &mut dyn RegionWorld, id: i32, arrival: Arrival, target: RegionHandle) {
    let stack = stack_of(world, id);
    let carries = |world: &dyn RegionWorld, member: i32| {
        world
            .entity(member)
            .is_some_and(|tracked| tracked.entity.uses_portals())
    };
    if stack.is_empty() || !stack.iter().all(|member| carries(world, *member)) {
        return;
    }
    if arrival.leaves_end
        && stack.iter().any(|member| {
            world
                .entity(*member)
                .is_some_and(|tracked| unseen_credits(world, tracked))
        })
    {
        return;
    }
    let links: Vec<(Vec<i32>, Option<Mount>)> = stack
        .iter()
        .filter_map(|member| world.entity(*member))
        .map(|tracked| {
            let base = tracked.entity.base();
            (base.entity_riders.clone(), base.vehicle.clone())
        })
        .collect();
    let mut seated: Vec<(i32, Rider)> = Vec::new();
    for member in &stack {
        if let Some(tracked) = world.entity_mut(*member) {
            let base = tracked.entity.base_mut();
            let vehicle = base.id;
            seated.extend(
                std::mem::take(&mut base.riders)
                    .into_iter()
                    .map(|rider| (vehicle, rider)),
            );
        }
    }
    let mut taken = Vec::with_capacity(stack.len());
    for (member, (riders, vehicle)) in stack.iter().zip(links) {
        let Some(mut tracked) = world.remove_entity_kept(*member) else {
            continue;
        };
        let base = tracked.entity.base_mut();
        base.entity_riders = riders;
        base.vehicle = vehicle;
        taken.push(tracked);
    }
    let Some(root) = taken.first().map(|tracked| tracked.entity.base().position) else {
        return;
    };
    let mut moved = Vec::with_capacity(taken.len());
    let mut origin = Vec::with_capacity(taken.len());
    let mut root_turn = 0.0;
    for (index, tracked) in taken.into_iter().enumerate() {
        let seq = tracked.seq + 1;
        let mut entity = tracked.entity;
        entity.used_portal();
        if let Some(leash) = entity.leash_mut() {
            *leash = None;
        }
        let base = entity.base_mut();
        origin.push((base.position, base.velocity, base.yaw, base.pitch));
        let offset = [0, 1, 2].map(|axis| base.position[axis] - root[axis]);
        let turn = match arrival.yaw {
            Turn::To(yaw) => yaw - base.yaw,
            Turn::By(turn) => turn,
        };
        if index == 0 {
            root_turn = turn;
        }
        base.position = [0, 1, 2].map(|axis| arrival.position[axis] + offset[axis]);
        base.yaw += turn;
        if let Some(pitch) = arrival.pitch {
            base.pitch += pitch;
        }
        let (sin, cos) = f64::from(turn).to_radians().sin_cos();
        let [vx, vy, vz] = base.velocity;
        base.velocity = [vx * cos - vz * sin, vy, vx * sin + vz * cos];
        base.fall_distance = 0.0;
        base.portal_cooldown = if index == 0 {
            let hold = seated.first().is_some_and(|(vehicle, rider)| {
                *vehicle == base.id && entity::all_passengers(base).first() == Some(&rider.id)
            });
            if hold {
                i32::try_from(portal::PLAYER_PORTAL_COOLDOWN).unwrap_or(10)
            } else {
                ENTITY_PORTAL_COOLDOWN
            }
        } else {
            ENTITY_PORTAL_COOLDOWN
        };
        moved.push((entity, seq));
    }
    let mut returned = Vec::new();
    for (entity, seq) in moved {
        if let Err(entity) = target.try_entity(entity, seq) {
            returned.push(entity);
        }
    }
    let failed = !returned.is_empty();
    for (mut entity, (position, velocity, yaw, pitch)) in returned.into_iter().zip(origin) {
        let base = entity.base_mut();
        (base.position, base.velocity, base.yaw, base.pitch) = (position, velocity, yaw, pitch);
        world.add_entity(entity);
    }
    if failed {
        for (vehicle, rider) in seated {
            if let Some(tracked) = world.entity_mut(vehicle) {
                tracked.entity.base_mut().riders.push(rider);
            }
        }
        return;
    }
    for (_, rider) in &seated {
        rider.handle.send(PlayerEvent::PortalRide {
            dimension: arrival.dimension,
            position: arrival.position,
            turn: root_turn,
        });
    }
    if !seated.is_empty() {
        target.internal(RegionInternal::Run(RegionTask::new(move |world| {
            reseat(world, seated, SEAT_ATTEMPTS);
        })));
    }
}

const SEAT_ATTEMPTS: u32 = 40;

/// Seats the players that followed their vehicles, trying again while a
/// vehicle has not landed in the region yet.
fn reseat(world: &mut dyn RegionWorld, seated: Vec<(i32, Rider)>, attempts: u32) {
    let mut waiting = Vec::new();
    for (vehicle, rider) in seated {
        let answer = world.ride(RideOp::Mount {
            passenger: RidePassenger::Player {
                handle: rider.handle.clone(),
                uuid: rider.uuid,
            },
            vehicle,
        });
        if matches!(answer, RideAnswer::NotHere) {
            waiting.push((vehicle, rider));
        }
    }
    if waiting.is_empty() || attempts == 0 {
        return;
    }
    world
        .handle()
        .internal(RegionInternal::Run(RegionTask::new(move |world| {
            reseat(world, waiting, attempts - 1);
        })));
}

/// `id` followed by everything riding it, directly or not.
fn stack_of(world: &dyn RegionWorld, id: i32) -> Vec<i32> {
    let mut stack = Vec::new();
    let mut next = vec![id];
    while let Some(member) = next.pop() {
        if stack.contains(&member) || stack.len() > MAX_STACK {
            continue;
        }
        let Some(tracked) = world.entity(member) else {
            continue;
        };
        stack.push(member);
        next.extend(tracked.entity.base().entity_riders.iter().rev().copied());
    }
    stack
}

const MAX_STACK: usize = 16;

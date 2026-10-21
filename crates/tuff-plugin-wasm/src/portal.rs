//! Plugin portals: bodies inside blocks a plugin watches, and moving
//! players and entities between dimensions when a plugin asks.

use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::contact::{PLUGIN_PORTAL_COOLDOWN, PortalContact};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::entity::TrackedEntity;
use tuff_core::world::messages::{
    self, CoordinatorMessage, ManagerMessage, PlayerUpdate, RegionHandle, RegionInternal,
};
use tuff_core::world::region_api::{RegionHooks, RegionTask, RegionWorld};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks};
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use crate::bridge;
use crate::host::WasmHost;
use crate::sandbox::Player;
use crate::wit::bindings::types::{BlockPos as WitPos, Event, EventKinds, Inside};

/// Ticks an entity ignores a watched block after touching one.
const ENTITY_COOLDOWN: i32 = 300;

const HALF_WIDTH: f64 = 0.3;

/// The block id with its `minecraft:` namespace spelled out.
pub fn qualified(name: &str) -> String {
    match name.contains(':') {
        true => name.to_owned(),
        false => format!("minecraft:{name}"),
    }
}

/// Whether two block ids are the same block, the namespace optional.
pub fn same_block(a: &str, b: &str) -> bool {
    let bare = |name: &'_ str| name.strip_prefix("minecraft:").map(str::to_owned);
    a == b || bare(a).as_deref().unwrap_or(a) == bare(b).as_deref().unwrap_or(b)
}

/// Who a queued teleport moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Player(u128),
    Entity(u128),
}

/// A teleport a plugin queued.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Teleport {
    pub target: Target,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub rotation: Option<[f32; 2]>,
}

/// Carries out the teleports a call queued, in order for players and each
/// entity on its own task.
pub async fn apply(shared: &Arc<Shared>, teleports: Vec<Teleport>) {
    for teleport in teleports {
        match teleport.target {
            Target::Player(uuid) => {
                let _ = shared
                    .coordinator
                    .send(CoordinatorMessage::TeleportPlayer {
                        uuid,
                        teleport: messages::Teleport {
                            dimension: teleport.dimension,
                            position: teleport.position,
                            relative: [false; 3],
                            rotation: teleport.rotation,
                            relative_rotation: [false; 2],
                            look_at: None,
                        },
                    })
                    .await;
            }
            Target::Entity(uuid) => {
                tokio::spawn(move_entity(shared.clone(), uuid, teleport));
            }
        }
    }
}

async fn move_entity(shared: Arc<Shared>, uuid: u128, teleport: Teleport) {
    let (reply, regions) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return;
    }
    let mut found = None;
    for region in regions.await.unwrap_or_default() {
        let id = region
            .ask(move |world| {
                world.entity_ids().into_iter().find(|id| {
                    world
                        .entity(*id)
                        .is_some_and(|e| e.entity.base().uuid == uuid)
                })
            })
            .await
            .flatten();
        if let Some(id) = id {
            found = Some((region, id));
            break;
        }
    }
    let Some((origin, id)) = found else {
        return;
    };
    let region = ChunkPos::of_position(teleport.position).region();
    let (reply, handle) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Acquire {
            dimension: teleport.dimension,
            region,
            reply,
        })
        .await
        .is_err()
    {
        return;
    }
    if let Ok(target) = handle.await {
        origin.internal(RegionInternal::Run(RegionTask::new(move |world| {
            transfer(world, id, teleport, target);
        })));
    }
    let _ = shared
        .manager
        .send(ManagerMessage::Release {
            dimension: teleport.dimension,
            region,
        })
        .await;
}

fn transfer(world: &mut dyn RegionWorld, id: i32, teleport: Teleport, target: RegionHandle) {
    let Some(tracked) = world.remove_entity_intact(id) else {
        return;
    };
    let seq = tracked.seq + 1;
    let mut entity = tracked.entity;
    if let Some(leash) = entity.leash_mut() {
        *leash = None;
    }
    let base = entity.base_mut();
    let before = (base.position, base.yaw, base.pitch);
    base.position = teleport.position;
    if let Some([yaw, pitch]) = teleport.rotation {
        base.yaw = yaw;
        base.pitch = pitch;
    }
    base.fall_distance = 0.0;
    base.portal_cooldown = ENTITY_COOLDOWN;
    if let Err(mut entity) = target.try_entity(entity, seq) {
        let base = entity.base_mut();
        (base.position, base.yaw, base.pitch) = before;
        world.add_entity(entity);
    }
}

/// The first watched block overlapping `bounds` (min x, y, z, max x, y, z).
fn find(
    watched: &[String],
    bounds: [f64; 6],
    block: &mut dyn FnMut(BlockPos) -> Option<BlockState>,
) -> Option<(BlockPos, BlockState)> {
    const DEFLATE: f64 = 1.0e-5;
    let range = |axis: usize| {
        (bounds[axis] + DEFLATE).floor() as i32..=((bounds[axis + 3] - DEFLATE).ceil() as i32 - 1)
    };
    for y in range(1) {
        for z in range(2) {
            for x in range(0) {
                let pos = BlockPos::new(x, y, z);
                let Some(state) = block(pos) else {
                    continue;
                };
                let name = state.block().name();
                if watched.iter().any(|w| same_block(w, name)) {
                    return Some((pos, state));
                }
            }
        }
    }
    None
}

/// Reports where watched blocks are touched: players in their home region,
/// entities as they move.
pub struct InsideHooks;

impl RegionHooks for InsideHooks {
    fn portal_contact(
        &self,
        world: &mut dyn RegionWorld,
        update: &PlayerUpdate,
    ) -> Option<PortalContact> {
        let shared = world.shared().clone();
        let host = shared.services.get::<WasmHost>()?;
        if !host.watching() {
            return None;
        }
        let watched = host.snapshot();
        let [x, y, z] = update.position;
        let bounds = [
            x - HALF_WIDTH,
            y,
            z - HALF_WIDTH,
            x + HALF_WIDTH,
            y + tuff_core::world::PLAYER_HEIGHT,
            z + HALF_WIDTH,
        ];
        let (pos, block) = find(&watched.watched, bounds, &mut |pos| world.block(pos))?;
        Some(PortalContact {
            pos,
            block,
            claimed: true,
        })
    }

    fn entity_moved(
        &self,
        world: &mut dyn RegionWorld,
        tracked: &mut TrackedEntity,
        _before: [f64; 3],
    ) {
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.watching() || !host.listens(EventKinds::INSIDE_BLOCK) {
            return;
        }
        let entity = &tracked.entity;
        let base = entity.base();
        if !entity.uses_portals()
            || base.vehicle.is_some()
            || !base.riders.is_empty()
            || !base.entity_riders.is_empty()
        {
            return;
        }
        let watched = host.snapshot();
        let Some((pos, block)) = find(&watched.watched, entity.bounds(), &mut |pos| {
            world.block(pos)
        }) else {
            return;
        };
        let waiting = tracked.entity.base().portal_cooldown > 0;
        tracked.entity.base_mut().portal_cooldown = ENTITY_COOLDOWN;
        if waiting {
            return;
        }
        let inside = Inside {
            is_player: false,
            uuid: crate::sandbox::uuid(tracked.entity.base().uuid),
            name: tracked.entity.base().kind.name().to_owned(),
            pos: WitPos {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            },
            dimension: world.dimension_info().key.clone(),
            state: block.to_string(),
        };
        host.event(Event::InsideBlock(inside), None);
    }
}

/// Reports the home region's portal contacts plugins claimed.
pub struct Contacts;

impl SessionHooks for Contacts {
    fn surroundings<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        portal: Option<PortalContact>,
    ) -> HookFuture<'a> {
        if let Some(contact) = portal.filter(|contact| contact.claimed) {
            inside(session, contact);
        }
        Box::pin(async {})
    }
}

fn inside(session: &mut PlayerSession, contact: PortalContact) {
    let shared = session.shared.clone();
    let Some(host) = shared.services.get::<WasmHost>() else {
        return;
    };
    let waiting = session.actions.portal_cooldown > 0;
    session.actions.portal_cooldown = PLUGIN_PORTAL_COOLDOWN;
    if waiting || !host.listens(EventKinds::INSIDE_BLOCK) {
        return;
    }
    let who: Player = bridge::player(session);
    let dimension = session.shared.dimensions.get(session.dimension).key.clone();
    let inside = Inside {
        is_player: true,
        uuid: crate::sandbox::uuid(who.uuid),
        name: who.name.clone(),
        pos: WitPos {
            x: contact.pos.x,
            y: contact.pos.y,
            z: contact.pos.z,
        },
        dimension,
        state: contact.block.to_string(),
    };
    host.event(Event::InsideBlock(inside), Some(who));
}

//! End gateways in a region: their age and beams, entities passing
//! through, and the region's part in finding where they lead.
//!
//! Gateways stay saved block entity NBT; the region finds them among its
//! chunks' raw block entities every second and ticks them every tick.
//! Cooldowns are not saved, as in the game.

use std::collections::HashMap;

use tokio::sync::oneshot;
use tuff_vanilla_data::registry::EntityType;

use crate::world::gateway::{self, ExitLookup, GatewayRequest};
use crate::world::portal;
use tuff_core::world::contact::PortalContact;
use tuff_core::world::entity::TrackedEntity;
use tuff_core::world::messages::{ManagerMessage, PlayerUpdate, RegionInternal};
use tuff_core::world::region_api::{RegionHooks, RegionWorld, TickPhase};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Gateway blocks are looked for again this often.
const RESCAN_TICKS: u64 = 20;

/// A region's gateways and what they are doing.
#[derive(Default)]
struct Gateways {
    /// Gateways among the loaded chunks, found every second.
    found: Vec<BlockPos>,
    scanned: u64,
    /// Gateways cooling down after a use, with the tick it ends.
    cooldowns: HashMap<BlockPos, u64>,
    /// Gateways whose exit someone is building, with those waiting on it.
    searches: HashMap<BlockPos, Vec<oneshot::Sender<ExitLookup>>>,
}

/// End gateways' region hooks.
pub struct GatewayHooks;

impl RegionHooks for GatewayHooks {
    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase == TickPhase::Late {
            tick_gateways(world);
        }
    }

    fn entity_moved(
        &self,
        world: &mut dyn RegionWorld,
        tracked: &mut TrackedEntity,
        before: [f64; 3],
    ) {
        entity_gateway(world, tracked, before);
    }

    fn update_shape(
        &self,
        state: tuff_vanilla_data::BlockState,
        pos: BlockPos,
        direction: tuff_vanilla_data::block::Facing,
        neighbour: tuff_vanilla_data::BlockState,
        level: &dyn Fn(BlockPos) -> tuff_vanilla_data::BlockState,
        min_y: i32,
    ) -> Option<tuff_vanilla_data::BlockState> {
        (state.block() == tuff_vanilla_data::Block::NetherPortal).then(|| {
            if portal::portal_survives(level, pos, min_y, state, direction, neighbour) {
                state
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            }
        })
    }

    fn spawn_position(
        &self,
        world: &mut dyn RegionWorld,
        center: BlockPos,
        radius: i32,
        offset: u32,
    ) -> Option<[f64; 3]> {
        Some(tuff_vanilla_common::spawn_search::find_spawn(
            world, center, radius, offset,
        ))
    }

    fn portal_contact(
        &self,
        world: &mut dyn RegionWorld,
        update: &PlayerUpdate,
    ) -> Option<PortalContact> {
        let contact = portal::contact(update.position, &|pos| world.block(pos))?;
        if claimed_by_plugin(world, contact.pos) {
            return None;
        }
        if contact.kind == portal::PortalKind::Gateway && !enter_gateway(world, contact.pos) {
            return None;
        }
        Some(PortalContact {
            pos: contact.pos,
            block: world.block(contact.pos)?,
            claimed: false,
        })
    }
}

fn overlaps_block(bounds: [f64; 6], pos: BlockPos) -> bool {
    const DEFLATE: f64 = 1.0e-5;
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    bounds[0] + DEFLATE < x + 1.0
        && bounds[3] - DEFLATE > x
        && bounds[1] + DEFLATE < y + 1.0
        && bounds[4] - DEFLATE > y
        && bounds[2] + DEFLATE < z + 1.0
        && bounds[5] - DEFLATE > z
}

fn claimed_by_plugin(world: &mut dyn RegionWorld, pos: BlockPos) -> bool {
    let shared = world.shared().clone();
    let Some(claims) = shared
        .services
        .get::<tuff_core::world::contact::PortalClaims>()
    else {
        return false;
    };
    if claims.is_empty() || world.dimension_info().key == tuff_core::storage::level::THE_END {
        return false;
    }
    world
        .block(pos)
        .is_some_and(|state| claims.claims(state.block().name()))
}

fn scan_gateways(world: &mut dyn RegionWorld) {
    let mut found = world.raw_block_entities_where(&gateway::is_gateway);
    found.retain(|pos| !claimed_by_plugin(world, *pos));
    found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
    let state = world.state().get::<Gateways>();
    state.cooldowns.retain(|pos, _| {
        found
            .binary_search_by_key(&(pos.y, pos.z, pos.x), |p| (p.y, p.z, p.x))
            .is_ok()
    });
    state.found = found;
}

fn gateway_nbt(world: &mut dyn RegionWorld, pos: BlockPos) -> Option<&mut Compound> {
    world
        .raw_block_entity_mut(pos)
        .filter(|nbt| gateway::is_gateway(nbt))
}

/// Ages every gateway, counts down cooldowns and flashes each beam every
/// 2400 ticks.
fn tick_gateways(world: &mut dyn RegionWorld) {
    let tick = world.tick();
    let state = world.state().get::<Gateways>();
    if tick >= state.scanned + RESCAN_TICKS {
        state.scanned = tick;
        scan_gateways(world);
    }
    for pos in world.state().get::<Gateways>().found.clone() {
        let cooling = world.state().get::<Gateways>().cooldowns.get(&pos).copied();
        let Some(nbt) = gateway_nbt(world, pos) else {
            continue;
        };
        let age = gateway::age(nbt) + 1;
        gateway::set_age(nbt, age);
        let mut changed = age == gateway::SPAWN_TICKS;
        match cooling {
            Some(until) if until <= tick => {
                world.state().get::<Gateways>().cooldowns.remove(&pos);
                changed = true;
            }
            Some(_) => {}
            None if age % gateway::ATTENTION_INTERVAL == 0 => trigger_gateway(world, pos),
            None => {}
        }
        if changed {
            world.mark_blocks_dirty(pos);
        }
    }
}

/// Starts a gateway's cooldown and shows its beam.
fn trigger_gateway(world: &mut dyn RegionWorld, pos: BlockPos) {
    let until = world.tick() + gateway::COOLDOWN_TICKS;
    world.state().get::<Gateways>().cooldowns.insert(pos, until);
    world.block_event(pos, gateway::BEAM_EVENT, 0);
    world.mark_blocks_dirty(pos);
}

/// Something stands in the gateway at `pos`: true (starting the cooldown)
/// if it lets things through now. A gateway this region does not own is
/// left to its owner and lets them through.
fn enter_gateway(world: &mut dyn RegionWorld, pos: BlockPos) -> bool {
    if !world.owns_chunk(ChunkPos::of_block(pos)) {
        return true;
    }
    let tick = world.tick();
    let cooling = world
        .state()
        .get::<Gateways>()
        .cooldowns
        .get(&pos)
        .is_some_and(|until| *until > tick);
    if cooling || gateway_nbt(world, pos).is_none() {
        return false;
    }
    trigger_gateway(world, pos);
    true
}

/// Sends an entity that moved from `before` into a gateway through it,
/// once the exit is known.
fn entity_gateway(world: &mut dyn RegionWorld, tracked: &mut TrackedEntity, before: [f64; 3]) {
    if !tracked.entity.uses_portals() {
        return;
    }
    let now = tracked.entity.bounds();
    let position = tracked.entity.base().position;
    let shift: Vec<f64> = (0..3).map(|i| before[i] - position[i]).collect();
    let swept = [
        now[0].min(now[0] + shift[0]),
        now[1].min(now[1] + shift[1]),
        now[2].min(now[2] + shift[2]),
        now[3].max(now[3] + shift[0]),
        now[4].max(now[4] + shift[1]),
        now[5].max(now[5] + shift[2]),
    ];
    let local = world
        .state()
        .get::<Gateways>()
        .found
        .iter()
        .copied()
        .find(|pos| overlaps_block(swept, *pos));
    let Some(gateway) = local else {
        ask_neighbours(world, tracked.entity.base().id, swept);
        return;
    };
    if !enter_gateway(world, gateway) {
        return;
    }
    let base = tracked.entity.base_mut();
    let waiting = base.portal_cooldown > 0;
    base.portal_cooldown = gateway::ENTITY_PORTAL_COOLDOWN;
    if waiting {
        return;
    }
    launch(world, gateway, base.id);
}

/// Asks the neighbour regions the swept box reaches into whether one of
/// their gateways was crossed.
fn ask_neighbours(world: &mut dyn RegionWorld, entity: i32, swept: [f64; 6]) {
    let own = world.region_pos();
    let corner = |x: f64, z: f64| {
        ChunkPos::of_block(BlockPos::new(x.floor() as i32, 0, z.floor() as i32)).region()
    };
    let (low, high) = (corner(swept[0], swept[2]), corner(swept[3], swept[5]));
    let origin = world.handle();
    for x in low.x..=high.x {
        for z in low.z..=high.z {
            let region = tuff_world_model::pos::RegionPos::new(x, z);
            if region == own {
                continue;
            }
            let Some(handle) = world.neighbor(region) else {
                continue;
            };
            let origin = origin.clone();
            handle.internal(RegionInternal::Run(
                tuff_core::world::region_api::RegionTask::new(move |neighbour| {
                    let Some(gateway) = neighbour
                        .state()
                        .get::<Gateways>()
                        .found
                        .iter()
                        .copied()
                        .find(|pos| overlaps_block(swept, *pos))
                    else {
                        return;
                    };
                    if !enter_gateway(neighbour, gateway) {
                        return;
                    }
                    origin.internal(RegionInternal::Run(
                        tuff_core::world::region_api::RegionTask::new(move |world| {
                            let Some(tracked) = world.entity_mut(entity) else {
                                return;
                            };
                            let base = tracked.entity.base_mut();
                            let waiting = base.portal_cooldown > 0;
                            base.portal_cooldown = gateway::ENTITY_PORTAL_COOLDOWN;
                            if !waiting {
                                launch(world, gateway, entity);
                            }
                        }),
                    ));
                }),
            ));
        }
    }
}

/// Moves `entity` to the exit of `gateway` once it is known.
fn launch(world: &mut dyn RegionWorld, gateway: BlockPos, entity: i32) {
    let (shared, dimension, origin) =
        (world.shared().clone(), world.dimension_id(), world.handle());
    tokio::spawn(async move {
        let Some(position) = gateway::destination(&shared, dimension, gateway).await else {
            return;
        };
        let region = ChunkPos::of_position(position).region();
        let (reply, handle) = oneshot::channel();
        let acquire = ManagerMessage::Acquire {
            dimension,
            region,
            reply,
        };
        if shared.manager.send(acquire).await.is_err() {
            return;
        }
        let Ok(target) = handle.await else {
            return;
        };
        origin.internal(RegionInternal::Run(
            tuff_core::world::region_api::RegionTask::new(move |world| {
                let still = world
                    .entity(entity)
                    .is_some_and(|t| t.entity.base().kind == EntityType::EnderPearl);
                world.relocate_entity(entity, position, target, still);
            }),
        ));
        let _ = shared
            .manager
            .send(ManagerMessage::Release { dimension, region })
            .await;
    });
}

/// Answers a gateway exit search's request once its chunks are loaded.
pub fn answer(world: &mut dyn RegionWorld, request: GatewayRequest) {
    match request {
        GatewayRequest::Exit { pos, reply } => {
            let in_end = world.dimension_info().key == tuff_core::storage::level::THE_END;
            let lookup = match gateway_nbt(world, pos) {
                None => ExitLookup::Missing,
                Some(nbt) => match gateway::exit(nbt) {
                    Some(exit) => ExitLookup::Known {
                        exit,
                        exact: gateway::exact(nbt),
                    },
                    None if in_end => ExitLookup::Build {
                        exact: gateway::exact(nbt),
                    },
                    None => ExitLookup::Missing,
                },
            };
            let searches = &mut world.state().get::<Gateways>().searches;
            match lookup {
                ExitLookup::Build { .. } => match searches.get_mut(&pos) {
                    Some(waiting) => waiting.push(reply),
                    None => {
                        searches.insert(pos, Vec::new());
                        let _ = reply.send(lookup);
                    }
                },
                lookup => {
                    let _ = reply.send(lookup);
                }
            }
        }
        GatewayRequest::SetExit { pos, exit, exact } => {
            if let Some(exit) = exit
                && let Some(nbt) = gateway_nbt(world, pos)
            {
                gateway::set_exit(nbt, exit, exact);
                world.mark_blocks_dirty(pos);
            }
            let answer = match exit {
                Some(exit) => ExitLookup::Known { exit, exact },
                None => ExitLookup::Missing,
            };
            let waiting = world
                .state()
                .get::<Gateways>()
                .searches
                .remove(&pos)
                .unwrap_or_default();
            for waiting in waiting {
                let _ = waiting.send(answer);
            }
        }
        GatewayRequest::ChunkEmpty { chunk, reply } => {
            let empty = world.chunk(chunk).is_none_or(gateway::chunk_is_empty);
            let _ = reply.send(empty);
        }
        GatewayRequest::SpawnInChunk { chunk, reply } => {
            let spawn = world
                .chunk(chunk)
                .and_then(|c| gateway::spawn_in_chunk(c, chunk));
            let _ = reply.send(spawn);
        }
        GatewayRequest::ColumnTops {
            around,
            radius,
            allow_bedrock,
            reply,
        } => {
            let (min_y, max_y) = (world.min_y(), world.max_y());
            let mut tops = Vec::new();
            for x in around.x - radius..=around.x + radius {
                for z in around.z - radius..=around.z + radius {
                    let pos = BlockPos::new(x, min_y, z);
                    let chunk_pos = ChunkPos::of_block(pos);
                    if !world.owns_chunk(chunk_pos)
                        || !gateway::in_square(around, radius, allow_bedrock, x, z)
                    {
                        continue;
                    }
                    let Some(chunk) = world.chunk(chunk_pos) else {
                        continue;
                    };
                    let top = gateway::column_top(
                        |y| Some(chunk.get(x, y, z)),
                        min_y,
                        max_y,
                        allow_bedrock,
                    );
                    if let Some(y) = top {
                        tops.push((x, z, y));
                    }
                }
            }
            let _ = reply.send(tops);
        }
    }
}

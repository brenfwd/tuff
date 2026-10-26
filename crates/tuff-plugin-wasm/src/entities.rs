//! Entity spawns, removals and edits a plugin queued, and reads of entity
//! data, each run on the region that owns the entity.

use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::ManagerMessage;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::pos::ChunkPos;

/// A change to the entities a plugin queued.
#[derive(Clone, Debug, PartialEq)]
pub enum EntityOp {
    Spawn {
        dimension: DimensionId,
        nbt: Compound,
        position: [f64; 3],
        fresh: bool,
    },
    Remove(u128),
    Merge(u128, Compound),
    Rename(u128, Option<String>),
    Strike {
        attacker: u128,
        victim: u128,
    },
    Glide(Vec<Glide>),
}

/// One entity placed by a smooth move.
#[derive(Clone, Debug, PartialEq)]
pub struct Glide {
    pub uuid: u128,
    pub position: [f64; 3],
    pub rotation: Option<[f32; 2]>,
}

/// The cheap live state of an entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub position: [f64; 3],
    pub motion: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub health: f32,
}

/// Carries out the ops a call queued, in order.
pub async fn apply(shared: &Arc<Shared>, ops: Vec<EntityOp>) {
    for op in ops {
        match op {
            EntityOp::Spawn {
                dimension,
                nbt,
                position,
                fresh,
            } => spawn(shared, dimension, nbt, position, fresh).await,
            EntityOp::Remove(uuid) => {
                on_entity(shared, uuid, |world, id| {
                    world.remove_entity(id);
                })
                .await;
            }
            EntityOp::Strike { attacker, victim } => {
                on_entity(shared, attacker, move |world, id| {
                    world.strike_entity(id, victim);
                })
                .await;
            }
            EntityOp::Rename(uuid, name) => {
                on_entity(shared, uuid, move |world, id| {
                    let Some(tracked) = world.entity(id) else {
                        return;
                    };
                    let mut nbt = tuff_core::world::entity::save(&*tracked.entity);
                    match name {
                        Some(name) => nbt.insert("CustomName", Tag::String(name)),
                        None => nbt.remove("CustomName"),
                    };
                    world.replace_entity(id, nbt);
                })
                .await;
            }
            EntityOp::Glide(moves) => glide(shared, moves).await,
            EntityOp::Merge(uuid, patch) => {
                on_entity(shared, uuid, move |world, id| {
                    let Some(tracked) = world.entity(id) else {
                        return;
                    };
                    let mut nbt = tuff_core::world::entity::save(&*tracked.entity);
                    for (key, value) in patch.iter() {
                        if key != "UUID" && key != "id" {
                            nbt.insert(key, value.clone());
                        }
                    }
                    world.replace_entity(id, nbt);
                })
                .await;
            }
        }
    }
}

async fn spawn(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    nbt: Compound,
    position: [f64; 3],
    fresh: bool,
) {
    let region = ChunkPos::of_position(position).region();
    let (reply, handle) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Acquire {
            dimension,
            region,
            reply,
        })
        .await
        .is_err()
    {
        return;
    }
    if let Ok(handle) = handle.await {
        handle
            .ask(move |world| world.summon(nbt, position, fresh))
            .await;
    }
    let _ = shared
        .manager
        .send(ManagerMessage::Release { dimension, region })
        .await;
}

async fn regions(shared: &Arc<Shared>) -> Vec<tuff_core::world::messages::RegionHandle> {
    try_regions(shared).await.unwrap_or_default()
}

async fn try_regions(
    shared: &Arc<Shared>,
) -> Option<Vec<tuff_core::world::messages::RegionHandle>> {
    let (reply, regions) = oneshot::channel();
    shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .ok()?;
    regions.await.ok()
}

/// Runs `run` on the loaded region that owns the non-player entity `uuid`.
pub(crate) async fn on_entity<T: Send + 'static>(
    shared: &Arc<Shared>,
    uuid: u128,
    run: impl FnOnce(&mut dyn tuff_core::world::region_api::RegionWorld, i32) -> T + Send + 'static,
) -> Option<T> {
    let mut run = Some(run);
    for region in regions(shared).await {
        let found = region
            .ask(move |world| {
                world.entity_ids().into_iter().find(|id| {
                    world
                        .entity(*id)
                        .is_some_and(|e| e.entity.base().uuid == uuid)
                })
            })
            .await
            .flatten();
        if let Some(id) = found {
            let run = run.take()?;
            return region.ask(move |world| run(world, id)).await;
        }
    }
    None
}

/// Runs `run` once per loaded region with the ids of the wanted entities
/// that region owns, so a batch costs one pass over the regions.
async fn per_region<T: Send + 'static>(
    shared: &Arc<Shared>,
    wanted: Vec<u128>,
    run: impl Fn(&mut dyn tuff_core::world::region_api::RegionWorld, Vec<(u128, i32)>) -> Vec<T>
    + Clone
    + Send
    + 'static,
) -> Vec<T> {
    sweep(shared, wanted, run).await.0
}

/// `per_region`, also telling whether every region answered.
async fn sweep<T: Send + 'static>(
    shared: &Arc<Shared>,
    wanted: Vec<u128>,
    run: impl Fn(&mut dyn tuff_core::world::region_api::RegionWorld, Vec<(u128, i32)>) -> Vec<T>
    + Clone
    + Send
    + 'static,
) -> (Vec<T>, bool) {
    let mut out = Vec::new();
    let Some(handles) = try_regions(shared).await else {
        return (out, false);
    };
    let mut complete = true;
    for region in handles {
        let wanted = wanted.clone();
        let run = run.clone();
        let answer = region
            .ask(move |world| {
                let found: Vec<(u128, i32)> = world
                    .entity_ids()
                    .into_iter()
                    .filter_map(|id| {
                        let uuid = world.entity(id)?.entity.base().uuid;
                        wanted.contains(&uuid).then_some((uuid, id))
                    })
                    .collect();
                if found.is_empty() {
                    Vec::new()
                } else {
                    run(world, found)
                }
            })
            .await;
        match answer {
            Some(found) => out.extend(found),
            None => complete = false,
        }
    }
    (out, complete)
}

async fn glide(shared: &Arc<Shared>, moves: Vec<Glide>) {
    let wanted = moves.iter().map(|m| m.uuid).collect();
    per_region(shared, wanted, move |world, found| {
        for (uuid, id) in found {
            if let Some(m) = moves.iter().rev().find(|m| m.uuid == uuid) {
                world.glide_entity(id, m.position, m.rotation);
            }
        }
        Vec::<()>::new()
    })
    .await;
}

/// Passes over the regions a lookup makes before an entity that no region
/// reported counts as gone: one that is between two regions is in neither
/// for a moment.
const SWEEPS: usize = 4;

/// The state of each entity of `uuids` that is in a loaded region. `None`
/// when an entity was not found and a region could not be asked, so its
/// absence is unknown rather than a fact.
pub async fn states(shared: Arc<Shared>, uuids: Vec<u128>) -> Option<Vec<Option<State>>> {
    let mut found: Vec<(u128, State)> = Vec::new();
    let mut complete = true;
    for _ in 0..SWEEPS {
        let missing: Vec<u128> = uuids
            .iter()
            .copied()
            .filter(|uuid| !found.iter().any(|(u, _)| u == uuid))
            .collect();
        if missing.is_empty() {
            break;
        }
        let (now, whole) = sweep(&shared, missing, |world, found| {
            found
                .into_iter()
                .filter_map(|(uuid, id)| {
                    let entity = &world.entity(id)?.entity;
                    let base = entity.base();
                    Some((
                        uuid,
                        State {
                            position: base.position,
                            motion: base.velocity,
                            yaw: base.yaw,
                            pitch: base.pitch,
                            on_ground: base.on_ground,
                            health: entity.living().map_or(0.0, |l| l.health),
                        },
                    ))
                })
                .collect()
        })
        .await;
        found.extend(now);
        complete = whole;
        tokio::task::yield_now().await;
    }
    let states: Vec<Option<State>> = uuids
        .iter()
        .map(|uuid| found.iter().find(|(u, _)| u == uuid).map(|(_, s)| *s))
        .collect();
    (complete || states.iter().all(Option::is_some)).then_some(states)
}

/// The saved data of the non-player entity `uuid` in a loaded region.
pub async fn data(shared: Arc<Shared>, uuid: u128) -> Option<Compound> {
    on_entity(&shared, uuid, |world, id| {
        world
            .entity(id)
            .map(|tracked| tuff_core::world::entity::save(&*tracked.entity))
    })
    .await
    .flatten()
}

/// The saved form for a new entity of `kind` with a fresh uuid.
pub fn fresh_nbt(kind: &str, data: Option<Compound>, uuid: u128) -> (Compound, bool) {
    let fresh = data.is_none();
    let mut nbt = data.unwrap_or_default();
    nbt.insert("id", Tag::String(kind.to_owned()));
    nbt.insert("UUID", Tag::uuid(uuid));
    (nbt, fresh)
}

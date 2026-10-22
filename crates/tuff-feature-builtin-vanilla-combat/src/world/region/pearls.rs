//! Ender pearls in flight keep the chunks around them loaded for their
//! thrower, as long as they keep reporting where they are: each region
//! holds the 3x3 chunks around its pearls under its own ticket and lets a
//! chunk go 40 ticks after the last pearl left it. A thrower who logs out
//! takes its pearls out of the world into its player file, and they fly
//! on from where they were once it is back.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use tuff_core::world::messages::{ChunkTicket, ManagerMessage, RegionInternal, RegionRequest};
use tuff_core::world::region_api::{RegionHooks, RegionTask, RegionWorld, TickPhase};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use crate::projectile::thrown::Thrown;

/// The player file's list of the pearls its player took along.
const PEARLS: &str = "ender_pearls";
/// Each saved pearl's dimension.
const PEARL_DIMENSION: &str = "ender_pearl_dimension";
/// How long a leaving player waits for each region to hand its pearls over.
const TAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Ticks a chunk stays held after a pearl last reported it.
const HOLD_TICKS: u64 = 40;

/// The chunks a region holds for pearls, with the tick each hold lapses.
#[derive(Debug, Default)]
pub struct PearlTickets {
    reported: Vec<ChunkPos>,
    held: HashMap<ChunkPos, u64>,
    /// In-order hand-off to the manager, so a region never waits on it.
    outbox: Option<mpsc::UnboundedSender<ManagerMessage>>,
}

impl PearlTickets {
    /// A pearl is in `chunk` this tick.
    pub fn report(&mut self, chunk: ChunkPos) {
        self.reported.push(chunk);
    }

    pub fn holds(&self, chunk: ChunkPos) -> bool {
        self.held.contains_key(&chunk)
    }
}

pub struct PearlHooks;

impl RegionHooks for PearlHooks {
    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase != TickPhase::Late {
            return;
        }
        let tick = world.tick();
        let region = world.region_pos();
        let dimension = world.dimension_id();
        let manager = world.shared().manager.clone();
        let state = world.state().get::<PearlTickets>();
        if state.reported.is_empty() && state.held.is_empty() {
            return;
        }
        let mut gained = Vec::new();
        for center in std::mem::take(&mut state.reported) {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let chunk = ChunkPos::new(center.x + dx, center.z + dz);
                    if state.held.insert(chunk, tick + HOLD_TICKS).is_none() {
                        gained.push(chunk);
                    }
                }
            }
        }
        let lapsed: Vec<ChunkPos> = state
            .held
            .iter()
            .filter(|(_, until)| **until <= tick)
            .map(|(chunk, _)| *chunk)
            .collect();
        for chunk in &lapsed {
            state.held.remove(chunk);
        }
        let outbox = state.outbox.get_or_insert_with(|| {
            let (outbox, mut inbox) = mpsc::unbounded_channel::<ManagerMessage>();
            tokio::spawn(async move {
                while let Some(message) = inbox.recv().await {
                    if manager.send(message).await.is_err() {
                        break;
                    }
                }
            });
            outbox
        });
        for (chunks, on) in [(gained, true), (lapsed, false)] {
            if chunks.is_empty() {
                continue;
            }
            let (reply, _) = oneshot::channel();
            let _ = outbox.send(ManagerMessage::ForceLoad {
                dimension,
                chunks,
                ticket: ChunkTicket::EnderPearl(region),
                on,
                reply,
            });
        }
    }
}

/// Takes the pearls `owner` threw out of the region, saved with their
/// dimension.
fn take_pearls(world: &mut dyn RegionWorld, owner: u128) -> Vec<Compound> {
    let dimension = world.shared().dimension(world.dimension_id()).key.clone();
    let mut taken = Vec::new();
    for id in world.entity_ids() {
        let owned = world.entity(id).is_some_and(|tracked| {
            tracked.entity.base().kind == EntityType::EnderPearl
                && tracked
                    .entity
                    .downcast_ref::<Thrown>()
                    .is_some_and(|pearl| pearl.owner.uuid == Some(owner))
        });
        if !owned {
            continue;
        }
        if let Some(tracked) = world.remove_entity_intact(id) {
            let mut saved = tuff_core::world::entity::save(&*tracked.entity);
            saved.insert(PEARL_DIMENSION, dimension.as_str());
            tuff_item::item::conservation::release(tracked);
            taken.push(saved);
        }
    }
    taken
}

/// Takes the player's pearls out of every region.
async fn take_all_pearls(session: &PlayerSession) -> Vec<Compound> {
    let (reply, regions) = oneshot::channel();
    if session
        .shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let owner = session.profile.id;
    let mut answers = Vec::new();
    for region in regions.await.unwrap_or_default() {
        let (reply, answer) = oneshot::channel();
        let task = RegionTask::new(move |world| {
            let _ = reply.send(take_pearls(world, owner));
        });
        if region.internal(RegionInternal::Run(task)) {
            answers.push(answer);
        }
    }
    let mut pearls = Vec::new();
    for answer in answers {
        if let Ok(Ok(taken)) = tokio::time::timeout(TAKE_TIMEOUT, answer).await {
            pearls.extend(taken);
        }
    }
    pearls
}

#[derive(Default)]
struct PearlsVanished(bool);

/// Moves a player's pearls between the world and its player file.
pub struct PearlSessionHooks;

impl SessionHooks for PearlSessionHooks {
    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            let pearls = take_all_pearls(session).await;
            if !pearls.is_empty() {
                session.record.keep(PEARLS, Tag::from(pearls));
            }
        })
    }

    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase != SessionTick::Late {
                return;
            }
            let dead = session.is_dead();
            let vanished = session.ext.entry::<PearlsVanished>();
            if !dead {
                vanished.0 = false;
            } else if !vanished.0 && session.shared.rule_bool("ender_pearls_vanish_on_death") {
                vanished.0 = true;
                take_all_pearls(session).await;
            }
        })
    }

    fn joined<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            let Some(Tag::List(pearls)) = session.record.take_kept(PEARLS) else {
                return;
            };
            for pearl in pearls {
                let Tag::Compound(mut pearl) = pearl else {
                    continue;
                };
                let key = pearl.remove(PEARL_DIMENSION);
                let Some(dimension) = key
                    .as_ref()
                    .and_then(Tag::as_str)
                    .and_then(|key| session.shared.dimensions.by_key(key))
                    .map(|d| d.id)
                else {
                    continue;
                };
                let Some([x, y, z]) = pearl.get_list("Pos").and_then(|pos| {
                    let pos: Vec<f64> = pos.iter().filter_map(Tag::as_double).collect();
                    <[f64; 3]>::try_from(pos).ok()
                }) else {
                    continue;
                };
                let at = BlockPos::containing([x, y, z]);
                let Some(region) = session
                    .region_in(dimension, ChunkPos::of_block(at).region())
                    .await
                else {
                    continue;
                };
                region
                    .request(RegionRequest::WhenLoaded {
                        min: at,
                        max: at,
                        run: RegionTask::new(move |world| world.spawn_saved_entity(pearl)),
                    })
                    .await;
            }
        })
    }
}

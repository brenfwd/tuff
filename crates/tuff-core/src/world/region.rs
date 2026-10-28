//! One actor per 32x32-chunk region: the owner of that region's chunks,
//! entities, and `.mca` files. Regions tick in parallel. CPU-heavy work
//! (terrain generation, NBT, compression, packet encoding) runs on the rayon
//! pool; file I/O runs on tokio's blocking pool with the region files handed
//! off and back, so nothing ever blocks the actor.
//!
//! Across region boundaries, each region keeps read-only snapshots of its
//! neighbours' bordering chunks (the "halo"), refreshed once per tick, so
//! physics near the edge can read blocks it does not own. Entities crossing
//! the boundary are handed to the neighbour by message.

use crate::world::block_entity::BlockEntity;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::{mpsc, watch};
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::block_entity::ContainerKind;
use crate::world::dimension::{Dimension, DimensionId};
use crate::world::entity::{RegionEntity, TrackedEntity};
use crate::world::lighting::{LightChange, LightOp, Lighting};
use crate::world::messages::{
    ActionOutcome, BlockOp, BlockOpResult, BlockUpdate, ContainerChange, Packets, PlayerEvent,
    PlayerHandle, PlayerId, RegionHandle, RegionInternal, RegionRequest,
};
use crate::world::region_api::{BlockCause, TickPhase};
use crate::world::tick_barrier::TickMember;
use crate::world::ticks::ChunkTicks;
use crate::world::{Shared, framed, packets};
use tuff_nbt::Compound;
use tuff_timings::span;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::chunk_meta::ChunkMeta;
use tuff_world_model::pos::{ChunkMap, ChunkPos, RegionPos};

type Orphans = HashMap<(PlayerId, u64), Vec<(BlockPos, Vec<Option<tuff_item::item::ItemStack>>)>>;

/// Chunks nobody views are kept this long before being saved and dropped.
const UNLOAD_DELAY_TICKS: u64 = 100;
/// An encoded chunk packet no new viewer asked for in this long is dropped
/// (it is about as big as the chunk) and encoded again when one does.
const PACKET_KEEP_TICKS: u64 = 100;
const UNLOAD_CHECK_TICKS: u64 = 20;
/// An item offered to a player is held back this long awaiting the answer.
const PICKUP_RESERVATION_TICKS: u64 = 20;
/// The lid of a shulker box needs this much of the block it opens into.
const SHULKER_LID_DEPTH: f64 = 0.5;
const SHULKER_LID_EPSILON: f64 = 1.0e-6;
/// Items within this box above a hopper's floor are pulled in.
const HOPPER_SUCK_MIN_Y: f64 = 11.0 / 16.0;
const HOPPER_SUCK_MAX_Y: f64 = 2.0;
const AIR: BlockState = Block::Air.default_state();
const UNBREAKABLE: [Block; 8] = [
    Block::Bedrock,
    Block::Barrier,
    Block::CommandBlock,
    Block::EndPortalFrame,
    Block::EndPortal,
    Block::NetherPortal,
    Block::StructureBlock,
    Block::Jigsaw,
];

use crate::world::chunk_io::{Blob, EntityWrite, IoOp, IoResult};

struct Prepared {
    pos: ChunkPos,
    chunk: Chunk,
    meta: ChunkMeta<BlockEntity>,
    persist: bool,
    generated: bool,
    /// The chunk still has to come from the generation pipeline; `chunk`
    /// is a placeholder until then.
    pending: bool,
    entities: Vec<Box<dyn RegionEntity>>,
    /// Saved entities of types no loader handles, kept as they are.
    raw_entities: Vec<Compound>,
    entities_persist: bool,
    /// The chunk's points of interest, when already found.
    pois: Option<Vec<(BlockPos, tuff_vanilla_data::registry::PointOfInterestType)>>,
    /// The points of interest saved with the chunk, with their tickets.
    saved_pois: Option<Vec<tuff_world_model::poi::Record>>,
    /// Storage holds no such chunk and nobody asked for it to be made.
    absent: bool,
}

impl Prepared {
    /// What preparing a chunk storage has never held gives, before the
    /// generation pipeline fills it in.
    fn blank(pos: ChunkPos, shape: tuff_world_model::chunk::ChunkShape) -> Self {
        Self {
            pos,
            chunk: Chunk::empty(shape),
            meta: ChunkMeta::generated(),
            persist: true,
            generated: true,
            pending: true,
            entities: Vec::new(),
            raw_entities: Vec::new(),
            entities_persist: true,
            pois: None,
            saved_pois: None,
            absent: false,
        }
    }

    fn is_blank(&self) -> bool {
        self.persist
            && self.entities_persist
            && self.entities.is_empty()
            && self.raw_entities.is_empty()
            && self.meta.extra.is_empty()
            && self.meta.block_entities.is_empty()
            && self.meta.raw_block_entities.is_empty()
    }
}

/// Which kind of job failed, so the region can undo its bookkeeping.
#[derive(Debug, Clone, Copy)]
enum Stage {
    Prepare,
    Encode,
    Save,
    Io,
    Light,
}

impl Stage {
    fn label(&self) -> &'static str {
        match self {
            Stage::Prepare => "prepare",
            Stage::Encode => "encode",
            Stage::Save => "save",
            Stage::Io => "io",
            Stage::Light => "light",
        }
    }
}

enum JobResult {
    /// A job panicked. Reported so a bug cannot leave chunks loading forever
    /// or make shutdown wait on a job that will never finish.
    Panicked {
        stage: Stage,
        positions: Vec<ChunkPos>,
    },
    Io(Vec<IoResult>),
    Prepared(Box<Prepared>),
    Lit {
        lighting: Box<Lighting>,
        changes: Vec<LightChange>,
    },
    Encoded {
        pos: ChunkPos,
        version: u64,
        packet: Bytes,
        /// The chunk as sent and its anti-xray cache, when obfuscated.
        obfuscated: Option<(Arc<Chunk>, Box<crate::world::anti_xray::Cache>)>,
    },
    Serialized {
        pos: ChunkPos,
        blocks: Option<Blob>,
        entities: EntityWrite,
        pois: EntityWrite,
        ticket: Option<tuff_storage::journal::EpochTicket>,
    },
}

struct Column {
    chunk: Chunk,
    meta: ChunkMeta<BlockEntity>,
    /// Ticks players have spent near the chunk, scaling local difficulty.
    inhabited_time: i64,
    /// False if the stored chunk was unreadable: it is then never
    /// overwritten, so the data stays recoverable.
    persist: bool,
    entities_persist: bool,
    blocks_dirty: bool,
    entities_dirty: bool,
    pois_dirty: bool,
    other_entities: Vec<Compound>,
    version: u64,
    /// Light has been computed; the chunk is not sent before that.
    lit: bool,
    packet: Option<Bytes>,
    /// When `packet` was last encoded or sent to a new viewer.
    packet_used: u64,
    /// The chunk as `packet` describes it; stale once `packet` is cleared.
    snapshot: Option<Arc<Chunk>>,
    xray_cache: Option<Box<crate::world::anti_xray::Cache>>,
    encoding: bool,
    awaiting_packet: HashSet<PlayerId>,
    /// Awaiting viewers that already hold the chunk and its entities.
    resending: HashSet<PlayerId>,
    viewers: HashMap<PlayerId, PlayerHandle>,
    /// Players keeping the chunk loaded and lit but not sent to them.
    held: HashSet<PlayerId>,
    last_used: u64,
    saving: u32,
    unloading: bool,
    /// Block ticks pending in this chunk (button releases).
    block_ticks: ChunkTicks<Block>,
    fluid_ticks: ChunkTicks<tuff_vanilla_data::fluid::Fluid>,
    /// Blocks generation marked for a first fluid tick or shape update,
    /// run once the chunk's neighbours are loaded.
    post_processing: Vec<BlockPos>,
    /// Freshly generated and due its creatures once lit.
    creatures_pending: bool,
}

#[derive(Default)]
struct Loading {
    viewers: HashMap<PlayerId, PlayerHandle>,
    held: HashSet<PlayerId>,
    /// Viewers that only want the chunk if it already exists.
    peeking: HashSet<PlayerId>,
    /// Entities that arrived for the chunk, with their last sequence.
    entities: Vec<(Box<dyn RegionEntity>, u64)>,
}

/// A request that waits for the chunks it touches to load.
struct ChunkQuery {
    chunks: Vec<ChunkPos>,
    kind: ChunkQueryKind,
}

enum ChunkQueryKind {
    Blocks {
        op: BlockOp,
        reply: tokio::sync::oneshot::Sender<BlockOpResult>,
    },
    Respawn {
        pos: BlockPos,
        yaw: f32,
        pitch: f32,
        forced: bool,
        consume: bool,
        reply: tokio::sync::oneshot::Sender<Option<crate::world::messages::RespawnSpot>>,
    },
    Run(crate::world::region_api::RegionTask),
    Spawn {
        center: BlockPos,
        radius: i32,
        offset: u32,
        reply: tokio::sync::oneshot::Sender<[f64; 3]>,
    },
    AwaitChunk {
        reply: tokio::sync::oneshot::Sender<()>,
    },
}

struct Presence {
    handle: PlayerHandle,
    uuid: u128,
    name: String,
    position: [f64; 3],
    chunk: ChunkPos,
    alive: bool,
    spectator: bool,
    explosion_knockback_resistance: f64,
    creative_flying: bool,
    grounded: bool,
    looks: crate::world::messages::PlayerLooks,
    spawn: Packets,
    update: Packets,
    seq: u64,
}

impl Presence {
    /// Whether the player picks up items and blocks placement.
    fn is_physical(&self) -> bool {
        self.alive && !self.spectator
    }
}

pub struct RegionActor {
    dimension: DimensionId,
    pos: RegionPos,
    shared: Arc<Shared>,
    handle: RegionHandle,
    requests: mpsc::Receiver<RegionRequest>,
    internal: mpsc::UnboundedReceiver<RegionInternal>,
    datapacks: Option<watch::Receiver<Arc<crate::datapack::Contents>>>,
    columns: ChunkMap<Column>,
    /// Bumped whenever a block entity may have appeared or gone: a block
    /// with one changed, or a chunk loaded, unloaded or stopped unloading.
    block_entity_generation: u64,
    /// Block changes held back during a bulk operation, as section-relative
    /// indices by chunk and section y, sent as one packet per section.
    batched_changes: Option<HashMap<ChunkPos, BTreeMap<i32, BTreeSet<u16>>>>,
    /// Block entities whose data clients are sent once the current message
    /// is handled, after the block changes that caused it.
    block_entity_updates: Vec<BlockPos>,
    rng: StdRng,
    metrics: crate::metrics::Window,
    sched: Sched,
    menus: Menus,
    chunk_io: ChunkIo,
    light: LightState,
    border: Border,
    ents: EntityState,
    sim: Sim,
    debug: debug::DebugFeeds,
}

mod border;
mod chunk_io;
mod entity_state;
mod light_state;
mod menus;
mod sched;
mod sim;
use border::Border;
use chunk_io::ChunkIo;
use entity_state::EntityState;
use light_state::LightState;
use menus::Menus;
use sched::Sched;
use sim::Sim;
mod api;
mod beacon;
mod blocks;
mod boss_bar;
mod command_block;
mod conservation;
mod container_edits;
mod containers;
mod crafter;
mod debug;
mod entities;
mod entity_containers;
mod exploring;
mod explosions;
mod far_chunks;
mod fluids;
mod growth;
mod halves;
mod impacts;
mod internal;
mod item_merge;
mod item_ports;
mod loading;
mod metrics;
mod ownership;
mod passengers;
mod pistons;
mod pois;
mod pregen;
mod queries;
mod redstone;
mod riding;
mod signs;
mod supervise;
mod test_block;
mod tracking;
mod xray;

pub const REQUEST_CAPACITY: usize = 1024;

/// The tags of the next contents the data packs load; `None` once they are
/// gone.
async fn next_tags(
    contents: &mut Option<watch::Receiver<Arc<crate::datapack::Contents>>>,
) -> Option<Arc<tuff_datapack::tags::TagSet>> {
    let contents = contents.as_mut()?;
    contents.changed().await.ok()?;
    Some(contents.borrow_and_update().tags.clone())
}

impl RegionActor {
    /// Starts the actor. If `previous` is given (an earlier instance of this
    /// region still saving), its files are not touched until that finishes.
    pub fn spawn(
        dimension: DimensionId,
        pos: RegionPos,
        shared: Arc<Shared>,
        neighbors: Vec<RegionHandle>,
        previous: Option<watch::Receiver<bool>>,
    ) -> RegionHandle {
        let (handle, requests, internal) = RegionHandle::new(dimension, pos, REQUEST_CAPACITY);
        let neighbors = neighbors.into_iter().map(|h| (h.pos, h)).collect();
        let actor = Self::new(
            dimension,
            pos,
            shared,
            handle.clone(),
            requests,
            internal,
            neighbors,
        );
        let tags = actor.current_tags();
        let enchantments = actor.current_enchantments();
        let registries = actor.shared.datapacks.contents().registries.clone();
        tokio::spawn(tuff_item::item::conservation::actor(
            tuff_datapack::tags::scope(
                tags,
                tuff_item::registries::scope(
                    registries,
                    tuff_item::enchantment::scope(enchantments, async move {
                        if let Some(mut previous) = previous {
                            let _ = previous.wait_for(|done| *done).await;
                        }
                        actor.supervise().await;
                    }),
                ),
            ),
        ));
        handle
    }

    /// A fresh instance serving `handle`'s channels, with nothing loaded.
    fn new(
        dimension: DimensionId,
        pos: RegionPos,
        shared: Arc<Shared>,
        handle: RegionHandle,
        requests: mpsc::Receiver<RegionRequest>,
        internal: mpsc::UnboundedReceiver<RegionInternal>,
        neighbors: HashMap<RegionPos, RegionHandle>,
    ) -> Self {
        let datapacks = shared.datapacks.watch_contents();
        let lighting = {
            let dimension = shared.dimension(dimension);
            Lighting::new(dimension.shape, dimension.has_sky_light())
        };
        let rng = match shared.random_seed {
            Some(seed) => StdRng::seed_from_u64(
                seed ^ ((dimension.index() as u64) << 48)
                    ^ ((pos.x as u32 as u64) << 24)
                    ^ (pos.z as u32 as u64),
            ),
            None => StdRng::from_entropy(),
        };
        let mut actor = Self {
            dimension,
            pos,
            handle,
            requests,
            internal,
            datapacks: Some(datapacks),
            columns: ChunkMap::default(),
            block_entity_generation: 0,
            batched_changes: None,
            block_entity_updates: Vec::new(),
            rng,
            metrics: crate::metrics::Window::new(false),
            sched: Sched::new(&shared),
            menus: Menus::default(),
            chunk_io: ChunkIo::new(pos, xray::AntiXray::new(&shared, dimension)),
            light: LightState::new(lighting),
            border: Border::new(neighbors),
            ents: EntityState::default(),
            sim: Sim::default(),
            debug: debug::DebugFeeds::default(),
            shared,
        };
        actor.sim.fluid_settings = actor.current_fluid_settings();
        actor.light.sky_darken = crate::world::sky_darken(&actor.shared, actor.dimension());
        actor.sim.rand_value = actor.rng.r#gen();
        actor.sched.beat = Some(crate::world::stall::Beat::register(format!(
            "region {:?} at {},{}",
            dimension, pos.x, pos.z
        )));
        actor
    }

    fn beat_begin(&self, phase: crate::world::stall::Phase) {
        if let Some(beat) = &self.sched.beat {
            beat.begin(phase);
        }
    }

    fn beat_end(&self) {
        if let Some(beat) = &self.sched.beat {
            beat.end();
        }
    }

    fn current_enchantments(&self) -> Arc<tuff_item::enchantment::EnchantmentData> {
        self.datapacks
            .as_ref()
            .expect("a fresh instance watches the data packs")
            .borrow()
            .enchantments
            .clone()
    }

    fn current_tags(&self) -> Arc<tuff_datapack::tags::TagSet> {
        self.datapacks
            .as_ref()
            .expect("a fresh instance watches the data packs")
            .borrow()
            .tags
            .clone()
    }

    async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                tags = next_tags(&mut self.datapacks), if self.datapacks.is_some() => match tags {
                    Some(tags) => tuff_datapack::tags::refresh(&tags),
                    None => self.datapacks = None,
                },
                Some(result) = self.sched.jobs.recv() => {
                    self.beat_begin(crate::world::stall::Phase::Job);
                    self.on_job(result);
                    self.beat_end();
                }
                message = self.internal.recv(), if self.sched.internal_open => match message {
                    Some(message) => {
                        self.beat_begin(crate::world::stall::Phase::Internal);
                        self.on_internal(message);
                        self.beat_end();
                    }
                    None => self.sched.internal_open = false,
                },
                request = self.requests.recv(), if self.sched.requests_open => match request {
                    Some(request) => {
                        self.beat_begin(crate::world::stall::Phase::Request);
                        self.on_request(request);
                        self.beat_end();
                    }
                    None => self.sched.requests_open = false,
                },
                changed = self.sched.server_ticks.changed(), if self.sched.closing.is_none() => {
                    if changed.is_err() {
                        self.begin_close(watch::channel(false).0);
                    } else {
                        self.sched.server_tick = *self.sched.server_ticks.borrow_and_update();
                        let game = self.sched.clock.has_changed().unwrap_or(false);
                        let tick = *self.sched.clock.borrow_and_update();
                        self.beat_begin(if game {
                            crate::world::stall::Phase::Tick
                        } else {
                            crate::world::stall::Phase::FrozenTick
                        });
                        self.drain_before_tick();
                        let started = std::time::Instant::now();
                        if self.sched.closing.is_none() {
                            if game {
                                self.on_tick(tick);
                            } else {
                                self.on_frozen_tick();
                            }
                        }
                        let busy = started.elapsed();
                        self.beat_end();
                        if game && let Some(member) = &self.sched.tick_member {
                            member.finished(tick);
                        }
                        self.report_metrics(tick, busy);
                    }
                }
            }
            let _span = span!("region.flush", self.timings_tag());
            self.flush_container_changes();
            self.flush_block_entity_updates();
            self.poll_pregen();
            if self.sched.closing.is_some() && self.try_finish() {
                break;
            }
            self.pump_io();
            self.pump_light();
            self.answer_settled();
            if !self.sched.save_waiters.is_empty() && self.columns.values().all(|c| c.saving == 0) {
                for done in self.sched.save_waiters.drain(..) {
                    let _ = done.send(());
                }
            }
        }
        if let Some(done) = self.sched.closing.take() {
            let _ = done.send(true);
        }
    }

    /// Handles the messages already queued when the clock moved. The biased
    /// select prefers them to the clock, but the clock wins when they land
    /// after their channels were polled and before it was, so a message
    /// sent before a tick could otherwise miss that tick.
    fn drain_before_tick(&mut self) {
        for _ in 0..self.internal.len() {
            let Ok(message) = self.internal.try_recv() else {
                break;
            };
            self.on_internal(message);
        }
        self.drain_requests();
    }

    /// Applies the requests already queued, so a save snapshot taken next
    /// includes what a session sent before its own barrier snapshot.
    fn drain_requests(&mut self) {
        for _ in 0..self.requests.len() {
            let Ok(request) = self.requests.try_recv() else {
                break;
            };
            self.on_request(request);
        }
    }

    fn dimension(&self) -> &Dimension {
        self.shared.dimension(self.dimension)
    }

    fn answer_settled(&mut self) {
        if self.sched.settle_waiters.is_empty()
            || !self.chunk_io.loading.is_empty()
            || !self.light.light_idle()
            || self.sched.jobs_in_flight > 0
            || !self.chunk_io.io_queue.is_empty()
        {
            return;
        }
        let ticked = self.sched.ticked;
        let (ready, waiting) = std::mem::take(&mut self.sched.settle_waiters)
            .into_iter()
            .partition(|(tick, _)| *tick <= ticked);
        self.sched.settle_waiters = waiting;
        for (_, reply) in ready {
            let _ = reply.send(());
        }
    }

    fn begin_close(&mut self, done: watch::Sender<bool>) {
        for (pos, prepared) in std::mem::take(&mut self.chunk_io.generating) {
            let arrived = self.chunk_io.loading.remove(&pos);
            self.shelve_ungenerated(prepared, arrived);
        }
        self.sched.closing = Some(done);
        self.sched.tick_member = None;
        self.requests.close();
        self.internal.close();
    }

    /// Once every inbox is drained and pending loads have landed, saves
    /// everything; returns true when all of it is on disk.
    fn try_finish(&mut self) -> bool {
        if self.sched.requests_open
            || self.sched.internal_open
            || !self.chunk_io.loading.is_empty()
            || !self.light.light_idle()
        {
            return false;
        }
        if !self.sched.final_save {
            self.sched.final_save = true;
            let dirty: Vec<ChunkPos> = self
                .columns
                .iter()
                .filter(|(_, c)| c.needs_save())
                .map(|(pos, _)| *pos)
                .collect();
            for pos in dirty {
                self.start_save(pos);
            }
        }
        self.sched.jobs_in_flight == 0 && self.chunk_io.io_queue.is_empty()
    }

    fn on_request(&mut self, request: RegionRequest) {
        let player = match &request {
            RegionRequest::Break { player, .. } | RegionRequest::BreakRestricted { player, .. } => {
                Some(player.id)
            }
            RegionRequest::Place(place) | RegionRequest::PlaceOnWater(place) => {
                Some(place.player.id)
            }
            RegionRequest::UseBucket(request) => Some(request.player.id),
            RegionRequest::ContainerTransaction { player, .. }
            | RegionRequest::EntityTransaction { player, .. } => Some(player.id),
            RegionRequest::ResolveTransaction { player, .. } => Some(*player),
            _ => None,
        };
        match player {
            Some(player) => self.with_cause(BlockCause::Player(player), |region| {
                region.handle_request(request)
            }),
            None => self.handle_request(request),
        }
    }

    fn handle_request(&mut self, request: RegionRequest) {
        let _span = span!("region.request", self.timings_tag());
        match request {
            RegionRequest::Subscribe { player, chunks } => {
                if self.sched.closing.is_some() {
                    return;
                }
                let region = self.pos;
                for pos in chunks.into_iter().filter(|c| region.contains(*c)) {
                    self.subscribe(pos, player.clone());
                }
            }
            RegionRequest::Peek { player, chunks } => {
                if self.sched.closing.is_some() {
                    return;
                }
                let region = self.pos;
                for pos in chunks.into_iter().filter(|c| region.contains(*c)) {
                    self.peek(pos, player.clone());
                }
            }
            RegionRequest::Unsubscribe { player, chunks } => {
                for pos in chunks {
                    if let Some(waiting) = self.chunk_io.peekers.get_mut(&pos) {
                        waiting.remove(&player);
                    }
                    if let Some(column) = self.columns.get_mut(&pos) {
                        column.viewers.remove(&player);
                        column.held.remove(&player);
                        column.awaiting_packet.remove(&player);
                        column.resending.remove(&player);
                    } else if let Some(loading) = self.chunk_io.loading.get_mut(&pos) {
                        loading.viewers.remove(&player);
                        loading.peeking.remove(&player);
                        loading.held.remove(&player);
                    }
                }
            }
            RegionRequest::HoldLit { player, chunks } => {
                if self.sched.closing.is_some() {
                    return;
                }
                let region = self.pos;
                for pos in chunks.into_iter().filter(|c| region.contains(*c)) {
                    self.hold_lit(pos, player);
                }
            }
            RegionRequest::Player(update) => {
                self.ents.seen = None;
                self.player_update(update);
            }
            RegionRequest::PlayerRemoved { player, seq } => {
                self.ents.seen = None;
                if let Some(presence) = self.ents.presence.remove(&player)
                    && self.pos.contains(presence.chunk)
                {
                    self.fanout_chunk(presence.chunk, Some(player), |_| PlayerEvent::Untrack {
                        entity: player,
                        seq,
                    });
                }
            }
            RegionRequest::PresenceGone { player } => {
                self.ents.seen = None;
                self.ents.presence.remove(&player);
                self.ents.orb_pickups.remove(&player);
            }
            RegionRequest::Break {
                player,
                pos,
                tool,
                mode,
                sequence,
            } => self.break_block(player, pos, tool, mode, sequence),
            RegionRequest::BreakRestricted {
                player,
                pos,
                tool,
                mode,
                sequence,
            } => self.break_restricted(player, pos, tool, mode, sequence),
            RegionRequest::Place(place) => self.place(place),
            RegionRequest::PlaceOnWater(place) => self.place_on_water(place),
            RegionRequest::Notify { target, notice } => {
                if let Some(tracked) = self.ents.entities.get_mut(&target) {
                    tracked.entity.notice(notice);
                }
            }
            RegionRequest::DestroyProgress(progress) => self.destroy_progress(*progress),
            RegionRequest::DestroyAborted { player, pos } => self.abort_destroying(player, pos),
            RegionRequest::MeltPowderSnow { pos } => {
                if self
                    .block(pos)
                    .is_some_and(|s| s.block() == tuff_vanilla_data::Block::PowderSnow)
                {
                    self.destroy_block(pos, false);
                }
            }
            RegionRequest::LowerCauldron { pos } => self.lower_cauldron(pos),
            RegionRequest::UpdateSign {
                player,
                pos,
                front,
                lines,
                filtered,
                eye,
            } => self.update_sign(&player, pos, front, &lines, filtered.as_deref(), eye),
            RegionRequest::Resend {
                player,
                pos,
                sequence,
            } => {
                let corrections = self.corrections(&[pos]);
                player.send(PlayerEvent::ActionResult {
                    sequence,
                    outcome: ActionOutcome::Rejected(corrections),
                });
            }
            RegionRequest::BlockAt { pos, reply } => {
                let _ = reply.send(self.block(pos).filter(|_| self.owns(pos)));
            }
            RegionRequest::DebugStickSet { pos, state } => {
                if self.owns(pos) && self.block(pos).is_some() {
                    self.set_block_flags(
                        pos,
                        state,
                        crate::world::redstone::UPDATE_CLIENTS
                            | crate::world::redstone::UPDATE_KNOWN_SHAPE,
                        crate::world::redstone::UPDATE_LIMIT,
                    );
                }
            }
            RegionRequest::BlockWithEntity { pos, reply } => {
                let answer = self
                    .block(pos)
                    .filter(|_| self.owns(pos))
                    .map(|state| (state, self.saved_block_entity(pos)));
                let _ = reply.send(answer);
            }
            RegionRequest::RandomTeleport {
                origin,
                diameter,
                width,
                height,
                reply,
            } => {
                let target = self.random_teleport_target(origin, diameter, width, height);
                let _ = reply.send(target);
            }
            RegionRequest::SpawnItem {
                stack,
                position,
                velocity,
                pickup_delay,
            } => self.spawn_item_at(stack, position, velocity, pickup_delay),
            RegionRequest::SpawnFakeItem { stack, position } => {
                if !self.columns.contains_key(&ChunkPos::of_position(position)) {
                    return;
                }
                let id = self.shared.next_entity_id();
                let uuid = self.rng.r#gen();
                #[expect(
                    clippy::disallowed_methods,
                    reason = "preview stand-in stack for a slot or click, burned as Preview (handle_request)"
                )]
                let mut item = crate::world::items::ItemEntity::new(
                    id,
                    uuid,
                    tuff_item::item::ItemStack::mint_like(
                        &stack,
                        stack.count(),
                        tuff_item::item::MintReason::Preview,
                    ),
                    position,
                    [0.0; 3],
                    crate::world::items::NEVER_PICKUP,
                );
                item.age = self.despawn_settings().item_age - 1;
                self.spawn_entity(item);
            }
            RegionRequest::SpawnEntity(entity) => self.receive_entity(entity, 0),
            RegionRequest::SpawnExperience { position, amount } => {
                self.spawn_experience(position, amount);
            }
            RegionRequest::Broadcast {
                chunk,
                entity,
                packets,
                except,
            } => match entity {
                Some(entity) => self.fanout_chunk(chunk, except, |_| PlayerEvent::EntityPackets {
                    entity,
                    packets: packets.clone(),
                }),
                None => {
                    let dimension = self.dimension;
                    self.fanout_chunk(chunk, except, |_| PlayerEvent::ChunkPackets {
                        dimension,
                        pos: chunk,
                        packets: packets.clone(),
                    })
                }
            },
            RegionRequest::BroadcastNear {
                center,
                range,
                packets,
                except,
            } => self.send_near(center, range, except, packets),
            RegionRequest::ContainerTransaction {
                player,
                window_id,
                changes,
                pool,
                took_result,
            } => self.container_transaction(player, window_id, changes, pool, took_result),
            RegionRequest::Blocks { op, reply } => {
                let positions: Vec<BlockPos> = match &op {
                    BlockOp::Get { positions, .. } | BlockOp::Biomes { positions, .. } => {
                        positions.clone()
                    }
                    BlockOp::Set { changes, .. } => changes.iter().map(|(pos, _)| *pos).collect(),
                    BlockOp::Copy { positions } | BlockOp::TestBlocks { positions, .. } => {
                        positions.clone()
                    }
                    BlockOp::Paste { changes, .. } => {
                        changes.iter().map(|(pos, ..)| *pos).collect()
                    }
                    BlockOp::ReplacePortItem { pos, .. } => vec![*pos],
                    BlockOp::CopyTicks { .. } => Vec::new(),
                    BlockOp::PasteTicks { ticks } => ticks.iter().map(|tick| tick.pos).collect(),
                    BlockOp::FillBiome { .. } | BlockOp::TestInstances { .. } => Vec::new(),
                };
                let load = !matches!(
                    op,
                    BlockOp::Get { load: false, .. }
                        | BlockOp::Biomes { load: false, .. }
                        | BlockOp::FillBiome { .. }
                        | BlockOp::TestInstances { .. }
                );
                let chunks: Vec<ChunkPos> = {
                    let mut chunks: Vec<ChunkPos> = positions
                        .iter()
                        .map(|pos| ChunkPos::of_block(*pos))
                        .filter(|chunk| self.pos.contains(*chunk))
                        .collect();
                    chunks.sort_unstable_by_key(|c| (c.x, c.z));
                    chunks.dedup();
                    chunks
                };
                if load && self.sched.closing.is_none() {
                    for pos in &chunks {
                        if !self.columns.contains_key(pos) {
                            self.load(*pos);
                        }
                    }
                }
                let kind = ChunkQueryKind::Blocks { op, reply };
                // Without `load`, only chunks already on their way in are
                // waited for, so a forced or viewed chunk counts as loaded
                // as soon as it is.
                let chunks: Vec<ChunkPos> = if load {
                    chunks
                } else {
                    chunks
                        .into_iter()
                        .filter(|c| {
                            self.chunk_io.loading.contains_key(c)
                                || self.chunk_io.forced.contains(c)
                        })
                        .collect()
                };
                self.chunk_io
                    .chunk_queries
                    .push(ChunkQuery { chunks, kind });
                self.poll_chunk_queries();
            }
            RegionRequest::Transmitters { reply } => {
                let found = self
                    .ents
                    .entities
                    .values()
                    .filter_map(|tracked| {
                        let range = tracked.entity.waypoint_transmit_range();
                        let base = tracked.entity.base();
                        (range > 0.0).then_some(crate::world::messages::WaypointTransmitter {
                            id: base.id,
                            uuid: base.uuid,
                            dimension: self.dimension,
                            position: base.position,
                            range,
                            indirect_passengers: self.ents.indirect_passengers(base.id),
                        })
                    })
                    .collect();
                let _ = reply.send(found);
            }
            RegionRequest::LoadedAmong { chunks, reply } => {
                let key = self.dimension().key.clone();
                let found = chunks
                    .iter()
                    .filter(|c| {
                        c.dimension == key && self.columns.contains_key(&ChunkPos::new(c.x, c.z))
                    })
                    .cloned()
                    .collect();
                let _ = reply.send(found);
            }
            RegionRequest::Entities { reply } => {
                let _ = reply.send(self.entity_snapshots(false));
            }
            RegionRequest::EntitiesWithData { reply } => {
                let _ = reply.send(self.entity_snapshots(true));
            }
            RegionRequest::EntityData { id, reply } => {
                let data = self
                    .ents
                    .entities
                    .get(&id)
                    .map(|tracked| crate::world::entity::save(&*tracked.entity));
                let _ = reply.send(data);
            }
            RegionRequest::Settled { tick, reply } => self.sched.settle_waiters.push((tick, reply)),
            RegionRequest::AttackEntity {
                attacker,
                target,
                attack,
                sweep,
            } => self.attack_entity(attacker, target, attack, sweep),
            RegionRequest::StabDismount { target } => {
                let riding = self
                    .ents
                    .entities
                    .get(&target)
                    .is_some_and(|t| t.entity.base().vehicle.is_some());
                if riding {
                    self.ents.mount_requests.push((target, None));
                }
            }
            RegionRequest::StabKnock {
                target,
                power,
                dx,
                dz,
            } => {
                if let Some(t) = self.ents.entities.get_mut(&target) {
                    t.entity.knocked(power, dx, dz);
                }
            }
            RegionRequest::InteractEntity {
                player,
                target,
                interaction,
            } => self.interact_entity(player, target, interaction),
            RegionRequest::Trade {
                player,
                window_id,
                request,
            } => self.trade(player, window_id, request),
            RegionRequest::StopTrading { merchant, player } => self.stop_trading(merchant, player),
            RegionRequest::UseBucket(request) => self.use_bucket(request),
            RegionRequest::Vehicle(request) => self.vehicle_request(request),
            RegionRequest::ReclaimVehicle { player, uuid } => {
                self.reclaim_offline_rider(&player, uuid);
            }
            RegionRequest::PlaceBoat(request) => self.place_boat(request),
            RegionRequest::WhenLoaded { min, max, run } => {
                self.chunk_query(min, max, ChunkQueryKind::Run(run))
            }
            RegionRequest::Run(run) => run.run(self),
            RegionRequest::Pregen(op) => self.pregen_request(op),
            RegionRequest::OccupyBed { pos } => self.occupy_bed(pos, true),
            RegionRequest::LeaveBed { player, pos, yaw } => self.leave_bed(player, pos, yaw),
            RegionRequest::AnchorSpawnSet { pos } => {
                self.play_sound(SoundEvent::BlockRespawnAnchorSetSpawn, pos, 1.0, 1.0, None);
            }
            RegionRequest::FindSpawn {
                center,
                radius,
                offset,
                reply,
            } => {
                let min = BlockPos::new(center.x - radius, center.y, center.z - radius);
                let max = BlockPos::new(center.x + radius, center.y, center.z + radius);
                self.chunk_query(
                    min,
                    max,
                    ChunkQueryKind::Spawn {
                        center,
                        radius,
                        offset,
                        reply,
                    },
                );
            }
            RegionRequest::RespawnPoint {
                pos,
                yaw,
                pitch,
                forced,
                consume,
                reply,
            } => {
                let min = BlockPos::new(pos.x - 3, pos.y, pos.z - 3);
                let max = BlockPos::new(pos.x + 3, pos.y, pos.z + 3);
                self.chunk_query(
                    min,
                    max,
                    ChunkQueryKind::Respawn {
                        pos,
                        yaw,
                        pitch,
                        forced,
                        consume,
                        reply,
                    },
                );
            }
            RegionRequest::AwaitChunk { pos, reply } => {
                self.chunk_query(pos, pos, ChunkQueryKind::AwaitChunk { reply });
            }
            RegionRequest::Summon {
                nbt,
                position,
                fresh,
                reply,
            } => {
                let _ = reply.send(self.summon(nbt, position, fresh));
            }
            RegionRequest::PrepareTransaction {
                player,
                window_id,
                txn,
                changes,
            } => self.prepare_transaction(player, window_id, txn, changes),
            RegionRequest::ResolveTransaction {
                player,
                txn,
                commit,
                items,
            } => self.resolve_transaction(player, txn, commit, items),
            RegionRequest::CommandBlockDone {
                pos,
                success,
                output,
            } => self.command_block_done(pos, success, output),
            RegionRequest::SetCommandBlock { player, pos, edit } => {
                self.edit_command_block(&player, pos, edit)
            }
            RegionRequest::AnvilUsed { pos, damage } => self.anvil_used(pos, damage),
            RegionRequest::GrindstoneUsed { pos, experience } => {
                self.grindstone_used(pos, experience)
            }
            RegionRequest::CrafterSlotState { pos, slot, enabled } => {
                self.set_crafter_slot_state(pos, slot, enabled)
            }
            RegionRequest::LecternButton {
                player,
                pos,
                button,
                may_build,
            } => self.menu_button(player, pos, button, may_build),
            RegionRequest::SetBeaconEffects {
                pos,
                primary,
                secondary,
            } => self.set_beacon_effects(pos, primary, secondary),
            RegionRequest::RefreshContainer { player, pos } => self.refresh_container(&player, pos),
            RegionRequest::CloseContainer { player, positions } => {
                for pos in positions {
                    self.remove_viewer(pos, player);
                }
            }
            RegionRequest::EntityTransaction {
                player,
                window_id,
                entity,
                changes,
                pool,
            } => self.entity_transaction(player, window_id, entity, changes, pool),
            RegionRequest::CloseEntityContainer { player, entity } => {
                let viewing = self
                    .menus
                    .entity_viewers
                    .get(&entity)
                    .is_some_and(|viewers| viewers.iter().any(|v| v.id == player));
                self.menus.remove_entity_viewer(entity, player);
                if viewing {
                    self.entity_container_event(
                        player,
                        entity,
                        crate::world::game_event::GameEvent::ContainerClose,
                    );
                }
            }
        }
    }

    fn on_tick(&mut self, tick: u64) {
        let _span = span!("region.tick", self.timings_tag());
        let mut profile = crate::world::long_tick::TickProfile::start();
        self.sched.tick = tick;
        self.sched.ticked = tick;
        self.sim.fluid_settings = self.current_fluid_settings();
        self.refresh_anti_xray();
        self.light.sky_darken = crate::world::sky_darken(&self.shared, self.dimension());
        self.poll_chunk_queries();
        self.poll_exploring();
        self.refresh_debug_subscribers();
        if self.border.neighbourhood_cache.is_none() {
            self.border.neighbourhood_cache = Some(self.loaded_neighbourhoods());
        }
        crate::world::long_tick::TickProfile::mark(&mut profile, "setup");
        self.batch_block_changes(|region| {
            region.replay_deferred_updates();
            region.apply_block_updates();
            region.run_area_work(Some(tick));
            region.run_post_processing();
            region.run_block_ticks();
            region.run_fluid_ticks();
            region.run_random_ticks();
            region.run_tick_hooks(TickPhase::Weather);
            region.run_block_events();
            region.apply_current_block_updates();
            region.tick_entities();
            region.tick_entity_hoppers();
            region.share_footprints();
            region.entities_inside_blocks(false);
            region.tick_moving_blocks();
            region.run_slow_ticks();
            region.run_tick_hooks(TickPhase::Spawning);
        });
        crate::world::long_tick::TickProfile::mark(&mut profile, "world simulation");
        self.tick_block_entities();
        self.check_item_frames();
        if !self.menus.entity_viewers.is_empty() {
            self.sweep_entity_viewers();
        }
        crate::world::long_tick::TickProfile::mark(&mut profile, "block entities and viewers");
        self.flush_container_changes();
        self.run_tick_hooks(TickPhase::Late);
        self.tick_sign_editors();
        self.offer_items();
        self.flush_halo();
        self.housekeeping();
        self.sync_debug_feeds();
        crate::world::long_tick::TickProfile::mark(&mut profile, "hooks and housekeeping");
        if let Some(profile) = profile {
            let label = format!(
                "dimension {} region {},{}",
                self.dimension.index(),
                self.pos.x,
                self.pos.z
            );
            profile.finish(&label, tick);
        }
    }

    /// Runs the area work due on game tick `now` (all of it when frozen).
    fn run_area_work(&mut self, now: Option<u64>) {
        if self.border.area.is_empty() {
            return;
        }
        let _span = span!("region.area_work");
        for deferred in self.border.area.due(now) {
            deferred.task.run(self);
        }
    }

    /// Runs `task` in the area phase of the next tick of each loaded region
    /// `area` reaches (this one only when `local`); returns its send order.
    fn area_run(
        &mut self,
        area: crate::world::area::Area,
        local: bool,
        task: crate::world::area::AreaTask,
    ) -> u64 {
        let seq = self.border.area.next_seq();
        let (tick, from) = (self.sched.tick, self.pos);
        let deferred = || crate::world::area::Deferred {
            tick: Some(tick),
            from,
            seq,
            task: task.clone().once(),
        };
        for region in crate::world::area::regions_in(area, from) {
            if region == from {
                if local {
                    self.border.area.push(deferred());
                }
            } else if let Some(neighbor) = self.border.neighbors.get(&region) {
                neighbor.internal(RegionInternal::Area(deferred()));
            }
        }
        seq
    }

    /// A server tick while the game is frozen: nothing simulates, but
    /// changes made meanwhile (by players, commands, neighbours) still
    /// reach viewers and neighbours, and chunks still unload and save.
    fn on_frozen_tick(&mut self) {
        let _span = span!("region.tick", self.timings_tag());
        self.refresh_anti_xray();
        self.poll_chunk_queries();
        if self.border.neighbourhood_cache.is_none() {
            self.border.neighbourhood_cache = Some(self.loaded_neighbourhoods());
        }
        self.batch_block_changes(|region| {
            region.replay_deferred_updates();
            region.apply_frozen_block_updates();
            region.run_area_work(None);
            region.sched.frozen = true;
            region.tick_entities();
            region.sched.frozen = false;
            region.entities_inside_blocks(true);
        });
        if !self.menus.entity_viewers.is_empty() {
            self.sweep_entity_viewers();
        }
        self.flush_container_changes();
        self.tick_sign_editors();
        self.offer_items();
        self.flush_halo();
        self.housekeeping();
    }

    fn housekeeping(&mut self) {
        self.flush_light();
        let tick = self.sched.server_tick;
        if tick.is_multiple_of(UNLOAD_CHECK_TICKS) {
            self.unload_idle();
        }
    }
}

/// The block at `pos` from a region's own chunks or its halo.
fn block_at(
    columns: &ChunkMap<Column>,
    halo: &ChunkMap<Chunk>,
    pos: BlockPos,
) -> Option<BlockState> {
    let chunk = ChunkPos::of_block(pos);
    columns
        .get(&chunk)
        .map(|c| &c.chunk)
        .or_else(|| halo.get(&chunk))
        .map(|chunk| chunk.get(pos.x, pos.y, pos.z))
}

impl Column {
    fn needs_save(&self) -> bool {
        (self.blocks_dirty && self.persist)
            || (self.entities_dirty && self.entities_persist)
            || (self.pois_dirty && self.persist)
    }
}

/// The statistic opening the menu of `block` counts towards, as each
/// block awards it.
fn menu_stat(block: Block) -> Option<tuff_vanilla_data::registry::CustomStat> {
    use tuff_vanilla_data::registry::CustomStat;
    Some(match block {
        Block::CraftingTable => CustomStat::InteractWithCraftingTable,
        Block::Stonecutter => CustomStat::InteractWithStonecutter,
        Block::SmithingTable => CustomStat::InteractWithSmithingTable,
        Block::Anvil | Block::ChippedAnvil | Block::DamagedAnvil => CustomStat::InteractWithAnvil,
        Block::CartographyTable => CustomStat::InteractWithCartographyTable,
        Block::Loom => CustomStat::InteractWithLoom,
        Block::Grindstone => CustomStat::InteractWithGrindstone,
        Block::Lectern => CustomStat::InteractWithLectern,
        Block::Beacon => CustomStat::InteractWithBeacon,
        Block::EnderChest => CustomStat::OpenEnderchest,
        Block::Furnace => CustomStat::InteractWithFurnace,
        Block::BlastFurnace => CustomStat::InteractWithBlastFurnace,
        Block::Smoker => CustomStat::InteractWithSmoker,
        Block::BrewingStand => CustomStat::InteractWithBrewingstand,
        Block::Barrel => CustomStat::OpenBarrel,
        Block::Hopper => CustomStat::InspectHopper,
        Block::Dropper => CustomStat::InspectDropper,
        Block::Dispenser => CustomStat::InspectDispenser,
        Block::TrappedChest => CustomStat::TriggerTrappedChest,
        _ if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::SHULKER_BOXES, block) => {
            CustomStat::OpenShulkerBox
        }
        _ if ContainerKind::of_block(block) == Some(ContainerKind::Chest) => CustomStat::OpenChest,
        _ => return None,
    })
}

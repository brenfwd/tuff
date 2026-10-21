//! The generation pipeline for generators whose features reach across chunk
//! borders (vanilla's trees, lakes, ores, ...).
//!
//! A chunk goes through two steps: terrain (independent per chunk) and
//! decoration, which places the features of one chunk and may write into
//! all eight of its neighbours. A chunk is finished once it and its eight
//! neighbours are decorated; only then is it handed to its region, which
//! owns it from there on. So decorating chunk N needs the terrain of the
//! 3x3 around N, and finishing chunk C needs the terrain of the 5x5 around
//! C. Every step runs on the rayon pool.
//!
//! The chunks in progress are split between shard actors by position:
//! squares of [`CELL`] x [`CELL`] chunks are spread over the shards, and
//! each shard alone owns the chunks, structure starts and requests of its
//! squares. The owner of a chunk decorates it. A decoration window that
//! reaches into other shards borrows those chunks: the coordinating shard
//! asks every owner for its part of the window, one owner after another
//! in shard order, and an owner hands over its whole part at once when all
//! of it is ready and lent to nobody. Asking in a fixed order means no two
//! decorations ever wait on each other, and a chunk is only ever in one
//! window at a time, so nothing is shared mutably. Owners learn that
//! neighbouring chunks of other shards were decorated from the shard that
//! decorated them, which tells everyone that asked.
//!
//! Each shard commits to a bounded number of its wanted chunks at a time
//! and asks only for their decorations, served in the order they were
//! committed. Among the chunks still waiting it commits first the one with
//! the fewest decorations nobody asked for yet, so finished areas grow
//! together rather than scattering when players outrun generation; then
//! the most recently asked for. Urgent chunks are committed at once.
//!
//! With storage, chunks in progress are saved the way the game saves them
//! (their status says how far they got): a chunk idle long enough is
//! written out and dropped, everything left is written when the world
//! stops, and a chunk a shard needs again is read back before any terrain
//! is generated for it. A chunk is only ever written while the pipeline
//! owns it, so a region's finished chunk is never overwritten. Without
//! storage, dropped chunks are regenerated; decoration is deterministic, so
//! that only matters where the redone decoration sees neighbours in a
//! different state than the first time (the same order dependence vanilla
//! has).

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot, watch};

use crate::chunk_generator::{StagedGenerator, StartsBlock};
use crate::proto::ProtoChunk;
use crate::saved::{self, Saved};
use tuff_core::world::chunk_io::IoResult;
use tuff_core::world::generator::{ChunkPipeline, Finished, PipelineContext, ProtoStorage, URGENT};
use tuff_core::world::messages::{RegionHandle, RegionInternal};
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// Chunks in progress that nothing has needed for this long are saved and
/// dropped. Short, since a chunk in progress holds every block of its
/// column and players outrunning generation leave many behind.
const SAVE_IDLE_TICKS: u64 = 100;
/// What is not written to storage (delivered chunks' decorated flags,
/// structure starts, chunks that cannot be saved) is dropped after this.
const EVICT_TICKS: u64 = 6000;
const EVICT_CHECK_TICKS: u64 = 20;
/// Decorations a shard runs at once, per thread of the rayon pool over the
/// number of shards. Needed decorations beyond that wait in line, so the
/// chunks in progress stay bounded however far demand runs ahead.
const DECORATIONS_PER_THREAD: usize = 16;
/// The side of the squares of chunks shards own, matching the blocks of
/// structure starts so each block of starts has one owner.
pub const CELL: i32 = crate::chunk_generator::STARTS_BLOCK;
/// Wanted chunks asked for within this many milliseconds of each other
/// form a batch: the latest batch is committed first, a batch in the order
/// it was asked for.
const BATCH_MILLIS: u64 = 1000;

/// When a wanted chunk was committed, the most urgent priority first:
/// decorations are served in this order, so committed chunks finish in
/// the order they were committed.
type Rank = u64;

/// A wanted chunk waiting to be committed: priority, how many of its
/// decorations nobody asked for yet, batch (latest first), order of asking.
type Candidate = (u32, u8, Reverse<u64>, u64, ChunkPos);

/// How many shards a pipeline runs by default: one per four threads of
/// the rayon pool, which does the actual work.
pub fn default_shards() -> usize {
    rayon::current_num_threads().div_ceil(4).clamp(1, 64)
}

/// Which shard owns what, and how shards reach each other. Shards hold
/// each other weakly, so they stop once every handle is gone.
struct Routing {
    shards: Vec<mpsc::WeakUnboundedSender<ShardMessage>>,
    /// The clock commitments are stamped with, shared so every shard ranks
    /// decorations the same way.
    started: Instant,
}

impl Routing {
    fn owner_of_cell(&self, cell: (i32, i32)) -> usize {
        let n = self.shards.len();
        if n == 1 {
            return 0;
        }
        let hash = (cell.0 as u32 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(29)
            ^ (cell.1 as u32 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        ((hash ^ (hash >> 32)) % n as u64) as usize
    }

    fn owner(&self, pos: ChunkPos) -> usize {
        self.owner_of_cell((pos.x.div_euclid(CELL), pos.z.div_euclid(CELL)))
    }

    fn starts_owner(&self, block: (i32, i32)) -> usize {
        self.owner_of_cell(block)
    }

    fn send(&self, shard: usize, message: ShardMessage) -> bool {
        self.shards[shard]
            .upgrade()
            .is_some_and(|tx| tx.send(message).is_ok())
    }
}

/// Where regions ask for finished chunks.
#[derive(Clone)]
pub struct GenerationHandle {
    shards: Arc<Vec<mpsc::UnboundedSender<ShardMessage>>>,
    routing: Arc<Routing>,
}

impl ChunkPipeline for GenerationHandle {
    fn want(&self, pos: ChunkPos, reply: RegionHandle, priority: u32) -> bool {
        self.shards[self.routing.owner(pos)]
            .send(ShardMessage::Want {
                pos,
                reply,
                priority,
            })
            .is_ok()
    }

    fn unwant(&self, pos: ChunkPos, region: RegionPos) {
        let _ = self.shards[self.routing.owner(pos)].send(ShardMessage::Unwant { pos, region });
    }

    fn forget(&self, area: Arc<dyn Fn(ChunkPos) -> bool + Send + Sync>, done: oneshot::Sender<()>) {
        let forgotten: Vec<_> = self
            .shards
            .iter()
            .filter_map(|shard| {
                let (done, forgotten) = oneshot::channel();
                let area = area.clone();
                shard
                    .send(ShardMessage::Forget { area, done })
                    .ok()
                    .map(|_| forgotten)
            })
            .collect();
        let shards = self.shards.clone();
        tokio::spawn(async move {
            for forgotten in forgotten {
                let _ = forgotten.await;
            }
            let renewed: Vec<_> = shards
                .iter()
                .filter_map(|shard| {
                    let (done, renewed) = oneshot::channel();
                    shard
                        .send(ShardMessage::Renew { done })
                        .ok()
                        .map(|_| renewed)
                })
                .collect();
            for renewed in renewed {
                let _ = renewed.await;
            }
            let _ = done.send(());
        });
    }

    fn flush(&self, done: oneshot::Sender<()>) {
        let shards = self.shards.clone();
        tokio::spawn(async move {
            quiesce(&shards).await;
            let saved: Vec<_> = shards
                .iter()
                .filter_map(|shard| {
                    let (done, saved) = oneshot::channel();
                    shard.send(ShardMessage::Save { done }).ok().map(|_| saved)
                })
                .collect();
            for saved in saved {
                let _ = saved.await;
            }
            let _ = done.send(());
        });
    }

    fn idle(&self, done: oneshot::Sender<()>) {
        let shards = self.shards.clone();
        tokio::spawn(async move {
            quiesce(&shards).await;
            let shrunk: Vec<_> = shards
                .iter()
                .filter_map(|shard| {
                    let (done, shrunk) = oneshot::channel();
                    shard.send(ShardMessage::Idle { done }).ok().map(|_| shrunk)
                })
                .collect();
            for shrunk in shrunk {
                let _ = shrunk.await;
            }
            let _ = done.send(());
        });
    }
}

/// Waits until no shard has work left and no message between shards is
/// on its way: two rounds of reports that agree, every shard idle and as
/// many messages received as sent.
async fn quiesce(shards: &[mpsc::UnboundedSender<ShardMessage>]) {
    let mut previous: Option<Vec<Activity>> = None;
    loop {
        let mut round = Vec::with_capacity(shards.len());
        for shard in shards {
            let (reply, answer) = oneshot::channel();
            if shard.send(ShardMessage::Probe { reply }).is_err() {
                return;
            }
            match answer.await {
                Ok(activity) => round.push(activity),
                Err(_) => return,
            }
        }
        let sent: u64 = round.iter().map(|a| a.sent).sum();
        let received: u64 = round.iter().map(|a| a.received).sum();
        let settled = round.iter().all(|a| a.idle) && sent == received;
        if settled && previous.as_ref() == Some(&round) {
            return;
        }
        previous = settled.then_some(round);
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Activity {
    idle: bool,
    sent: u64,
    received: u64,
}

/// Names a decoration in progress: the shard coordinating it and its
/// number there.
#[derive(Debug, Clone, Copy)]
struct Ticket {
    shard: usize,
    id: u64,
}

enum ShardMessage {
    Want {
        pos: ChunkPos,
        reply: RegionHandle,
        priority: u32,
    },
    /// The region no longer waits for `pos`.
    Unwant {
        pos: ChunkPos,
        region: RegionPos,
    },
    /// Some shard's wanted chunk waits on `center` being decorated.
    Need {
        center: ChunkPos,
        from: usize,
        priority: Rank,
    },
    /// No wanted chunk of `from` waits on `center` any more.
    Unneed {
        center: ChunkPos,
        from: usize,
    },
    /// `center`, owned by the sender, is decorated.
    Decorated {
        center: ChunkPos,
    },
    /// A decoration asks for the part of its window the recipient owns.
    Acquire {
        ticket: Ticket,
        positions: Vec<ChunkPos>,
    },
    Granted {
        id: u64,
        chunks: Vec<ProtoChunk>,
    },
    /// Lent chunks coming back after a decoration.
    Release {
        chunks: Vec<ProtoChunk>,
    },
    /// Lent chunks lost to a panicking decoration.
    Lost {
        positions: Vec<ChunkPos>,
    },
    StartsWanted {
        block: (i32, i32),
        from: usize,
    },
    StartsReady {
        block: (i32, i32),
        starts: Arc<StartsBlock>,
    },
    Probe {
        reply: oneshot::Sender<Activity>,
    },
    /// Saves every chunk in progress, then answers.
    Save {
        done: oneshot::Sender<()>,
    },
    /// No region is loaded: drops what can be rebuilt, saves chunks in
    /// progress, then answers.
    Idle {
        done: oneshot::Sender<()>,
    },
    /// Drops what is known of finished and idle chunks `area` matches.
    Forget {
        area: Arc<dyn Fn(ChunkPos) -> bool + Send + Sync>,
        done: oneshot::Sender<()>,
    },
    /// Asks again for the decorations committed chunks wait on, once every
    /// shard has forgotten.
    Renew {
        done: oneshot::Sender<()>,
    },
}

impl ShardMessage {
    /// Whether the message passes between shards (and counts towards
    /// knowing when none is on its way).
    fn between_shards(&self) -> bool {
        !matches!(
            self,
            ShardMessage::Want { .. }
                | ShardMessage::Unwant { .. }
                | ShardMessage::Probe { .. }
                | ShardMessage::Save { .. }
                | ShardMessage::Idle { .. }
                | ShardMessage::Forget { .. }
                | ShardMessage::Renew { .. }
        )
    }
}

enum JobResult {
    Starts((i32, i32), Arc<StartsBlock>),
    Terrain(ChunkPos, Box<ProtoChunk>),
    Decorated(ChunkPos, Vec<ProtoChunk>),
    /// What storage holds for chunks the shard asked about; `None` where
    /// it could not be read.
    Loaded(Vec<(ChunkPos, Option<Saved>)>),
    /// Chunks being saved, encoded for storage.
    Encoded(Vec<(ChunkPos, Vec<u8>)>),
    /// Chunks whose saved data reached storage.
    Stored(Vec<ChunkPos>),
    Delivered(ChunkPos),
    /// Every region that asked for a finished chunk was gone; it comes
    /// back to be kept.
    Undelivered(ChunkPos, Box<ProtoChunk>),
    TerrainPanicked(ChunkPos),
    DecorationPanicked(ChunkPos, Vec<ChunkPos>),
}

enum SlotState {
    /// Storage is asked whether it already holds the chunk.
    Reading,
    /// Terrain waits for the structure starts around the chunk.
    WaitingStarts,
    /// Read back from storage; waits for the structure starts around it.
    Resuming(Box<ProtoChunk>),
    Generating,
    Ready(Box<ProtoChunk>),
    /// In a decoration window.
    Lent,
    /// Being finished for the regions that asked.
    Delivering,
    /// Handed to its region; only the decorated flag is kept, so its
    /// neighbours know. A window that needs it again gets a fresh copy.
    Delivered,
    /// Being written to storage before it is dropped; still usable.
    Saving(Box<ProtoChunk>),
}

struct Slot {
    state: SlotState,
    decorated: bool,
    /// Whether the pipeline may write the chunk: never once a region owns
    /// it or storage could not be read.
    persist: bool,
    last_used: u64,
}

impl Slot {
    fn new(state: SlotState, tick: u64) -> Self {
        Self {
            state,
            decorated: false,
            persist: true,
            last_used: tick,
        }
    }
}

enum StartsState {
    Computing,
    Ready(Arc<StartsBlock>),
}

struct StartsSlot {
    state: StartsState,
    last_used: u64,
    /// Chunks of this shard whose terrain waits for this block.
    waiting: Vec<ChunkPos>,
    /// Shards waiting for this block, which this shard owns.
    requesters: Vec<usize>,
}

/// A decoration this shard coordinates, collecting its window.
struct Acquisition {
    center: ChunkPos,
    window: Vec<Option<ProtoChunk>>,
    /// Owners still to ask, in shard order, with their part of the window.
    groups: VecDeque<(usize, Vec<ChunkPos>)>,
}

/// A request for chunks of this shard waiting until they are all ready.
struct PendingGrant {
    ticket: Ticket,
    positions: Vec<ChunkPos>,
}

struct Shard {
    index: usize,
    routing: Arc<Routing>,
    generator: Arc<dyn StagedGenerator>,
    shape: ChunkShape,
    storage: Option<ProtoStorage>,
    jobs_tx: mpsc::UnboundedSender<JobResult>,
    clock: watch::Receiver<u64>,
    tags: tuff_datapack::tags::TagFeed,
    tick: u64,
    slots: HashMap<ChunkPos, Slot>,
    /// Structure starts by block (see [`StartsBlock`]), owned or borrowed.
    starts: HashMap<(i32, i32), StartsSlot>,
    /// Finished chunks asked for, and who asked.
    wanted: HashMap<ChunkPos, Vec<RegionHandle>>,
    /// The most urgent priority each wanted chunk was asked with.
    priorities: HashMap<ChunkPos, u32>,
    /// Wanted chunks whose decorations are not asked for yet, in the order
    /// they are committed.
    candidates: BTreeSet<Candidate>,
    candidate_keys: HashMap<ChunkPos, Candidate>,
    want_seq: u64,
    /// Wanted chunks whose decorations are asked for, with their rank.
    committed: HashMap<ChunkPos, Rank>,
    /// How many wanted chunks that are not urgent may be committed at once.
    max_committed: usize,
    /// Undecorated chunks of this shard some committed chunk waits on.
    needed: HashMap<ChunkPos, Rank>,
    /// Shards to tell once a chunk of this shard is decorated.
    subscribers: HashMap<ChunkPos, Vec<usize>>,
    /// Chunks of other shards known to be decorated.
    known_decorated: HashSet<ChunkPos>,
    /// Chunks of other shards whose decoration was asked for, with the
    /// rank it was asked with.
    requested: HashMap<ChunkPos, Rank>,
    acquisitions: HashMap<u64, Acquisition>,
    next_ticket: u64,
    /// Centers whose decoration is being coordinated here.
    decorating: HashSet<ChunkPos>,
    /// How many decorations may be coordinated here at once.
    max_decorating: usize,
    /// Needed centers waiting for room, most urgent first and the oldest
    /// among equals; entries replaced by a newer one are skipped.
    queue: BinaryHeap<(Reverse<Rank>, Reverse<u64>, ChunkPos)>,
    queued: HashMap<ChunkPos, (Rank, u64)>,
    queue_seq: u64,
    pending_grants: Vec<PendingGrant>,
    /// Chunks of this shard currently in some decoration window.
    lent: usize,
    /// Messages this shard sent itself, handled before anything else.
    local: VecDeque<ShardMessage>,
    sent: u64,
    received: u64,
    /// Chunks to ask storage about once the current message is handled.
    to_read: Vec<ChunkPos>,
    /// Jobs and storage requests whose result has not come back.
    in_flight: usize,
    /// Answered once a save has stored everything.
    saving: Vec<oneshot::Sender<()>>,
    save_queued: bool,
}

fn around(center: ChunkPos) -> impl Iterator<Item = ChunkPos> {
    within(center, 1)
}

fn within(center: ChunkPos, radius: i32) -> impl Iterator<Item = ChunkPos> {
    (-radius..=radius).flat_map(move |dz| {
        (-radius..=radius).map(move |dx| ChunkPos::new(center.x + dx, center.z + dz))
    })
}

/// Starts pipelines.
pub struct GenerationActor;

/// Overrides of how much work a shard takes on at once.
#[derive(Debug, Default, Clone, Copy)]
struct Limits {
    decorating: Option<usize>,
    committed: Option<usize>,
}

impl GenerationActor {
    /// Starts the pipeline of a dimension whose generator is staged, with
    /// [`default_shards`] shards.
    pub fn spawn(generator: Arc<dyn StagedGenerator>, cx: PipelineContext) -> GenerationHandle {
        Self::spawn_sharded(generator, cx, default_shards())
    }

    /// Starts a pipeline split over `shards` shard actors.
    pub fn spawn_sharded(
        generator: Arc<dyn StagedGenerator>,
        cx: PipelineContext,
        shards: usize,
    ) -> GenerationHandle {
        Self::spawn_with(generator, cx, shards, Limits::default())
    }

    fn spawn_with(
        generator: Arc<dyn StagedGenerator>,
        cx: PipelineContext,
        shards: usize,
        limits: Limits,
    ) -> GenerationHandle {
        let shards = shards.max(1);
        let mut receivers = Vec::with_capacity(shards);
        let mut senders = Vec::with_capacity(shards);
        for _ in 0..shards {
            let (tx, rx) = mpsc::unbounded_channel();
            senders.push(tx);
            receivers.push(rx);
        }
        let routing = Arc::new(Routing {
            shards: senders.iter().map(|tx| tx.downgrade()).collect(),
            started: Instant::now(),
        });
        let tick = *cx.clock.borrow();
        for (index, messages) in receivers.into_iter().enumerate() {
            let (jobs_tx, jobs) = mpsc::unbounded_channel();
            let mut shard = Shard::new(
                index,
                routing.clone(),
                generator.clone(),
                &cx,
                jobs_tx,
                tick,
            );
            if let Some(max) = limits.decorating {
                shard.max_decorating = max;
            }
            if let Some(max) = limits.committed {
                shard.max_committed = max;
            }
            tuff_supervise::spawn("a generation pipeline shard", shard.run(messages, jobs));
        }
        GenerationHandle {
            shards: Arc::new(senders),
            routing,
        }
    }
}

impl Shard {
    fn new(
        index: usize,
        routing: Arc<Routing>,
        generator: Arc<dyn StagedGenerator>,
        cx: &PipelineContext,
        jobs_tx: mpsc::UnboundedSender<JobResult>,
        tick: u64,
    ) -> Self {
        let max_decorating = (rayon::current_num_threads() * DECORATIONS_PER_THREAD)
            .div_ceil(routing.shards.len())
            .max(DECORATIONS_PER_THREAD);
        Self {
            index,
            routing,
            generator,
            shape: cx.shape,
            storage: cx.storage.clone(),
            jobs_tx,
            clock: cx.clock.clone(),
            tags: cx.tags.clone(),
            tick,
            slots: HashMap::new(),
            starts: HashMap::new(),
            wanted: HashMap::new(),
            needed: HashMap::new(),
            priorities: HashMap::new(),
            candidates: BTreeSet::new(),
            candidate_keys: HashMap::new(),
            want_seq: 0,
            committed: HashMap::new(),
            max_committed: max_decorating,
            subscribers: HashMap::new(),
            known_decorated: HashSet::new(),
            requested: HashMap::new(),
            acquisitions: HashMap::new(),
            next_ticket: 0,
            decorating: HashSet::new(),
            max_decorating,
            queue: BinaryHeap::new(),
            queued: HashMap::new(),
            queue_seq: 0,
            pending_grants: Vec::new(),
            lent: 0,
            local: VecDeque::new(),
            sent: 0,
            received: 0,
            to_read: Vec::new(),
            in_flight: 0,
            saving: Vec::new(),
            save_queued: false,
        }
    }

    async fn run(
        mut self,
        mut messages: mpsc::UnboundedReceiver<ShardMessage>,
        mut jobs: mpsc::UnboundedReceiver<JobResult>,
    ) {
        let mut clock_open = true;
        loop {
            self.commit();
            while let Some(message) = self.local.pop_front() {
                self.on_message(message);
            }
            self.read_requested();
            self.continue_save();
            tokio::select! {
                message = messages.recv() => match message {
                    Some(message) => {
                        if message.between_shards() {
                            self.received += 1;
                        }
                        self.on_message(message);
                    }
                    None => return,
                },
                Some(result) = jobs.recv() => {
                    self.in_flight -= 1;
                    self.on_job(result);
                }
                changed = self.clock.changed(), if clock_open => {
                    if changed.is_err() {
                        clock_open = false;
                        continue;
                    }
                    self.tick = *self.clock.borrow_and_update();
                    if self.tick.is_multiple_of(EVICT_CHECK_TICKS) {
                        self.evict();
                    }
                }
            }
        }
    }

    fn owns(&self, pos: ChunkPos) -> bool {
        self.routing.owner(pos) == self.index
    }

    /// Sends a message to a shard, this one included.
    fn post(&mut self, shard: usize, message: ShardMessage) {
        if shard == self.index {
            self.local.push_back(message);
        } else if self.routing.send(shard, message) {
            self.sent += 1;
        }
    }

    fn on_message(&mut self, message: ShardMessage) {
        match message {
            ShardMessage::Want {
                pos,
                reply,
                priority,
            } => {
                let requesters = self.wanted.entry(pos).or_default();
                if !requesters.iter().any(|r| r.pos == reply.pos) {
                    requesters.push(reply);
                }
                if self
                    .priorities
                    .get(&pos)
                    .is_some_and(|best| *best <= priority)
                {
                    return;
                }
                self.priorities.insert(pos, priority);
                if self.committed.contains_key(&pos) {
                    let rank = self.rank(priority);
                    self.committed.insert(pos, rank);
                    for neighbour in around(pos) {
                        self.need(neighbour, rank);
                    }
                } else {
                    self.drop_candidate(pos);
                    self.want_seq += 1;
                    let batch = self.routing.started.elapsed().as_millis() as u64 / BATCH_MILLIS;
                    let cost = self.cost(pos);
                    let candidate = (priority, cost, Reverse(batch), self.want_seq, pos);
                    self.candidates.insert(candidate);
                    self.candidate_keys.insert(pos, candidate);
                }
                self.try_deliver(pos);
            }
            ShardMessage::Unwant { pos, region } => {
                let Some(requesters) = self.wanted.get_mut(&pos) else {
                    return;
                };
                requesters.retain(|r| r.pos != region);
                if requesters.is_empty() {
                    self.wanted.remove(&pos);
                    self.priorities.remove(&pos);
                    self.drop_candidate(pos);
                    if self.committed.remove(&pos).is_some() {
                        for center in around(pos) {
                            self.release_need(center);
                        }
                    }
                }
            }
            ShardMessage::Unneed { center, from } => {
                if let Some(subscribers) = self.subscribers.get_mut(&center) {
                    subscribers.retain(|shard| *shard != from);
                    if subscribers.is_empty() {
                        self.subscribers.remove(&center);
                    }
                }
                if !self.waits_on(center) && !self.subscribers.contains_key(&center) {
                    self.forget_need(center);
                }
            }
            ShardMessage::Need {
                center,
                from,
                priority,
            } => {
                if self.is_decorated(center) {
                    self.post(from, ShardMessage::Decorated { center });
                } else {
                    let subscribers = self.subscribers.entry(center).or_default();
                    if !subscribers.contains(&from) {
                        subscribers.push(from);
                    }
                    self.mark_needed(center, priority);
                    self.try_decorate(center);
                }
            }
            ShardMessage::Decorated { center } => {
                if !self.owns(center) {
                    self.known_decorated.insert(center);
                    self.requested.remove(&center);
                    self.touch(center);
                }
                for pos in around(center) {
                    self.try_deliver(pos);
                }
            }
            ShardMessage::Acquire { ticket, positions } => {
                for pos in &positions {
                    self.ensure_terrain(*pos);
                }
                self.pending_grants.push(PendingGrant { ticket, positions });
                self.try_grants();
            }
            ShardMessage::Granted { id, chunks } => self.granted(id, chunks),
            ShardMessage::Release { chunks } => {
                let positions: Vec<ChunkPos> = chunks.iter().map(|c| c.pos).collect();
                for chunk in chunks {
                    if let Some(slot) = self.slots.get_mut(&chunk.pos)
                        && matches!(slot.state, SlotState::Lent)
                    {
                        slot.state = SlotState::Ready(Box::new(chunk));
                        self.lent -= 1;
                    }
                }
                self.try_grants();
                for pos in positions {
                    self.try_deliver(pos);
                }
            }
            ShardMessage::Lost { positions } => {
                for pos in positions {
                    if self
                        .slots
                        .get(&pos)
                        .is_some_and(|slot| matches!(slot.state, SlotState::Lent))
                    {
                        self.slots.remove(&pos);
                        self.lent -= 1;
                    }
                }
                self.renew_needs();
            }
            ShardMessage::StartsWanted { block, from } => {
                let slot = self.starts_slot(block);
                match &slot.state {
                    StartsState::Ready(starts) => {
                        let starts = starts.clone();
                        self.post(from, ShardMessage::StartsReady { block, starts });
                    }
                    StartsState::Computing => {
                        if !slot.requesters.contains(&from) {
                            slot.requesters.push(from);
                        }
                    }
                }
            }
            ShardMessage::StartsReady { block, starts } => self.starts_ready(block, starts),
            ShardMessage::Probe { reply } => {
                let _ = reply.send(Activity {
                    idle: self.in_flight == 0
                        && self.acquisitions.is_empty()
                        && self.pending_grants.is_empty()
                        && self.lent == 0
                        && self.local.is_empty()
                        && self.to_read.is_empty(),
                    sent: self.sent,
                    received: self.received,
                });
            }
            ShardMessage::Save { done } => {
                self.saving.push(done);
                self.save_queued = false;
            }
            ShardMessage::Idle { done } => {
                self.on_idle();
                self.saving.push(done);
                self.save_queued = false;
            }
            ShardMessage::Forget { area, done } => {
                self.forget(&*area);
                let _ = done.send(());
            }
            ShardMessage::Renew { done } => {
                self.renew_committed();
                let _ = done.send(());
            }
        }
    }

    /// Needs the neighbours of every committed chunk again: a forget may
    /// have dropped decorations they counted on, and nothing else would
    /// ask for them.
    fn renew_committed(&mut self) {
        let committed: Vec<(ChunkPos, Rank)> = self
            .committed
            .iter()
            .map(|(pos, rank)| (*pos, *rank))
            .collect();
        for (pos, rank) in committed {
            for neighbour in around(pos) {
                self.need(neighbour, rank);
            }
        }
    }

    /// Nothing waits on this shard and no region holds a chunk, so
    /// storage answers for every delivered chunk and structure starts can
    /// be computed again: drops both and the generator's caches. Chunks in
    /// progress are saved (and then dropped) by the save that follows.
    fn on_idle(&mut self) {
        let busy = !self.wanted.is_empty()
            || !self.needed.is_empty()
            || !self.committed.is_empty()
            || !self.decorating.is_empty()
            || !self.acquisitions.is_empty()
            || !self.pending_grants.is_empty()
            || self.lent > 0;
        if busy {
            return;
        }
        if self.storage.is_some() {
            self.slots
                .retain(|_, slot| !matches!(slot.state, SlotState::Delivered));
        }
        self.starts.retain(|_, slot| {
            !matches!(slot.state, StartsState::Ready(_)) || !slot.waiting.is_empty()
        });
        self.known_decorated.clear();
        self.shrink_tables();
        if self.index == 0 {
            self.generator.shrink();
        }
    }

    fn shrink_tables(&mut self) {
        self.slots.shrink_to_fit();
        self.starts.shrink_to_fit();
        self.wanted.shrink_to_fit();
        self.priorities.shrink_to_fit();
        self.candidate_keys.shrink_to_fit();
        self.committed.shrink_to_fit();
        self.needed.shrink_to_fit();
        self.subscribers.shrink_to_fit();
        self.known_decorated.shrink_to_fit();
        self.requested.shrink_to_fit();
        self.acquisitions.shrink_to_fit();
        self.decorating.shrink_to_fit();
        self.queue.shrink_to_fit();
        self.queued.shrink_to_fit();
        self.pending_grants.shrink_to_fit();
        self.local.shrink_to_fit();
        self.to_read.shrink_to_fit();
    }

    /// Drops the delivered chunks (also those whose delivery is still in
    /// flight) `area` matches and the ready ones no
    /// decoration waits on, with what is known of other shards' ones, so
    /// storage (cleared by their regions) is asked about them again.
    fn forget(&mut self, area: &dyn Fn(ChunkPos) -> bool) {
        let mut protected: HashSet<ChunkPos> = HashSet::new();
        for pos in self.wanted.keys().chain(self.needed.keys()) {
            protected.extend(around(*pos));
        }
        for grant in &self.pending_grants {
            protected.extend(grant.positions.iter().copied());
        }
        self.slots.retain(|pos, slot| {
            !area(*pos)
                || match slot.state {
                    SlotState::Delivered | SlotState::Delivering => false,
                    SlotState::Ready(_) => protected.contains(pos),
                    _ => true,
                }
        });
        self.known_decorated.retain(|pos| !area(*pos));
    }

    /// Makes sure `center` gets decorated, asking its owner if that is
    /// another shard.
    fn need(&mut self, center: ChunkPos, priority: Rank) {
        if self.is_decorated(center) {
            return;
        }
        if self.owns(center) {
            self.mark_needed(center, priority);
            self.try_decorate(center);
        } else if self
            .requested
            .get(&center)
            .is_none_or(|asked| priority < *asked)
        {
            if self.requested.insert(center, priority).is_none() {
                self.touch(center);
            }
            let owner = self.routing.owner(center);
            let from = self.index;
            self.post(
                owner,
                ShardMessage::Need {
                    center,
                    from,
                    priority,
                },
            );
        }
    }

    fn forget_need(&mut self, center: ChunkPos) {
        self.queued.remove(&center);
        if self.needed.remove(&center).is_some() {
            self.touch(center);
        }
    }

    /// Whether `center` is decorated or its decoration is asked for.
    fn covered(&self, center: ChunkPos) -> bool {
        self.needed.contains_key(&center)
            || self.requested.contains_key(&center)
            || self.is_decorated(center)
    }

    /// How many decorations a wanted chunk waits on that nobody asked for.
    fn cost(&self, pos: ChunkPos) -> u8 {
        around(pos).filter(|center| !self.covered(*center)).count() as u8
    }

    /// Ranks the candidates around `center` again after it was covered or
    /// uncovered.
    fn touch(&mut self, center: ChunkPos) {
        for pos in around(center) {
            let Some(&old) = self.candidate_keys.get(&pos) else {
                continue;
            };
            let cost = self.cost(pos);
            if cost != old.1 {
                self.candidates.remove(&old);
                let new = (old.0, cost, old.2, old.3, pos);
                self.candidates.insert(new);
                self.candidate_keys.insert(pos, new);
            }
        }
    }

    /// Drops queue entries that were replaced or whose need went away once
    /// they outnumber the live ones.
    fn compact_queue(&mut self) {
        if self.queue.len() <= 2 * self.queued.len() + 1024 {
            return;
        }
        let queued = &self.queued;
        self.queue.retain(|(_, Reverse(seq), center)| {
            queued.get(center).is_some_and(|(_, latest)| latest == seq)
        });
    }

    /// Records that `center` (owned here) is needed, keeping its most
    /// urgent priority.
    fn mark_needed(&mut self, center: ChunkPos, priority: Rank) {
        match self.needed.get_mut(&center) {
            Some(best) => *best = (*best).min(priority),
            None => {
                self.needed.insert(center, priority);
                self.touch(center);
            }
        }
    }

    /// Whether a committed chunk of this shard waits on `center`.
    fn waits_on(&self, center: ChunkPos) -> bool {
        around(center).any(|pos| self.committed.contains_key(&pos))
    }

    fn rank(&self, priority: u32) -> Rank {
        ((priority as u64) << 44) | self.routing.started.elapsed().as_millis() as u64
    }

    fn drop_candidate(&mut self, pos: ChunkPos) {
        if let Some(candidate) = self.candidate_keys.remove(&pos) {
            self.candidates.remove(&candidate);
        }
    }

    /// Asks for the decorations of waiting wanted chunks while fewer than
    /// `max_committed` are committed; urgent ones always.
    fn commit(&mut self) {
        while let Some(&(priority, _, _, _, pos)) = self.candidates.first() {
            if priority > URGENT && self.committed.len() >= self.max_committed {
                break;
            }
            self.drop_candidate(pos);
            let rank = self.rank(priority);
            self.committed.insert(pos, rank);
            for neighbour in around(pos) {
                self.need(neighbour, rank);
            }
            self.try_deliver(pos);
        }
    }

    /// Lets go of `center`'s decoration once no wanted chunk of this shard
    /// waits on it, telling its owner if that is another shard. A
    /// decoration already under way still finishes.
    fn release_need(&mut self, center: ChunkPos) {
        if self.waits_on(center) {
            return;
        }
        if self.owns(center) {
            if !self.subscribers.contains_key(&center) {
                self.forget_need(center);
            }
        } else if self.requested.remove(&center).is_some() {
            self.touch(center);
            let owner = self.routing.owner(center);
            let from = self.index;
            self.post(owner, ShardMessage::Unneed { center, from });
        }
    }

    /// Asks again for the decorations wanted chunks wait on, after chunks
    /// were lost to a panic.
    fn renew_needs(&mut self) {
        let wanted: Vec<(ChunkPos, Rank)> = self
            .committed
            .iter()
            .map(|(pos, rank)| (*pos, *rank))
            .collect();
        for (pos, priority) in wanted {
            for neighbour in around(pos) {
                if self.owns(neighbour) {
                    self.need(neighbour, priority);
                }
            }
        }
        let needed: Vec<ChunkPos> = self.needed.keys().copied().collect();
        for center in needed {
            self.try_decorate(center);
        }
    }

    fn on_job(&mut self, result: JobResult) {
        match result {
            JobResult::Starts(block, starts) => self.starts_ready(block, starts),
            JobResult::Terrain(pos, proto) => {
                if let Some(slot) = self.slots.get_mut(&pos) {
                    slot.state = SlotState::Ready(proto);
                }
                self.try_grants();
                self.try_deliver(pos);
            }
            JobResult::Decorated(center, chunks) => self.decorated(center, chunks),
            JobResult::Loaded(results) => self.loaded(results),
            JobResult::Encoded(blobs) => self.store(blobs),
            JobResult::Stored(positions) => {
                for pos in positions {
                    if matches!(
                        self.slots.get(&pos).map(|s| &s.state),
                        Some(SlotState::Saving(_))
                    ) {
                        self.slots.remove(&pos);
                    }
                }
            }
            JobResult::Delivered(pos) => {
                if let Some(slot) = self
                    .slots
                    .get_mut(&pos)
                    .filter(|slot| matches!(slot.state, SlotState::Delivering))
                {
                    slot.state = SlotState::Delivered;
                    slot.persist = false;
                }
                self.retry_grants();
            }
            JobResult::Undelivered(pos, proto) => {
                if let Some(slot) = self
                    .slots
                    .get_mut(&pos)
                    .filter(|slot| matches!(slot.state, SlotState::Delivering))
                {
                    slot.state = SlotState::Ready(proto);
                }
                self.try_grants();
            }
            JobResult::TerrainPanicked(pos) => {
                tuff_core::log::error!("terrain of chunk {pos:?} panicked; retrying");
                self.slots.remove(&pos);
                self.renew_needs();
                self.retry_grants();
            }
            JobResult::DecorationPanicked(center, positions) => {
                tuff_core::log::error!("decorating chunk {center:?} panicked; retrying");
                self.decorating.remove(&center);
                self.admit();
                let mut by_owner: HashMap<usize, Vec<ChunkPos>> = HashMap::new();
                for pos in positions {
                    by_owner
                        .entry(self.routing.owner(pos))
                        .or_default()
                        .push(pos);
                }
                for (owner, positions) in by_owner {
                    self.post(owner, ShardMessage::Lost { positions });
                }
            }
        }
    }

    /// Asks again for chunks of pending grants that were dropped.
    fn retry_grants(&mut self) {
        let positions: Vec<ChunkPos> = self
            .pending_grants
            .iter()
            .flat_map(|g| g.positions.iter().copied())
            .collect();
        for pos in positions {
            self.ensure_terrain(pos);
        }
    }

    fn spawn_job(&mut self, job: impl FnOnce() -> JobResult + Send + 'static, panicked: JobResult) {
        self.in_flight += 1;
        let tx = self.jobs_tx.clone();
        let tags = self.tags.current();
        rayon::spawn(move || {
            let job = std::panic::AssertUnwindSafe(|| tuff_datapack::tags::sync_scope(tags, job));
            let result = std::panic::catch_unwind(job).unwrap_or(panicked);
            let _ = tx.send(result);
        });
    }

    /// Makes sure the terrain of `pos` (owned here) exists or is on its
    /// way; true if it is here and free.
    fn ensure_terrain(&mut self, pos: ChunkPos) -> bool {
        let tick = self.tick;
        if let Some(slot) = self.slots.get_mut(&pos) {
            slot.last_used = tick;
            match std::mem::replace(&mut slot.state, SlotState::WaitingStarts) {
                SlotState::Saving(proto) => {
                    slot.state = SlotState::Ready(proto);
                    return true;
                }
                SlotState::Delivered => {}
                other => {
                    let ready = matches!(other, SlotState::Ready(_));
                    slot.state = other;
                    return ready;
                }
            }
        } else if self.storage.is_some() {
            self.slots.insert(pos, Slot::new(SlotState::Reading, tick));
            self.to_read.push(pos);
            return false;
        } else {
            self.slots
                .insert(pos, Slot::new(SlotState::WaitingStarts, tick));
        }
        self.request_terrain(pos);
        false
    }

    /// Asks storage about the chunks gathered while handling the last
    /// message; the answer is decoded on the rayon pool.
    fn read_requested(&mut self) {
        let Some(storage) = &self.storage else {
            return;
        };
        if self.to_read.is_empty() {
            return;
        }
        let positions = std::mem::take(&mut self.to_read);
        let answer = storage.read(&positions);
        let storage = storage.clone();
        let shape = self.shape;
        let tx = self.jobs_tx.clone();
        self.in_flight += 1;
        tokio::spawn(async move {
            let results = answer.await.ok();
            rayon::spawn(move || {
                let _span = tuff_timings::span!("worldgen.load");
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    decode_reads(&storage, shape, &positions, results)
                }))
                .unwrap_or_else(|_| positions.iter().map(|pos| (*pos, None)).collect());
                let _ = tx.send(JobResult::Loaded(decoded));
            });
        });
    }

    fn loaded(&mut self, results: Vec<(ChunkPos, Option<Saved>)>) {
        for (pos, saved) in results {
            let Some(slot) = self.slots.get_mut(&pos) else {
                continue;
            };
            if !matches!(slot.state, SlotState::Reading) {
                continue;
            }
            match saved {
                Some(Saved::Proto(proto, decorated)) => {
                    slot.decorated = decorated;
                    slot.state = SlotState::Resuming(proto);
                }
                Some(Saved::Full) => {
                    slot.decorated = true;
                    slot.persist = false;
                    slot.state = SlotState::Delivered;
                }
                Some(Saved::Nothing) => slot.state = SlotState::WaitingStarts,
                None => {
                    slot.persist = false;
                    slot.state = SlotState::WaitingStarts;
                }
            }
            let decorated = slot.decorated;
            let delivered = matches!(slot.state, SlotState::Delivered);
            if decorated {
                self.became_decorated(pos);
            }
            if delivered {
                self.retry_grants();
            } else {
                self.request_terrain(pos);
            }
        }
    }

    /// Tells everyone waiting that `center`, owned here, is decorated.
    fn became_decorated(&mut self, center: ChunkPos) {
        self.needed.remove(&center);
        self.queued.remove(&center);
        self.touch(center);
        for shard in self.subscribers.remove(&center).unwrap_or_default() {
            self.post(shard, ShardMessage::Decorated { center });
        }
        for pos in around(center) {
            self.try_deliver(pos);
        }
    }

    fn starts_slot(&mut self, block: (i32, i32)) -> &mut StartsSlot {
        let tick = self.tick;
        if !self.starts.contains_key(&block) {
            if self.routing.starts_owner(block) == self.index {
                let generator = self.generator.clone();
                let shape = self.shape;
                let tx = self.jobs_tx.clone();
                let tags = self.tags.current();
                self.in_flight += 1;
                rayon::spawn(move || {
                    let _span = tuff_timings::span!("worldgen.structure_starts");
                    let staged = &*generator;
                    let starts = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        tuff_datapack::tags::sync_scope(tags, || staged.starts(block, shape))
                    }))
                    .unwrap_or_else(|_| {
                        tuff_core::log::error!(
                            "structure starts of block {block:?} panicked; none start"
                        );
                        StartsBlock {
                            block,
                            starts: Vec::new(),
                        }
                    });
                    let _ = tx.send(JobResult::Starts(block, Arc::new(starts)));
                });
            } else {
                let owner = self.routing.starts_owner(block);
                let from = self.index;
                self.post(owner, ShardMessage::StartsWanted { block, from });
            }
            self.starts.insert(
                block,
                StartsSlot {
                    state: StartsState::Computing,
                    last_used: tick,
                    waiting: Vec::new(),
                    requesters: Vec::new(),
                },
            );
        }
        let slot = self.starts.get_mut(&block).expect("inserted above");
        slot.last_used = tick;
        slot
    }

    fn starts_ready(&mut self, block: (i32, i32), starts: Arc<StartsBlock>) {
        let tick = self.tick;
        let slot = self.starts.entry(block).or_insert_with(|| StartsSlot {
            state: StartsState::Computing,
            last_used: tick,
            waiting: Vec::new(),
            requesters: Vec::new(),
        });
        slot.state = StartsState::Ready(starts.clone());
        slot.last_used = tick;
        let waiting = std::mem::take(&mut slot.waiting);
        let requesters = std::mem::take(&mut slot.requesters);
        for shard in requesters {
            self.post(
                shard,
                ShardMessage::StartsReady {
                    block,
                    starts: starts.clone(),
                },
            );
        }
        for pos in waiting {
            self.request_terrain(pos);
        }
    }

    /// Starts the terrain job (or the resumption) of a chunk waiting for
    /// structure starts once every block of starts around it is here,
    /// asking for missing ones.
    fn request_terrain(&mut self, pos: ChunkPos) {
        if !matches!(
            self.slots.get(&pos).map(|s| &s.state),
            Some(SlotState::WaitingStarts | SlotState::Resuming(_))
        ) {
            return;
        }
        let mut ready = Vec::new();
        let mut missing = false;
        for block in StartsBlock::around(pos) {
            let slot = self.starts_slot(block);
            match &slot.state {
                StartsState::Ready(starts) => ready.push(starts.clone()),
                StartsState::Computing => {
                    missing = true;
                    if !slot.waiting.contains(&pos) {
                        slot.waiting.push(pos);
                    }
                }
            }
        }
        if missing {
            return;
        }
        let slot = self.slots.get_mut(&pos).expect("checked above");
        let state = std::mem::replace(&mut slot.state, SlotState::Generating);
        let generator = self.generator.clone();
        let shape = self.shape;
        self.spawn_job(
            move || {
                let _span = tuff_timings::span!("worldgen.terrain");
                let staged = &*generator;
                let proto = match state {
                    SlotState::Resuming(proto) => staged.resume(*proto, &ready),
                    _ => staged.terrain(pos, shape, &ready),
                };
                JobResult::Terrain(pos, Box::new(proto))
            },
            JobResult::TerrainPanicked(pos),
        );
    }
}

mod decorate;

/// Decodes what storage answered for `positions`; `None` for chunks it
/// could not read.
fn decode_reads(
    storage: &ProtoStorage,
    shape: ChunkShape,
    positions: &[ChunkPos],
    results: Option<Vec<IoResult>>,
) -> Vec<(ChunkPos, Option<Saved>)> {
    let Some(results) = results else {
        return positions.iter().map(|pos| (*pos, None)).collect();
    };
    results
        .into_iter()
        .filter_map(|result| match result {
            IoResult::Read { pos, blocks, .. } => Some((pos, blocks)),
            IoResult::Written(_) | IoResult::Removed { .. } => None,
        })
        .map(|(pos, blocks)| {
            let saved = match blocks {
                Ok(None) => Some(Saved::Nothing),
                Ok(Some(blob)) => match storage.codec().decode_root(&blob, shape) {
                    Ok(root) => Some(saved::from_nbt(&root, pos, shape)),
                    Err(error) => {
                        tuff_core::log::error!("chunk {pos:?} in progress is unreadable: {error}");
                        None
                    }
                },
                Err(error) => {
                    tuff_core::log::error!("chunk {pos:?} in progress is unreadable: {error}");
                    None
                }
            };
            (pos, saved)
        })
        .collect()
}

/// Sends a finished chunk to every region that asked for it; hands it
/// back if none of them is there any more.
fn send_finished(
    pos: ChunkPos,
    finished: Finished,
    requesters: &[RegionHandle],
) -> Option<Finished> {
    let mut delivered = false;
    for requester in requesters {
        delivered |= requester.internal(RegionInternal::Generated {
            pos,
            finished: Box::new(finished.clone()),
        });
    }
    (!delivered).then_some(finished)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod throughput_tests;

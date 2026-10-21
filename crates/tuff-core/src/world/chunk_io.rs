//! Chunk storage as the world sees it: region actors send batches of chunk
//! reads and writes keyed only by chunk position, and this actor maps them
//! onto the backend's file regions. Each file region's store is owned here
//! and used by one blocking job at a time, so region actors of any size can
//! share a file safely.
//!
//! The actor also owns the save gate: whether automatic saving is on
//! (`/save-off`, `/save-on`) and whether a hold keeps every write away from
//! storage (a backup copying the world's files). Actors read the state
//! from a `watch`; while a hold is closed, chunk writes that still arrive
//! are parked here and document writes are deferred by the gated backend,
//! both until the hold is released.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use crate::metrics::{ChunkIoReport, Metrics, Pacer, Report, millis};
use tuff_storage::backend::RegionStore;
use tuff_storage::backend::WorldBackend;
use tuff_storage::gate::{DocumentGate, GatedBackend};
use tuff_supervise::{CatchUnwind, Critical, Fatal};
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

/// Idle file stores kept open beyond this are closed.
const MAX_IDLE_STORES: usize = 256;

/// Chunk, entity and point of interest data as the backend stores it.
pub type Blob = Vec<u8>;

#[derive(Debug)]
pub enum IoOp {
    Read(ChunkPos),
    /// Reads the block blob only; the entity and point of interest
    /// results are `Ok(None)` without touching their stores.
    ReadBlocks(ChunkPos),
    Write {
        pos: ChunkPos,
        blocks: Option<Blob>,
        entities: EntityWrite,
        pois: EntityWrite,
    },
    /// Deletes the chunk with its entity and point of interest records.
    Remove(ChunkPos),
}

impl IoOp {
    pub fn pos(&self) -> ChunkPos {
        match self {
            IoOp::Read(pos)
            | IoOp::ReadBlocks(pos)
            | IoOp::Write { pos, .. }
            | IoOp::Remove(pos) => *pos,
        }
    }

    /// Whether the op changes storage.
    pub fn writes(&self) -> bool {
        !matches!(self, IoOp::Read(_) | IoOp::ReadBlocks(_))
    }
}

/// What to do with a chunk's entity (or point of interest) record.
#[derive(Debug)]
pub enum EntityWrite {
    Keep,
    Clear,
    Write(Blob),
}

pub enum IoResult {
    Read {
        pos: ChunkPos,
        blocks: Result<Option<Blob>, String>,
        entities: Result<Option<Blob>, String>,
        pois: Result<Option<Blob>, String>,
    },
    Written(ChunkPos),
    /// The delete of a chunk is done; `ok` is false when storage failed.
    Removed {
        pos: ChunkPos,
        ok: bool,
    },
}

/// Where a hold on saving is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldPhase {
    /// Automatic saves stop while the holder flushes the world.
    Flushing,
    /// Nothing reaches storage.
    Held,
}

/// What saving is allowed now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveState {
    /// Off after `/save-off`.
    pub autosave: bool,
    pub hold: Option<HoldPhase>,
    /// Who holds saving, while `hold` is set.
    pub holder: Option<Arc<str>>,
}

impl Default for SaveState {
    fn default() -> Self {
        Self {
            autosave: true,
            hold: None,
            holder: None,
        }
    }
}

impl SaveState {
    /// Periodic saves, and saving idle chunks so they can be unloaded.
    pub fn autosaves(&self) -> bool {
        self.autosave && self.hold.is_none()
    }

    /// Saves asked for explicitly (`/save-all`).
    pub fn saves(&self) -> bool {
        self.hold != Some(HoldPhase::Held)
    }
}

/// Another hold is in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldBusy {
    pub owner: String,
}

enum GateRequest {
    Autosave {
        on: bool,
        reply: oneshot::Sender<bool>,
    },
    Begin {
        owner: String,
        limit: Duration,
        reply: oneshot::Sender<Result<u64, HoldBusy>>,
    },
    Close {
        id: u64,
        reply: oneshot::Sender<bool>,
    },
    /// Releases hold `id`, or any hold when `None`.
    Release {
        id: Option<u64>,
        reply: Option<oneshot::Sender<()>>,
    },
}

enum Message {
    Batch(Batch),
    Gate(GateRequest),
    Shrink(oneshot::Sender<usize>),
    Scratch {
        dimension: Arc<str>,
        on: bool,
        reply: oneshot::Sender<bool>,
    },
    #[cfg(test)]
    Panic,
}

/// A handle to the server's chunk storage actor.
#[derive(Clone)]
pub struct ChunkIo {
    // Unbounded: each region keeps at most one batch in flight, so the
    // queue is bounded by the number of regions, and a region must never
    // wait on storage.
    tx: mpsc::UnboundedSender<Message>,
    saves: watch::Receiver<SaveState>,
    failures: watch::Receiver<u64>,
    backend: Arc<dyn WorldBackend>,
}

impl ChunkIo {
    pub fn start(backend: Arc<dyn WorldBackend>) -> Self {
        Self::start_with(backend, None, Fatal::default())
    }

    /// Starts the actor reporting its queue and throughput to `metrics`.
    pub fn start_metered(backend: Arc<dyn WorldBackend>, metrics: Metrics) -> Self {
        Self::start_with(backend, Some(metrics), Fatal::default())
    }

    /// Like [`Self::start_metered`], and a panic of this actor or its
    /// document gate stops the server through `fatal`.
    pub fn start_supervised(
        backend: Arc<dyn WorldBackend>,
        metrics: Metrics,
        fatal: Fatal,
    ) -> Self {
        Self::start_with(backend, Some(metrics), fatal)
    }

    fn start_with(backend: Arc<dyn WorldBackend>, metrics: Option<Metrics>, fatal: Fatal) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (done_tx, done) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (state, saves) = watch::channel(SaveState::default());
        let (failures_tx, failures) = watch::channel(0);
        let documents = DocumentGate::start_supervised(
            backend.clone(),
            tuff_supervise::Critical::new("the document save gate", fatal.clone()),
        );
        let gated: Arc<dyn WorldBackend> =
            Arc::new(GatedBackend::new(backend.clone(), documents.clone()));
        let actor = Actor {
            backend,
            scratch: HashMap::new(),
            voided: HashSet::new(),
            files: HashMap::new(),
            pending: HashMap::new(),
            next_batch: 0,
            done_tx,
            failures: failures_tx,
            metrics: metrics.map(|metrics| Meter {
                metrics,
                pacer: Pacer::default(),
                counts: IoCounts::default(),
                save_latency_max: std::time::Duration::ZERO,
            }),
            gate: Gate {
                state,
                hold: None,
                next_hold: 0,
                paused: false,
                writing: 0,
                documents,
                closing: None,
                events_tx,
            },
        };
        let inbox = Inbox {
            messages: rx,
            done,
            events,
            open: true,
        };
        tokio::spawn(actor.run(inbox, fatal));
        Self {
            tx,
            saves,
            failures,
            backend: gated,
        }
    }

    /// How many reads and writes storage has failed so far; it changes on
    /// every new failure.
    pub fn failures(&self) -> watch::Receiver<u64> {
        self.failures.clone()
    }

    /// Queues `ops` on `dimension`'s storage. The answer holds one result
    /// per op, in order; it is dropped if a storage job panicked.
    pub fn submit(&self, dimension: &str, ops: Vec<IoOp>) -> oneshot::Receiver<Vec<IoResult>> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Message::Batch(Batch {
            dimension: dimension.into(),
            ops,
            reply,
        }));
        answer
    }

    /// Keeps `dimension`'s chunks in memory instead of the world's storage
    /// (`on`), starting empty, or drops them all (`off`), after which its
    /// reads find nothing and its writes are dropped until it is kept in
    /// memory again. The answer is false when `on` finds it already kept,
    /// or `off` finds it not kept.
    pub fn scratch(&self, dimension: &str, on: bool) -> oneshot::Receiver<bool> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Message::Scratch {
            dimension: dimension.into(),
            on,
            reply,
        });
        answer
    }

    /// Closes every region file store with nothing queued and gives back
    /// spare table capacity, for when the world is idle. The answer is how
    /// many files were let go.
    pub fn shrink(&self) -> oneshot::Receiver<usize> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Message::Shrink(reply));
        answer
    }

    /// The backend everything but this actor should use: its document
    /// and extra-file writes obey the save gate.
    pub fn backend(&self) -> &Arc<dyn WorldBackend> {
        &self.backend
    }

    /// What saving is allowed now; cheap enough to ask every tick.
    pub fn saves(&self) -> SaveState {
        self.saves.borrow().clone()
    }

    pub fn watch_saves(&self) -> watch::Receiver<SaveState> {
        self.saves.clone()
    }

    async fn ask<T>(&self, request: impl FnOnce(oneshot::Sender<T>) -> GateRequest) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Message::Gate(request(reply))).ok()?;
        answer.await.ok()
    }

    /// Turns automatic saving on or off (`/save-on`, `/save-off`); false
    /// if it already was.
    pub async fn set_autosave(&self, on: bool) -> bool {
        self.ask(|reply| GateRequest::Autosave { on, reply })
            .await
            .unwrap_or(false)
    }

    /// Starts a hold for `owner`: automatic saves stop so the world can be
    /// flushed before [`Self::close_hold`]. The hold lifts by itself after
    /// `limit`. One hold at a time.
    pub async fn begin_hold(&self, owner: &str, limit: Duration) -> Result<u64, HoldBusy> {
        let name = owner.to_owned();
        self.ask(|reply| GateRequest::Begin {
            owner: name,
            limit,
            reply,
        })
        .await
        .unwrap_or_else(|| {
            Err(HoldBusy {
                owner: owner.to_owned(),
            })
        })
    }

    /// Stops every write for hold `id` and answers once the writes already
    /// under way are done; false if that hold is gone.
    pub async fn close_hold(&self, id: u64) -> bool {
        self.ask(|reply| GateRequest::Close { id, reply })
            .await
            .unwrap_or(false)
    }

    /// Lifts hold `id`; answers once what it held back is written.
    pub async fn release_hold(&self, id: u64) {
        self.ask(|reply| GateRequest::Release {
            id: Some(id),
            reply: Some(reply),
        })
        .await;
    }

    /// Lifts hold `id` without waiting.
    pub fn release_hold_soon(&self, id: u64) {
        let _ = self.tx.send(Message::Gate(GateRequest::Release {
            id: Some(id),
            reply: None,
        }));
    }

    /// Lifts any hold, for shutdown's final save.
    pub async fn release_all(&self) {
        self.ask(|reply| GateRequest::Release {
            id: None,
            reply: Some(reply),
        })
        .await;
    }
}

/// [`ChunkIo`], started the first time a region needs it.
#[derive(Default)]
pub struct ChunkIoCell(OnceLock<ChunkIo>);

impl ChunkIoCell {
    /// A cell holding an actor already started.
    pub fn with(io: ChunkIo) -> Self {
        Self(OnceLock::from(io))
    }

    pub fn get(&self, backend: &Arc<dyn WorldBackend>) -> &ChunkIo {
        self.0.get_or_init(|| ChunkIo::start(backend.clone()))
    }
}

struct Batch {
    dimension: Arc<str>,
    ops: Vec<IoOp>,
    reply: oneshot::Sender<Vec<IoResult>>,
}

type FileKey = (Arc<str>, FileRegionPos);

/// Ops of one batch that fall in one file, with their indices in the batch.
type Part = (u64, Vec<(usize, IoOp)>);

#[derive(Default)]
struct File {
    store: Option<Box<dyn RegionStore>>,
    busy: bool,
    queue: VecDeque<Part>,
    /// Parts kept back while writes are paused, in arrival order.
    parked: VecDeque<Part>,
    /// The running job's store belongs to a memory store since dropped.
    stale: bool,
}

impl File {
    /// Whether `ops` must wait while writes are paused: they write, or
    /// read a chunk a parked part writes.
    fn must_park(&self, ops: &[(usize, IoOp)]) -> bool {
        if ops.iter().any(|(_, op)| op.writes()) {
            return true;
        }
        if self.parked.is_empty() {
            return false;
        }
        let held: HashSet<ChunkPos> = self
            .parked
            .iter()
            .flat_map(|(_, ops)| ops.iter().map(|(_, op)| op.pos()))
            .collect();
        ops.iter().any(|(_, op)| held.contains(&op.pos()))
    }
}

struct Pending {
    parts: usize,
    results: Vec<(usize, IoResult)>,
    failed: bool,
    reply: oneshot::Sender<Vec<IoResult>>,
    accepted: Instant,
    writes: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct IoCounts {
    reads: u64,
    writes: u64,
    bytes_read: u64,
    bytes_written: u64,
    failures: u64,
}

impl IoCounts {
    fn add(&mut self, other: IoCounts) {
        self.reads += other.reads;
        self.writes += other.writes;
        self.bytes_read += other.bytes_read;
        self.bytes_written += other.bytes_written;
        self.failures += other.failures;
    }
}

/// What the actor reports to the metrics actor, summed over a window.
struct Meter {
    metrics: Metrics,
    pacer: Pacer,
    counts: IoCounts,
    save_latency_max: std::time::Duration,
}

struct Done {
    file: FileKey,
    batch: u64,
    store: Option<Box<dyn RegionStore>>,
    /// `None` if the job panicked.
    results: Option<Vec<(usize, IoResult)>>,
    counts: IoCounts,
    writes: bool,
}

/// Answers the actor with a failed job if dropped unfinished, so a file's
/// queue and its waiters are never lost.
struct JobGuard {
    done: mpsc::UnboundedSender<Done>,
    target: Option<(FileKey, u64)>,
    store: Option<Box<dyn RegionStore>>,
    writes: bool,
}

impl JobGuard {
    fn finish(&mut self, results: Option<Vec<(usize, IoResult)>>, counts: IoCounts) {
        if let Some((file, batch)) = self.target.take() {
            let store = if results.is_some() {
                self.store.take()
            } else {
                None
            };
            let _ = self.done.send(Done {
                file,
                batch,
                store,
                results,
                counts,
                writes: self.writes,
            });
        }
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.finish(None, IoCounts::default());
    }
}

struct Hold {
    id: u64,
    owner: Arc<str>,
    limit: Duration,
    deadline: tokio::time::Instant,
}

struct Closing {
    id: u64,
    reply: oneshot::Sender<bool>,
    documents_paused: bool,
}

/// The save gate's state, owned by the actor.
struct Gate {
    state: watch::Sender<SaveState>,
    hold: Option<Hold>,
    next_hold: u64,
    /// Chunk writes are parked instead of run.
    paused: bool,
    /// Jobs with writes still running.
    writing: usize,
    documents: DocumentGate,
    closing: Option<Closing>,
    // Unbounded: at most one event per close.
    events_tx: mpsc::UnboundedSender<u64>,
}

impl Gate {
    fn publish(&self) {
        let hold = self.hold.as_ref().map(|_| {
            if self.paused {
                HoldPhase::Held
            } else {
                HoldPhase::Flushing
            }
        });
        let holder = self.hold.as_ref().map(|hold| hold.owner.clone());
        self.state.send_if_modified(|state| {
            let changed = state.hold != hold || state.holder != holder;
            state.hold = hold;
            state.holder = holder;
            changed
        });
    }

    /// Resumes document writes and answers `reply` once the held-back
    /// documents are written. The document gate answers in order, so this
    /// also waits for a resume already under way.
    fn resume_documents(&self, reply: Option<oneshot::Sender<()>>) {
        let resumed = self.documents.resume();
        tokio::spawn(async move {
            if let Ok(written) = resumed.await
                && written > 0
            {
                log::info!("wrote {written} documents held back while saving was held");
            }
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
        });
    }

    fn try_finish_closing(&mut self) {
        let ready = self.writing == 0
            && self
                .closing
                .as_ref()
                .is_some_and(|closing| closing.documents_paused);
        if ready && let Some(closing) = self.closing.take() {
            let _ = closing.reply.send(true);
        }
    }
}

/// The actor's receivers, kept outside its loop so they outlive a panic.
struct Inbox {
    messages: mpsc::UnboundedReceiver<Message>,
    done: mpsc::UnboundedReceiver<Done>,
    events: mpsc::UnboundedReceiver<u64>,
    /// Handles still exist.
    open: bool,
}

struct Actor {
    backend: Arc<dyn WorldBackend>,
    /// Dimensions whose chunks live in memory, each in its own store.
    scratch: HashMap<Arc<str>, Arc<dyn WorldBackend>>,
    /// Dimensions whose memory store was dropped: nothing is read or kept.
    voided: HashSet<Arc<str>>,
    files: HashMap<FileKey, File>,
    pending: HashMap<u64, Pending>,
    next_batch: u64,
    // Unbounded: at most one completion per file in flight.
    done_tx: mpsc::UnboundedSender<Done>,
    /// Counts the reads and writes storage failed, for whoever warns
    /// about a full disk.
    failures: watch::Sender<u64>,
    metrics: Option<Meter>,
    gate: Gate,
}

impl Actor {
    /// Serves until every handle is gone and nothing is pending. Storage
    /// is critical: a panic asks the server to stop and the loop resumes
    /// on the same files and queues, so the shutdown's saves still land.
    /// A store taken by a job that was not started when the loop
    /// panicked is lost with its file's queue.
    async fn run(mut self, mut inbox: Inbox, fatal: Fatal) {
        let mut critical = Critical::new("chunk storage", fatal);
        loop {
            match CatchUnwind::new(Box::pin(self.serve(&mut inbox))).await {
                Ok(()) => return,
                Err(panic) if critical.panicked(&*panic) => {}
                Err(_) => return,
            }
        }
    }

    async fn serve(&mut self, inbox: &mut Inbox) {
        let Inbox {
            messages,
            done,
            events,
            open,
        } = inbox;
        let mut report = tokio::time::interval(
            self.metrics
                .as_ref()
                .map_or(std::time::Duration::from_secs(1), |m| {
                    m.metrics.config().interval
                }),
        );
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let deadline = self.gate.hold.as_ref().map(|hold| hold.deadline);
            tokio::select! {
                message = messages.recv(), if *open => match message {
                    Some(Message::Batch(batch)) => self.accept(batch),
                    Some(Message::Gate(request)) => self.on_gate(request),
                    Some(Message::Shrink(reply)) => {
                        let _ = reply.send(self.shrink());
                    }
                    Some(Message::Scratch { dimension, on, reply }) => {
                        let _ = reply.send(self.set_scratch(dimension, on));
                    }
                    #[cfg(test)]
                    Some(Message::Panic) => panic!("chunk storage test panic"),
                    None => {
                        *open = false;
                        self.release(None, None);
                    }
                },
                Some(finished) = done.recv() => self.finished(finished),
                Some(id) = events.recv() => self.documents_paused(id),
                _ = sleep_until(deadline) => {
                    if let Some(hold) = &self.gate.hold {
                        log::warn!(
                            "{} held saving past its {:?} limit; saving resumes",
                            hold.owner,
                            hold.limit
                        );
                    }
                    self.release(None, None);
                }
                _ = report.tick(), if self.metrics.is_some() => {}
            }
            self.report_metrics();
            if !*open && self.pending.is_empty() {
                return;
            }
        }
    }

    fn on_gate(&mut self, request: GateRequest) {
        match request {
            GateRequest::Autosave { on, reply } => {
                let changed = self.gate.state.send_if_modified(|state| {
                    let changed = state.autosave != on;
                    state.autosave = on;
                    changed
                });
                let _ = reply.send(changed);
            }
            GateRequest::Begin {
                owner,
                limit,
                reply,
            } => {
                if let Some(hold) = &self.gate.hold {
                    let _ = reply.send(Err(HoldBusy {
                        owner: hold.owner.to_string(),
                    }));
                    return;
                }
                self.gate.next_hold += 1;
                let id = self.gate.next_hold;
                log::info!("{owner} holds saving");
                self.gate.hold = Some(Hold {
                    id,
                    owner: owner.into(),
                    limit,
                    deadline: tokio::time::Instant::now() + limit,
                });
                self.gate.publish();
                let _ = reply.send(Ok(id));
            }
            GateRequest::Close { id, reply } => {
                if self.gate.hold.as_ref().map(|hold| hold.id) != Some(id) || self.gate.paused {
                    let _ = reply.send(false);
                    return;
                }
                self.gate.paused = true;
                self.gate.publish();
                self.gate.closing = Some(Closing {
                    id,
                    reply,
                    documents_paused: false,
                });
                let paused = self.gate.documents.pause();
                let events = self.gate.events_tx.clone();
                tokio::spawn(async move {
                    if paused.await.is_ok() {
                        let _ = events.send(id);
                    }
                });
            }
            GateRequest::Release { id, reply } => self.release(id, reply),
        }
    }

    fn documents_paused(&mut self, id: u64) {
        if let Some(closing) = &mut self.gate.closing
            && closing.id == id
        {
            closing.documents_paused = true;
        }
        self.gate.try_finish_closing();
    }

    /// Lifts hold `id` (any with `None`): parked chunk writes run and
    /// deferred documents are written, then `reply` is answered.
    fn release(&mut self, id: Option<u64>, reply: Option<oneshot::Sender<()>>) {
        let matches = self
            .gate
            .hold
            .as_ref()
            .is_some_and(|hold| id.is_none_or(|id| id == hold.id));
        if !matches {
            match reply {
                Some(reply) if self.gate.hold.is_some() => {
                    let _ = reply.send(());
                }
                Some(reply) => self.gate.resume_documents(Some(reply)),
                None => {}
            }
            return;
        }
        if let Some(hold) = self.gate.hold.take() {
            log::info!("{} released saving", hold.owner);
        }
        if let Some(closing) = self.gate.closing.take() {
            let _ = closing.reply.send(false);
        }
        self.gate.paused = false;
        self.gate.publish();
        self.gate.resume_documents(reply);
        let keys: Vec<FileKey> = self
            .files
            .iter_mut()
            .filter(|(_, file)| !file.parked.is_empty())
            .map(|(key, file)| {
                let mut queue = std::mem::take(&mut file.parked);
                queue.append(&mut file.queue);
                file.queue = queue;
                key.clone()
            })
            .collect();
        for key in keys {
            self.pump(key);
        }
    }

    fn set_scratch(&mut self, dimension: Arc<str>, on: bool) -> bool {
        self.files.retain(|(key, _), file| {
            if *key != dimension {
                return true;
            }
            file.store = None;
            file.stale = file.busy;
            file.busy || !file.queue.is_empty() || !file.parked.is_empty()
        });
        if on {
            self.voided.remove(&dimension);
            if self.scratch.contains_key(&dimension) {
                return false;
            }
            let store: Arc<dyn WorldBackend> =
                Arc::new(tuff_storage::memory::MemoryBackend::default());
            self.scratch.insert(dimension, store);
            true
        } else {
            let kept = self.scratch.remove(&dimension).is_some();
            self.voided.insert(dimension);
            kept
        }
    }

    fn accept(&mut self, batch: Batch) {
        let id = self.next_batch;
        self.next_batch += 1;
        if batch.ops.is_empty() {
            let _ = batch.reply.send(Vec::new());
            return;
        }
        if self.voided.contains(&batch.dimension) {
            let _ = batch
                .reply
                .send(batch.ops.into_iter().map(voided).collect());
            return;
        }
        let writes = batch.ops.iter().any(IoOp::writes);
        let mut parts: HashMap<FileRegionPos, Vec<(usize, IoOp)>> = HashMap::new();
        for (index, op) in batch.ops.into_iter().enumerate() {
            parts
                .entry(op.pos().file_region())
                .or_default()
                .push((index, op));
        }
        self.pending.insert(
            id,
            Pending {
                parts: parts.len(),
                results: Vec::new(),
                failed: false,
                reply: batch.reply,
                accepted: Instant::now(),
                writes,
            },
        );
        for (region, ops) in parts {
            let key = (batch.dimension.clone(), region);
            self.files
                .entry(key.clone())
                .or_default()
                .queue
                .push_back((id, ops));
            self.pump(key);
        }
    }

    fn shrink(&mut self) -> usize {
        let before = self.files.len();
        self.files
            .retain(|_, file| file.busy || !file.queue.is_empty() || !file.parked.is_empty());
        self.files.shrink_to_fit();
        self.pending.shrink_to_fit();
        before - self.files.len()
    }

    fn pump(&mut self, key: FileKey) {
        let Some(file) = self.files.get_mut(&key) else {
            return;
        };
        if file.busy {
            return;
        }
        let (batch, ops) = loop {
            let Some((batch, ops)) = file.queue.pop_front() else {
                return;
            };
            if self.gate.paused && file.must_park(&ops) {
                file.parked.push_back((batch, ops));
                continue;
            }
            break (batch, ops);
        };
        file.busy = true;
        let writes = ops.iter().any(|(_, op)| op.writes());
        self.gate.writing += usize::from(writes);
        let store = file.store.take();
        let backend = self.scratch.get(&key.0).unwrap_or(&self.backend).clone();
        let mut guard = JobGuard {
            done: self.done_tx.clone(),
            target: Some((key, batch)),
            store,
            writes,
        };
        tokio::task::spawn_blocking(move || {
            let (key, _) = guard.target.clone().expect("job target");
            let results = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_io(&mut guard.store, ops, &*backend, &key.0, key.1)
            }));
            match results {
                Ok((results, counts)) => guard.finish(Some(results), counts),
                Err(_) => guard.finish(None, IoCounts::default()),
            }
        });
    }

    fn finished(&mut self, done: Done) {
        if done.counts.failures > 0 {
            self.failures
                .send_modify(|total| *total += done.counts.failures);
        }
        if let Some(meter) = &mut self.metrics {
            meter.counts.add(done.counts);
        }
        if done.writes {
            self.gate.writing -= 1;
            self.gate.try_finish_closing();
        }
        if let Some(file) = self.files.get_mut(&done.file) {
            file.busy = false;
            file.store = done.store.filter(|_| !std::mem::take(&mut file.stale));
            if file.queue.is_empty() && file.parked.is_empty() && self.files.len() > MAX_IDLE_STORES
            {
                self.files.remove(&done.file);
            } else {
                self.pump(done.file);
            }
        }
        let Some(pending) = self.pending.get_mut(&done.batch) else {
            return;
        };
        pending.parts -= 1;
        match done.results {
            Some(results) => pending.results.extend(results),
            None => pending.failed = true,
        }
        if pending.parts > 0 {
            return;
        }
        let pending = self.pending.remove(&done.batch).expect("pending batch");
        if pending.writes
            && let Some(meter) = &mut self.metrics
        {
            meter.save_latency_max = meter.save_latency_max.max(pending.accepted.elapsed());
        }
        if !pending.failed {
            let mut results = pending.results;
            results.sort_by_key(|(index, _)| *index);
            let _ = pending
                .reply
                .send(results.into_iter().map(|(_, result)| result).collect());
        }
    }
}

impl Actor {
    /// Sends the window's figures once the pace allows; never waits.
    fn report_metrics(&mut self) {
        let Some(meter) = &mut self.metrics else {
            return;
        };
        let Some(window) = meter.pacer.time(meter.metrics.pace(), Instant::now()) else {
            return;
        };
        let mut report = ChunkIoReport {
            window_ms: millis(window),
            reads: meter.counts.reads,
            writes: meter.counts.writes,
            bytes_read: meter.counts.bytes_read,
            bytes_written: meter.counts.bytes_written,
            save_latency_max_ms: millis(meter.save_latency_max),
            ..ChunkIoReport::default()
        };
        for file in self.files.values() {
            report.queued += file
                .queue
                .iter()
                .map(|(_, ops)| ops.len() as u64)
                .sum::<u64>();
            report.running += u64::from(file.busy);
            report.open_files += u64::from(file.store.is_some());
        }
        meter.counts = IoCounts::default();
        meter.save_latency_max = std::time::Duration::ZERO;
        meter.metrics.report(Report::ChunkIo(report));
    }
}

/// The result of an op on a dimension whose chunks were dropped.
fn voided(op: IoOp) -> IoResult {
    match op {
        IoOp::Read(pos) | IoOp::ReadBlocks(pos) => IoResult::Read {
            pos,
            blocks: Ok(None),
            entities: Ok(None),
            pois: Ok(None),
        },
        IoOp::Write { pos, .. } => IoResult::Written(pos),
        IoOp::Remove(pos) => IoResult::Removed { pos, ok: true },
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn run_io(
    storage: &mut Option<Box<dyn RegionStore>>,
    ops: Vec<(usize, IoOp)>,
    backend: &dyn WorldBackend,
    dimension: &str,
    region: FileRegionPos,
) -> (Vec<(usize, IoResult)>, IoCounts) {
    let mut counts = IoCounts::default();
    let mut wrote = false;
    let mut results = Vec::with_capacity(ops.len());
    let store = match storage {
        Some(store) => store,
        None => match backend.open_region(dimension, region) {
            Ok(store) => storage.insert(store),
            Err(err) => {
                let message = format!("cannot open region {region:?}: {err}");
                counts.failures += ops.len() as u64;
                for (index, op) in ops {
                    results.push((
                        index,
                        match op {
                            IoOp::Read(pos) | IoOp::ReadBlocks(pos) => IoResult::Read {
                                pos,
                                blocks: Err(message.clone()),
                                entities: Err(message.clone()),
                                pois: Err(message.clone()),
                            },
                            IoOp::Write { pos, .. } => {
                                log::error!("failed to save chunk {pos:?}: {message}");
                                IoResult::Written(pos)
                            }
                            IoOp::Remove(pos) => {
                                log::error!("failed to delete chunk {pos:?}: {message}");
                                IoResult::Removed { pos, ok: false }
                            }
                        },
                    ));
                }
                return (results, counts);
            }
        },
    };
    for (index, op) in ops {
        let result = match op {
            IoOp::Read(pos) => {
                let _span = tuff_timings::span!("chunk_io.read");
                let blocks = store.read_chunk(pos).map_err(|e| e.to_string());
                let entities = store.read_entities(pos).map_err(|e| e.to_string());
                let pois = store.read_pois(pos).map_err(|e| e.to_string());
                counts.reads += 1;
                counts.failures += u64::from(blocks.is_err() || entities.is_err() || pois.is_err());
                for blob in [&blocks, &entities, &pois].into_iter().flatten().flatten() {
                    counts.bytes_read += blob.len() as u64;
                }
                IoResult::Read {
                    pos,
                    blocks,
                    entities,
                    pois,
                }
            }
            IoOp::ReadBlocks(pos) => {
                let _span = tuff_timings::span!("chunk_io.read");
                let blocks = store.read_chunk(pos).map_err(|e| e.to_string());
                counts.reads += 1;
                counts.failures += u64::from(blocks.is_err());
                if let Ok(Some(blob)) = &blocks {
                    counts.bytes_read += blob.len() as u64;
                }
                IoResult::Read {
                    pos,
                    blocks,
                    entities: Ok(None),
                    pois: Ok(None),
                }
            }
            IoOp::Write {
                pos,
                blocks,
                entities,
                pois,
            } => {
                let _span = tuff_timings::span!("chunk_io.write");
                counts.writes += 1;
                counts.bytes_written += blocks.as_ref().map_or(0, |b| b.len() as u64);
                for record in [&entities, &pois] {
                    if let EntityWrite::Write(blob) = record {
                        counts.bytes_written += blob.len() as u64;
                    }
                }
                if let Some(blob) = blocks {
                    wrote = true;
                    if let Err(err) = store.write_chunk(pos, &blob) {
                        counts.failures += 1;
                        log::error!("failed to save chunk {pos:?}: {err}");
                    }
                }
                let entity_result = match &entities {
                    EntityWrite::Keep => Ok(()),
                    EntityWrite::Clear => store.write_entities(pos, None),
                    EntityWrite::Write(blob) => store.write_entities(pos, Some(blob)),
                };
                wrote |= !matches!(entities, EntityWrite::Keep);
                if let Err(err) = entity_result {
                    counts.failures += 1;
                    log::error!("failed to save entities {pos:?}: {err}");
                }
                let poi_result = match &pois {
                    EntityWrite::Keep => Ok(()),
                    EntityWrite::Clear => store.write_pois(pos, None),
                    EntityWrite::Write(blob) => store.write_pois(pos, Some(blob)),
                };
                wrote |= !matches!(pois, EntityWrite::Keep);
                if let Err(err) = poi_result {
                    counts.failures += 1;
                    log::error!("failed to save points of interest {pos:?}: {err}");
                }
                IoResult::Written(pos)
            }
            IoOp::Remove(pos) => {
                let _span = tuff_timings::span!("chunk_io.write");
                counts.writes += 1;
                wrote = true;
                let mut ok = true;
                if let Err(err) = store.remove_chunk(pos) {
                    log::error!("failed to delete chunk {pos:?}: {err}");
                    counts.failures += 1;
                    ok = false;
                }
                for (what, result) in [
                    ("entities", store.write_entities(pos, None)),
                    ("points of interest", store.write_pois(pos, None)),
                ] {
                    if let Err(err) = result {
                        log::error!("failed to delete the {what} of chunk {pos:?}: {err}");
                        counts.failures += 1;
                        ok = false;
                    }
                }
                IoResult::Removed { pos, ok }
            }
        };
        results.push((index, result));
    }
    if wrote && let Err(err) = tuff_timings::timed!("chunk_io.flush", store.flush()) {
        log::error!("failed to sync region {region:?}: {err}");
    }
    (results, counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::memory::MemoryBackend;

    const DIMENSION: &str = "minecraft:overworld";

    fn write(pos: ChunkPos, blob: &[u8]) -> IoOp {
        IoOp::Write {
            pos,
            blocks: Some(blob.to_vec()),
            entities: EntityWrite::Keep,
            pois: EntityWrite::Keep,
        }
    }

    fn read_blocks(results: Vec<IoResult>) -> Option<Blob> {
        match results.into_iter().next() {
            Some(IoResult::Read { blocks, .. }) => blocks.unwrap(),
            _ => panic!("a read result"),
        }
    }

    fn stored(backend: &MemoryBackend, pos: ChunkPos) -> Option<Blob> {
        backend
            .open_region(DIMENSION, pos.file_region())
            .unwrap()
            .read_chunk(pos)
            .unwrap()
    }

    struct FailingBackend(MemoryBackend);

    impl WorldBackend for FailingBackend {
        fn name(&self) -> &str {
            "failing"
        }

        fn lock(&self) -> std::io::Result<Box<dyn tuff_storage::backend::WorldLock>> {
            self.0.lock()
        }

        fn documents(&self) -> &dyn tuff_storage::backend::Documents {
            self.0.documents()
        }

        fn open_region(&self, _: &str, _: FileRegionPos) -> std::io::Result<Box<dyn RegionStore>> {
            Err(std::io::Error::other("disk full"))
        }

        fn blob_codec(&self) -> Arc<dyn tuff_storage::backend::BlobCodec> {
            self.0.blob_codec()
        }

        fn list_dimensions(&self) -> std::io::Result<Vec<String>> {
            self.0.list_dimensions()
        }

        fn list_regions(&self, dimension: &str) -> std::io::Result<Vec<FileRegionPos>> {
            self.0.list_regions(dimension)
        }
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn failed_reads_and_writes_are_counted_for_the_disk_warning() {
        let io = ChunkIo::start(Arc::new(FailingBackend(MemoryBackend::default())));
        let mut failures = io.failures();
        assert_eq!(*failures.borrow(), 0);
        let pos = ChunkPos::new(0, 0);
        io.submit(DIMENSION, vec![write(pos, b"a"), IoOp::Read(pos)])
            .await
            .unwrap();
        failures.changed().await.unwrap();
        assert_eq!(*failures.borrow(), 2);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn scratch_dimensions_stay_in_memory_and_vanish_when_dropped() {
        let backend = Arc::new(MemoryBackend::default());
        let io = ChunkIo::start(backend.clone());
        let pos = ChunkPos::new(3, -2);
        const ARENA: &str = "game:arena";
        assert!(io.scratch(ARENA, true).await.unwrap());
        assert!(!io.scratch(ARENA, true).await.unwrap(), "already kept");
        io.submit(ARENA, vec![write(pos, b"a")]).await.unwrap();
        let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
        assert_eq!(read_blocks(read).as_deref(), Some(&b"a"[..]));
        assert!(backend.list_dimensions().unwrap().is_empty());

        assert!(io.scratch(ARENA, false).await.unwrap());
        assert!(!io.scratch(ARENA, false).await.unwrap());
        io.submit(ARENA, vec![write(pos, b"b")]).await.unwrap();
        let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
        assert_eq!(read_blocks(read), None);
        assert!(backend.list_dimensions().unwrap().is_empty());

        assert!(io.scratch(ARENA, true).await.unwrap());
        let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
        assert_eq!(read_blocks(read), None, "a new scratch starts empty");
        io.submit(DIMENSION, vec![write(pos, b"c")]).await.unwrap();
        assert_eq!(stored(&backend, pos).as_deref(), Some(&b"c"[..]));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn blocks_only_read_skips_entities_and_pois() {
        let backend = Arc::new(MemoryBackend::default());
        let io = ChunkIo::start(backend);
        let pos = ChunkPos::new(1, 1);
        let op = IoOp::Write {
            pos,
            blocks: Some(b"b".to_vec()),
            entities: EntityWrite::Write(b"e".to_vec()),
            pois: EntityWrite::Write(b"p".to_vec()),
        };
        io.submit(DIMENSION, vec![op]).await.unwrap();
        let full = io.submit(DIMENSION, vec![IoOp::Read(pos)]).await.unwrap();
        assert!(matches!(
            &full[0],
            IoResult::Read {
                entities: Ok(Some(_)),
                pois: Ok(Some(_)),
                ..
            }
        ));
        let only = io
            .submit(DIMENSION, vec![IoOp::ReadBlocks(pos)])
            .await
            .unwrap();
        match &only[0] {
            IoResult::Read {
                blocks,
                entities,
                pois,
                ..
            } => {
                assert_eq!(blocks.as_ref().unwrap().as_deref(), Some(&b"b"[..]));
                assert!(matches!(entities, Ok(None)) && matches!(pois, Ok(None)));
            }
            _ => panic!("a read result"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dropped_job_reports_failure() {
        let (done, mut rx) = mpsc::unbounded_channel();
        drop(JobGuard {
            done,
            target: Some((("overworld".into(), FileRegionPos::new(0, 0)), 7)),
            store: None,
            writes: true,
        });
        let sent = rx.try_recv().expect("failed job reported");
        assert_eq!(sent.batch, 7);
        assert!(sent.results.is_none());
        assert!(sent.writes);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_stops_the_server_and_storage_keeps_serving() {
        let backend = Arc::new(MemoryBackend::default());
        let (stop, mut stops) = mpsc::unbounded_channel();
        let io = ChunkIo::start_with(backend.clone(), None, Fatal::channel(stop));
        let (a, b) = (ChunkPos::new(0, 0), ChunkPos::new(40, 0));
        io.submit(DIMENSION, vec![write(a, b"a")]).await.unwrap();
        let before = crate::metrics::actor_panics().get();
        assert!(io.tx.send(Message::Panic).is_ok());
        let held = io
            .begin_hold("test", Duration::from_secs(600))
            .await
            .expect("the gate still answers");
        assert!(io.close_hold(held).await);
        let parked = io.submit(DIMENSION, vec![write(b, b"b")]);
        io.release_hold(held).await;
        parked.await.expect("the write is answered");
        assert_eq!(stored(&backend, a).as_deref(), Some(&b"a"[..]));
        assert_eq!(stored(&backend, b).as_deref(), Some(&b"b"[..]));
        assert_eq!(stops.recv().await, Some(()));
        assert!(crate::metrics::actor_panics().get() > before);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn storage_gives_up_after_repeated_panics() {
        let backend = Arc::new(MemoryBackend::default());
        let (stop, mut stops) = mpsc::unbounded_channel();
        let io = ChunkIo::start_with(backend.clone(), None, Fatal::channel(stop));
        for _ in 0..tuff_supervise::CRITICAL_PANICS {
            assert!(io.tx.send(Message::Panic).is_ok());
        }
        let answer = io.submit(DIMENSION, vec![write(ChunkPos::new(0, 0), b"a")]);
        assert!(answer.await.is_err(), "a dead actor drops the answer");
        assert_eq!(stops.recv().await, Some(()));
        assert!(
            stops.try_recv().is_err(),
            "the server is asked to stop once"
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn held_writes_wait_and_reads_of_them_wait_behind_them() {
        let backend = Arc::new(MemoryBackend::default());
        let io = ChunkIo::start(backend.clone());
        let (a, b) = (ChunkPos::new(0, 0), ChunkPos::new(1, 0));
        let id = io
            .begin_hold("test", Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(io.saves().hold, Some(HoldPhase::Flushing));
        assert!(io.close_hold(id).await);
        assert_eq!(io.saves().hold, Some(HoldPhase::Held));
        assert!(!io.saves().saves());

        let mut written = io.submit(DIMENSION, vec![write(a, b"new")]);
        let mut read_back = io.submit(DIMENSION, vec![IoOp::Read(a)]);
        let other = io.submit(DIMENSION, vec![IoOp::Read(b)]).await.unwrap();
        assert_eq!(read_blocks(other), None, "other chunks still read");
        assert!(written.try_recv().is_err());
        assert!(read_back.try_recv().is_err());
        assert_eq!(stored(&backend, a), None);

        io.release_hold(id).await;
        assert_eq!(io.saves(), SaveState::default());
        written.await.unwrap();
        assert_eq!(read_blocks(read_back.await.unwrap()), Some(b"new".to_vec()));
        assert_eq!(stored(&backend, a), Some(b"new".to_vec()));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn shrink_closes_idle_stores_and_reopens_on_use() {
        let backend = Arc::new(MemoryBackend::default());
        let io = ChunkIo::start(backend.clone());
        let chunks = [
            ChunkPos::new(0, 0),
            ChunkPos::new(40, 0),
            ChunkPos::new(0, 40),
        ];
        let ops = chunks.iter().map(|pos| write(*pos, b"data")).collect();
        io.submit(DIMENSION, ops).await.unwrap();
        assert_eq!(io.shrink().await.unwrap(), 3);
        assert_eq!(io.shrink().await.unwrap(), 0);
        let read = io
            .submit(DIMENSION, vec![IoOp::Read(chunks[1])])
            .await
            .unwrap();
        assert_eq!(read_blocks(read), Some(b"data".to_vec()));
        assert_eq!(io.shrink().await.unwrap(), 1);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_remove_deletes_the_chunk_and_its_records_and_waits_out_a_hold() {
        let backend = Arc::new(MemoryBackend::default());
        let io = ChunkIo::start(backend.clone());
        let a = ChunkPos::new(3, -2);
        let full = IoOp::Write {
            pos: a,
            blocks: Some(b"chunk".to_vec()),
            entities: EntityWrite::Write(b"pig".to_vec()),
            pois: EntityWrite::Write(b"bed".to_vec()),
        };
        io.submit(DIMENSION, vec![full]).await.unwrap();
        let id = io
            .begin_hold("test", Duration::from_secs(600))
            .await
            .unwrap();
        assert!(io.close_hold(id).await);
        let mut removed = io.submit(DIMENSION, vec![IoOp::Remove(a), IoOp::Read(a)]);
        tokio::task::yield_now().await;
        assert!(removed.try_recv().is_err(), "a remove is a write");
        assert_eq!(stored(&backend, a), Some(b"chunk".to_vec()));
        io.release_hold(id).await;
        let results = removed.await.unwrap();
        assert!(matches!(results[0], IoResult::Removed { pos, ok: true } if pos == a));
        let IoResult::Read {
            blocks,
            entities,
            pois,
            ..
        } = &results[1]
        else {
            panic!("a read result");
        };
        assert_eq!(
            (blocks.clone(), entities.clone(), pois.clone()),
            (Ok(None), Ok(None), Ok(None))
        );
        let mut store = backend.open_region(DIMENSION, a.file_region()).unwrap();
        assert_eq!(store.list_chunks().unwrap(), Vec::<ChunkPos>::new());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn one_hold_at_a_time_and_stale_ids_do_nothing() {
        let io = ChunkIo::start(Arc::new(MemoryBackend::default()));
        let id = io
            .begin_hold("first", Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(
            io.begin_hold("second", Duration::from_secs(600)).await,
            Err(HoldBusy {
                owner: "first".into()
            })
        );
        io.release_hold(id + 1).await;
        assert!(io.saves().hold.is_some());
        io.release_hold(id).await;
        assert!(!io.close_hold(id).await);
        let next = io
            .begin_hold("second", Duration::from_secs(600))
            .await
            .unwrap();
        assert_ne!(next, id);
        io.release_all().await;
        assert_eq!(io.saves(), SaveState::default());
    }
}

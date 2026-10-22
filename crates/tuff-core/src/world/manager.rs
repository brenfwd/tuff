//! Directory and lifecycle of region actors. Players hold a ticket on every
//! region they view; a region nobody holds a ticket on is shut down (saving
//! everything) after a grace period. A region reopened while its previous
//! instance is still saving waits for that save before touching its files,
//! so two actors never share a region file.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::messages::{
    ChunkTicket, CoordinatorMessage, ManagerMessage, RegionHandle, RegionInternal, TimedTicket,
};
use crate::world::region::RegionActor;
use tuff_storage::chunk_tickets::ChunkTickets;
use tuff_supervise::{CatchUnwind, Critical, Fatal};
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// A region actor's identity: regions of different dimensions never touch.
type Key = (DimensionId, RegionPos);

/// Ticks an unreferenced region stays up, so players walking back and forth
/// across a boundary don't reload it every time.
const IDLE_TICKS: u64 = 600;

struct Entry {
    handle: RegionHandle,
    tickets: usize,
    idle_since: Option<u64>,
}

pub struct RegionManager {
    shared: Arc<Shared>,
    messages: mpsc::Receiver<ManagerMessage>,
    clock: watch::Receiver<u64>,
    regions: HashMap<Key, Entry>,
    closing: HashMap<Key, watch::Receiver<bool>>,
    /// Chunks held loaded, per ticket kind.
    held: HashMap<(DimensionId, ChunkTicket), HashSet<ChunkPos>>,
    /// How many ticket kinds hold each chunk; a chunk is loaded while any
    /// does.
    holds: HashMap<(DimensionId, ChunkPos), usize>,
    /// Held chunks per region; a region keeps one ticket while it has any.
    region_holds: HashMap<Key, usize>,
    /// Each dimension's saved chunk tickets as last loaded or saved.
    tickets: HashMap<DimensionId, ChunkTickets>,
    /// When each chunk held under a timed ticket lets go.
    expiries: HashMap<(DimensionId, TimedTicket, ChunkPos), u64>,
    /// Dimensions whose forced chunks changed since the last save.
    dirty: HashSet<DimensionId>,
    /// The last save started, awaited before the next one so writes of
    /// one dimension's tickets never reorder.
    saving: Option<tokio::task::JoinHandle<()>>,
    metrics: crate::metrics::Pacer,
    /// No region is up or closing and no chunk is held: the world has
    /// shrunk its caches since it last was.
    idle: bool,
}

impl RegionManager {
    pub fn new(shared: Arc<Shared>, messages: mpsc::Receiver<ManagerMessage>) -> Self {
        let clock = shared.server_ticks.clone();
        Self {
            shared,
            messages,
            clock,
            regions: HashMap::new(),
            closing: HashMap::new(),
            held: HashMap::new(),
            holds: HashMap::new(),
            region_holds: HashMap::new(),
            tickets: HashMap::new(),
            expiries: HashMap::new(),
            dirty: HashSet::new(),
            saving: None,
            metrics: Default::default(),
            idle: false,
        }
    }

    /// Reads every dimension's saved forced chunks and holds them again.
    async fn load_tickets(&mut self) {
        let shared = self.shared.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            shared
                .dimensions
                .iter()
                .map(|dimension| {
                    let tickets = ChunkTickets::load(shared.backend.documents(), &dimension.key)
                        .unwrap_or_else(|err| {
                            log::warn!("could not read chunk tickets of {}: {err}", dimension.key);
                            ChunkTickets::default()
                        });
                    (dimension.id, tickets)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (dimension, tickets) in loaded {
            if !tickets.forced.is_empty() {
                self.force_load(dimension, ChunkTicket::Forced, &tickets.forced, true);
            }
            self.tickets.insert(dimension, tickets);
        }
        self.dirty.clear();
    }

    /// Writes the forced chunks of every dimension that changed, on the
    /// blocking pool after the previous save.
    async fn save_tickets(&mut self) {
        if let Some(previous) = self.saving.take() {
            let _ = previous.await;
        }
        self.start_ticket_save();
        if let Some(saving) = self.saving.take() {
            let _ = saving.await;
        }
    }

    fn start_ticket_save(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        let mut writes = Vec::new();
        for dimension in self.dirty.drain() {
            let tickets = self.tickets.entry(dimension).or_default();
            let mut forced: Vec<_> = self
                .held
                .get(&(dimension, ChunkTicket::Forced))
                .map(|set| set.iter().copied().collect())
                .unwrap_or_default();
            forced.sort_by_key(|c| (c.z, c.x));
            tickets.forced = forced;
            writes.push((dimension, tickets.clone()));
        }
        let shared = self.shared.clone();
        let previous = self.saving.take();
        self.saving = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = tokio::task::spawn_blocking(move || {
                for (dimension, tickets) in writes {
                    let key = &shared.dimensions.get(dimension).key;
                    if let Err(err) = tickets.save(shared.backend.documents(), key) {
                        log::warn!("could not save chunk tickets of {key}: {err}");
                    }
                }
            })
            .await;
        }));
    }

    /// Answers once every ticket save started so far is written.
    fn ticket_save_done(&mut self) -> tokio::sync::oneshot::Receiver<()> {
        let (done, saved) = tokio::sync::oneshot::channel();
        let previous = self.saving.take();
        self.saving = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = done.send(());
        }));
        saved
    }

    /// Serves until shut down, then closes (saving) every region. The
    /// manager is critical: a panic asks the server to stop and the loop
    /// resumes on the same tables, so the shutdown still closes every
    /// region it knows.
    pub async fn run(mut self) {
        let coordinator = self.shared.coordinator.clone();
        let mut critical = Critical::new(
            "the region manager",
            Fatal::new(move || {
                let coordinator = coordinator.clone();
                tokio::spawn(async move {
                    let _ = coordinator.send(CoordinatorMessage::Stop).await;
                });
            }),
        );
        let mut loaded = false;
        let done = loop {
            match CatchUnwind::new(Box::pin(self.serve(&mut loaded))).await {
                Ok(done) => break done,
                Err(panic) if critical.panicked(&*panic) => {}
                Err(_) => break None,
            }
        };
        match CatchUnwind::new(Box::pin(self.shutdown())).await {
            Ok(()) => {
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            Err(panic) => critical.panicked_closing(&*panic),
        }
    }

    /// The manager's loop; ends with the shutdown's `done`, if one asked.
    async fn serve(&mut self, loaded: &mut bool) -> Option<oneshot::Sender<()>> {
        if !std::mem::replace(loaded, true) {
            self.load_tickets().await;
        }
        loop {
            tokio::select! {
                message = self.messages.recv() => match message {
                    Some(ManagerMessage::Shutdown { done }) => return Some(done),
                    #[cfg(feature = "test-hooks")]
                    Some(ManagerMessage::Panic(message)) => panic!("{message}"),
                    Some(message) => self.handle(message),
                    None => return None,
                },
                changed = self.clock.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                    let tick = *self.clock.borrow_and_update();
                    self.close_idle(tick);
                    self.expire_timed(tick);
                    self.report_metrics();
                }
            }
        }
    }

    fn handle(&mut self, message: ManagerMessage) {
        if matches!(
            message,
            ManagerMessage::Hold { .. } | ManagerMessage::ForceLoad { .. }
        ) {
            let tick = *self.clock.borrow();
            self.expire_timed(tick);
        }
        match message {
            ManagerMessage::Acquire {
                dimension,
                region,
                reply,
            } => {
                if self.shared.dimensions.is_discarded(dimension) {
                    return;
                }
                let entry = self.entry((dimension, region));
                entry.tickets += 1;
                entry.idle_since = None;
                let _ = reply.send(entry.handle.clone());
            }
            ManagerMessage::Release { dimension, region } => {
                let tick = *self.clock.borrow();
                if let Some(entry) = self.regions.get_mut(&(dimension, region)) {
                    entry.tickets = entry.tickets.saturating_sub(1);
                    if entry.tickets == 0 {
                        entry.idle_since = Some(tick);
                    }
                }
            }
            ManagerMessage::ForceLoad {
                dimension,
                chunks,
                ticket,
                on,
                reply,
            } => {
                if self.shared.dimensions.is_discarded(dimension) {
                    let _ = reply.send(vec![false; chunks.len()]);
                    return;
                }
                let changed = self.force_load(dimension, ticket, &chunks, on);
                if ticket == ChunkTicket::Forced
                    && changed.contains(&true)
                    && !self.shared.dimensions.is_runtime(dimension)
                {
                    self.dirty.insert(dimension);
                }
                let _ = reply.send(changed);
            }
            ManagerMessage::Hold {
                dimension,
                center,
                ticket,
            } => {
                if !self.shared.dimensions.is_discarded(dimension) {
                    self.hold_timed(dimension, center, ticket);
                }
            }
            ManagerMessage::ForcedChunks { dimension, reply } => {
                let mut chunks: Vec<ChunkPos> = self
                    .held
                    .get(&(dimension, ChunkTicket::Forced))
                    .map(|set| set.iter().copied().collect())
                    .unwrap_or_default();
                chunks.sort_by_key(|c| (c.z, c.x));
                let _ = reply.send(chunks);
            }
            ManagerMessage::SaveAll {
                done,
                snapshotted,
                epoch,
            } => {
                self.start_ticket_save();
                let mut saved = Vec::new();
                let mut taken = Vec::new();
                for entry in self.regions.values() {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let tx = done.is_some().then_some(tx);
                    let (snap, snap_rx) = tokio::sync::oneshot::channel();
                    entry.handle.internal(RegionInternal::SaveAll {
                        done: tx,
                        snapshotted: snapshotted.is_some().then_some(snap),
                        epoch: epoch.clone(),
                    });
                    saved.push(rx);
                    taken.push(snap_rx);
                }
                if let Some(snapshotted) = snapshotted {
                    tokio::spawn(async move {
                        for taken in taken {
                            let _ = taken.await;
                        }
                        let _ = snapshotted.send(());
                    });
                }
                if let Some(done) = done {
                    let tickets = self.ticket_save_done();
                    let closing: Vec<_> = self.closing.values().cloned().collect();
                    tokio::spawn(async move {
                        for saved in saved {
                            let _ = saved.await;
                        }
                        for mut closed in closing {
                            let _ = closed.wait_for(|closed| *closed).await;
                        }
                        let _ = tickets.await;
                        let _ = done.send(());
                    });
                }
            }
            ManagerMessage::Regions { reply } => {
                let _ = reply.send(self.regions.values().map(|e| e.handle.clone()).collect());
            }
            ManagerMessage::DropDimension { dimension, done } => {
                self.drop_dimension(dimension, done)
            }
            ManagerMessage::Shutdown { .. } => {
                unreachable!("handled in serve")
            }
            #[cfg(feature = "test-hooks")]
            ManagerMessage::Panic(_) => {
                unreachable!("handled in serve")
            }
        }
    }

    /// Queues a summary for the metrics actor once the pace allows.
    fn report_metrics(&mut self) {
        let pace = self.shared.metrics.pace();
        if self.metrics.tick(pace, std::time::Instant::now()).is_none() {
            return;
        }
        let report = crate::metrics::ManagerReport {
            regions: self.regions.len() as u64,
            closing: self.closing.len() as u64,
            region_tickets: self.regions.values().map(|e| e.tickets as u64).sum(),
            held_chunks: self.holds.len() as u64,
            forced_chunks: self
                .held
                .iter()
                .filter(|((_, ticket), _)| *ticket == ChunkTicket::Forced)
                .map(|(_, chunks)| chunks.len() as u64)
                .sum(),
        };
        self.shared
            .metrics
            .report(crate::metrics::Report::Manager(report));
    }

    fn hold_timed(&mut self, dimension: DimensionId, center: ChunkPos, ticket: TimedTicket) {
        let until = *self.clock.borrow() + ticket.timeout();
        let mut gained = Vec::new();
        for chunk in ticket.chunks_around(center) {
            if self
                .expiries
                .insert((dimension, ticket, chunk), until)
                .is_none()
            {
                gained.push(chunk);
            }
        }
        if !gained.is_empty() {
            self.force_load(dimension, ChunkTicket::Timed(ticket), &gained, true);
        }
    }

    fn expire_timed(&mut self, tick: u64) {
        if self.expiries.is_empty() {
            return;
        }
        let mut lapsed: HashMap<(DimensionId, TimedTicket), Vec<ChunkPos>> = HashMap::new();
        self.expiries.retain(|(dimension, ticket, chunk), until| {
            let alive = *until > tick;
            if !alive {
                lapsed
                    .entry((*dimension, *ticket))
                    .or_default()
                    .push(*chunk);
            }
            alive
        });
        for ((dimension, ticket), chunks) in lapsed {
            self.force_load(dimension, ChunkTicket::Timed(ticket), &chunks, false);
        }
    }

    fn force_load(
        &mut self,
        dimension: DimensionId,
        ticket: ChunkTicket,
        chunks: &[ChunkPos],
        on: bool,
    ) -> Vec<bool> {
        let mut changed = Vec::with_capacity(chunks.len());
        let mut by_region: HashMap<RegionPos, Vec<ChunkPos>> = HashMap::new();
        for chunk in chunks {
            let set = self.held.entry((dimension, ticket)).or_default();
            let flipped = if on {
                set.insert(*chunk)
            } else {
                set.remove(chunk)
            };
            changed.push(flipped);
            if flipped && self.hold_changed(dimension, *chunk, on) {
                by_region.entry(chunk.region()).or_default().push(*chunk);
            }
        }
        for (region, chunks) in by_region {
            let handle = self.entry((dimension, region)).handle.clone();
            handle.internal(RegionInternal::ForceChunks { chunks, on });
        }
        changed
    }

    /// Counts one ticket kind more or less on `chunk`; true when that
    /// loaded or released the chunk itself, taking or dropping its
    /// region's ticket with its first or last held chunk.
    fn hold_changed(&mut self, dimension: DimensionId, chunk: ChunkPos, on: bool) -> bool {
        let holds = self.holds.entry((dimension, chunk)).or_default();
        if on {
            *holds += 1;
            if *holds > 1 {
                return false;
            }
        } else {
            *holds = holds.saturating_sub(1);
            if *holds > 0 {
                return false;
            }
            self.holds.remove(&(dimension, chunk));
        }
        let key = (dimension, chunk.region());
        let count = self.region_holds.entry(key).or_default();
        if on {
            *count += 1;
            if *count == 1 {
                let entry = self.entry(key);
                entry.tickets += 1;
                entry.idle_since = None;
            }
        } else {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.region_holds.remove(&key);
                let tick = *self.clock.borrow();
                if let Some(entry) = self.regions.get_mut(&key) {
                    entry.tickets = entry.tickets.saturating_sub(1);
                    if entry.tickets == 0 {
                        entry.idle_since = Some(tick);
                    }
                }
            }
        }
        true
    }

    fn entry(&mut self, key: Key) -> &mut Entry {
        if !self.regions.contains_key(&key) {
            let (dimension, region) = key;
            let neighbors: Vec<RegionHandle> = region
                .neighbors()
                .filter_map(|pos| {
                    self.regions
                        .get(&(dimension, pos))
                        .map(|e| e.handle.clone())
                })
                .collect();
            let previous = self.closing.remove(&key).filter(|done| !*done.borrow());
            let handle = RegionActor::spawn(
                dimension,
                region,
                self.shared.clone(),
                neighbors.clone(),
                previous,
            );
            for neighbor in &neighbors {
                neighbor.internal(RegionInternal::NeighborUp(handle.clone()));
            }
            self.regions.insert(
                key,
                Entry {
                    handle,
                    tickets: 0,
                    idle_since: None,
                },
            );
        }
        self.regions.get_mut(&key).unwrap()
    }

    fn close(&mut self, key: Key) {
        let Some(entry) = self.regions.remove(&key) else {
            return;
        };
        let (dimension, region) = key;
        let (done, finished) = watch::channel(false);
        entry.handle.internal(RegionInternal::Shutdown { done });
        for neighbor in region.neighbors() {
            if let Some(neighbor) = self.regions.get(&(dimension, neighbor)) {
                neighbor
                    .handle
                    .internal(RegionInternal::NeighborDown(region));
            }
        }
        self.closing.insert(key, finished);
    }

    fn drop_dimension(&mut self, dimension: DimensionId, done: oneshot::Sender<()>) {
        let keys: Vec<Key> = self
            .regions
            .keys()
            .filter(|(d, _)| *d == dimension)
            .copied()
            .collect();
        for key in keys {
            self.close(key);
        }
        self.expiries.retain(|(d, _, _), _| *d != dimension);
        self.held.retain(|(d, _), _| *d != dimension);
        self.holds.retain(|(d, _), _| *d != dimension);
        self.region_holds.retain(|(d, _), _| *d != dimension);
        self.tickets.remove(&dimension);
        self.dirty.remove(&dimension);
        let closing: Vec<_> = self
            .closing
            .iter()
            .filter(|((d, _), _)| *d == dimension)
            .map(|(_, closed)| closed.clone())
            .collect();
        tokio::spawn(async move {
            for mut closed in closing {
                let _ = closed.wait_for(|closed| *closed).await;
            }
            let _ = done.send(());
        });
    }

    fn close_idle(&mut self, tick: u64) {
        let idle: Vec<Key> = self
            .regions
            .iter()
            .filter(|(_, e)| e.idle_since.is_some_and(|since| tick - since >= IDLE_TICKS))
            .map(|(pos, _)| *pos)
            .collect();
        if self.shared.saves().autosaves() {
            for region in idle {
                self.close(region);
            }
        }
        self.closing.retain(|_, done| !*done.borrow());
        let idle = self.regions.is_empty() && self.closing.is_empty() && self.holds.is_empty();
        if idle && !self.idle {
            self.shrink();
        }
        self.idle = idle;
    }

    /// The world just went idle: gives back table capacity, then has every
    /// generation pipeline drop what it can rebuild and storage close its
    /// idle files, in that order since pipelines write as they drop.
    fn shrink(&mut self) {
        self.regions.shrink_to_fit();
        self.closing.shrink_to_fit();
        self.held.retain(|_, chunks| !chunks.is_empty());
        self.held.shrink_to_fit();
        self.holds.shrink_to_fit();
        self.region_holds.shrink_to_fit();
        let pipelines: Vec<_> = self.shared.generation.iter().flatten().cloned().collect();
        let io = self.shared.chunk_io.get(&self.shared.backend).clone();
        tokio::spawn(async move {
            let idle: Vec<_> = pipelines
                .iter()
                .map(|pipeline| {
                    let (done, idle) = tokio::sync::oneshot::channel();
                    pipeline.idle(done);
                    idle
                })
                .collect();
            for idle in idle {
                let _ = idle.await;
            }
            if let Ok(closed) = io.shrink().await {
                log::debug!("world idle: closed {closed} region files");
            }
        });
    }

    async fn shutdown(&mut self) {
        self.shared
            .chunk_io
            .get(&self.shared.backend)
            .release_all()
            .await;
        self.save_tickets().await;
        let all: Vec<Key> = self.regions.keys().copied().collect();
        for region in all {
            self.close(region);
        }
        for (_, mut done) in self.closing.drain() {
            let _ = done.wait_for(|done| *done).await;
        }
        let flushed: Vec<_> = self
            .shared
            .generation
            .iter()
            .flatten()
            .map(|pipeline| {
                let (done, flushed) = tokio::sync::oneshot::channel();
                pipeline.flush(done);
                flushed
            })
            .collect();
        for flushed in flushed {
            let _ = flushed.await;
        }
    }
}

//! Chunk pre-generation (`/tuff chunk generate` and the world map panel):
//! one job at a time walks an [`Area`] in spiral order and asks each
//! chunk's region to generate it in full, save it and unload it again, a
//! bounded number of chunks at a time so players' chunk loading keeps
//! the generation pipeline's attention (pre-generated chunks are asked for
//! at [`crate::world::generator::BACKGROUND`] priority). A job that
//! regenerates first has every chunk of the area dropped and deleted
//! through its region, then the pipeline forget them, before any is
//! generated, so the new chunks decorate each other as fresh ones do.
//!
//! Regions are held with ordinary region tickets only while one of their
//! chunks is in flight, and no chunk ticket is ever taken, so memory comes
//! back once a job ends. Progress is on a `watch` ([`Pregen::watch`]),
//! and every generated chunk is announced on [`Pregen::subscribe`] so
//! views of saved chunks (the world map) can pick them up.
//!
//! Every answer the runner waits for is bounded by a timeout: a chunk or
//! region that does not answer is counted as failed and logged with what
//! it was waiting on, so a job always ends.

pub mod area;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;

pub use area::{Area, MAX_RADIUS, Shape};

use crate::config::{Key, Value};
use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::messages::{ManagerMessage, RegionHandle, RegionRequest};
use tuff_world_model::pos::{ChunkPos, REGION_CHUNKS, RegionPos};

pub const PARALLEL_KEY: &str = "tuff.pregen.parallel";
pub const MAX_PRIORITY_KEY: &str = "tuff.pregen.maxPriority";

const QUEUE: usize = 16;
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
const RATE_WINDOW: Duration = Duration::from_secs(10);
const BATCH: usize = 256;
/// How long a region, the pipeline or the runner may take to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one chunk may stay in flight before it counts as failed.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
/// In-flight chunks status lists, oldest first.
const SHOWN_WAITING: usize = 3;

pub fn config_keys() -> Vec<Key> {
    vec![
        Key::new(
            PARALLEL_KEY,
            Value::Int(0),
            "Chunks a pre-generation job keeps in flight at once (0 picks twice the worker threads)",
        )
        .validated(|v| match v {
            Value::Int(n) if (0..=1024).contains(n) => Ok(()),
            _ => Err("must be between 0 and 1024".to_owned()),
        }),
        Key::new(
            MAX_PRIORITY_KEY,
            Value::Bool(false),
            "Pre-generation keeps many times more chunks in flight so it runs as fast as the CPU or storage allow, at the cost of slower world ticks while it runs",
        ),
    ]
}

/// What a region does with one chunk of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// Load or generate the chunk, save it and unload it unless something
    /// else keeps it loaded.
    Generate,
    /// Drop the chunk with everything in it and delete it from storage.
    Remove,
    /// Let the loads of the removed chunks of the region go ahead, once
    /// the pipeline forgot them.
    Release,
}

/// What a chunk in flight waits on, as its region last saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum WaitingOn {
    /// Not looked at by its region yet.
    #[default]
    Region,
    /// Storage reading the chunk.
    Read,
    /// The generation pipeline.
    Generation,
    Light,
    /// A backup holding every save.
    SaveHold,
    Save,
    /// A load of the chunk under way before it can be dropped.
    Load,
    /// Storage deleting the chunk.
    Delete,
}

impl WaitingOn {
    const ALL: [Self; 8] = [
        Self::Region,
        Self::Read,
        Self::Generation,
        Self::Light,
        Self::SaveHold,
        Self::Save,
        Self::Load,
        Self::Delete,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Region => "its region",
            Self::Read => "a storage read",
            Self::Generation => "generation",
            Self::Light => "lighting",
            Self::SaveHold => "a backup holding saves",
            Self::Save => "a save",
            Self::Load => "a load before removal",
            Self::Delete => "a storage delete",
        }
    }
}

/// Where a region reports what a chunk of a job waits on.
#[derive(Clone, Debug, Default)]
pub struct WaitingCell(Arc<AtomicU8>);

impl WaitingCell {
    pub fn set(&self, on: WaitingOn) {
        self.0.store(on as u8, Ordering::Relaxed);
    }

    pub fn get(&self) -> WaitingOn {
        WaitingOn::ALL
            .get(usize::from(self.0.load(Ordering::Relaxed)))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The chunk could not be read or saved, or its region closed.
    Failed,
}

/// A job's request to the region owning `pos`.
#[derive(Debug)]
pub struct RegionOp {
    pub pos: ChunkPos,
    pub kind: OpKind,
    pub reply: oneshot::Sender<Outcome>,
    pub waiting: WaitingCell,
}

/// What to generate.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub dimension: DimensionId,
    pub area: Area,
    /// Drop and delete every chunk of the area first.
    pub regenerate: bool,
    /// Who started it, for status and logs.
    pub by: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Removing,
    Generating,
}

/// A chunk in flight as status shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InFlight {
    pub x: i32,
    pub z: i32,
    pub on: WaitingOn,
    pub secs: u64,
}

/// A running job as status and the panel show it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Progress {
    pub id: u64,
    pub dimension: String,
    pub x: i32,
    pub z: i32,
    pub radius: i32,
    pub shape: &'static str,
    pub regenerate: bool,
    pub by: String,
    pub phase: Phase,
    /// Chunks of the current phase finished, failures included.
    pub done: u64,
    pub total: u64,
    pub failed: u64,
    /// Chunks the removing phase finished (failures included) and failed.
    pub removed: u64,
    pub remove_failed: u64,
    /// Chunks sent to their regions and not answered yet.
    pub in_flight: u64,
    /// The longest waiting of them.
    pub waiting: Vec<InFlight>,
    /// Chunks per second over the last few seconds.
    pub rate: f64,
    pub eta_secs: Option<u64>,
    pub started_unix: u64,
    pub cancelling: bool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finished {
    pub id: u64,
    pub dimension: String,
    pub by: String,
    pub generated: u64,
    pub total: u64,
    pub failed: u64,
    /// Chunks a regenerating job could not delete.
    pub remove_failed: u64,
    pub cancelled: bool,
    /// Why it stopped early, if it did.
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    pub running: Option<Progress>,
    pub last: Option<Finished>,
}

/// Chunks a job just generated and saved.
#[derive(Clone, Debug)]
pub struct Generated {
    pub dimension: DimensionId,
    pub chunks: Vec<ChunkPos>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PregenError {
    /// A job is running already (its id).
    Busy(u64),
    NotRunning,
    /// The world is not running.
    Stopped,
    /// The job runner did not answer in time.
    Unanswered,
}

impl std::fmt::Display for PregenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(id) => write!(f, "chunk generation job {id} is still running"),
            Self::NotRunning => f.write_str("no chunk generation job is running"),
            Self::Stopped => f.write_str("the world is not running"),
            Self::Unanswered => f.write_str("the chunk generation runner did not answer"),
        }
    }
}

impl std::error::Error for PregenError {}

enum Request {
    #[cfg(test)]
    Panic,
    Start {
        shared: Weak<Shared>,
        spec: JobSpec,
        reply: oneshot::Sender<Result<Progress, PregenError>>,
    },
    Cancel {
        reply: oneshot::Sender<Result<Progress, PregenError>>,
    },
}

/// The handle to a world's pre-generation jobs.
#[derive(Clone)]
pub struct Pregen {
    tx: mpsc::Sender<Request>,
    status: watch::Receiver<Status>,
    chunks: broadcast::Sender<Arc<Generated>>,
}

impl std::fmt::Debug for Pregen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pregen").finish_non_exhaustive()
    }
}

impl Pregen {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (status_tx, status) = watch::channel(Status::default());
        let (chunks, _) = broadcast::channel(64);
        let actor = Actor {
            status: status_tx,
            chunks: chunks.clone(),
            next_id: 1,
            job: None,
            last: None,
        };
        tuff_supervise::spawn(
            "the chunk pregen runner",
            tuff_supervise::restarting("the chunk pregen runner", (actor, rx), |(actor, rx)| {
                actor.abandon_job();
                Box::pin(actor.run(rx))
            }),
        );
        Self { tx, status, chunks }
    }

    /// A handle whose every request fails, for worlds built without one.
    pub fn detached() -> Self {
        Self {
            tx: mpsc::channel(1).0,
            status: watch::channel(Status::default()).1,
            chunks: broadcast::channel(1).0,
        }
    }

    async fn ask(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<Progress, PregenError>>) -> Request,
    ) -> Result<Progress, PregenError> {
        let (reply, answer) = oneshot::channel();
        tokio::time::timeout(ANSWER_TIMEOUT, self.tx.send(request(reply)))
            .await
            .map_err(|_| PregenError::Unanswered)?
            .map_err(|_| PregenError::Stopped)?;
        tokio::time::timeout(ANSWER_TIMEOUT, answer)
            .await
            .map_err(|_| PregenError::Unanswered)?
            .map_err(|_| PregenError::Stopped)?
    }

    pub async fn start(
        &self,
        shared: &Arc<Shared>,
        spec: JobSpec,
    ) -> Result<Progress, PregenError> {
        let shared = Arc::downgrade(shared);
        self.ask(|reply| Request::Start {
            shared,
            spec,
            reply,
        })
        .await
    }

    /// Stops the running job once the chunks in flight are done.
    pub async fn cancel(&self) -> Result<Progress, PregenError> {
        self.ask(|reply| Request::Cancel { reply }).await
    }

    #[cfg(test)]
    async fn panic_for_test(&self) {
        let _ = self.tx.send(Request::Panic).await;
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Batches of chunks as jobs generate and save them.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Generated>> {
        self.chunks.subscribe()
    }

    /// Waits until no job runs and gives the last one's result.
    pub async fn wait(&self) -> Option<Finished> {
        let mut status = self.status.clone();
        let done = status.wait_for(|s| s.running.is_none()).await.ok()?;
        done.last.clone()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn parallelism(shared: &Shared) -> usize {
    let explicit = shared.config.get_int(PARALLEL_KEY).unwrap_or(0);
    let max_priority = shared.config.get_bool(MAX_PRIORITY_KEY).unwrap_or(false);
    parallelism_for(explicit, max_priority, rayon::current_num_threads())
}

pub(crate) fn parallelism_for(explicit: i64, max_priority: bool, threads: usize) -> usize {
    let normal = match explicit {
        n if n > 0 => n as usize,
        _ => (threads * 2).clamp(4, 128),
    };
    if max_priority {
        normal.max((threads * 8).clamp(32, 1024))
    } else {
        normal
    }
}

struct Job {
    id: u64,
    spec: JobSpec,
    shared: Weak<Shared>,
    phase: Phase,
    order: Box<dyn Iterator<Item = ChunkPos> + Send>,
    total: u64,
    done: u64,
    failed: u64,
    removed: u64,
    remove_failed: u64,
    generated: u64,
    started: Instant,
    started_unix: u64,
    cancelling: bool,
    error: Option<String>,
    parallel: usize,
    in_flight: JoinSet<(ChunkPos, Outcome)>,
    /// When each chunk in flight was sent, and what it waits on.
    pending: HashMap<ChunkPos, (Instant, WaitingCell)>,
    regions: HashMap<RegionPos, (RegionHandle, usize)>,
    /// Regions the removing phase parked chunks in.
    parked: HashSet<RegionPos>,
    samples: Vec<(Instant, u64)>,
    batch: Vec<ChunkPos>,
}

fn walk(area: Area) -> Box<dyn Iterator<Item = ChunkPos> + Send> {
    Box::new(area::spiral(area.centre(), area.reach()).filter(move |c| area.contains(*c)))
}

impl Job {
    fn progress(&self) -> Progress {
        let rate = self.rate();
        let left = self.total.saturating_sub(self.done);
        let removing = u64::from(self.phase == Phase::Removing);
        let eta_secs = (rate > 0.0).then(|| ((left + removing * self.total) as f64 / rate) as u64);
        let dimension = self
            .shared
            .upgrade()
            .map(|s| s.dimension(self.spec.dimension).key.clone())
            .unwrap_or_default();
        Progress {
            id: self.id,
            dimension,
            x: self.spec.area.x,
            z: self.spec.area.z,
            radius: self.spec.area.radius,
            shape: self.spec.area.shape.name(),
            regenerate: self.spec.regenerate,
            by: self.spec.by.clone(),
            phase: self.phase,
            done: self.done,
            total: self.total,
            failed: self.failed,
            removed: self.removed,
            remove_failed: self.remove_failed,
            in_flight: self.pending.len() as u64,
            waiting: self.waiting(),
            rate: (rate * 10.0).round() / 10.0,
            eta_secs,
            started_unix: self.started_unix,
            cancelling: self.cancelling,
        }
    }

    fn waiting(&self) -> Vec<InFlight> {
        let mut oldest: Vec<(&ChunkPos, &(Instant, WaitingCell))> = self.pending.iter().collect();
        oldest.sort_by_key(|(pos, (sent, _))| (*sent, pos.x, pos.z));
        oldest
            .into_iter()
            .take(SHOWN_WAITING)
            .map(|(pos, (sent, cell))| InFlight {
                x: pos.x,
                z: pos.z,
                on: cell.get(),
                secs: sent.elapsed().as_secs(),
            })
            .collect()
    }

    fn rate(&self) -> f64 {
        let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
            return 0.0;
        };
        let seconds = last.0.duration_since(first.0).as_secs_f64();
        if seconds < 0.5 {
            return 0.0;
        }
        (last.1 - first.1) as f64 / seconds
    }

    fn sample(&mut self, now: Instant) {
        self.samples.push((now, self.done));
        self.samples
            .retain(|(at, _)| now.duration_since(*at) <= RATE_WINDOW);
    }

    fn finished(&self) -> Finished {
        Finished {
            id: self.id,
            dimension: self.progress().dimension,
            by: self.spec.by.clone(),
            generated: self.generated,
            total: self.total,
            failed: self.failed,
            remove_failed: self.remove_failed,
            cancelled: self.cancelling,
            error: self.error.clone(),
            duration_ms: self.started.elapsed().as_millis() as u64,
        }
    }

    async fn region(&mut self, shared: &Shared, region: RegionPos) -> Option<RegionHandle> {
        if let Some((handle, count)) = self.regions.get_mut(&region) {
            *count += 1;
            return Some(handle.clone());
        }
        let (reply, answer) = oneshot::channel();
        let acquire = ManagerMessage::Acquire {
            dimension: self.spec.dimension,
            region,
            reply,
        };
        let handle = tokio::time::timeout(ANSWER_TIMEOUT, async {
            shared.manager.send(acquire).await.ok()?;
            answer.await.ok()
        })
        .await;
        let handle = match handle {
            Ok(handle) => handle?,
            Err(_) => {
                crate::log::error!(
                    "chunk generation job {}: the region manager did not hand out region {region:?}",
                    self.id
                );
                return None;
            }
        };
        self.regions.insert(region, (handle.clone(), 1));
        Some(handle)
    }

    async fn release(&mut self, shared: Option<&Shared>, region: RegionPos) {
        let Some((_, count)) = self.regions.get_mut(&region) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        self.regions.remove(&region);
        if let Some(shared) = shared {
            let release = ManagerMessage::Release {
                dimension: self.spec.dimension,
                region,
            };
            if tokio::time::timeout(ANSWER_TIMEOUT, shared.manager.send(release))
                .await
                .is_err()
            {
                crate::log::error!(
                    "chunk generation job {}: the region manager did not take back region {region:?}",
                    self.id
                );
            }
        }
    }

    /// Sends chunks until `parallel` are in flight; false once the world
    /// is gone.
    async fn fill(&mut self) -> bool {
        if self.cancelling || self.error.is_some() {
            return true;
        }
        let Some(shared) = self.shared.upgrade() else {
            self.error = Some(PregenError::Stopped.to_string());
            return false;
        };
        let kind = match self.phase {
            Phase::Removing => OpKind::Remove,
            Phase::Generating => OpKind::Generate,
        };
        self.parallel = parallelism(&shared);
        while self.in_flight.len() < self.parallel {
            let Some(pos) = self.order.next() else {
                break;
            };
            let Some(handle) = self.region(&shared, pos.region()).await else {
                self.error = Some(format!("region {:?} could not be loaded", pos.region()));
                return false;
            };
            let waiting = WaitingCell::default();
            let (reply, answer) = oneshot::channel();
            let op = RegionOp {
                pos,
                kind,
                reply,
                waiting: waiting.clone(),
            };
            let sent =
                tokio::time::timeout(ANSWER_TIMEOUT, handle.request(RegionRequest::Pregen(op)))
                    .await;
            if sent != Ok(true) {
                self.release(Some(&shared), pos.region()).await;
                self.error = Some(format!(
                    "region {:?} {}",
                    pos.region(),
                    if sent.is_err() {
                        "did not take the request"
                    } else {
                        "closed"
                    }
                ));
                return false;
            }
            if kind == OpKind::Remove {
                self.parked.insert(pos.region());
            }
            self.pending.insert(pos, (Instant::now(), waiting.clone()));
            let id = self.id;
            self.in_flight.spawn(async move {
                let outcome = match tokio::time::timeout(CHUNK_TIMEOUT, answer).await {
                    Ok(answer) => answer.unwrap_or(Outcome::Failed),
                    Err(_) => {
                        crate::log::error!(
                            "chunk generation job {id}: chunk {pos:?} waited on {} for {} s; counting it as failed",
                            waiting.get().name(),
                            CHUNK_TIMEOUT.as_secs()
                        );
                        Outcome::Failed
                    }
                };
                (pos, outcome)
            });
        }
        true
    }

    /// Has the pipeline forget the area and lets the regions load the
    /// chunks the removing phase parked.
    async fn finish_removing(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        if let Some(pipeline) = shared.generation(self.spec.dimension) {
            let area = self.spec.area;
            let (done, forgotten) = oneshot::channel();
            pipeline.forget(Arc::new(move |pos| area.contains(pos)), done);
            if !matches!(
                tokio::time::timeout(ANSWER_TIMEOUT, forgotten).await,
                Ok(Ok(()))
            ) {
                crate::log::error!(
                    "chunk generation job {}: the generation pipeline did not forget the area",
                    self.id
                );
            }
        }
        for region in std::mem::take(&mut self.parked) {
            let Some(handle) = self.region(&shared, region).await else {
                continue;
            };
            let (reply, answer) = oneshot::channel();
            let op = RegionOp {
                pos: ChunkPos::new(region.x * REGION_CHUNKS, region.z * REGION_CHUNKS),
                kind: OpKind::Release,
                reply,
                waiting: WaitingCell::default(),
            };
            let released = tokio::time::timeout(ANSWER_TIMEOUT, async {
                handle.request(RegionRequest::Pregen(op)).await && answer.await.is_ok()
            })
            .await;
            if released != Ok(true) {
                crate::log::error!(
                    "chunk generation job {}: region {region:?} did not release its removed chunks",
                    self.id
                );
            }
            self.release(Some(&shared), region).await;
        }
    }

    /// Whether the current phase has nothing left to send or wait for.
    fn phase_over(&self) -> bool {
        self.in_flight.is_empty()
    }
}

struct Actor {
    status: watch::Sender<Status>,
    chunks: broadcast::Sender<Arc<Generated>>,
    next_id: u64,
    job: Option<Job>,
    last: Option<Finished>,
}

async fn next_done(job: &mut Option<Job>) -> Option<(ChunkPos, Outcome)> {
    match job {
        Some(job) if !job.in_flight.is_empty() => job.in_flight.join_next().await?.ok(),
        _ => std::future::pending().await,
    }
}

impl Actor {
    /// A job that was mid-step at a panic may be inconsistent and panic
    /// again at once, so a restart drops it and publishes an idle status.
    fn abandon_job(&mut self) {
        if self.job.take().is_some() {
            log::error!("the running chunk pregen job was dropped after a panic");
            self.publish();
        }
    }

    async fn run(&mut self, rx: &mut mpsc::Receiver<Request>) {
        let mut publish = tokio::time::interval(PUBLISH_EVERY);
        publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                request = rx.recv() => match request {
                    Some(request) => self.on_request(request).await,
                    None => break,
                },
                Some((pos, outcome)) = next_done(&mut self.job) => self.on_done(pos, outcome).await,
                _ = publish.tick(), if self.job.is_some() => {
                    if let Some(job) = &mut self.job {
                        job.sample(Instant::now());
                    }
                    self.flush_batch();
                    self.publish();
                }
            }
            self.advance().await;
        }
    }

    fn publish(&self) {
        let running = self.job.as_ref().map(Job::progress);
        self.status.send_replace(Status {
            running,
            last: self.last.clone(),
        });
    }

    fn flush_batch(&mut self) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.batch.is_empty() {
            return;
        }
        let chunks = std::mem::take(&mut job.batch);
        let _ = self.chunks.send(Arc::new(Generated {
            dimension: job.spec.dimension,
            chunks,
        }));
    }

    async fn on_request(&mut self, request: Request) {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Start {
                shared,
                spec,
                reply,
            } => {
                if let Some(job) = &self.job {
                    let _ = reply.send(Err(PregenError::Busy(job.id)));
                    return;
                }
                let Some(strong) = shared.upgrade() else {
                    let _ = reply.send(Err(PregenError::Stopped));
                    return;
                };
                let id = self.next_id;
                self.next_id += 1;
                let phase = if spec.regenerate {
                    Phase::Removing
                } else {
                    Phase::Generating
                };
                let now = Instant::now();
                let job = Job {
                    id,
                    total: spec.area.count(),
                    order: walk(spec.area),
                    parallel: parallelism(&strong),
                    spec,
                    shared,
                    phase,
                    done: 0,
                    failed: 0,
                    removed: 0,
                    remove_failed: 0,
                    generated: 0,
                    started: now,
                    started_unix: unix_now(),
                    cancelling: false,
                    error: None,
                    in_flight: JoinSet::new(),
                    pending: HashMap::new(),
                    regions: HashMap::new(),
                    parked: HashSet::new(),
                    samples: vec![(now, 0)],
                    batch: Vec::new(),
                };
                let progress = job.progress();
                crate::log::info!(
                    "{} started chunk generation job {id} ({}): {} of radius {} around {} {} in {}, {} chunks",
                    progress.by,
                    if progress.regenerate {
                        "regenerating"
                    } else {
                        "generating"
                    },
                    progress.shape,
                    progress.radius,
                    progress.x,
                    progress.z,
                    progress.dimension,
                    progress.total
                );
                self.job = Some(job);
                self.publish();
                let _ = reply.send(Ok(progress));
            }
            Request::Cancel { reply } => match &mut self.job {
                Some(job) => {
                    job.cancelling = true;
                    let progress = job.progress();
                    self.publish();
                    let _ = reply.send(Ok(progress));
                }
                None => {
                    let _ = reply.send(Err(PregenError::NotRunning));
                }
            },
        }
    }

    async fn on_done(&mut self, pos: ChunkPos, outcome: Outcome) {
        let Some(job) = &mut self.job else {
            return;
        };
        job.done += 1;
        job.pending.remove(&pos);
        match outcome {
            Outcome::Done if job.phase == Phase::Generating => {
                job.generated += 1;
                job.batch.push(pos);
            }
            Outcome::Done => {}
            Outcome::Failed => job.failed += 1,
        }
        let shared = job.shared.upgrade();
        job.release(shared.as_deref(), pos.region()).await;
        if job.batch.len() >= BATCH {
            self.flush_batch();
        }
    }

    /// Sends more chunks, moves to the next phase or ends the job.
    async fn advance(&mut self) {
        let Some(job) = &mut self.job else {
            return;
        };
        if !job.fill().await {
            job.cancelling = true;
        }
        if !job.phase_over() {
            return;
        }
        let stopping = job.cancelling || job.error.is_some();
        if job.phase == Phase::Removing {
            job.finish_removing().await;
            job.removed = job.done;
            job.remove_failed = job.failed;
        }
        if job.phase == Phase::Removing && !stopping {
            job.phase = Phase::Generating;
            job.order = walk(job.spec.area);
            job.done = 0;
            job.failed = 0;
            job.samples = vec![(Instant::now(), 0)];
            if !job.fill().await {
                job.cancelling = true;
            }
            if !job.phase_over() {
                self.publish();
                return;
            }
        }
        self.flush_batch();
        let Some(job) = self.job.take() else {
            return;
        };
        let finished = job.finished();
        crate::log::info!(
            "chunk generation job {} {}: {} of {} chunks generated, {} failed{}, in {} s",
            finished.id,
            if finished.cancelled {
                "cancelled"
            } else {
                "finished"
            },
            finished.generated,
            finished.total,
            finished.failed,
            if job.spec.regenerate {
                format!(", {} could not be removed", finished.remove_failed)
            } else {
                String::new()
            },
            finished.duration_ms / 1000
        );
        self.last = Some(finished);
        self.publish();
    }
}

#[cfg(test)]
mod max_priority_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_runner() {
        let pregen = Pregen::spawn();
        pregen.panic_for_test().await;
        assert!(matches!(
            pregen.cancel().await,
            Err(PregenError::NotRunning)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn max_priority_raises_in_flight_limit() {
        assert_eq!(parallelism_for(0, false, 8), 16);
        assert_eq!(parallelism_for(0, true, 8), 64);
        assert_eq!(parallelism_for(1, true, 8), 64);
        assert_eq!(parallelism_for(1, false, 8), 1);
        assert_eq!(parallelism_for(500, true, 4), 500);
    }

    #[tuff_core::watchdog]
    #[test]
    fn max_priority_defaults_off() {
        let key = config_keys()
            .into_iter()
            .find(|k| k.name == MAX_PRIORITY_KEY)
            .unwrap();
        assert_eq!(key.default, Value::Bool(false));
    }
}

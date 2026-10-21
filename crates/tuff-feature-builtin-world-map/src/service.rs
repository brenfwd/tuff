//! The map actor: owns the tile cache, schedules renders on its own
//! rayon pool, keeps cached tiles current by polling the stamps of loaded
//! chunks, and publishes changed tiles and player markers while someone
//! has the map open.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use rayon::prelude::*;
use tokio::sync::{broadcast, mpsc, oneshot};
use tuff_core::log;
use tuff_core::metrics::{Kind, Series};
use tuff_core::world::Shared;
use tuff_core::world::dimension::{DimensionId, Dimensions};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_core::world::pregen::Generated;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_storage::admin::format_uuid;
use tuff_world_model::pos::{ChunkPos, FileRegionPos, RegionPos};

use crate::gather::{self, Source};
use crate::png;
use crate::surface::{ChunkSamples, sample_chunk};
use crate::tile::{BaseTile, CHUNKS, MAX_LEVEL, PIXELS, Rect, SIDE, TileKey, downsample};

mod persist;
mod revalidate;
pub use revalidate::RECHECK_BASE_BUDGET;

pub const TILES_TOPIC: &str = "plugin.world-map.tiles";
pub const MARKERS_TOPIC: &str = "plugin.world-map.markers";
pub const PREGEN_TOPIC: &str = "plugin.world-map.pregen";

pub const CACHE_KEY: &str = "tuff.features.world-map.cacheMiB";
pub const THREADS_KEY: &str = "tuff.features.world-map.renderThreads";
pub const RATE_KEY: &str = "tuff.features.world-map.tilesPerSecond";
pub const DISK_KEY: &str = "tuff.features.world-map.diskCacheMiB";

const QUEUE_CAP: usize = 256;
const COMMANDS: usize = 256;
const LEASE: Duration = Duration::from_secs(15);
const IDLE: Duration = Duration::from_secs(600);
const LISTING_AGE: Duration = Duration::from_secs(30);
const MAX_EVENT_TILES: usize = 256;
const FINAL_FLUSH: Duration = Duration::from_secs(30);

/// A tile request's answer.
#[derive(Debug)]
pub enum TileReply {
    Png {
        etag: String,
        png: Bytes,
    },
    NotModified {
        etag: String,
    },
    /// The caller's tile budget is spent; retry after this many seconds.
    Limited(u64),
    /// The render queue is full.
    Busy,
    Failed,
    NotFound,
}

/// What a panel page is looking at, in blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub dim: u16,
    pub level: u8,
    pub min: [i32; 2],
    pub max: [i32; 2],
}

/// Counters for tests and the API.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tiles: usize,
    pub bytes: usize,
    pub rendered: u64,
    pub patched: u64,
    pub saved_reads: u64,
    /// Tiles served from the persistent cache.
    pub disk_loads: u64,
    /// Persisted tiles ignored as unreadable or damaged.
    pub disk_errors: u64,
    pub disk_writes: u64,
    /// Tiles the persistent cache holds, and the bytes it takes.
    pub disk_entries: usize,
    pub disk_bytes: u64,
    /// Persisted tiles checked against the world, and how many of those
    /// had changed.
    pub rechecked: u64,
    pub recheck_changed: u64,
    /// Level-0 tiles left stale for a later view because a recheck hit
    /// its budget.
    pub recheck_deferred: u64,
}

enum Command {
    Tile {
        key: TileKey,
        client: String,
        if_none_match: Option<String>,
        reply: oneshot::Sender<TileReply>,
    },
    Viewport {
        client: String,
        view: Viewport,
    },
    Markers {
        reply: oneshot::Sender<Arc<str>>,
    },
    Poll {
        reply: oneshot::Sender<()>,
    },
    Stats {
        reply: oneshot::Sender<Stats>,
    },
    Version {
        key: TileKey,
        reply: oneshot::Sender<Option<u64>>,
    },
    Pixels {
        key: TileKey,
        reply: oneshot::Sender<Option<Vec<u8>>>,
    },
    Flush {
        reply: oneshot::Sender<()>,
    },
    Idle {
        reply: oneshot::Sender<()>,
    },
}

/// The map actor's handle.
#[derive(Clone)]
pub struct MapHandle {
    tx: mpsc::Sender<Command>,
    epoch: u32,
}

impl MapHandle {
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub async fn tile(
        &self,
        key: TileKey,
        client: &str,
        if_none_match: Option<String>,
    ) -> TileReply {
        let (reply, answer) = oneshot::channel();
        let command = Command::Tile {
            key,
            client: client.to_owned(),
            if_none_match,
            reply,
        };
        if self.tx.send(command).await.is_err() {
            return TileReply::Failed;
        }
        answer.await.unwrap_or(TileReply::Failed)
    }

    pub async fn viewport(&self, client: &str, view: Viewport) {
        let _ = self
            .tx
            .send(Command::Viewport {
                client: client.to_owned(),
                view,
            })
            .await;
    }

    /// The last player marker snapshot, as JSON.
    pub async fn markers(&self) -> Arc<str> {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Markers { reply }).await.is_err() {
            return Arc::from(EMPTY_MARKERS);
        }
        answer.await.unwrap_or_else(|_| Arc::from(EMPTY_MARKERS))
    }

    /// Runs one invalidation poll now and waits until its changes landed.
    pub async fn poll(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Poll { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }

    pub async fn stats(&self) -> Stats {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Stats { reply }).await.is_err() {
            return Stats::default();
        }
        answer.await.unwrap_or_default()
    }

    /// The cached pixels of `key` (all transparent for an empty tile).
    pub async fn pixels(&self, key: TileKey) -> Option<Vec<u8>> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Command::Pixels { key, reply }).await.ok()?;
        answer.await.ok().flatten()
    }

    /// The cached version of `key`, if it is cached.
    pub async fn version(&self, key: TileKey) -> Option<u64> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Command::Version { key, reply }).await.ok()?;
        answer.await.ok().flatten()
    }

    /// Writes every changed tile to the persistent cache now (unless
    /// saving is held or off) and waits until that is done.
    pub async fn flush(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Flush { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }

    /// Waits until no tile is being loaded, drawn or checked.
    pub async fn idle(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Idle { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }
}

const EMPTY_MARKERS: &str = r#"{"players":[]}"#;

/// The world service running the map actor.
#[derive(Default)]
pub struct MapService {
    handle: OnceLock<MapHandle>,
}

impl MapService {
    pub fn handle(&self) -> Option<&MapHandle> {
        self.handle.get()
    }
}

impl WorldService for MapService {
    fn start(&self, shared: &Arc<Shared>) {
        let _ = self.handle.set(spawn(shared.clone()));
    }

    fn flush(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Some(handle) = self.handle.get() {
                let _ = tokio::time::timeout(FINAL_FLUSH, handle.flush()).await;
            }
        })
    }
}

pub struct MapServiceFactory;

impl ServiceFactory for MapServiceFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(MapService::default())
    }
}

fn render_threads(shared: &Shared) -> usize {
    match shared.config.get_int(THREADS_KEY).unwrap_or(0) {
        n if n > 0 => n.min(256) as usize,
        _ => std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .max(1),
    }
}

fn spawn(shared: Arc<Shared>) -> MapHandle {
    let (tx, rx) = mpsc::channel(COMMANDS);
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let threads = render_threads(&shared);
    let pool = tuff_core::threads::rayon_pool("map")
        .num_threads(threads)
        .build()
        .expect("the map render pool starts");
    let pool = Arc::new(pool);
    tuff_core::threads::register_pool(&pool);
    let epoch = rand::random::<u32>();
    let metrics = &shared.metrics;
    let series = MapSeries {
        rendered: metrics.series("map.tiles_rendered", Kind::Counter),
        patched: metrics.series("map.tiles_patched", Kind::Counter),
        render_ms: metrics.series("map.render_ms", Kind::Gauge),
        cache_bytes: metrics.series("map.cache_bytes", Kind::Gauge),
        queue: metrics.series("map.queue", Kind::Gauge),
    };
    let mut actor = Actor {
        empty_png: Bytes::from(png::encode(&vec![0; PIXELS], SIDE)),
        shared,
        internal_tx,
        pool,
        epoch,
        next_version: 0,
        tiles: HashMap::new(),
        bytes: 0,
        pending: HashMap::new(),
        queue: VecDeque::new(),
        in_flight: 0,
        max_jobs: threads,
        listings: HashMap::new(),
        listing_waits: HashMap::new(),
        running: HashMap::new(),
        live: HashMap::new(),
        buckets: HashMap::new(),
        leases: HashMap::new(),
        changed: BTreeMap::new(),
        markers: Arc::from(EMPTY_MARKERS),
        polling: false,
        entities: String::new(),
        entities_busy: false,
        poll_waiters: Vec::new(),
        ticks: 0,
        stats: Stats::default(),
        series,
        disk: persist::Disk::default(),
        rechecks: HashMap::new(),
        recheck_queue: VecDeque::new(),
        recheck_requests: Vec::new(),
        recheck_budget: 0,
        rechecking: 0,
        idle_waiters: Vec::new(),
    };
    tuff_supervise::spawn(
        "the world map pregen feed",
        publish_pregen(actor.shared.clone()),
    );
    actor.start_disk();
    tuff_supervise::spawn("the world map", actor.run(rx, internal_rx));
    MapHandle { tx, epoch }
}

/// Forwards every pre-generation status change to the page's topic.
async fn publish_pregen(shared: Arc<Shared>) {
    let mut status = shared.pregen.watch();
    let events = shared.web.events.clone();
    drop(shared);
    while status.changed().await.is_ok() {
        let body = serde_json::to_string(&*status.borrow_and_update()).unwrap_or_default();
        events.publish(PREGEN_TOPIC, "status", body);
    }
}

struct MapSeries {
    rendered: Series,
    patched: Series,
    render_ms: Series,
    cache_bytes: Series,
    queue: Series,
}

enum Data {
    Empty,
    Base(Box<BaseTile>),
    Zoomed(Box<[u8]>),
}

impl Data {
    fn bytes(&self) -> usize {
        match self {
            Data::Empty => 0,
            Data::Base(_) => BaseTile::BYTES,
            Data::Zoomed(_) => PIXELS,
        }
    }

    fn pixels(&self) -> Option<&[u8]> {
        match self {
            Data::Empty => None,
            Data::Base(tile) => Some(&tile.pixels),
            Data::Zoomed(pixels) => Some(pixels),
        }
    }
}

struct Entry {
    version: u64,
    used: Instant,
    png: Option<Bytes>,
    data: Data,
    /// Read from the persistent cache and not yet checked against the
    /// world (or drawn from such tiles).
    stale: bool,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.data.bytes() + self.png.as_ref().map_or(0, Bytes::len)
    }
}

enum Waiter {
    Client {
        if_none_match: Option<String>,
        reply: oneshot::Sender<TileReply>,
    },
    Parent(TileKey),
}

#[derive(Default)]
struct Pending {
    waiters: Vec<Waiter>,
    /// Children a zoomed tile still waits for.
    missing: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Base(TileKey),
    Encode(TileKey),
    Load(TileKey),
}

impl Job {
    fn key(self) -> TileKey {
        match self {
            Job::Base(key) | Job::Encode(key) | Job::Load(key) => key,
        }
    }
}

struct Listing {
    regions: Arc<HashSet<FileRegionPos>>,
    fetched: Instant,
    refreshing: bool,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

struct Lease {
    view: Viewport,
    until: Instant,
}

/// One chunk's new look, from a poll.
struct Update {
    dim: u16,
    pos: ChunkPos,
    stamp: Option<u64>,
    samples: Box<ChunkSamples>,
}

enum Internal {
    Base {
        key: TileKey,
        result: Option<BaseResult>,
        recheck: bool,
    },
    DiskLoaded(crate::disk::Loaded),
    Loaded {
        key: TileKey,
        result: Result<crate::disk::Persisted, persist::LoadError>,
    },
    Flushed(persist::Flushed),
    Encoded {
        key: TileKey,
        version: u64,
        png: Option<Bytes>,
    },
    Listing {
        dim: u16,
        regions: HashSet<FileRegionPos>,
        running: Vec<RegionPos>,
    },
    Polled {
        running: Vec<(u16, Vec<RegionPos>)>,
        updates: Vec<Update>,
        forgotten: Vec<(u16, ChunkPos)>,
        saved_reads: u64,
    },
    Players(String),
    Entities(String),
    /// Chunks a pre-generation job saved, read back and sampled.
    Refreshed {
        updates: Vec<Update>,
        saved_reads: u64,
    },
}

struct BaseResult {
    tile: Option<Box<BaseTile>>,
    png: Option<Bytes>,
    stamps: Vec<(ChunkPos, u64)>,
    saved_reads: u64,
    millis: u64,
}

struct Actor {
    shared: Arc<Shared>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    pool: Arc<rayon::ThreadPool>,
    epoch: u32,
    next_version: u64,
    tiles: HashMap<TileKey, Entry>,
    bytes: usize,
    pending: HashMap<TileKey, Pending>,
    queue: VecDeque<Job>,
    in_flight: usize,
    max_jobs: usize,
    listings: HashMap<u16, Listing>,
    listing_waits: HashMap<u16, Vec<(TileKey, Waiter)>>,
    running: HashMap<u16, Vec<RegionPos>>,
    /// The stamp each loaded chunk had when a cached tile last drew it.
    live: HashMap<(u16, ChunkPos), u64>,
    buckets: HashMap<String, Bucket>,
    leases: HashMap<String, Lease>,
    changed: BTreeMap<TileKey, u64>,
    markers: Arc<str>,
    polling: bool,
    entities: String,
    entities_busy: bool,
    poll_waiters: Vec<oneshot::Sender<()>>,
    ticks: u64,
    stats: Stats,
    series: MapSeries,
    empty_png: Bytes,
    disk: persist::Disk,
    rechecks: HashMap<TileKey, revalidate::Recheck>,
    /// Level-0 tiles to draw again to check them, after all other work.
    recheck_queue: VecDeque<TileKey>,
    /// Stale tiles served since the last turn of the loop.
    recheck_requests: Vec<TileKey>,
    recheck_budget: usize,
    rechecking: usize,
    idle_waiters: Vec<oneshot::Sender<()>>,
}

fn dimension_id(shared: &Shared, dim: u16) -> Option<DimensionId> {
    shared.dimensions.iter().nth(usize::from(dim)).map(|d| d.id)
}

/// The file regions a running region's chunks fall in.
fn region_files(region: RegionPos) -> ((i32, i32), (i32, i32)) {
    let side = tuff_world_model::pos::REGION_CHUNKS;
    let a = ChunkPos::new(region.x * side, region.z * side).file_region();
    let b = ChunkPos::new(region.x * side + side - 1, region.z * side + side - 1).file_region();
    ((a.x, b.x), (a.z, b.z))
}

fn overlaps(a: ((i32, i32), (i32, i32)), b: ((i32, i32), (i32, i32))) -> bool {
    a.0.0 <= b.0.1 && b.0.0 <= a.0.1 && a.1.0 <= b.1.1 && b.1.0 <= a.1.1
}

impl Actor {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Command>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        let period = Duration::from_secs(1);
        let mut clock = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut generated = self.shared.pregen.subscribe();
        let mut generating = true;
        loop {
            tokio::select! {
                biased;
                Some(message) = internal.recv() => self.on_internal(message),
                command = rx.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
                batch = generated.recv(), if generating => match batch {
                    Ok(batch) => self.on_generated(&batch),
                    Err(broadcast::error::RecvError::Lagged(_)) => self.resync(),
                    Err(broadcast::error::RecvError::Closed) => generating = false,
                },
                _ = clock.tick() => self.on_tick(),
            }
            self.request_rechecks();
            self.pump();
            self.maybe_flush();
            self.answer_idle();
            self.series.queue.set(self.queue.len() as i64);
        }
    }

    fn version(&mut self) -> u64 {
        self.next_version += 1;
        self.next_version
    }

    fn answer_idle(&mut self) {
        if self.idle_waiters.is_empty() {
            return;
        }
        let idle = self.pending.is_empty()
            && self.queue.is_empty()
            && self.in_flight == 0
            && self.rechecks.is_empty()
            && self.recheck_queue.is_empty()
            && self.recheck_requests.is_empty()
            && self.listing_waits.is_empty()
            && self.disk.waits.is_empty()
            && !self.disk_pending();
        if idle {
            for waiter in self.idle_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }

    fn etag(&self, version: u64) -> String {
        format!("\"{:08x}.{version}\"", self.epoch)
    }

    fn active(&self) -> bool {
        let now = Instant::now();
        self.leases.values().any(|lease| lease.until > now)
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Tile {
                key,
                client,
                if_none_match,
                reply,
            } => {
                if !self.take_token(&client) {
                    let _ = reply.send(TileReply::Limited(1));
                    return;
                }
                if dimension_id(&self.shared, key.dim).is_none() || key.level > MAX_LEVEL {
                    let _ = reply.send(TileReply::NotFound);
                    return;
                }
                let needs_work = !self.tiles.contains_key(&key) && !self.pending.contains_key(&key);
                if needs_work && self.queue.len() >= QUEUE_CAP {
                    let _ = reply.send(TileReply::Busy);
                    return;
                }
                self.want(
                    key,
                    Waiter::Client {
                        if_none_match,
                        reply,
                    },
                );
            }
            Command::Viewport { client, view } => {
                let until = Instant::now() + LEASE;
                self.leases.insert(client, Lease { view, until });
            }
            Command::Markers { reply } => {
                let _ = reply.send(self.markers.clone());
            }
            Command::Poll { reply } => {
                self.poll_waiters.push(reply);
                self.start_poll();
            }
            Command::Stats { reply } => {
                let mut stats = self.stats.clone();
                stats.tiles = self.tiles.len();
                stats.bytes = self.bytes;
                stats.disk_entries = self.disk.index.entries.len();
                stats.disk_bytes = if stats.disk_entries == 0 {
                    0
                } else {
                    self.disk.index.total()
                };
                let _ = reply.send(stats);
            }
            Command::Flush { reply } => self.request_flush(reply),
            Command::Idle { reply } => self.idle_waiters.push(reply),
            Command::Version { key, reply } => {
                let _ = reply.send(self.tiles.get(&key).map(|e| e.version));
            }
            Command::Pixels { key, reply } => {
                let pixels = self.tiles.get(&key).map(|e| match e.data.pixels() {
                    Some(pixels) => pixels.to_vec(),
                    None => vec![0; PIXELS],
                });
                let _ = reply.send(pixels);
            }
        }
    }

    fn take_token(&mut self, client: &str) -> bool {
        let rate = self
            .shared
            .config
            .get_int(RATE_KEY)
            .unwrap_or(32)
            .clamp(1, 100_000) as f64;
        let now = Instant::now();
        let bucket = self.buckets.entry(client.to_owned()).or_insert(Bucket {
            tokens: rate * 4.0,
            at: now,
        });
        let elapsed = now.saturating_duration_since(bucket.at).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(rate * 4.0);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn reply_to(
        &self,
        entry: &Entry,
        if_none_match: Option<String>,
        reply: oneshot::Sender<TileReply>,
    ) {
        let etag = self.etag(entry.version);
        let answer = if if_none_match.is_some_and(|tags| {
            tags.split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag)
        }) {
            TileReply::NotModified { etag }
        } else {
            match (&entry.png, &entry.data) {
                (Some(png), _) => TileReply::Png {
                    etag,
                    png: png.clone(),
                },
                (None, Data::Empty) => TileReply::Png {
                    etag,
                    png: self.empty_png.clone(),
                },
                (None, _) => TileReply::Failed,
            }
        };
        let _ = reply.send(answer);
    }

    /// Satisfies `waiter` from the cache if it can; otherwise hands it back.
    fn try_satisfy(&mut self, key: TileKey, waiter: Waiter) -> Option<Waiter> {
        let Some(entry) = self.tiles.get_mut(&key) else {
            return Some(waiter);
        };
        entry.used = Instant::now();
        match waiter {
            Waiter::Parent(parent) => {
                self.child_ready(parent);
                None
            }
            Waiter::Client {
                if_none_match,
                reply,
            } => {
                let entry = &self.tiles[&key];
                let current = self.etag(entry.version);
                let matched = if_none_match.as_deref().is_some_and(|tags| {
                    tags.split(',')
                        .any(|t| t.trim().trim_start_matches("W/") == current)
                });
                if matched || entry.png.is_some() || matches!(entry.data, Data::Empty) {
                    if entry.stale {
                        self.recheck_requests.push(key);
                    }
                    self.reply_to(entry, if_none_match, reply);
                    self.touch_disk(key);
                    None
                } else {
                    Some(Waiter::Client {
                        if_none_match,
                        reply,
                    })
                }
            }
        }
    }

    fn want(&mut self, key: TileKey, waiter: Waiter) {
        self.unspill(key);
        let Some(waiter) = self.try_satisfy(key, waiter) else {
            return;
        };
        if self.tiles.contains_key(&key) {
            let pending = self.pending.entry(key).or_default();
            let first = pending.waiters.is_empty();
            pending.waiters.push(waiter);
            if first {
                self.queue.push_back(Job::Encode(key));
            }
            return;
        }
        if let Some(pending) = self.pending.get_mut(&key) {
            pending.waiters.push(waiter);
            return;
        }
        match self.may_have_content(key) {
            None => {
                self.listing_waits
                    .entry(key.dim)
                    .or_default()
                    .push((key, waiter));
                self.fetch_listing(key.dim);
            }
            Some(false) => {
                self.install(key, Data::Empty, None);
                if let Some(waiter) = self.try_satisfy(key, waiter) {
                    self.pending.entry(key).or_default().waiters.push(waiter);
                }
            }
            Some(true) => {
                if self.disk_pending() {
                    self.disk.waits.push((key, waiter));
                    return;
                }
                self.pending.entry(key).or_default().waiters.push(waiter);
                self.build(key);
            }
        }
    }

    /// Starts making `key`, which has waiters: from the persistent cache
    /// when it holds the tile (unless a zoomed tile's children are all
    /// at hand), else by drawing or composing it.
    fn build(&mut self, key: TileKey) {
        let children_cached = key
            .children()
            .is_some_and(|children| children.iter().all(|c| self.tiles.contains_key(c)));
        if !children_cached && self.persisted(key) {
            self.queue.push_back(Job::Load(key));
            return;
        }
        match key.children() {
            None => self.queue.push_back(Job::Base(key)),
            Some(children) => {
                let missing = children
                    .iter()
                    .filter(|c| !self.tiles.contains_key(c))
                    .count();
                if let Some(pending) = self.pending.get_mut(&key) {
                    pending.missing = missing;
                }
                if missing == 0 {
                    self.compose(key);
                } else {
                    for child in children {
                        if !self.tiles.contains_key(&child) {
                            self.want(child, Waiter::Parent(key));
                        }
                    }
                }
            }
        }
    }

    fn child_ready(&mut self, parent: TileKey) {
        let Some(pending) = self.pending.get_mut(&parent) else {
            return;
        };
        pending.missing = pending.missing.saturating_sub(1);
        if pending.missing == 0 {
            self.compose(parent);
        }
    }

    /// Whether anything can be drawn in `key`: `None` until the storage
    /// listing of its dimension is known.
    fn may_have_content(&self, key: TileKey) -> Option<bool> {
        let listing = self.listings.get(&key.dim)?;
        let ((x0, x1), (z0, z1)) = key.file_regions();
        let span = i64::from(x1 - x0 + 1) * i64::from(z1 - z0 + 1);
        let stored = if span as usize <= listing.regions.len() {
            (x0..=x1)
                .any(|x| (z0..=z1).any(|z| listing.regions.contains(&FileRegionPos::new(x, z))))
        } else {
            listing
                .regions
                .iter()
                .any(|r| r.x >= x0 && r.x <= x1 && r.z >= z0 && r.z <= z1)
        };
        let live = self.running.get(&key.dim).is_some_and(|regions| {
            regions
                .iter()
                .any(|r| overlaps(region_files(*r), ((x0, x1), (z0, z1))))
        });
        Some(stored || live)
    }

    fn fetch_listing(&mut self, dim: u16) {
        if let Some(listing) = self.listings.get_mut(&dim) {
            if listing.refreshing {
                return;
            }
            listing.refreshing = true;
        } else if self.listing_waits.get(&dim).is_some_and(|w| w.len() > 1) {
            return;
        }
        let Some(id) = dimension_id(&self.shared, dim) else {
            return;
        };
        let shared = self.shared.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let key = shared.dimensions.get(id).key.clone();
            let regions = gather::stored_regions(&shared, &key).await;
            let running = gather::running(&shared, id)
                .await
                .into_iter()
                .map(|h| h.pos)
                .collect();
            let _ = tx.send(Internal::Listing {
                dim,
                regions,
                running,
            });
        });
    }

    fn install(&mut self, key: TileKey, data: Data, png: Option<Bytes>) {
        let version = self.version();
        let empty = matches!(data, Data::Empty);
        let entry = Entry {
            version,
            used: Instant::now(),
            png,
            data,
            stale: false,
        };
        self.bytes += entry.bytes();
        if let Some(old) = self.tiles.insert(key, entry) {
            self.bytes -= old.bytes();
        }
        self.changed.insert(key, version);
        self.series.cache_bytes.set(self.bytes as i64);
        if empty {
            self.disk.dirty.remove(&key);
            self.forget_disk(key);
        } else {
            self.disk.dirty.insert(key);
        }
    }

    fn remove(&mut self, key: TileKey) -> Option<Entry> {
        let old = self.tiles.remove(&key)?;
        self.disk.dirty.remove(&key);
        self.bytes -= old.bytes();
        self.changed.insert(key, 0);
        Some(old)
    }

    fn set_png(&mut self, key: TileKey, png: Option<Bytes>) {
        if let Some(entry) = self.tiles.get_mut(&key) {
            self.bytes -= entry.bytes();
            entry.png = png;
            self.bytes += entry.bytes();
        }
    }

    /// Hands every waiter of `key` what it can get now; the rest wait for
    /// an encode.
    fn notify(&mut self, key: TileKey) {
        let Some(pending) = self.pending.remove(&key) else {
            return;
        };
        let mut left = Vec::new();
        for waiter in pending.waiters {
            if let Some(waiter) = self.try_satisfy(key, waiter) {
                left.push(waiter);
            }
        }
        if !left.is_empty() {
            if self.tiles.contains_key(&key) {
                self.queue.push_back(Job::Encode(key));
                self.pending.insert(
                    key,
                    Pending {
                        waiters: left,
                        missing: 0,
                    },
                );
            } else {
                for waiter in left {
                    self.fail(waiter);
                }
            }
        }
    }

    fn fail(&mut self, waiter: Waiter) {
        match waiter {
            Waiter::Client { reply, .. } => {
                let _ = reply.send(TileReply::Failed);
            }
            Waiter::Parent(parent) => {
                if let Some(pending) = self.pending.remove(&parent) {
                    for waiter in pending.waiters {
                        self.fail(waiter);
                    }
                }
            }
        }
    }

    /// Builds a zoomed tile from its four cached children.
    fn compose(&mut self, key: TileKey) {
        let children = key.children().expect("zoomed tiles have children");
        let gone: Vec<TileKey> = children
            .iter()
            .copied()
            .filter(|c| !self.tiles.contains_key(c))
            .collect();
        if !gone.is_empty() {
            if let Some(pending) = self.pending.get_mut(&key) {
                pending.missing = gone.len();
            }
            for child in gone {
                self.want(child, Waiter::Parent(key));
            }
            return;
        }
        let (data, stale) = self.downsampled(children);
        self.install(key, data, None);
        if let Some(entry) = self.tiles.get_mut(&key) {
            entry.stale = stale;
        }
        self.notify(key);
        self.evict();
    }

    /// A zoomed tile from its four cached children, and whether any of
    /// them is stale.
    fn downsampled(&self, children: [TileKey; 4]) -> (Data, bool) {
        let mut pixels = vec![0u8; PIXELS];
        let mut any = false;
        let mut stale = false;
        for child in children {
            let entry = &self.tiles[&child];
            stale |= entry.stale;
            let data = entry.data.pixels();
            any |= data.is_some();
            downsample(data, child.quadrant(), Rect::FULL, &mut pixels);
        }
        let data = if any {
            Data::Zoomed(pixels.into_boxed_slice())
        } else {
            Data::Empty
        };
        (data, stale)
    }

    /// Whether every waiter of `key`, through its parents, has gone.
    fn abandoned(&self, key: TileKey) -> bool {
        self.pending.get(&key).is_none_or(|pending| {
            pending.waiters.iter().all(|waiter| match waiter {
                Waiter::Client { reply, .. } => reply.is_closed(),
                Waiter::Parent(parent) => self.abandoned(*parent),
            })
        })
    }

    fn drop_pending(&mut self, key: TileKey) {
        if let Some(pending) = self.pending.remove(&key) {
            for waiter in pending.waiters {
                if let Waiter::Parent(parent) = waiter {
                    self.drop_pending(parent);
                }
            }
        }
    }

    fn pump(&mut self) {
        while self.in_flight < self.max_jobs {
            let Some(job) = self.queue.pop_front() else {
                self.pump_rechecks();
                return;
            };
            if self.abandoned(job.key()) {
                self.drop_pending(job.key());
                continue;
            }
            match job {
                Job::Base(key) => self.start_base(key, false),
                Job::Load(key) => self.start_load(key),
                Job::Encode(key) => {
                    let Some(entry) = self.tiles.get(&key) else {
                        self.notify(key);
                        continue;
                    };
                    if entry.png.is_some() {
                        self.notify(key);
                        continue;
                    }
                    let Some(pixels) = entry.data.pixels() else {
                        self.notify(key);
                        continue;
                    };
                    let pixels = pixels.to_vec();
                    let version = entry.version;
                    let tx = self.internal_tx.clone();
                    self.in_flight += 1;
                    self.pool.spawn(move || {
                        let png = std::panic::catch_unwind(|| png::encode(&pixels, SIDE))
                            .ok()
                            .map(Bytes::from);
                        let _ = tx.send(Internal::Encoded { key, version, png });
                    });
                }
            }
        }
    }

    fn start_base(&mut self, key: TileKey, recheck: bool) {
        let Some(id) = dimension_id(&self.shared, key.dim) else {
            if recheck {
                self.end_recheck(key);
            } else {
                self.notify(key);
            }
            return;
        };
        let stored = self
            .listings
            .get(&key.dim)
            .map(|l| l.regions.clone())
            .unwrap_or_default();
        let ceiling = self.shared.dimensions.get(id).kind.has_ceiling;
        let shared = self.shared.clone();
        let pool = self.pool.clone();
        let tx = self.internal_tx.clone();
        self.in_flight += 1;
        if recheck {
            self.rechecking += 1;
        }
        tokio::spawn(async move {
            let started = Instant::now();
            let dimension = shared.dimensions.get(id);
            let (dim_key, shape) = (dimension.key.clone(), dimension.shape);
            let chunks: Vec<ChunkPos> = key.chunks().chain(key.north_chunks()).collect();
            let sources = gather::collect(&shared, id, &dim_key, &chunks, &stored).await;
            let (reply, answer) = oneshot::channel();
            let render_shared = shared.clone();
            pool.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render_base(&render_shared, key, shape, ceiling, sources)
                }));
                let _ = reply.send(result.ok());
            });
            let result = answer.await.ok().flatten().map(|mut result| {
                result.millis = started.elapsed().as_millis() as u64;
                result
            });
            let _ = tx.send(Internal::Base {
                key,
                result,
                recheck,
            });
        });
    }

    fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Base {
                key,
                result,
                recheck: true,
            } => {
                self.in_flight -= 1;
                self.rechecking -= 1;
                self.finish_base_recheck(key, result);
            }
            Internal::Base {
                key,
                result,
                recheck: false,
            } => {
                self.in_flight -= 1;
                match result {
                    None => {
                        log::warn!("world map: rendering tile {key:?} failed");
                        if let Some(pending) = self.pending.remove(&key) {
                            for waiter in pending.waiters {
                                self.fail(waiter);
                            }
                        }
                    }
                    Some(result) => {
                        self.stats.rendered += 1;
                        self.stats.saved_reads += result.saved_reads;
                        self.series.rendered.inc();
                        self.series.render_ms.set(result.millis as i64);
                        for (pos, stamp) in result.stamps {
                            self.live.insert((key.dim, pos), stamp);
                        }
                        let data = match result.tile {
                            Some(tile) => Data::Base(tile),
                            None => Data::Empty,
                        };
                        self.install(key, data, result.png);
                        self.notify(key);
                        self.evict();
                    }
                }
            }
            Internal::DiskLoaded(loaded) => self.on_disk_loaded(loaded),
            Internal::Loaded { key, result } => self.on_loaded(key, result),
            Internal::Flushed(flushed) => self.on_flushed(flushed),
            Internal::Encoded { key, version, png } => {
                self.in_flight -= 1;
                let current = self.tiles.get(&key).map(|e| e.version);
                if current == Some(version) {
                    self.set_png(key, png);
                }
                self.notify(key);
            }
            Internal::Listing {
                dim,
                regions,
                running,
            } => {
                self.listings.insert(
                    dim,
                    Listing {
                        regions: Arc::new(regions),
                        fetched: Instant::now(),
                        refreshing: false,
                    },
                );
                self.running.insert(dim, running);
                for (key, waiter) in self.listing_waits.remove(&dim).unwrap_or_default() {
                    self.want(key, waiter);
                }
            }
            Internal::Polled {
                running,
                updates,
                forgotten,
                saved_reads,
            } => {
                self.polling = false;
                self.stats.saved_reads += saved_reads;
                for (dim, regions) in running {
                    self.running.insert(dim, regions);
                }
                for key in forgotten {
                    self.live.remove(&key);
                }
                self.apply(updates);
                for waiter in self.poll_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            }
            Internal::Refreshed {
                updates,
                saved_reads,
            } => {
                self.stats.saved_reads += saved_reads;
                self.apply(updates);
            }
            Internal::Entities(json) => {
                self.entities_busy = false;
                if self.entities != json {
                    self.entities = json.clone();
                    if self.active() {
                        self.shared
                            .web
                            .events
                            .publish(MARKERS_TOPIC, "entities", json);
                    }
                }
            }
            Internal::Players(json) => {
                if *self.markers != *json {
                    self.markers = Arc::from(json.as_str());
                    if self.active() {
                        self.shared
                            .web
                            .events
                            .publish(MARKERS_TOPIC, "players", json);
                    }
                }
            }
        }
    }

    fn on_tick(&mut self) {
        self.ticks += 1;
        let now = Instant::now();
        self.leases.retain(|_, lease| lease.until > now);
        self.buckets.retain(|_, bucket| {
            now.saturating_duration_since(bucket.at) < Duration::from_secs(300)
        });
        let active = self.active();
        if active || self.ticks.is_multiple_of(5) {
            self.start_poll();
        }
        let stale: Vec<u16> = self
            .listings
            .iter()
            .filter(|(_, l)| now.saturating_duration_since(l.fetched) > LISTING_AGE)
            .map(|(d, _)| *d)
            .collect();
        for dim in stale {
            self.fetch_listing(dim);
        }
        if active {
            self.publish_changes();
            self.fetch_players();
            self.fetch_entities();
        } else {
            self.changed.clear();
        }
        if self.ticks.is_multiple_of(60) {
            self.evict();
            self.prune_live();
        }
    }
}

/// Decodes and samples a level-0 tile's chunks on the map pool.
fn render_base(
    shared: &Shared,
    key: TileKey,
    shape: tuff_world_model::chunk::ChunkShape,
    ceiling: bool,
    sources: HashMap<ChunkPos, Source>,
) -> BaseResult {
    let mut stamps = Vec::new();
    let mut saved_reads = 0;
    let mut work = Vec::with_capacity(sources.len());
    for (pos, source) in sources {
        match &source {
            Source::Live(stamp, _) => stamps.push((pos, *stamp)),
            Source::Saved(_) => saved_reads += 1,
        }
        work.push((pos, source));
    }
    let sampled: Vec<(ChunkPos, Box<ChunkSamples>)> = work
        .into_par_iter()
        .filter_map(|(pos, source)| {
            let chunk = gather::decode(shared, pos, shape, source)?;
            Some((pos, Box::new(sample_chunk(&chunk, ceiling))))
        })
        .collect();
    let first = key.first_chunk();
    let mut tile = Box::new(BaseTile::empty());
    let mut any = false;
    for (pos, samples) in &sampled {
        let (cx, cz) = (pos.x - first.x, pos.z - first.z);
        if cz < 0 {
            tile.place_north(cx as usize, samples);
        } else {
            any |= samples.iter().any(|s| !s.is_empty());
            tile.place(cx as usize, cz as usize, samples);
        }
    }
    if !any {
        return BaseResult {
            tile: None,
            png: None,
            stamps,
            saved_reads,
            millis: 0,
        };
    }
    tile.shade(Rect::FULL, key.origin());
    let png = Bytes::from(png::encode(&tile.pixels, SIDE));
    BaseResult {
        tile: Some(tile),
        png: Some(png),
        stamps,
        saved_reads,
        millis: 0,
    }
}
mod cache;
mod sync;

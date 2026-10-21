//! The map actor's side of the persistent tile cache ([`crate::disk`]):
//! the in-memory index, loading persisted tiles, and coalesced, rate
//! limited flushes. All storage access runs on the blocking pool and all
//! encoding on the map pool; the actor only snapshots and bookkeeps.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::oneshot;

use super::{Actor, DISK_KEY, Data, Entry, Internal, Waiter};
use crate::disk::{self, DiskIndex, DiskKey, Outcome, Persisted, Write};
use crate::png;
use crate::surface::Sample;
use crate::tile::{SIDE, TileKey};

/// Least time between two flushes.
const FLUSH_GAP: Duration = Duration::from_secs(2);
/// Least time between two flushes made only to record last uses.
const INDEX_GAP: Duration = Duration::from_secs(60);
/// How long to leave storage alone after the save gate refused a write.
const BUSY_BACKOFF: Duration = Duration::from_secs(5);
const FLUSH_TILES: usize = 16;
const FLUSH_DELETES: usize = 64;
/// Changed tiles evicted from memory before they were written wait here.
const SPILL_BYTES: usize = 16 << 20;

#[derive(Debug)]
pub enum LoadError {
    Missing,
    Io(String),
    Corrupt(disk::Corrupt),
}

pub struct Flushed {
    outcome: Outcome,
    keys: Vec<(DiskKey, TileKey)>,
    doomed: Vec<String>,
}

pub struct Disk {
    pub ready: bool,
    pub index: DiskIndex,
    /// The index's dimension for each of the world's.
    dims: Vec<u16>,
    /// Tiles in memory that differ from what is persisted.
    pub dirty: HashSet<TileKey>,
    spill: Vec<(TileKey, Entry)>,
    spill_bytes: usize,
    doomed: Vec<String>,
    index_dirty: bool,
    flushing: bool,
    /// Entries dropped while a flush was under way.
    forgotten: HashSet<DiskKey>,
    last_flush: Instant,
    index_at: Instant,
    retry_at: Instant,
    next_try: Instant,
    pub waits: Vec<(TileKey, Waiter)>,
    flush_waiters: Vec<oneshot::Sender<()>>,
    flush_all: bool,
}

impl Default for Disk {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            ready: false,
            index: DiskIndex::default(),
            dims: Vec::new(),
            dirty: HashSet::new(),
            spill: Vec::new(),
            spill_bytes: 0,
            doomed: Vec::new(),
            index_dirty: false,
            flushing: false,
            forgotten: HashSet::new(),
            last_flush: now,
            index_at: now,
            retry_at: now,
            next_try: now,
            waits: Vec::new(),
            flush_waiters: Vec::new(),
            flush_all: false,
        }
    }
}

enum Snapshot {
    Base {
        samples: Box<[Sample]>,
        north: Box<[Sample]>,
        png: Option<Bytes>,
        pixels: Option<Box<[u8]>>,
    },
    Zoomed {
        pixels: Box<[u8]>,
        png: Option<Bytes>,
    },
}

fn snapshot(entry: &Entry) -> Option<Snapshot> {
    match &entry.data {
        Data::Empty => None,
        Data::Base(tile) => Some(Snapshot::Base {
            samples: tile.samples.clone(),
            north: tile.north.clone(),
            png: entry.png.clone(),
            pixels: entry.png.is_none().then(|| tile.pixels.clone()),
        }),
        Data::Zoomed(pixels) => Some(Snapshot::Zoomed {
            pixels: pixels.clone(),
            png: entry.png.clone(),
        }),
    }
}

fn take_snapshot(entry: Entry) -> Option<Snapshot> {
    match entry.data {
        Data::Empty => None,
        Data::Base(tile) => {
            let tile = *tile;
            let pixels = entry.png.is_none().then_some(tile.pixels);
            Some(Snapshot::Base {
                samples: tile.samples,
                north: tile.north,
                png: entry.png,
                pixels,
            })
        }
        Data::Zoomed(pixels) => Some(Snapshot::Zoomed {
            pixels,
            png: entry.png,
        }),
    }
}

fn encode(dim: &str, key: TileKey, snapshot: Snapshot) -> Vec<u8> {
    match snapshot {
        Snapshot::Base {
            samples,
            north,
            png,
            pixels,
        } => {
            let png = png
                .map(|p| p.to_vec())
                .or_else(|| pixels.map(|p| png::encode(&p, SIDE)));
            disk::encode_tile(dim, key, Some((&samples, &north)), png.as_deref())
        }
        Snapshot::Zoomed { pixels, png } => {
            let png = png.map_or_else(|| png::encode(&pixels, SIDE), |p| p.to_vec());
            disk::encode_tile(dim, key, None, Some(&png))
        }
    }
}

impl Actor {
    fn disk_budget(&self) -> u64 {
        self.shared
            .config
            .get_int(DISK_KEY)
            .unwrap_or(128)
            .clamp(0, 1 << 20) as u64
            * 1024
            * 1024
    }

    /// Whether tile requests must wait for the persisted index first.
    pub(super) fn disk_pending(&self) -> bool {
        !self.disk.ready && self.disk_budget() > 0
    }

    fn dim_name(&self, dim: u16) -> Option<String> {
        self.shared
            .dimensions
            .iter()
            .nth(usize::from(dim))
            .map(|d| d.key.to_string())
    }

    fn disk_key(&self, key: TileKey) -> Option<DiskKey> {
        let dim = *self.disk.dims.get(usize::from(key.dim))?;
        Some(DiskKey {
            dim,
            level: key.level,
            x: key.x,
            z: key.z,
        })
    }

    pub(super) fn persisted(&self, key: TileKey) -> bool {
        self.disk.ready
            && self.disk_budget() > 0
            && self
                .disk_key(key)
                .is_some_and(|k| self.disk.index.entries.contains_key(&k))
    }

    pub(super) fn start_disk(&mut self) {
        let backend = self.shared.backend.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let loaded = tokio::task::spawn_blocking(move || disk::load(backend.as_ref()))
                .await
                .unwrap_or_else(|_| disk::Loaded {
                    index: DiskIndex::default(),
                    orphans: Vec::new(),
                });
            let _ = tx.send(Internal::DiskLoaded(loaded));
        });
    }

    pub(super) fn on_disk_loaded(&mut self, loaded: disk::Loaded) {
        let mut index = loaded.index;
        let names: Vec<String> = self
            .shared
            .dimensions
            .iter()
            .map(|d| d.key.to_string())
            .collect();
        self.disk.dims = names.iter().map(|name| index.dim(name)).collect();
        self.disk.index = index;
        self.disk.doomed.extend(loaded.orphans);
        self.disk.ready = true;
        for (key, waiter) in std::mem::take(&mut self.disk.waits) {
            self.want(key, waiter);
        }
        if self.disk.flush_all {
            self.continue_flush();
        }
    }

    pub(super) fn touch_disk(&mut self, key: TileKey) {
        let Some(k) = self.disk_key(key) else { return };
        if self.disk.index.entries.contains_key(&k) {
            let used = self.disk.index.tick();
            if let Some(meta) = self.disk.index.entries.get_mut(&k) {
                meta.used = used;
            }
            self.disk.index_dirty = true;
        }
    }

    /// Drops the persisted copy of `key`, which no longer shows anything.
    pub(super) fn forget_disk(&mut self, key: TileKey) {
        let Some(k) = self.disk_key(key) else { return };
        if self.disk.flushing {
            self.disk.forgotten.insert(k);
        }
        if self.disk.index.entries.remove(&k).is_some() {
            let path = self.disk.index.path(k);
            self.disk.doomed.push(path);
            self.disk.index_dirty = true;
        }
    }

    /// Takes `key` out of memory; a change not yet written waits in the
    /// spill so it is not lost.
    pub(super) fn evict_entry(&mut self, key: TileKey) {
        let dirty = self.disk.dirty.contains(&key);
        let Some(entry) = self.remove(key) else {
            return;
        };
        let bytes = entry.bytes();
        if dirty
            && entry.data.pixels().is_some()
            && self.disk.spill_bytes + bytes <= SPILL_BYTES
            && self.disk_budget() > 0
        {
            self.disk.spill_bytes += bytes;
            self.disk.spill.push((key, entry));
        }
    }

    /// Puts a spilled tile back in memory when it is wanted again.
    pub(super) fn unspill(&mut self, key: TileKey) {
        if self.tiles.contains_key(&key) {
            return;
        }
        let Some(at) = self.disk.spill.iter().position(|(k, _)| *k == key) else {
            return;
        };
        let (_, mut entry) = self.disk.spill.swap_remove(at);
        self.disk.spill_bytes -= entry.bytes();
        entry.used = Instant::now();
        self.bytes += entry.bytes();
        self.tiles.insert(key, entry);
        self.disk.dirty.insert(key);
    }

    pub(super) fn start_load(&mut self, key: TileKey) {
        let (Some(k), Some(dim)) = (self.disk_key(key), self.dim_name(key.dim)) else {
            self.build(key);
            return;
        };
        let path = self.disk.index.path(k);
        let backend = self.shared.backend.clone();
        let pool = self.pool.clone();
        let tx = self.internal_tx.clone();
        self.in_flight += 1;
        tokio::spawn(async move {
            let read =
                tokio::task::spawn_blocking(move || disk::read(backend.as_ref(), &path)).await;
            let result = match read {
                Ok(Ok(Some(bytes))) => {
                    let (reply, answer) = oneshot::channel();
                    pool.spawn(move || {
                        let decoded =
                            std::panic::catch_unwind(|| disk::decode_tile(&bytes, &dim, key));
                        let _ = reply.send(decoded);
                    });
                    match answer.await {
                        Ok(Ok(Ok(persisted))) => Ok(persisted),
                        Ok(Ok(Err(corrupt))) => Err(LoadError::Corrupt(corrupt)),
                        _ => Err(LoadError::Io("decoding failed".to_owned())),
                    }
                }
                Ok(Ok(None)) => Err(LoadError::Missing),
                Ok(Err(err)) => Err(LoadError::Io(err.to_string())),
                Err(err) => Err(LoadError::Io(err.to_string())),
            };
            let _ = tx.send(Internal::Loaded { key, result });
        });
    }

    pub(super) fn on_loaded(&mut self, key: TileKey, result: Result<Persisted, LoadError>) {
        self.in_flight -= 1;
        match result {
            Ok(persisted) => {
                if !self.tiles.contains_key(&key) {
                    let (data, png) = match persisted {
                        Persisted::Base { tile, png } => (Data::Base(tile), png),
                        Persisted::Zoomed { pixels, png } => (Data::Zoomed(pixels), Some(png)),
                    };
                    self.install(key, data, png);
                    self.disk.dirty.remove(&key);
                    if let Some(entry) = self.tiles.get_mut(&key) {
                        entry.stale = true;
                    }
                    self.stats.disk_loads += 1;
                }
                self.notify(key);
                self.evict();
            }
            Err(err) => {
                if !matches!(err, LoadError::Missing) {
                    self.stats.disk_errors += 1;
                }
                let reason = match &err {
                    LoadError::Missing => "the file is missing".to_owned(),
                    LoadError::Io(message) => message.clone(),
                    LoadError::Corrupt(corrupt) => format!("{corrupt:?}"),
                };
                tuff_core::log::debug!("world map: ignoring persisted tile {key:?}: {reason}");
                self.forget_disk(key);
                if self.pending.contains_key(&key) {
                    self.build(key);
                }
            }
        }
    }

    pub(super) fn request_flush(&mut self, reply: oneshot::Sender<()>) {
        self.disk.flush_waiters.push(reply);
        self.disk.flush_all = true;
        if self.disk.ready {
            self.continue_flush();
        }
    }

    /// Runs the next round of an explicit flush, or answers its waiters
    /// when nothing more can be written now.
    fn continue_flush(&mut self) {
        if self.disk.flushing {
            return;
        }
        if !self.start_flush(true) {
            self.disk.flush_all = false;
            for waiter in self.disk.flush_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }

    /// Starts a background flush when one is due; checked at most once a
    /// second.
    pub(super) fn maybe_flush(&mut self) {
        let now = Instant::now();
        if self.disk.flushing || self.disk.flush_all || now < self.disk.next_try {
            return;
        }
        self.disk.next_try = now + Duration::from_secs(1);
        self.start_flush(false);
    }

    fn has_disk_work(&self, force: bool, budget: u64) -> bool {
        let now = Instant::now();
        !self.disk.dirty.is_empty()
            || !self.disk.spill.is_empty()
            || !self.disk.doomed.is_empty()
            || (self.disk.index_dirty
                && (force || now.saturating_duration_since(self.disk.index_at) >= INDEX_GAP))
            || (!self.disk.index.entries.is_empty() && self.disk.index.total() > budget)
    }

    /// Starts a flush of up to [`FLUSH_TILES`] changed tiles if one may
    /// run now; false if none started.
    fn start_flush(&mut self, force: bool) -> bool {
        let now = Instant::now();
        let budget = self.disk_budget();
        if !self.disk.ready
            || self.disk.flushing
            || (!force && now < self.disk.retry_at)
            || (!force && now.saturating_duration_since(self.disk.last_flush) < FLUSH_GAP)
            || !self.shared.saves().autosaves()
            || !self.has_disk_work(force, budget)
        {
            return false;
        }
        let mut jobs: Vec<(DiskKey, TileKey, String, u64, Snapshot)> = Vec::new();
        let spilled = std::mem::take(&mut self.disk.spill);
        self.disk.spill_bytes = 0;
        for (key, entry) in spilled {
            if budget == 0 {
                continue;
            }
            if jobs.len() >= FLUSH_TILES {
                self.disk.spill_bytes += entry.bytes();
                self.disk.spill.push((key, entry));
                continue;
            }
            let (Some(k), Some(dim)) = (self.disk_key(key), self.dim_name(key.dim)) else {
                continue;
            };
            if let Some(snapshot) = take_snapshot(entry) {
                let used = self.disk.index.tick();
                jobs.push((k, key, dim, used, snapshot));
            }
        }
        self.disk.dirty.retain(|key| self.tiles.contains_key(key));
        if budget > 0 {
            let mut dirty: Vec<(Instant, TileKey)> = self
                .disk
                .dirty
                .iter()
                .map(|key| (self.tiles[key].used, *key))
                .collect();
            dirty.sort_unstable_by_key(|d| std::cmp::Reverse(d.0));
            dirty.truncate(FLUSH_TILES.saturating_sub(jobs.len()));
            for (_, key) in dirty.into_iter().rev() {
                self.disk.dirty.remove(&key);
                let (Some(k), Some(dim)) = (self.disk_key(key), self.dim_name(key.dim)) else {
                    continue;
                };
                if let Some(snapshot) = snapshot(&self.tiles[&key]) {
                    let used = self.disk.index.tick();
                    jobs.push((k, key, dim, used, snapshot));
                }
            }
        } else {
            self.disk.dirty.clear();
        }
        let take = self.disk.doomed.len().min(FLUSH_DELETES);
        let doomed: Vec<String> = self.disk.doomed.drain(..take).collect();
        let writing: HashSet<String> = jobs
            .iter()
            .map(|(k, ..)| self.disk.index.path(*k))
            .collect();
        self.disk.doomed.retain(|path| !writing.contains(path));
        let doomed: Vec<String> = doomed
            .into_iter()
            .filter(|p| !writing.contains(p))
            .collect();
        let write_index = self.disk.index_dirty;
        let over = !self.disk.index.entries.is_empty() && self.disk.index.total() > budget;
        if jobs.is_empty() && doomed.is_empty() && !write_index && !over {
            return false;
        }
        self.disk.index_dirty = false;
        self.disk.flushing = true;
        self.disk.forgotten.clear();
        let keys: Vec<(DiskKey, TileKey)> = jobs.iter().map(|(k, key, ..)| (*k, *key)).collect();
        let index = self.disk.index.clone();
        let backend = self.shared.backend.clone();
        let pool = self.pool.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let (reply, answer) = oneshot::channel();
            pool.spawn(move || {
                let writes: Vec<Write> = jobs
                    .into_iter()
                    .filter_map(|(k, key, dim, used, snapshot)| {
                        let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            encode(&dim, key, snapshot)
                        }))
                        .ok()?;
                        Some(Write {
                            key: k,
                            used,
                            bytes,
                        })
                    })
                    .collect();
                let _ = reply.send(writes);
            });
            let writes = answer.await.unwrap_or_default();
            let taken = doomed.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                disk::flush(backend.as_ref(), index, budget, doomed, writes, write_index)
            })
            .await
            .unwrap_or_else(|_| Outcome {
                busy: true,
                ..Outcome::default()
            });
            let _ = tx.send(Internal::Flushed(Flushed {
                outcome,
                keys,
                doomed: taken,
            }));
        });
        true
    }

    pub(super) fn on_flushed(&mut self, flushed: Flushed) {
        let Flushed {
            outcome,
            keys,
            doomed,
        } = flushed;
        let now = Instant::now();
        self.disk.flushing = false;
        self.disk.last_flush = now;
        let deleted: HashSet<&String> = outcome.deleted.iter().collect();
        self.disk
            .doomed
            .extend(doomed.iter().filter(|p| !deleted.contains(p)).cloned());
        for key in &outcome.evicted {
            self.disk.index.entries.remove(key);
        }
        for (k, meta) in &outcome.written {
            if self.disk.forgotten.contains(k) {
                let path = self.disk.index.path(*k);
                self.disk.doomed.push(path);
                self.disk.index_dirty = true;
                continue;
            }
            let entry = self.disk.index.entries.entry(*k).or_insert(*meta);
            entry.size = meta.size;
            entry.used = entry.used.max(meta.used);
            self.stats.disk_writes += 1;
        }
        let tile_of = |k: &DiskKey| keys.iter().find(|(d, _)| d == k).map(|(_, t)| *t);
        for k in &outcome.unwritten {
            if let Some(key) = tile_of(k)
                && self
                    .tiles
                    .get(&key)
                    .is_some_and(|e| e.data.pixels().is_some())
            {
                self.disk.dirty.insert(key);
            }
        }
        match outcome.index {
            Some(seq) => {
                self.disk.index.seq = seq;
                self.disk.index_at = now;
            }
            None => {
                self.disk.index_dirty |= !outcome.deleted.is_empty()
                    || !outcome.written.is_empty()
                    || !outcome.evicted.is_empty();
            }
        }
        if !self.disk.forgotten.is_empty() {
            self.disk.index_dirty = true;
        }
        self.disk.forgotten.clear();
        if outcome.busy {
            self.disk.retry_at = now + BUSY_BACKOFF;
        }
        if self.disk.flush_all {
            let progressed = !outcome.written.is_empty()
                || !outcome.deleted.is_empty()
                || outcome.index.is_some();
            if outcome.busy || !progressed {
                self.disk.flush_all = false;
                for waiter in self.disk.flush_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            } else {
                self.continue_flush();
            }
        }
    }
}

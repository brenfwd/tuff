//! The save journal: the writes of one world save land together or not at
//! all.
//!
//! Writes are grouped into epochs. A world save opens a barrier on the
//! current epoch: its members (every player and every dirty chunk at the
//! barrier) write as that epoch, and so does every other write made before
//! the barrier is sealed; writes after the seal join the next epoch. Before
//! a key's first write in an epoch, the key's previous contents go to that
//! epoch's journal file and are synced. Once an epoch is sealed and all of
//! its writes have finished, it commits: the `committed` file records it and
//! its journal file is deleted.
//!
//! Opening a world rolls every epoch newer than the last committed one back
//! from its journal, newest first. A crash, an I/O error or an aborted save
//! therefore leaves the world as the last complete save wrote it: a player
//! is never persisted newer than the containers it traded with. A failed
//! write poisons the journal: nothing commits until the next start, which
//! restores the last complete save.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

use crate::backend::{BlobCodec, Documents, PreparedWrite, RegionStore, WorldBackend, WorldLock};
use crate::config::ConfigStore;
use crate::document::{Document, DocumentId};

const COMMITTED: &str = "committed";
const EPOCH_PREFIX: &str = "epoch-";
const EPOCH_SUFFIX: &str = ".log";
const MAX_RECORD_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    static WRITING_FOR: Cell<Option<u64>> = const { Cell::new(None) };
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BlobKind {
    Chunk,
    Entities,
    Pois,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Document(DocumentId),
    Blob {
        kind: BlobKind,
        dimension: String,
        region: FileRegionPos,
        pos: ChunkPos,
    },
}

#[derive(Default)]
struct Epoch {
    pending: usize,
    sealed: bool,
    failed: bool,
    recorded: bool,
    seen: HashSet<Key>,
}

struct State {
    open: u64,
    committed: u64,
    epochs: BTreeMap<u64, Epoch>,
    poisoned: bool,
}

/// Test hooks: what the next writes of a journal do instead of landing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// Chunk, entity and point of interest writes fail with an I/O error.
    pub fail_blob_writes: bool,
    /// Chunk, entity and point of interest writes wait (after their
    /// journal record is synced) until the hold is lifted, so a copy of
    /// the files taken meanwhile is what a crash before them would leave.
    pub hold_blob_writes: bool,
}

/// Where a journal keeps its records: synced before `append` returns.
pub trait JournalStore: Send + Sync {
    /// The newest committed epoch (0 for none).
    fn committed(&self) -> io::Result<u64>;
    fn set_committed(&self, epoch: u64) -> io::Result<()>;
    /// Epochs with records, oldest first.
    fn epochs(&self) -> io::Result<Vec<u64>>;
    fn append(&self, epoch: u64, record: &[u8]) -> io::Result<()>;
    /// Like [`Self::append`], but durable only after [`Self::sync`].
    fn append_unsynced(&self, epoch: u64, record: &[u8]) -> io::Result<()> {
        self.append(epoch, record)
    }
    /// Makes every record appended so far durable.
    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
    /// An epoch's records, concatenated in the order they were appended.
    fn records(&self, epoch: u64) -> io::Result<Vec<u8>>;
    fn remove(&self, epoch: u64) -> io::Result<()>;
}

/// A journal kept as files in a directory: `epoch-<n>.log` per epoch and
/// `committed`.
///
/// Appends are group-committed: a record is written, then one sync covers
/// every record written before it, so concurrent appenders share syncs.
pub struct DirStore {
    dir: PathBuf,
    files: Mutex<Appended>,
    synced: Mutex<u64>,
}

#[derive(Default)]
struct Appended {
    files: HashMap<u64, File>,
    /// Records written so far; each append's sequence number.
    written: u64,
}

impl DirStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            files: Mutex::default(),
            synced: Mutex::new(0),
        }
    }

    /// Syncs the epoch files until record `seq` is durable, sharing the
    /// sync with any appender that already covered it.
    fn sync_through(&self, seq: u64) -> io::Result<()> {
        let mut synced = locked(&self.synced);
        if *synced >= seq {
            return Ok(());
        }
        let (target, files) = {
            let appended = locked(&self.files);
            let files = appended
                .files
                .values()
                .map(File::try_clone)
                .collect::<io::Result<Vec<_>>>()?;
            (appended.written, files)
        };
        for file in &files {
            file.sync_data()?;
        }
        *synced = target;
        Ok(())
    }
}

impl JournalStore for DirStore {
    fn committed(&self) -> io::Result<u64> {
        read_committed(&self.dir)
    }

    fn set_committed(&self, epoch: u64) -> io::Result<()> {
        write_committed(&self.dir, epoch)
    }

    fn epochs(&self) -> io::Result<Vec<u64>> {
        epochs_in(&self.dir)
    }

    fn append(&self, epoch: u64, record: &[u8]) -> io::Result<()> {
        let seq = self.write_record(epoch, record)?;
        self.sync_through(seq)
    }

    fn append_unsynced(&self, epoch: u64, record: &[u8]) -> io::Result<()> {
        self.write_record(epoch, record).map(drop)
    }

    fn sync(&self) -> io::Result<()> {
        let written = locked(&self.files).written;
        self.sync_through(written)
    }

    fn records(&self, epoch: u64) -> io::Result<Vec<u8>> {
        fs::read(epoch_path(&self.dir, epoch))
    }

    fn remove(&self, epoch: u64) -> io::Result<()> {
        locked(&self.files).files.remove(&epoch);
        match fs::remove_file(epoch_path(&self.dir, epoch)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

impl DirStore {
    /// Writes `record` to its epoch file; its sequence number for
    /// [`Self::sync_through`].
    fn write_record(&self, epoch: u64, record: &[u8]) -> io::Result<u64> {
        let mut appended = locked(&self.files);
        let file = match appended.files.entry(epoch) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                fs::create_dir_all(&self.dir)?;
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(epoch_path(&self.dir, epoch))?;
                sync_dir(&self.dir)?;
                entry.insert(file)
            }
        };
        file.write_all(record)?;
        appended.written += 1;
        Ok(appended.written)
    }
}

/// The epochs of one world's saves.
pub struct SaveJournal {
    store: Box<dyn JournalStore>,
    state: Mutex<State>,
    faults: Mutex<Faults>,
    faults_changed: std::sync::Condvar,
}

impl SaveJournal {
    /// The epoch the next barrier covers.
    pub fn open_epoch(&self) -> u64 {
        locked(&self.state).open
    }

    /// The newest epoch whose writes all landed.
    pub fn committed(&self) -> u64 {
        locked(&self.state).committed
    }

    /// Whether a failed write stopped commits until the next start.
    pub fn poisoned(&self) -> bool {
        locked(&self.state).poisoned
    }

    pub fn set_faults(&self, faults: Faults) {
        *locked(&self.faults) = faults;
        self.faults_changed.notify_all();
    }

    fn faults(&self) -> Faults {
        *locked(&self.faults)
    }

    /// Waits out [`Faults::hold_blob_writes`].
    fn wait_for_blob_writes(&self) {
        let mut faults = locked(&self.faults);
        while faults.hold_blob_writes {
            faults = self
                .faults_changed
                .wait(faults)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The world stopped after its final save: the writes since commit
    /// with it once they have landed.
    pub fn finish(&self) {
        let open = locked(&self.state).open;
        self.seal(open);
    }

    /// Opens a barrier on the current epoch; its members take tickets
    /// before [`Barrier::seal`].
    pub fn barrier(self: &Arc<Self>) -> Barrier {
        let mut state = locked(&self.state);
        let epoch = state.open;
        state.epochs.entry(epoch).or_default();
        Barrier {
            journal: self.clone(),
            epoch,
            sealed: false,
        }
    }

    /// A ticket for `epoch`, or for the open epoch once `epoch` is sealed:
    /// a member that snapshots after the seal belongs to the next save.
    fn ticket(self: &Arc<Self>, epoch: u64) -> EpochTicket {
        let mut state = locked(&self.state);
        let sealed =
            epoch <= state.committed || state.epochs.get(&epoch).is_some_and(|entry| entry.sealed);
        let epoch = if sealed { state.open } else { epoch };
        state.epochs.entry(epoch).or_default().pending += 1;
        EpochTicket {
            journal: self.clone(),
            epoch,
            landed: false,
        }
    }

    /// A ticket for a save taken now outside a barrier (a chunk unloading,
    /// a player leaving): its epoch cannot commit before the write lands.
    pub fn ticket_now(self: &Arc<Self>) -> EpochTicket {
        let epoch = locked(&self.state).open;
        self.ticket(epoch)
    }

    fn free_ticket(self: &Arc<Self>) -> EpochTicket {
        self.ticket_now()
    }

    fn release(&self, epoch: u64) {
        let mut state = locked(&self.state);
        if let Some(entry) = state.epochs.get_mut(&epoch) {
            entry.pending = entry.pending.saturating_sub(1);
        }
        self.try_commit(&mut state);
    }

    fn seal(&self, epoch: u64) {
        let mut state = locked(&self.state);
        state.epochs.entry(epoch).or_default().sealed = true;
        if state.open <= epoch {
            state.open = epoch + 1;
        }
        self.try_commit(&mut state);
    }

    fn fail(&self, epoch: u64) {
        locked(&self.state).epochs.entry(epoch).or_default().failed = true;
    }

    /// Commits every finished epoch after the last committed one, in order.
    fn try_commit(&self, state: &mut State) {
        while !state.poisoned {
            let next = state.committed + 1;
            let Some(epoch) = state.epochs.get(&next) else {
                return;
            };
            if !epoch.sealed || epoch.pending > 0 {
                return;
            }
            if epoch.failed {
                state.poisoned = true;
                log::error!(
                    "a world save failed: nothing more is committed until a restart, \
                     which restores the last complete save"
                );
                return;
            }
            let recorded = epoch.recorded;
            if let Err(err) = self.store.set_committed(next) {
                state.poisoned = true;
                log::error!("cannot commit world save {next}: {err}");
                return;
            }
            state.epochs.remove(&next);
            state.committed = next;
            if recorded && let Err(err) = self.store.remove(next) {
                log::warn!("cannot remove the journal of world save {next}: {err}");
            }
        }
    }

    /// Whether `key` still needs its previous contents journaled in
    /// `epoch`.
    fn needs_image(&self, epoch: u64, key: &Key) -> bool {
        locked(&self.state)
            .epochs
            .get(&epoch)
            .is_none_or(|entry| !entry.seen.contains(key))
    }

    /// Journals `key`'s previous contents without holding the state lock
    /// over the I/O, so tickets and other keys never wait on a sync. Two
    /// racing first writes of one key both journal it; the earlier record
    /// holds the older contents and rollback applies it last.
    fn append(&self, epoch: u64, key: &Key, image: Option<&[u8]>) -> io::Result<()> {
        if !self.needs_image(epoch, key) {
            return Ok(());
        }
        let record = encode_record(key, image);
        locked(&self.state)
            .epochs
            .entry(epoch)
            .or_default()
            .recorded = true;
        self.store.append(epoch, &record)?;
        locked(&self.state)
            .epochs
            .entry(epoch)
            .or_default()
            .seen
            .insert(key.clone());
        Ok(())
    }

    /// Journals the previous contents of every key in `images` that still
    /// needs it, with one sync for all of them. The keys count as journaled
    /// only once the sync returned, so a racing writer of one of them
    /// journals it itself.
    fn prepare(&self, images: Vec<(u64, Key, Option<Vec<u8>>)>) -> io::Result<()> {
        let mut recorded = Vec::with_capacity(images.len());
        for (epoch, key, image) in images {
            if !self.needs_image(epoch, &key) {
                continue;
            }
            locked(&self.state)
                .epochs
                .entry(epoch)
                .or_default()
                .recorded = true;
            self.store
                .append_unsynced(epoch, &encode_record(&key, image.as_deref()))?;
            recorded.push((epoch, key));
        }
        if recorded.is_empty() {
            return Ok(());
        }
        self.store.sync()?;
        let mut state = locked(&self.state);
        for (epoch, key) in recorded {
            state.epochs.entry(epoch).or_default().seen.insert(key);
        }
        Ok(())
    }

    /// Runs `write` for `key`, journaling what `before` reads first when
    /// this is the key's first write in its epoch.
    fn write<T>(
        self: &Arc<Self>,
        key: Key,
        before: impl FnOnce() -> io::Result<Option<Vec<u8>>>,
        write: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let (epoch, free) = match WRITING_FOR.get() {
            Some(epoch) => (epoch, None),
            None => {
                let ticket = self.free_ticket();
                (ticket.epoch, Some(ticket))
            }
        };
        let result = self.journaled_write(epoch, &key, before, write);
        if result.is_err() {
            self.fail(epoch);
        }
        if let Some(free) = free {
            free.landed();
        }
        result
    }

    fn journaled_write<T>(
        &self,
        epoch: u64,
        key: &Key,
        before: impl FnOnce() -> io::Result<Option<Vec<u8>>>,
        write: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        if self.needs_image(epoch, key) {
            let image = before()?;
            self.append(epoch, key, image.as_deref())?;
        }
        write()
    }
}

/// A world save's hold on its epoch: sealing it lets the epoch commit once
/// every member's write has landed.
pub struct Barrier {
    journal: Arc<SaveJournal>,
    epoch: u64,
    sealed: bool,
}

impl Barrier {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// A member's right to write as this barrier's epoch.
    pub fn ticket(&self) -> EpochTicket {
        self.journal.ticket(self.epoch)
    }

    /// A handle members use to take tickets, for handing to other actors.
    pub fn handle(&self) -> BarrierHandle {
        BarrierHandle {
            journal: self.journal.clone(),
            epoch: self.epoch,
        }
    }

    /// Every member has its ticket: later writes join the next epoch, and
    /// this one commits once its tickets are gone.
    pub fn seal(mut self) {
        self.sealed = true;
        self.journal.seal(self.epoch);
    }

    /// Not every member snapshotted in time, so the snapshot may not be
    /// consistent: the epoch stays open and the next barrier completes it
    /// (its members rewrite everything that changed since).
    pub fn defer(mut self) {
        self.sealed = true;
    }
}

impl Drop for Barrier {
    fn drop(&mut self) {
        if !self.sealed {
            self.journal.fail(self.epoch);
            self.journal.seal(self.epoch);
        }
    }
}

/// Hands out tickets for one barrier's epoch.
#[derive(Clone)]
pub struct BarrierHandle {
    journal: Arc<SaveJournal>,
    epoch: u64,
}

impl BarrierHandle {
    pub fn ticket(&self) -> EpochTicket {
        self.journal.ticket(self.epoch)
    }
}

impl std::fmt::Debug for BarrierHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BarrierHandle").field(&self.epoch).finish()
    }
}

/// Writes made inside [`EpochTicket::scope`] belong to the ticket's epoch;
/// the epoch cannot commit while a ticket for it lives. A ticket dropped
/// before [`EpochTicket::landed`] lost its write: the epoch never commits.
pub struct EpochTicket {
    journal: Arc<SaveJournal>,
    epoch: u64,
    landed: bool,
}

impl EpochTicket {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn scope<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<u64>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WRITING_FOR.set(self.0);
            }
        }
        let _restore = Restore(WRITING_FOR.replace(Some(self.epoch)));
        f()
    }

    /// The member could not write its part: the epoch must not commit.
    pub fn fail(&self) {
        self.journal.fail(self.epoch);
    }

    /// The member's writes are done (failures already reported).
    pub fn landed(mut self) {
        self.landed = true;
    }
}

impl Drop for EpochTicket {
    fn drop(&mut self) {
        if !self.landed {
            self.journal.fail(self.epoch);
        }
        self.journal.release(self.epoch);
    }
}

impl std::fmt::Debug for EpochTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EpochTicket").field(&self.epoch).finish()
    }
}

fn epoch_path(dir: &Path, epoch: u64) -> PathBuf {
    dir.join(format!("{EPOCH_PREFIX}{epoch}{EPOCH_SUFFIX}"))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn write_committed(dir: &Path, epoch: u64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!("{COMMITTED}.tmp"));
    let mut file = File::create(&temp)?;
    file.write_all(epoch.to_string().as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp, dir.join(COMMITTED))?;
    sync_dir(dir)
}

fn read_committed(dir: &Path) -> io::Result<u64> {
    match fs::read_to_string(dir.join(COMMITTED)) {
        Ok(text) => text
            .trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad committed save number")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err),
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn encode_record(key: &Key, image: Option<&[u8]>) -> Vec<u8> {
    let mut payload = Vec::new();
    match key {
        Key::Document(id) => {
            payload.push(0);
            match id {
                DocumentId::Level => payload.push(0),
                DocumentId::SavedData(name) => {
                    payload.push(1);
                    put_str(&mut payload, name);
                }
                DocumentId::DimensionData { dimension, id } => {
                    payload.push(2);
                    put_str(&mut payload, dimension);
                    put_str(&mut payload, id);
                }
                DocumentId::Player(uuid) => {
                    payload.push(3);
                    payload.extend_from_slice(&uuid.to_le_bytes());
                }
                DocumentId::Stats(uuid) => {
                    payload.push(4);
                    payload.extend_from_slice(&uuid.to_le_bytes());
                }
                DocumentId::Advancements(uuid) => {
                    payload.push(5);
                    payload.extend_from_slice(&uuid.to_le_bytes());
                }
                DocumentId::PlayerData { id, uuid } => {
                    payload.push(6);
                    put_str(&mut payload, id);
                    payload.extend_from_slice(&uuid.to_le_bytes());
                }
            }
        }
        Key::Blob {
            kind,
            dimension,
            region,
            pos,
        } => {
            payload.push(match kind {
                BlobKind::Chunk => 1,
                BlobKind::Entities => 2,
                BlobKind::Pois => 3,
            });
            put_str(&mut payload, dimension);
            for value in [region.x, region.z, pos.x, pos.z] {
                payload.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    match image {
        Some(bytes) => {
            payload.push(1);
            payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            payload.extend_from_slice(bytes);
        }
        None => payload.push(0),
    }
    let mut record = Vec::with_capacity(payload.len() + 12);
    record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    record.extend_from_slice(&payload);
    record.extend_from_slice(&checksum(&payload).to_le_bytes());
    record
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

fn decode_payload(payload: &[u8]) -> Option<(Key, Option<Vec<u8>>)> {
    let mut reader = Reader(payload);
    let key = match reader.byte()? {
        0 => Key::Document(match reader.byte()? {
            0 => DocumentId::Level,
            1 => DocumentId::SavedData(reader.string()?),
            2 => DocumentId::DimensionData {
                dimension: reader.string()?,
                id: reader.string()?,
            },
            3 => DocumentId::Player(reader.u128()?),
            4 => DocumentId::Stats(reader.u128()?),
            5 => DocumentId::Advancements(reader.u128()?),
            6 => DocumentId::PlayerData {
                id: reader.string()?,
                uuid: reader.u128()?,
            },
            _ => return None,
        }),
        kind @ 1..=3 => Key::Blob {
            kind: match kind {
                1 => BlobKind::Chunk,
                2 => BlobKind::Entities,
                _ => BlobKind::Pois,
            },
            dimension: reader.string()?,
            region: FileRegionPos::new(reader.i32()?, reader.i32()?),
            pos: ChunkPos::new(reader.i32()?, reader.i32()?),
        },
        _ => return None,
    };
    let image = match reader.byte()? {
        0 => None,
        _ => {
            let len = reader.u32()? as usize;
            Some(reader.take(len)?.to_vec())
        }
    };
    Some((key, image))
}

/// The complete records of one epoch; a torn or damaged tail (the process
/// died while appending) ends them, and its write never happened.
fn parse_records(bytes: &[u8]) -> Vec<(Key, Option<Vec<u8>>)> {
    let mut reader = Reader(bytes);
    let mut records = Vec::new();
    while let Some(len) = reader.u32() {
        let len = len as usize;
        if len > MAX_RECORD_BYTES {
            break;
        }
        let Some(payload) = reader.take(len) else {
            break;
        };
        let Some(sum) = reader.take(8) else {
            break;
        };
        if u64::from_le_bytes(sum.try_into().expect("8 bytes")) != checksum(payload) {
            break;
        }
        match decode_payload(payload) {
            Some(record) => records.push(record),
            None => break,
        }
    }
    records
}

fn epochs_in(dir: &Path) -> io::Result<Vec<u64>> {
    let mut epochs = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(epochs),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let name = entry?.file_name();
        if let Some(epoch) = name
            .to_str()
            .and_then(|n| n.strip_prefix(EPOCH_PREFIX))
            .and_then(|n| n.strip_suffix(EPOCH_SUFFIX))
            .and_then(|n| n.parse().ok())
        {
            epochs.push(epoch);
        }
    }
    epochs.sort_unstable();
    Ok(epochs)
}

fn restore_document(
    inner: &dyn WorldBackend,
    id: &DocumentId,
    image: Option<Vec<u8>>,
) -> io::Result<()> {
    let documents = inner.documents();
    match image {
        Some(bytes) => {
            let (named, _) =
                NamedTag::from_bytes(&bytes, NbtLimits::TRUSTED).map_err(io::Error::other)?;
            let Tag::Compound(root) = named.tag else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a journaled document is not a compound",
                ));
            };
            documents.write(id.borrow(), root)
        }
        None => documents.remove(id.borrow()),
    }
}

/// Rolls back every epoch newer than the last committed one, newest first,
/// so the world is what the last complete save left. Answers how many
/// epochs were rolled back.
pub fn recover(inner: &dyn WorldBackend, store: &dyn JournalStore) -> io::Result<usize> {
    let committed = store.committed()?;
    let epochs = store.epochs()?;
    let newest = epochs.last().copied().unwrap_or(0).max(committed);
    let rolled: Vec<u64> = epochs
        .iter()
        .rev()
        .copied()
        .filter(|e| *e > committed)
        .collect();
    let mut stores: HashMap<(String, FileRegionPos), Box<dyn RegionStore>> = HashMap::new();
    for epoch in &rolled {
        let records = parse_records(&store.records(*epoch)?);
        for (key, image) in records.into_iter().rev() {
            match key {
                Key::Document(id) => restore_document(inner, &id, image)?,
                Key::Blob {
                    kind,
                    dimension,
                    region,
                    pos,
                } => {
                    let store = match stores.entry((dimension.clone(), region)) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(inner.open_region(&dimension, region)?)
                        }
                    };
                    match (kind, image) {
                        (BlobKind::Chunk, Some(blob)) => store.write_chunk(pos, &blob)?,
                        (BlobKind::Chunk, None) => store.remove_chunk(pos)?,
                        (BlobKind::Entities, blob) => store.write_entities(pos, blob.as_deref())?,
                        (BlobKind::Pois, blob) => store.write_pois(pos, blob.as_deref())?,
                    }
                }
            }
        }
    }
    for store in stores.values_mut() {
        store.flush()?;
    }
    drop(stores);
    for epoch in epochs.iter().rev() {
        store.remove(*epoch)?;
    }
    if newest > committed {
        store.set_committed(newest)?;
    }
    if !rolled.is_empty() {
        log::warn!(
            "the last world save did not finish: restored the world to the last complete save \
             (rolled back {} unfinished save epoch{})",
            rolled.len(),
            if rolled.len() == 1 { "" } else { "s" }
        );
    }
    Ok(rolled.len())
}

/// A backend whose document and chunk writes go through a [`SaveJournal`].
pub struct JournaledBackend {
    inner: Arc<dyn WorldBackend>,
    journal: Arc<SaveJournal>,
    documents: JournaledDocuments,
}

impl JournaledBackend {
    /// Recovers the journal in `store` onto `inner` (which must be
    /// locked), then journals every later write there.
    pub fn open(inner: Arc<dyn WorldBackend>, store: Box<dyn JournalStore>) -> io::Result<Self> {
        recover(&*inner, &*store)?;
        let committed = store.committed()?;
        let journal = Arc::new(SaveJournal {
            store,
            state: Mutex::new(State {
                open: committed + 1,
                committed,
                epochs: BTreeMap::new(),
                poisoned: false,
            }),
            faults: Mutex::default(),
            faults_changed: std::sync::Condvar::new(),
        });
        Ok(Self {
            documents: JournaledDocuments {
                inner: inner.clone(),
                journal: journal.clone(),
            },
            inner,
            journal,
        })
    }

    pub fn journal(&self) -> &Arc<SaveJournal> {
        &self.journal
    }
}

struct JournaledDocuments {
    inner: Arc<dyn WorldBackend>,
    journal: Arc<SaveJournal>,
}

fn document_image(
    documents: &dyn Documents,
    document: Document<'_>,
) -> io::Result<Option<Vec<u8>>> {
    documents
        .read(document)?
        .map(|root| {
            NamedTag::new("", root)
                .to_bytes(Compression::None)
                .map_err(io::Error::other)
        })
        .transpose()
}

impl Documents for JournaledDocuments {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        self.inner.documents().read(document)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        let documents = self.inner.documents();
        self.journal.write(
            Key::Document(document.into()),
            || document_image(documents, document),
            || documents.write(document, root),
        )
    }

    fn remove(&self, document: Document<'_>) -> io::Result<()> {
        let documents = self.inner.documents();
        self.journal.write(
            Key::Document(document.into()),
            || document_image(documents, document),
            || documents.remove(document),
        )
    }

    fn exists(&self, document: Document<'_>) -> bool {
        self.inner.documents().exists(document)
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        self.inner.documents().list()
    }
}

struct JournaledRegion {
    inner: Box<dyn RegionStore>,
    journal: Arc<SaveJournal>,
    dimension: String,
    region: FileRegionPos,
}

impl JournaledRegion {
    fn key(&self, kind: BlobKind, pos: ChunkPos) -> Key {
        Key::Blob {
            kind,
            dimension: self.dimension.clone(),
            region: self.region,
            pos,
        }
    }

    fn blob_write(
        &mut self,
        kind: BlobKind,
        pos: ChunkPos,
        write: impl FnOnce(&mut dyn RegionStore) -> io::Result<()>,
    ) -> io::Result<()> {
        let key = self.key(kind, pos);
        let faults = self.journal.faults();
        let journal = self.journal.clone();
        let store = std::cell::RefCell::new(&mut self.inner);
        journal.write(
            key,
            || {
                let mut store = store.borrow_mut();
                match kind {
                    BlobKind::Chunk => store.read_chunk(pos),
                    BlobKind::Entities => store.read_entities(pos),
                    BlobKind::Pois => store.read_pois(pos),
                }
            },
            || {
                if faults.fail_blob_writes {
                    return Err(io::Error::other("injected chunk write failure"));
                }
                journal.wait_for_blob_writes();
                let mut store = store.borrow_mut();
                write(&mut ***store)
            },
        )
    }
}

impl RegionStore for JournaledRegion {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_chunk(pos)
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        self.blob_write(BlobKind::Chunk, pos, |store| store.write_chunk(pos, blob))
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        self.blob_write(BlobKind::Chunk, pos, |store| store.remove_chunk(pos))
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_entities(pos)
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.blob_write(BlobKind::Entities, pos, |store| {
            store.write_entities(pos, blob)
        })
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_pois(pos)
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.blob_write(BlobKind::Pois, pos, |store| store.write_pois(pos, blob))
    }

    fn prepare_writes(&mut self, writes: &[PreparedWrite]) -> io::Result<()> {
        let mut images = Vec::new();
        for write in writes {
            let kinds = [
                (write.chunk, BlobKind::Chunk),
                (write.entities, BlobKind::Entities),
                (write.pois, BlobKind::Pois),
            ];
            for (_, kind) in kinds.into_iter().filter(|(wanted, _)| *wanted) {
                let key = self.key(kind, write.pos);
                if !self.journal.needs_image(write.epoch, &key) {
                    continue;
                }
                let image = match kind {
                    BlobKind::Chunk => self.inner.read_chunk(write.pos),
                    BlobKind::Entities => self.inner.read_entities(write.pos),
                    BlobKind::Pois => self.inner.read_pois(write.pos),
                }?;
                images.push((write.epoch, key, image));
            }
        }
        self.journal.prepare(images)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        self.inner.list_chunks()
    }
}

impl WorldBackend for JournaledBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        self.inner.lock()
    }

    fn upgrade(&self) -> io::Result<()> {
        self.inner.upgrade()
    }

    fn set_entity_index(&self, enabled: bool) {
        self.inner.set_entity_index(enabled);
    }

    fn sql_console(&self) -> Option<crate::sqlite::console::SqlConsole> {
        self.inner.sql_console()
    }

    fn documents(&self) -> &dyn Documents {
        &self.documents
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        Ok(Box::new(JournaledRegion {
            inner: self.inner.open_region(dimension, region)?,
            journal: self.journal.clone(),
            dimension: dimension.to_owned(),
            region,
        }))
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.inner.blob_codec()
    }

    fn config(&self) -> Option<Arc<dyn ConfigStore>> {
        self.inner.config()
    }

    fn backup_source(&self) -> crate::backup::BackupSource {
        self.inner.backup_source()
    }

    fn save_journal(&self) -> Option<Arc<SaveJournal>> {
        Some(self.journal.clone())
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.inner.list_dimensions()
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        self.inner.list_regions(dimension)
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        self.inner.list_extra_files()
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file(path)
    }

    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        self.inner.list_extra_files_under(prefix)
    }

    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file_capped(path, max)
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        self.inner.write_extra_file(path, data)
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        self.inner.remove_extra_file(path)
    }
}

#[cfg(test)]
mod tests;

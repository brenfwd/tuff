//! The server file: one SQLite database holding a whole server, the world
//! and its configuration, so a server is one portable file.
//!
//! One dedicated thread (the database actor) owns the only connection.
//! [`Documents`], [`RegionStore`] and [`ConfigStore`] calls arrive from many
//! blocking-pool threads at once; each becomes a job on a bounded channel
//! and the caller blocks on that job's reply channel. Funnelling everything
//! through one connection is what SQLite wants anyway (a writer excludes
//! every other writer), and it makes the order of transactions obvious.
//!
//! The request queue is bounded at [`REQUEST_QUEUE`]: a caller always
//! blocks for its reply, so at most one request per calling thread is ever
//! outstanding and the queue only has to absorb a burst of simultaneous
//! callers. A caller that finds it full waits on `send` instead of on the
//! reply, which is the same wait from its point of view, and the bound keeps
//! the memory held by queued chunk blobs proportional to the number of
//! threads rather than to the amount of pending work. The actor never sends
//! into the queue, so it cannot deadlock on it.
//!
//! Format ([`schema`]): `PRAGMA application_id` is [`APPLICATION_ID`] and
//! `user_version` the format version ([`FORMAT_VERSION`]); files with
//! another id are refused untouched, and older layouts are migrated in
//! place. Tables:
//! - `documents(kind, key, data)`: level (`level`, key `""`), saved data
//!   (`saved_data`, key = resource location), per-dimension data
//!   (`dimension_data`, key = `"<dimension> <id>"`, separated by a space,
//!   which resource locations never contain), player records (`player`),
//!   statistics (`stats`) and advancement progress (`advancements`), all
//!   keyed by hyphenated UUID. `data` is
//!   uncompressed NBT with an unnamed root; reads also accept gzip or zlib
//!   bytes through NBT header detection.
//! - `chunks`, `entities` and `pois` `(dimension, x, z, data)`:
//!   [`NbtBlobCodec`] blobs, a compression id byte followed by
//!   zlib-compressed NBT, the same bytes a vanilla region sector holds after
//!   its length prefix.
//! - `properties`, `ops`, `whitelist`, `banned_players`, `banned_ips`: the
//!   server configuration ([`config`]).
//! - `sqlar`: files kept byte for byte (the server icon), in SQLite's
//!   archive format, so `sqlite3 <file> -A -x` extracts them.
//!
//! The file is locked against a second server as [`Locking`] describes,
//! and write-ahead logging is on; closing the database (dropping the
//! [`WorldLock`] or the backend) checkpoints the log back into the file,
//! so a cleanly stopped server leaves no `-wal` or `-shm` behind.

mod config;
pub mod console;
pub mod inspect;
mod reader;
pub use reader::Importer;
pub mod schema;
mod snapshot;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, RwLock};
use std::thread;

use rusqlite::{Connection, OptionalExtension, params};

use crate::backend::{BlobCodec, Documents, RegionStore, WorldBackend, WorldLock};
use crate::blobs::NbtBlobCodec;
use crate::config::ConfigStore;
use crate::document::{Document, DocumentId};
use crate::files::MAX_FILE_NBT_BYTES;
use crate::region_file::ChunkCompression;
use crate::sqlar;
use crate::vanilla::player_file_stem;
pub use config::SqliteConfig;
pub use schema::{APPLICATION_ID, FORMAT_VERSION, Locking, lock_path};
pub use snapshot::LOCK_BYTES;
use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

pub const NAME: &str = "sqlite";
/// The server file's extension.
pub const EXTENSION: &str = "mcserver";
const REQUEST_QUEUE: usize = 32;
const THREAD_NAME: &str = "sqlite-server";
/// `sqlar` name prefix for files a conversion carries through unmodified
/// (e.g. a vanilla world's data packs or POI files), keeping them out of
/// the way of the server icon's own entry.
const EXTRA_PREFIX: &str = "extra/";
/// Largest single extra file read back from `sqlar`.
const MAX_EXTRA_FILE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Chunks,
    Entities,
    Pois,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Self::Chunks => "chunks",
            Self::Entities => "entities",
            Self::Pois => "pois",
        }
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

enum Request {
    Job(Job),
    /// Checkpoints, closes the connection and stops the thread; the reply
    /// comes once the file is released.
    Close(SyncSender<()>),
}

/// The caller's end of the database actor.
#[derive(Clone)]
pub struct Db {
    live: Arc<RwLock<Live>>,
}

struct Live {
    requests: SyncSender<Request>,
    snapshots: Arc<snapshot::Snapshots>,
}

fn closed() -> io::Error {
    io::Error::other("the server file is closed")
}

impl Db {
    fn start(file: &Path, locking: Locking, read_only: bool) -> io::Result<Live> {
        let (guard, conn) = if read_only {
            (None, schema::open_read_only(file)?)
        } else {
            let guard = match locking {
                Locking::Exclusive => None,
                Locking::Normal => schema::lock_file(file, true)?,
            };
            (guard, schema::open(file, locking)?)
        };
        let (requests, receiver) = mpsc::sync_channel(REQUEST_QUEUE);
        let snapshots = Arc::new(snapshot::Snapshots::default());
        let serving = snapshots.clone();
        thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || serve(conn, receiver, guard, serving))?;
        Ok(Live {
            requests,
            snapshots,
        })
    }

    fn snapshots(&self) -> Arc<snapshot::Snapshots> {
        self.live
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .snapshots
            .clone()
    }

    fn is_closed(&self) -> bool {
        self.snapshots().is_closing()
    }

    /// Starts a fresh connection and thread if the file was closed.
    fn reopen(&self, file: &Path, locking: Locking, read_only: bool) -> io::Result<()> {
        let mut live = self.live.write().unwrap_or_else(|e| e.into_inner());
        if live.snapshots.is_closing() {
            *live = Self::start(file, locking, read_only)?;
        }
        Ok(())
    }

    fn run_io<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let (reply, response) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(job(conn));
        });
        let requests = self
            .live
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .clone();
        requests.send(Request::Job(job)).map_err(|_| closed())?;
        response.recv().map_err(|_| closed())?
    }

    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        self.run_io(move |conn| job(conn).map_err(io::Error::other))
    }

    /// Returns once the database is closed; a no-op if it already is.
    fn close(&self) {
        let (requests, snapshots) = {
            let live = self.live.read().unwrap_or_else(|e| e.into_inner());
            (live.requests.clone(), live.snapshots.clone())
        };
        snapshots.closing();
        let (reply, done) = mpsc::sync_channel(1);
        if requests.send(Request::Close(reply)).is_ok() {
            let _ = done.recv();
        }
    }
}

static IDLE_HOOK: std::sync::OnceLock<(std::time::Duration, fn())> = std::sync::OnceLock::new();

/// Installs a function the database thread runs whenever it wakes, and
/// after `poll` of idleness; the first call wins.
pub fn set_idle_hook(poll: std::time::Duration, hook: fn()) {
    let _ = IDLE_HOOK.set((poll, hook));
}

fn idle_poll() -> std::time::Duration {
    IDLE_HOOK
        .get()
        .map_or(std::time::Duration::from_secs(5), |(poll, _)| *poll)
}

fn run_idle_hook() {
    if let Some((_, hook)) = IDLE_HOOK.get() {
        hook();
    }
}

/// `guard` is the lock file of [`Locking::Normal`], released only after
/// the connection has closed.
fn serve(
    mut conn: Connection,
    requests: Receiver<Request>,
    guard: Option<File>,
    snapshots: Arc<snapshot::Snapshots>,
) {
    let mut done = None;
    loop {
        let request = match requests.recv_timeout(idle_poll()) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => {
                run_idle_hook();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        run_idle_hook();
        match request {
            Request::Job(job) => job(&mut conn),
            Request::Close(reply) => {
                done = Some(reply);
                break;
            }
        }
    }
    snapshots.wait_thawed();
    if let Err(err) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
        log::error!("failed to checkpoint the server file: {err}");
    }
    if let Err((_, err)) = conn.close() {
        log::error!("failed to close the server file: {err}");
    }
    drop(guard);
    drop(requests);
    if let Some(done) = done {
        let _ = done.send(());
    }
}

/// Held while a world runs from the file; dropping it closes the database.
struct ServerFileLock {
    db: Db,
    locked: Arc<AtomicBool>,
}

impl WorldLock for ServerFileLock {}

impl Drop for ServerFileLock {
    fn drop(&mut self) {
        self.db.close();
        self.locked.store(false, Ordering::SeqCst);
    }
}

/// The file name SQLite reads as a private in-memory database.
pub const IN_MEMORY: &str = ":memory:";

pub struct SqliteBackend {
    file: PathBuf,
    db: Db,
    config: Arc<SqliteConfig>,
    codec: Arc<NbtBlobCodec>,
    locking: Locking,
    locked: Arc<AtomicBool>,
    entity_index: Arc<AtomicBool>,
    keeps_config: bool,
    read_only: bool,
}

impl SqliteBackend {
    /// Opens or creates the server file at `file` with the default
    /// [`Locking`].
    pub fn open(file: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_with(file, Locking::default())
    }

    /// Opens or creates the server file at `file`, migrating an older
    /// layout, and starts its thread. Fails if another server has it.
    pub fn open_with(file: impl Into<PathBuf>, locking: Locking) -> io::Result<Self> {
        Self::start(file.into(), locking, false)
    }

    /// Opens an existing server file without ever writing to it; writes
    /// through this backend fail. The file must already be in the current
    /// format.
    pub fn open_read_only(file: impl Into<PathBuf>) -> io::Result<Self> {
        Self::start(file.into(), Locking::Exclusive, true)
    }

    fn start(file: PathBuf, locking: Locking, read_only: bool) -> io::Result<Self> {
        if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        if file.is_dir() {
            return Err(io::Error::other(format!(
                "{} is a directory, not a server file",
                file.display()
            )));
        }
        let db = Db {
            live: Arc::new(RwLock::new(Db::start(&file, locking, read_only)?)),
        };
        Ok(Self {
            file,
            config: Arc::new(SqliteConfig { db: db.clone() }),
            db,
            codec: Arc::new(NbtBlobCodec {
                compression: Some(ChunkCompression::Zlib),
            }),
            locking,
            locked: Arc::new(AtomicBool::new(false)),
            entity_index: Arc::new(AtomicBool::new(false)),
            keeps_config: true,
            read_only,
        })
    }

    /// Leaves the server configuration to the instance directory.
    pub fn without_config(mut self) -> Self {
        self.keeps_config = false;
        self
    }

    /// A server file that lives only in memory: nothing touches disk and
    /// everything is gone when the backend is dropped or closed.
    pub fn open_in_memory() -> io::Result<Self> {
        Self::open_with(IN_MEMORY, Locking::Exclusive)
    }

    pub fn is_in_memory(&self) -> bool {
        self.file == Path::new(IN_MEMORY)
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Checkpoints the log into the file and closes it. Later calls on this
    /// backend, its region stores and its config fail until `lock` reopens it.
    pub fn close(&self) {
        self.db.close();
    }
}

impl Drop for SqliteBackend {
    fn drop(&mut self) {
        self.db.close();
    }
}

pub(crate) fn document_key(document: Document<'_>) -> (&'static str, String) {
    match document {
        Document::Level => ("level", String::new()),
        Document::SavedData(id) => ("saved_data", id.into()),
        Document::DimensionData { dimension, id } => {
            ("dimension_data", format!("{dimension} {id}"))
        }
        Document::Player(uuid) => ("player", player_file_stem(uuid)),
        Document::Stats(uuid) => ("stats", player_file_stem(uuid)),
        Document::Advancements(uuid) => ("advancements", player_file_stem(uuid)),
        Document::PlayerData { id, uuid } => {
            ("player_data", format!("{id} {}", player_file_stem(uuid)))
        }
    }
}

/// The inverse of [`document_key`], for listing what is stored.
fn parse_document(kind: &str, key: &str) -> Option<DocumentId> {
    use crate::admin::parse_uuid_text;
    Some(match kind {
        "level" => DocumentId::Level,
        "saved_data" => DocumentId::SavedData(key.to_owned()),
        "dimension_data" => {
            let (dimension, id) = key.split_once(' ')?;
            DocumentId::DimensionData {
                dimension: dimension.to_owned(),
                id: id.to_owned(),
            }
        }
        "player" => DocumentId::Player(parse_uuid_text(key)?),
        "stats" => DocumentId::Stats(parse_uuid_text(key)?),
        "advancements" => DocumentId::Advancements(parse_uuid_text(key)?),
        "player_data" => {
            let (id, uuid) = key.split_once(' ')?;
            DocumentId::PlayerData {
                id: id.to_owned(),
                uuid: parse_uuid_text(uuid)?,
            }
        }
        _ => return None,
    })
}

impl Documents for SqliteBackend {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        let (kind, key) = document_key(document);
        let data: Option<Vec<u8>> = self.db.run(move |conn| {
            conn.prepare_cached("SELECT data FROM documents WHERE kind = ?1 AND key = ?2")?
                .query_row(params![kind, key], |row| row.get(0))
                .optional()
        })?;
        let Some(data) = data else {
            return Ok(None);
        };
        let limits = NbtLimits {
            max_bytes: MAX_FILE_NBT_BYTES,
            ..NbtLimits::TRUSTED
        };
        let (named, _) = NamedTag::from_bytes(&data, limits).map_err(io::Error::other)?;
        match named.tag {
            Tag::Compound(mut root) => {
                crate::datafix::document(document, &mut root)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(root))
            }
            _ => Err(io::Error::other(format!(
                "{document:?} does not contain a compound"
            ))),
        }
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        let (kind, key) = document_key(document);
        let data = NamedTag::new("", root)
            .to_bytes(Compression::None)
            .map_err(io::Error::other)?;
        self.db.run(move |conn| {
            conn.prepare_cached(
                "INSERT INTO documents(kind, key, data) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(kind, key) DO UPDATE SET data = excluded.data",
            )?
            .execute(params![kind, key, data])
            .map(drop)
        })
    }

    fn exists(&self, document: Document<'_>) -> bool {
        let (kind, key) = document_key(document);
        self.db
            .run(move |conn| {
                conn.prepare_cached("SELECT 1 FROM documents WHERE kind = ?1 AND key = ?2")?
                    .exists(params![kind, key])
            })
            .unwrap_or(false)
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        let rows: Vec<(String, String)> = self.db.run(|conn| {
            conn.prepare_cached("SELECT kind, key FROM documents")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(kind, key)| parse_document(&kind, &key))
            .collect())
    }
}

impl WorldBackend for SqliteBackend {
    fn name(&self) -> &str {
        NAME
    }

    /// The file is locked from [`open_with`](Self::open_with) on; this hands
    /// out the guard whose drop closes it, once.
    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        if self.locked.swap(true, Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is already in use by this server", self.file.display()),
            ));
        }
        if self.db.is_closed()
            && let Err(err) = self.db.reopen(&self.file, self.locking, self.read_only)
        {
            self.locked.store(false, Ordering::SeqCst);
            return Err(err);
        }
        Ok(Box::new(ServerFileLock {
            db: self.db.clone(),
            locked: self.locked.clone(),
        }))
    }

    fn sql_console(&self) -> Option<console::SqlConsole> {
        (!self.read_only).then(|| console::SqlConsole {
            db: Some(self.db.clone()),
            source: if self.is_in_memory() {
                reader::Source::Shared
            } else if self.locking == Locking::Normal {
                reader::Source::live(self.file.clone())
            } else {
                reader::Source::snapshot(self.db.clone(), self.file.clone())
            },
        })
    }

    fn set_entity_index(&self, enabled: bool) {
        self.entity_index.store(enabled, Ordering::Relaxed);
    }

    fn documents(&self) -> &dyn Documents {
        self
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        Ok(Box::new(SqliteRegionStore {
            db: self.db.clone(),
            dimension: dimension.into(),
            region,
            entity_index: self.entity_index.clone(),
        }))
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.codec.clone()
    }

    fn backup_source(&self) -> crate::backup::BackupSource {
        if self.is_in_memory() {
            return crate::backup::BackupSource::Unsupported;
        }
        crate::backup::BackupSource::File(Arc::new(snapshot::ServerFileSnapshot {
            db: self.db.clone(),
            file: self.file.clone(),
        }))
    }

    fn config(&self) -> Option<Arc<dyn ConfigStore>> {
        if !self.keeps_config {
            return None;
        }
        Some(self.config.clone())
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.db.run(|conn| {
            let mut dims: Vec<String> = conn
                .prepare_cached(
                    "SELECT dimension FROM chunks UNION SELECT dimension FROM entities",
                )?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            dims.sort();
            Ok(dims)
        })
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        let dimension = dimension.to_owned();
        self.db.run(move |conn| {
            let rows: Vec<(i32, i32)> = conn
                .prepare_cached(
                    "SELECT x, z FROM chunks WHERE dimension = ?1 \
                     UNION SELECT x, z FROM entities WHERE dimension = ?1",
                )?
                .query_map(params![dimension], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut regions: Vec<FileRegionPos> = rows
                .into_iter()
                .map(|(x, z)| FileRegionPos::new(x >> 5, z >> 5))
                .collect();
            regions.sort();
            regions.dedup();
            Ok(regions)
        })
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        self.db.run(|conn| {
            let mut names: Vec<String> = conn
                .prepare_cached("SELECT name FROM sqlar WHERE substr(name, 1, ?1) = ?2")?
                .query_map(params![EXTRA_PREFIX.len() as i64, EXTRA_PREFIX], |row| {
                    row.get(0)
                })?
                .collect::<rusqlite::Result<_>>()?;
            names.sort();
            Ok(names
                .into_iter()
                .filter_map(|name| name.strip_prefix(EXTRA_PREFIX).map(str::to_owned))
                .collect())
        })
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        let name = format!("{EXTRA_PREFIX}{path}");
        self.db
            .run_io(move |conn| sqlar::read(conn, &name, MAX_EXTRA_FILE_BYTES))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))
    }

    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        let prefix = format!("{EXTRA_PREFIX}{prefix}");
        self.db.run(move |conn| {
            let mut names: Vec<String> = conn
                .prepare_cached(
                    "SELECT name FROM sqlar WHERE substr(name, 1, ?1) = ?2 AND data IS NOT NULL",
                )?
                .query_map(params![prefix.len() as i64, prefix], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            names.sort();
            Ok(names
                .into_iter()
                .filter_map(|name| name.strip_prefix(EXTRA_PREFIX).map(str::to_owned))
                .collect())
        })
    }

    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let name = format!("{EXTRA_PREFIX}{path}");
        let max = max.min(MAX_EXTRA_FILE_BYTES);
        self.db
            .run_io(move |conn| sqlar::read(conn, &name, max))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let name = format!("{EXTRA_PREFIX}{path}");
        let size = data.len();
        let encoded = sqlar::encode(data);
        self.db
            .run(move |conn| sqlar::write_encoded(conn, &name, size, &encoded))
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        let name = format!("{EXTRA_PREFIX}{path}");
        self.db.run(move |conn| sqlar::remove(conn, &name))
    }
}

/// Chunks are keyed by position, so a region store is just the dimension
/// name plus a handle on the database thread.
pub struct SqliteRegionStore {
    db: Db,
    dimension: Arc<str>,
    region: FileRegionPos,
    entity_index: Arc<AtomicBool>,
}

impl SqliteRegionStore {
    fn read(&self, table: Table, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let dimension = self.dimension.clone();
        self.db.run(move |conn| {
            conn.prepare_cached(&format!(
                "SELECT data FROM {} WHERE dimension = ?1 AND x = ?2 AND z = ?3",
                table.name()
            ))?
            .query_row(params![&*dimension, pos.x, pos.z], |row| row.get(0))
            .optional()
        })
    }

    fn write(&self, table: Table, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        let dimension = self.dimension.clone();
        let data = blob.map(<[u8]>::to_vec);
        let index = table == Table::Entities && self.entity_index.load(Ordering::Relaxed);
        self.db.run(move |conn| {
            match &data {
                Some(data) => conn
                    .prepare_cached(&format!(
                        "INSERT INTO {}(dimension, x, z, data) VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(dimension, x, z) DO UPDATE SET data = excluded.data",
                        table.name()
                    ))?
                    .execute(params![&*dimension, pos.x, pos.z, data])?,
                None => conn
                    .prepare_cached(&format!(
                        "DELETE FROM {} WHERE dimension = ?1 AND x = ?2 AND z = ?3",
                        table.name()
                    ))?
                    .execute(params![&*dimension, pos.x, pos.z])?,
            };
            if index
                && let Err(err) = tuff_sqlite_nbt::update_entity_index(
                    conn,
                    &dimension,
                    pos.x,
                    pos.z,
                    data.as_deref(),
                )
            {
                log::warn!("cannot update the entity index: {err}");
            }
            Ok(())
        })
    }
}

impl RegionStore for SqliteRegionStore {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.read(Table::Chunks, pos)
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        self.write(Table::Chunks, pos, Some(blob))
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        self.write(Table::Chunks, pos, None)
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.read(Table::Entities, pos)
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.write(Table::Entities, pos, blob)
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.read(Table::Pois, pos)
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.write(Table::Pois, pos, blob)
    }

    /// `synchronous = NORMAL` syncs the log only at checkpoints, so this is
    /// one. Committed transactions survive a process crash regardless.
    fn flush(&mut self) -> io::Result<()> {
        self.db
            .run(|conn| conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(())))
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        let dimension = self.dimension.clone();
        let (x0, z0) = (self.region.x * 32, self.region.z * 32);
        let (x1, z1) = (x0 + 31, z0 + 31);
        self.db.run(move |conn| {
            let mut positions = Vec::new();
            for table in [Table::Chunks, Table::Entities, Table::Pois] {
                let rows: Vec<(i32, i32)> = conn
                    .prepare_cached(&format!(
                        "SELECT x, z FROM {} WHERE dimension = ?1 \
                         AND x BETWEEN ?2 AND ?3 AND z BETWEEN ?4 AND ?5",
                        table.name()
                    ))?
                    .query_map(params![&*dimension, x0, x1, z0, z1], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                positions.extend(rows.into_iter().map(|(x, z)| ChunkPos::new(x, z)));
            }
            positions.sort();
            positions.dedup();
            Ok(positions)
        })
    }
}

#[cfg(test)]
mod tests;

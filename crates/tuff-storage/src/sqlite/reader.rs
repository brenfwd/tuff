//! Read-only connections for the SQL console, so a slow query never
//! stalls the file's own thread. With normal locking a reader opens the
//! live file (WAL gives it a consistent view while the server keeps
//! writing); with exclusive locking nothing else can open the file, so a
//! reader opens a recent snapshot copy instead.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, Rows};

use super::console::{Cancel, Cell, Column, ConsoleError, Guard, Limits, Mode, QueryOutput};
use super::snapshot::{LOCK_BYTES, ServerFileSnapshot};
use super::{Db, schema};
use crate::backup::FileSnapshot;

/// How long a snapshot copy answers new readers before a fresh one is cut.
const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

/// How long an idle cursor keeps its view of the file open.
const CURSOR_IDLE: Duration = Duration::from_secs(120);

const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type SnapCache = Arc<Mutex<Option<(Instant, Arc<SnapFile>)>>>;

#[derive(Clone)]
pub(super) enum Source {
    /// The live file, opened read-only next to the server's connection.
    Live(PathBuf),
    /// Snapshot copies of a file the server holds exclusively.
    Snapshot {
        db: Db,
        file: PathBuf,
        cache: SnapCache,
    },
    /// No separate connection is possible (in memory).
    Shared,
    /// A region-file world copied on demand into a temporary file.
    Import { build: Importer, cache: SnapCache },
}

/// Fills an empty server file from some other storage layout.
pub type Importer = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

impl Source {
    pub(super) fn live(file: PathBuf) -> Self {
        Self::Live(file)
    }

    pub(super) fn snapshot(db: Db, file: PathBuf) -> Self {
        Self::Snapshot {
            db,
            file,
            cache: Arc::default(),
        }
    }

    pub(super) fn import(build: Importer) -> Self {
        Self::Import {
            build,
            cache: Arc::default(),
        }
    }

    /// Forgets the cached snapshot, after a console write.
    pub(super) fn invalidate(&self) {
        if let Self::Snapshot { cache, .. } | Self::Import { cache, .. } = self {
            cache.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }

    pub(super) fn open(&self) -> io::Result<Option<Reader>> {
        match self {
            Self::Shared => Ok(None),
            Self::Live(file) => {
                let conn = Connection::open_with_flags(
                    file,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(sql)?;
                conn.busy_timeout(READ_BUSY_TIMEOUT).map_err(sql)?;
                tuff_sqlite_nbt::register(&conn).map_err(sql)?;
                Ok(Some(Reader { conn, _snap: None }))
            }
            Self::Import { build, cache } => {
                let snap = {
                    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                    match cache.as_ref() {
                        Some((at, snap)) if at.elapsed() < SNAPSHOT_TTL => snap.clone(),
                        _ => {
                            let snap = Arc::new(SnapFile::import(build)?);
                            *cache = Some((Instant::now(), snap.clone()));
                            snap
                        }
                    }
                };
                let conn = Connection::open_with_flags(
                    &snap.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(sql)?;
                tuff_sqlite_nbt::register(&conn).map_err(sql)?;
                Ok(Some(Reader {
                    conn,
                    _snap: Some(snap),
                }))
            }
            Self::Snapshot { db, file, cache } => {
                let snap = {
                    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                    match cache.as_ref() {
                        Some((at, snap)) if at.elapsed() < SNAPSHOT_TTL => snap.clone(),
                        _ => {
                            let snap = Arc::new(SnapFile::cut(db, file)?);
                            *cache = Some((Instant::now(), snap.clone()));
                            snap
                        }
                    }
                };
                let conn = schema::open_read_only(&snap.path)?;
                tuff_sqlite_nbt::register(&conn).map_err(sql)?;
                Ok(Some(Reader {
                    conn,
                    _snap: Some(snap),
                }))
            }
        }
    }
}

fn sql(err: rusqlite::Error) -> io::Error {
    io::Error::other(err.to_string())
}

/// A temporary copy of the server file, deleted when the last reader of
/// it is gone.
pub(super) struct SnapFile {
    path: PathBuf,
}

impl SnapFile {
    fn import(build: &Importer) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tuff-console-import-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let snap = Self { path };
        build(&snap.path)?;
        Ok(snap)
    }

    fn cut(db: &Db, file: &Path) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut name = file.as_os_str().to_owned();
        name.push(format!(
            ".console-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let path = PathBuf::from(name);
        let frozen = ServerFileSnapshot {
            db: db.clone(),
            file: file.to_path_buf(),
        }
        .freeze()?;
        let snap = Self { path };
        copy_skipping_locks(frozen.path(), &snap.path)?;
        Ok(snap)
    }
}

impl Drop for SnapFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn copy_skipping_locks(from: &Path, to: &Path) -> io::Result<()> {
    let mut input = File::open(from)?;
    let mut output = File::create(to)?;
    let len = input.metadata()?.len();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    while pos < len {
        if LOCK_BYTES.contains(&pos) {
            pos = LOCK_BYTES.end.min(len);
            input.seek(SeekFrom::Start(pos))?;
            output.seek(SeekFrom::Start(pos))?;
            continue;
        }
        let mut want = (buf.len() as u64).min(len - pos);
        if pos < LOCK_BYTES.start {
            want = want.min(LOCK_BYTES.start - pos);
        }
        let chunk = &mut buf[..want as usize];
        input.read_exact(chunk)?;
        output.write_all(chunk)?;
        pos += want;
    }
    output.set_len(len)
}

/// A read-only connection; the snapshot it reads (if any) outlives it.
pub(super) struct Reader {
    pub(super) conn: Connection,
    _snap: Option<Arc<SnapFile>>,
}

struct Fetch {
    limit: usize,
    limits: Limits,
    reply: mpsc::SyncSender<Result<QueryOutput, ConsoleError>>,
}

/// One statement kept open on its own thread, which hands out its rows a
/// page at a time. The view of the file it reads stays consistent until
/// the cursor is dropped or sits idle for two minutes; meanwhile the
/// write-ahead log cannot be checkpointed past it.
pub struct Cursor {
    fetches: mpsc::Sender<Fetch>,
}

impl Cursor {
    pub(super) fn start(reader: Reader, text: String, limits: Limits, cancel: Cancel) -> Self {
        let (fetches, requests) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sql-cursor".into())
            .spawn(move || serve(reader, &text, limits, cancel, requests));
        if let Err(err) = spawned {
            log::warn!("cannot start a SQL cursor thread: {err}");
        }
        Self { fetches }
    }

    /// The next page, blocking until the cursor thread has read it.
    pub fn fetch(&self, limit: usize, limits: Limits) -> Result<QueryOutput, ConsoleError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.fetches
            .send(Fetch {
                limit,
                limits,
                reply,
            })
            .map_err(|_| ConsoleError::Closed)?;
        response.recv().unwrap_or(Err(ConsoleError::Closed))
    }
}

fn serve(
    reader: Reader,
    text: &str,
    limits: Limits,
    cancel: Cancel,
    requests: mpsc::Receiver<Fetch>,
) {
    let outcome = open_statement(&reader.conn, text, limits, cancel);
    let (guard, mut stmt) = match outcome {
        Ok(opened) => opened,
        Err(err) => {
            while let Ok(fetch) = requests.recv_timeout(CURSOR_IDLE) {
                let _ = fetch.reply.send(Err(err.clone()));
            }
            return;
        }
    };
    let columns: Vec<Column> = stmt
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_owned(),
            decl_type: c.decl_type().map(str::to_owned),
        })
        .collect();
    let mut rows = match stmt.query([]) {
        Ok(rows) => rows,
        Err(err) => {
            let err = guard.explain(err);
            while let Ok(fetch) = requests.recv_timeout(CURSOR_IDLE) {
                let _ = fetch.reply.send(Err(err.clone()));
            }
            return;
        }
    };
    let mut state = Paging::default();
    while let Ok(fetch) = requests.recv_timeout(CURSOR_IDLE) {
        guard.rearm(fetch.limits.timeout);
        let page = state.page(&mut rows, &columns, fetch.limit, fetch.limits, &guard);
        let _ = fetch.reply.send(page);
    }
}

fn open_statement<'c>(
    conn: &'c Connection,
    text: &str,
    limits: Limits,
    cancel: Cancel,
) -> Result<(Guard<'c>, rusqlite::Statement<'c>), ConsoleError> {
    let guard = Guard::install(conn, Mode::Read, limits, cancel)?;
    let stmt = guard.prepare(text)?;
    if !stmt.readonly() {
        return Err(ConsoleError::Sql("this connection is read-only".into()));
    }
    Ok((guard, stmt))
}

#[derive(Default)]
struct Paging {
    ahead: Option<Vec<Cell>>,
    done: bool,
}

impl Paging {
    fn step(
        &mut self,
        rows: &mut Rows<'_>,
        width: usize,
        guard: &Guard<'_>,
    ) -> Result<Option<Vec<Cell>>, ConsoleError> {
        if let Some(row) = self.ahead.take() {
            return Ok(Some(row));
        }
        if self.done {
            return Ok(None);
        }
        let next = rows.next().map_err(|err| {
            self.done = true;
            guard.explain(err)
        })?;
        let Some(row) = next else {
            self.done = true;
            return Ok(None);
        };
        (0..width)
            .map(|i| row.get_ref(i).map(Cell::of))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map(Some)
            .map_err(|err| ConsoleError::Sql(err.to_string()))
    }

    fn page(
        &mut self,
        rows: &mut Rows<'_>,
        columns: &[Column],
        limit: usize,
        limits: Limits,
        guard: &Guard<'_>,
    ) -> Result<QueryOutput, ConsoleError> {
        let limit = limit.clamp(1, limits.rows.max(1));
        let mut out = QueryOutput {
            columns: columns.to_vec(),
            rows: Vec::new(),
            has_more: false,
            truncated: false,
        };
        let mut bytes = 0;
        while out.rows.len() < limit {
            let Some(cells) = self.step(rows, columns.len(), guard)? else {
                break;
            };
            bytes += cells.iter().map(Cell::weight).sum::<usize>();
            out.rows.push(cells);
            if bytes >= limits.bytes {
                out.truncated = true;
                break;
            }
        }
        if self.ahead.is_none() {
            self.ahead = self.step(rows, columns.len(), guard)?;
        }
        out.has_more = self.ahead.is_some();
        out.truncated &= out.has_more;
        Ok(out)
    }
}

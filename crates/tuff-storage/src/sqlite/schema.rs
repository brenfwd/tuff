//! Opening a server file: identifying it, creating or migrating its tables,
//! and taking the lock.
//!
//! The format version is `PRAGMA user_version`; files at older versions are
//! upgraded by [`MIGRATIONS`] on open, newer ones are refused.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, TransactionBehavior};

use super::config;
use crate::migrate::{self, Step};
use crate::sqlar;

/// `MCSF` ("Minecraft server file").
pub const APPLICATION_ID: i32 = 0x4D43_5346;
pub const FORMAT_VERSION: i32 = 3;
/// Upgrades from each older format, in order. None yet.
const MIGRATIONS: &[Step<Connection>] = &[];
const LOCK_SUFFIX: &str = ".lock";
/// How long a write waits on a program reading the file in normal locking.
const NORMAL_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const WORLD_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS documents(
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY(kind, key)
);
CREATE TABLE IF NOT EXISTS chunks(
    dimension TEXT NOT NULL,
    x INTEGER NOT NULL,
    z INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY(dimension, x, z)
);
CREATE TABLE IF NOT EXISTS entities(
    dimension TEXT NOT NULL,
    x INTEGER NOT NULL,
    z INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY(dimension, x, z)
);
CREATE TABLE IF NOT EXISTS pois(
    dimension TEXT NOT NULL,
    x INTEGER NOT NULL,
    z INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY(dimension, x, z)
);
";

/// A fresh file holding only the world tables, for filling from another
/// storage layout.
pub fn create_import_file(file: &Path) -> io::Result<Connection> {
    let conn = Connection::open(file).map_err(|err| io::Error::other(err.to_string()))?;
    conn.execute_batch(WORLD_SCHEMA)
        .map_err(|err| io::Error::other(err.to_string()))?;
    Ok(conn)
}

#[derive(Debug, PartialEq)]
enum Found {
    Empty,
    Version(i32),
}

pub(super) fn sql_error(file: &Path, err: rusqlite::Error) -> io::Error {
    match err.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{} is already in use by another server", file.display()),
        ),
        Some(ErrorCode::NotADatabase) => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a Minecraft server file: {err}", file.display()),
        ),
        _ => io::Error::other(format!("{}: {err}", file.display())),
    }
}

fn refuse(file: &Path, why: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} is not a Minecraft server file: {why}", file.display()),
    )
}

/// What the file is, read without writing anything, so a file that is
/// refused is left untouched.
fn identify(conn: &Connection, file: &Path) -> io::Result<Found> {
    let sql = |err| sql_error(file, err);
    let application_id: i32 = conn
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(sql)?;
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql)?;
    if application_id == APPLICATION_ID {
        return match version {
            1.. if version <= FORMAT_VERSION => Ok(Found::Version(version)),
            newer if newer > FORMAT_VERSION => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} uses server file format {newer}; this server supports up to {FORMAT_VERSION}",
                    file.display()
                ),
            )),
            other => Err(refuse(file, format!("unknown format version {other}"))),
        };
    }
    if application_id != 0 {
        return Err(refuse(
            file,
            format!(
                "its application id is {application_id:#010x}, expected {APPLICATION_ID:#010x}"
            ),
        ));
    }
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(sql)?;
    if tables.is_empty() && version == 0 {
        return Ok(Found::Empty);
    }
    Err(refuse(file, "it has no application id".into()))
}

fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// How the file is kept from being opened by a second server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Locking {
    /// SQLite's `locking_mode = EXCLUSIVE`: the database file itself stays
    /// locked from the first access until the connection closes, and WAL
    /// keeps its index in process memory (no `-shm` file, fewer lock
    /// calls). No other program can read the file while the server runs,
    /// so the SQL console queries a snapshot copy of it.
    Exclusive,
    /// SQLite's normal WAL locking, so other programs (the `sqlite3` shell,
    /// database browsers) and the SQL console can read the file while the
    /// server runs, on their own read-only connections. A second server is
    /// kept out by an OS lock on `<file>.lock` instead: a lock on the
    /// database file itself conflicts with SQLite's own locks on macOS and
    /// Windows. The lock file stays behind after shutdown. The file must be
    /// on a local disk (WAL needs shared memory), and a long read holds
    /// back checkpoints, so the log grows until it ends.
    #[default]
    Normal,
}

impl Locking {
    pub const ALL: [Self; 2] = [Self::Exclusive, Self::Normal];

    pub fn name(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::Normal => "normal",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|locking| locking.name() == name)
    }
}

/// Takes the OS lock on `<file>.lock`, which dies with the process.
pub(super) fn lock_file(file: &Path, create: bool) -> io::Result<Option<File>> {
    let path = sibling(file, LOCK_SUFFIX);
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(&path)
    {
        Ok(lock) => lock,
        Err(err) if !create && err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    lock.try_lock().map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{} is already in use by another server", file.display()),
        )
    })?;
    Ok(Some(lock))
}

/// Opens `file` (creating it if missing) and brings it to
/// [`FORMAT_VERSION`]. With [`Locking::Exclusive`] the connection holds the
/// file locked until it closes; with [`Locking::Normal`] the caller must
/// already hold [`lock_file`].
/// Opens an existing current-format `file` read-only.
pub(super) fn open_read_only(file: &Path) -> io::Result<Connection> {
    let mut wal = file.as_os_str().to_owned();
    wal.push("-wal");
    if Path::new(&wal).exists() {
        return Err(io::Error::other(format!(
            "{} was not closed cleanly; start once without --no-save",
            file.display()
        )));
    }
    let uri = url_path(file);
    let conn = Connection::open_with_flags(
        format!("file:{uri}?immutable=1"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|err| io::Error::other(format!("cannot open {}: {err}", file.display())))?;
    if identify(&conn, file)? != Found::Version(FORMAT_VERSION) {
        return Err(io::Error::other(format!(
            "{} needs upgrading, which writes to it; start once without --no-save",
            file.display()
        )));
    }
    Ok(conn)
}

pub(super) fn open(file: &Path, locking: Locking) -> io::Result<Connection> {
    let sql = |err| sql_error(file, err);
    let mut conn = Connection::open(file)
        .map_err(|err| io::Error::other(format!("cannot open {}: {err}", file.display())))?;
    conn.busy_timeout(Duration::ZERO).map_err(sql)?;
    if locking == Locking::Exclusive {
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(sql)?;
    }
    let found = identify(&conn, file)?;
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(sql)?;
    let in_memory = file == Path::new(super::IN_MEMORY);
    if !mode.eq_ignore_ascii_case("wal") && !(in_memory && mode.eq_ignore_ascii_case("memory")) {
        return Err(io::Error::other(format!(
            "{} cannot use write-ahead logging (journal mode {mode})",
            file.display()
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(sql)?;
    match found {
        Found::Empty => {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Exclusive)
                .map_err(sql)?;
            create(&tx).map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        Found::Version(version) => upgrade(file, version, &mut conn)?,
    }
    tuff_sqlite_nbt::register(&conn).map_err(sql)?;
    if locking == Locking::Normal {
        conn.busy_timeout(NORMAL_BUSY_TIMEOUT).map_err(sql)?;
    }

    Ok(conn)
}

fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(WORLD_SCHEMA)?;
    conn.execute_batch(config::SCHEMA)?;
    conn.execute_batch(sqlar::SCHEMA)?;
    conn.pragma_update(None, "application_id", APPLICATION_ID)?;
    conn.pragma_update(None, "user_version", FORMAT_VERSION)
}

/// Runs [`MIGRATIONS`] from `version` in one exclusive transaction, which
/// also takes the write lock an exclusive-locking connection then keeps.
fn upgrade(file: &Path, version: i32, conn: &mut Connection) -> io::Result<()> {
    upgrade_with(file, version, conn, MIGRATIONS)
}

fn upgrade_with(
    file: &Path,
    version: i32,
    conn: &mut Connection,
    steps: &[Step<Connection>],
) -> io::Result<()> {
    let sql = |err| sql_error(file, err);
    conn.execute_batch("BEGIN EXCLUSIVE").map_err(sql)?;
    let result = migrate::run(
        "server file",
        version as u32,
        FORMAT_VERSION as u32,
        steps,
        conn,
    )
    .map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {err}", file.display()),
        )
    })
    .and_then(|ran| {
        if ran {
            conn.pragma_update(None, "user_version", FORMAT_VERSION)
                .map_err(sql)
        } else {
            Ok(())
        }
    });
    match result {
        Ok(()) => conn.execute_batch("COMMIT").map_err(sql),
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

pub fn lock_path(file: &Path) -> PathBuf {
    sibling(file, LOCK_SUFFIX)
}

/// `path` with the characters a SQLite URI reserves percent-escaped.
fn url_path(path: &Path) -> String {
    let mut out = String::new();
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'%' | b'?' | b'#' | 0x80.. => out.push_str(&format!("%{byte:02X}")),
            _ => out.push(byte as char),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn upgrades_run_their_steps_and_stamp_the_current_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        let steps: Vec<Step<Connection>> = (1..FORMAT_VERSION as u32)
            .map(|from| Step {
                from,
                apply: |conn: &mut Connection| {
                    conn.execute_batch(
                        "CREATE TABLE IF NOT EXISTS seen(x); INSERT INTO seen VALUES (1)",
                    )
                    .map_err(|err| err.to_string())
                },
            })
            .collect();
        upgrade_with(Path::new("t"), 1, &mut conn, &steps).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, FORMAT_VERSION);
        let seen: i64 = conn
            .query_row("SELECT count(*) FROM seen", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, i64::from(FORMAT_VERSION - 1));
        assert!(MIGRATIONS.is_empty());
        upgrade(Path::new("t"), FORMAT_VERSION, &mut conn).unwrap();
    }
}

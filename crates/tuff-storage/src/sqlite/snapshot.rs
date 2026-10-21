//! Freezing the server file for a backup copy. In WAL mode the main file
//! only changes when a checkpoint copies the log back into it, so after a
//! full checkpoint with automatic checkpoints off the file on disk is a
//! complete, unchanging snapshot while every write keeps landing in the
//! log. The database actor keeps serving meanwhile; closing waits until
//! the copy has stopped (it stops early once closing begins).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::{APPLICATION_ID, Db, EXTENSION};
use crate::backup::{FileSnapshot, FrozenFile};

/// Longest a close waits for a copy that ignores its interruption.
const CLOSE_WAIT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Snapshots {
    frozen: Mutex<usize>,
    thawed: Condvar,
    closing: AtomicBool,
}

impl Snapshots {
    pub(super) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    pub(super) fn closing(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Blocks the database actor's close until no copy is running.
    pub(super) fn wait_thawed(&self) {
        let frozen = self.frozen.lock().unwrap_or_else(|e| e.into_inner());
        let (frozen, timeout) = self
            .thawed
            .wait_timeout_while(frozen, CLOSE_WAIT, |count| *count > 0)
            .unwrap_or_else(|e| e.into_inner());
        if timeout.timed_out() && *frozen > 0 {
            log::warn!("closing the server file while a backup still copies it");
        }
    }

    fn adjust(&self, by: isize) {
        let mut frozen = self.frozen.lock().unwrap_or_else(|e| e.into_inner());
        *frozen = frozen.saturating_add_signed(by);
        self.thawed.notify_all();
    }
}

pub(super) struct ServerFileSnapshot {
    pub(super) db: Db,
    pub(super) file: PathBuf,
}

struct Frozen {
    db: Db,
    snapshots: Arc<Snapshots>,
    file: PathBuf,
    autocheckpoint: i64,
}

impl FrozenFile for Frozen {
    fn path(&self) -> &Path {
        &self.file
    }

    fn interrupted(&self) -> bool {
        self.snapshots.closing.load(Ordering::SeqCst)
    }

    fn skip(&self) -> Option<std::ops::Range<u64>> {
        Some(LOCK_BYTES)
    }
}

/// The bytes SQLite locks (pending, reserved and shared, from 1 GiB on).
/// It never stores data in them, and on Windows the server's own locks
/// make them unreadable through a second handle.
pub const LOCK_BYTES: std::ops::Range<u64> = 0x4000_0000..0x4000_0200;

impl Drop for Frozen {
    fn drop(&mut self) {
        self.snapshots.adjust(-1);
        let pages = self.autocheckpoint;
        let _ = self.db.run(move |conn| {
            conn.query_row(&format!("PRAGMA wal_autocheckpoint = {pages}"), [], |_| {
                Ok(())
            })
        });
    }
}

impl FileSnapshot for ServerFileSnapshot {
    fn extension(&self) -> &'static str {
        EXTENSION
    }

    fn freeze(&self) -> std::io::Result<Box<dyn FrozenFile>> {
        let snapshots = self.db.snapshots();
        let autocheckpoint = self.db.run_io(move |conn| {
            let sql = std::io::Error::other;
            let pages: i64 = conn
                .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
                .map_err(sql)?;
            let (busy, log, copied): (i64, i64, i64) = conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(sql)?;
            if busy != 0 || log != copied {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ResourceBusy,
                    "another connection is reading the server file; try again",
                ));
            }
            conn.query_row("PRAGMA wal_autocheckpoint = 0", [], |_| Ok(()))
                .map_err(sql)?;
            snapshots.adjust(1);
            Ok(pages)
        })?;
        Ok(Box::new(Frozen {
            db: self.db.clone(),
            snapshots: self.db.snapshots(),
            file: self.file.clone(),
            autocheckpoint,
        }))
    }

    fn restorable(&self) -> Option<&Path> {
        Some(&self.file)
    }

    fn exclude(&self, copy: &Path, prefixes: &[String]) -> std::io::Result<()> {
        let sql = std::io::Error::other;
        let conn = Connection::open_with_flags(
            copy,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql)?;
        for prefix in prefixes {
            let name = format!("{}{prefix}", super::EXTRA_PREFIX);
            conn.execute(
                "DELETE FROM sqlar WHERE substr(name, 1, ?1) = ?2",
                rusqlite::params![name.len() as i64, name],
            )
            .map_err(sql)?;
        }
        conn.execute_batch("VACUUM").map_err(sql)?;
        conn.close().map_err(|(_, err)| sql(err))
    }

    fn verify(&self, copy: &Path) -> std::io::Result<()> {
        let sql =
            |err: rusqlite::Error| std::io::Error::other(format!("the copy is unreadable: {err}"));
        let conn = Connection::open_with_flags(
            copy,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql)?;
        let id: i32 = conn
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(sql)?;
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(sql)?;
        conn.close().map_err(|(_, err)| sql(err))?;
        if id != APPLICATION_ID || check != "ok" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("the copy failed its check: {check}"),
            ));
        }
        Ok(())
    }
}

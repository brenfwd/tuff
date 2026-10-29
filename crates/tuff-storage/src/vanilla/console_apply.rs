//! Writes SQL console edits back to region files, all or nothing.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use tuff_world_model::pos::{ChunkPos, FileRegionPos};

use super::{VanillaBackend, WorldPaths};
use crate::backend::WorldBackend;

pub(super) fn apply_console_rows(
    root: &Path,
    tx: &rusqlite::Transaction<'_>,
    keys: &[crate::sqlite::console::ChunkKey],
) -> io::Result<crate::sqlite::Finish> {
    let paths = WorldPaths::new(root);
    let mut files = Vec::new();
    for key in keys {
        let (rx, rz) = (key.x >> 5, key.z >> 5);
        for dir in [
            paths.region_dir(&key.dimension),
            paths.entities_dir(&key.dimension),
            paths.poi_dir(&key.dimension),
        ] {
            let file = dir.join(crate::region_file::region_file_name(rx, rz));
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    let mut backup = Backup::take(files)?;
    if let Err(err) = write_rows(root, tx, keys) {
        return Err(backup.restore_after(err));
    }
    Ok(Box::new(move |committed| {
        if committed {
            backup.discard();
        }
    }))
}

fn write_rows(
    root: &Path,
    tx: &rusqlite::Transaction<'_>,
    keys: &[crate::sqlite::console::ChunkKey],
) -> io::Result<()> {
    use rusqlite::OptionalExtension;
    let sql = |err: rusqlite::Error| io::Error::other(err.to_string());
    let backend = VanillaBackend::new(root);
    for (index, key) in keys.iter().enumerate() {
        #[cfg(test)]
        if FAIL_BEFORE_KEY.with(|f| f.get()) == Some(index) {
            return Err(io::Error::other("injected failure"));
        }
        #[cfg(not(test))]
        let _ = index;
        let row = |table: &str| -> io::Result<Option<Vec<u8>>> {
            tx.query_row(
                &format!("SELECT data FROM {table} WHERE dimension = ?1 AND x = ?2 AND z = ?3"),
                rusqlite::params![key.dimension, key.x, key.z],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)
        };
        let (chunk, entities, pois) = (row("chunks")?, row("entities")?, row("pois")?);
        let pos = ChunkPos::new(key.x, key.z);
        let mut store =
            backend.open_region(&key.dimension, FileRegionPos::new(key.x >> 5, key.z >> 5))?;
        match chunk {
            Some(blob) => store.write_chunk(pos, &blob)?,
            None => store.remove_chunk(pos)?,
        }
        store.write_entities(pos, entities.as_deref())?;
        store.write_pois(pos, pois.as_deref())?;
        store.flush()?;
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_BEFORE_KEY: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    pub(super) static BACKUP_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Copies of the region files a console commit may rewrite, so a failure
/// partway through leaves the world exactly as it was. Dropping it armed
/// (error, panic) restores the files.
struct Backup {
    dir: PathBuf,
    /// Each file's backup copy, or `None` when it did not exist yet.
    files: Vec<(PathBuf, Option<PathBuf>)>,
    armed: bool,
}

impl Backup {
    fn take(targets: Vec<PathBuf>) -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        #[cfg(test)]
        let base = BACKUP_ROOT
            .with(|r| r.borrow().clone())
            .unwrap_or_else(std::env::temp_dir);
        #[cfg(not(test))]
        let base = std::env::temp_dir();
        let dir = base.join(format!(
            "tuff-console-backup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir)?;
        let mut backup = Self {
            dir,
            files: Vec::new(),
            armed: false,
        };
        let mut result = Ok(());
        for (index, file) in targets.into_iter().enumerate() {
            let copy = backup.dir.join(index.to_string());
            match fs::metadata(&file) {
                Ok(_) => {
                    result = fs::copy(&file, &copy)
                        .and_then(|_| File::open(&copy)?.sync_all())
                        .inspect(|_| backup.files.push((file, Some(copy))));
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    backup.files.push((file, None));
                }
                Err(err) => result = Err(err),
            }
            if result.is_err() {
                break;
            }
        }
        match result {
            Ok(()) => {
                backup.armed = true;
                Ok(backup)
            }
            Err(err) => {
                backup.remove_dir();
                Err(err)
            }
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first = None;
        for (file, copy) in &self.files {
            let result = match copy {
                Some(copy) => {
                    let staged = file.with_extension("mca.restore");
                    fs::copy(copy, &staged).and_then(|_| fs::rename(&staged, file))
                }
                None => match fs::remove_file(file) {
                    Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
                    _ => Ok(()),
                },
            };
            if let Err(err) = result {
                first.get_or_insert(err);
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn restore_after(&mut self, original: io::Error) -> io::Error {
        self.armed = false;
        match self.restore() {
            Ok(()) => {
                self.remove_dir();
                original
            }
            Err(err) => io::Error::other(format!(
                "{original}; restoring the region files also failed: {err} (backup kept in {})",
                self.dir.display()
            )),
        }
    }

    fn discard(&mut self) {
        self.armed = false;
        self.remove_dir();
    }

    fn remove_dir(&self) {
        if let Err(err) = fs::remove_dir_all(&self.dir) {
            log::warn!(
                "could not remove console backup {}: {err}",
                self.dir.display()
            );
        }
    }
}

impl Drop for Backup {
    fn drop(&mut self) {
        if self.armed {
            self.armed = false;
            match self.restore() {
                Ok(()) => self.remove_dir(),
                Err(err) => log::error!(
                    "restoring region files failed: {err} (backup kept in {})",
                    self.dir.display()
                ),
            }
        }
    }
}

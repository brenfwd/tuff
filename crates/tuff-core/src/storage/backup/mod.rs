//! Backup jobs and settings. The blocking steps live in
//! `tuff_storage::backup`; [`jobs`] runs them under a save hold of a
//! running world.

pub mod jobs;

#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

use crate::config::{Key, Value};
use crate::storage::BackendOptions;
use crate::storage::Storage;
use tuff_storage::backup::restore;
use tuff_storage::sqlite;
use tuff_storage::vanilla;

pub const DIRECTORY_KEY: &str = "tuff.backup.directory";
pub const KEEP_KEY: &str = "tuff.backup.keep";
pub const MAX_AGE_KEY: &str = "tuff.backup.maxAgeDays";
pub const INTERVAL_KEY: &str = "tuff.backup.interval";
pub const STAGED_KEY: &str = "tuff.backup.staged";
pub const COMPRESSION_KEY: &str = "tuff.backup.compression";

pub fn config_keys() -> Vec<Key> {
    let at_least_zero = |v: &Value| match v {
        Value::Int(n) if *n >= 0 => Ok(()),
        _ => Err("must be 0 or more".to_owned()),
    };
    vec![
        Key::new(
            DIRECTORY_KEY,
            Value::String(String::new()),
            "Folder backups are written to (empty: backups/ in the instance directory)",
        ),
        Key::new(
            KEEP_KEY,
            Value::Int(10),
            "Backups kept after a new one succeeds, oldest deleted first (0 keeps all)",
        )
        .validated(at_least_zero),
        Key::new(
            MAX_AGE_KEY,
            Value::Int(0),
            "Delete backups older than this many days after a new one succeeds (0 never)",
        )
        .validated(at_least_zero),
        Key::new(
            INTERVAL_KEY,
            Value::Int(0),
            "Minutes between automatic backups (0 turns them off)",
        )
        .validated(at_least_zero),
        Key::new(
            STAGED_KEY,
            Value::Bool(true),
            "Copy a folder world aside while saving is held and compress after it resumes (shortest pause; needs room for one uncompressed copy)",
        ),
        Key::new(
            COMPRESSION_KEY,
            Value::Int(6),
            "Deflate level for folder world backups, 0 (store) to 9",
        )
        .validated(|v| match v {
            Value::Int(n) if (0..=9).contains(n) => Ok(()),
            _ => Err("must be 0 to 9".to_owned()),
        }),
    ]
}

/// Applies a staged restore before a server opens a built-in backend at
/// `world`; other storage never restores.
pub fn apply_before_open(
    storage: &Storage,
    world: &Path,
    options: BackendOptions,
) -> io::Result<()> {
    let Storage::Named(name) = storage else {
        return Ok(());
    };
    if name != vanilla::NAME && name != sqlite::NAME {
        return Ok(());
    }
    let applied = restore::apply_staged(world, || {
        let backend = storage.open_with(world, options)?;
        drop(backend.lock()?);
        drop(backend);
        Ok(())
    })?;
    if let Some(applied) = applied {
        match &applied.previous {
            Some(previous) => log::info!(
                "restored backup {} into {}; the world it replaced is now {}",
                applied.backup,
                world.display(),
                previous.display()
            ),
            None => log::info!(
                "restored backup {} into {}",
                applied.backup,
                world.display()
            ),
        }
    }
    Ok(())
}

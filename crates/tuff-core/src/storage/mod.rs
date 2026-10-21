//! World persistence: the backend-neutral interface and the backends live in
//! `tuff_storage`; this module holds the world codecs over them. The in-memory forms the world actors
//! work with (`Chunk`, `ChunkMeta`, `LevelData`,
//! `PlayerRecord`) and the NBT schemas they convert to are shared, so a new
//! backend only decides where and how bytes are kept.

pub mod backup;
pub mod chunk_nbt;
pub mod codec;
pub mod convert;
pub mod level;
pub mod normalise;
pub mod player_data;

use std::io;
use std::path::Path;
use std::sync::Arc;

use tuff_storage::backend::WorldBackend;
use tuff_storage::config::ConfigStore;
use tuff_storage::{memory, sqlite, vanilla};

/// A [`ConfigStore`]'s server file area as the place timing reports are saved.
pub struct ReportStore(pub Arc<dyn ConfigStore>);

impl tuff_timings::ReportFiles for ReportStore {
    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        self.0.list_files(prefix)
    }

    fn read_file(&self, path: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
        self.0.read_file(path, max)
    }

    fn write_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        self.0.write_file(path, contents)
    }
}

/// How a world is persisted, as configured for a server.
///
/// The world path handed to [`Storage::open`] means different things to
/// different backends: `vanilla` takes the world directory (e.g.
/// `<instance>/world`), `sqlite` takes the database file, and `memory`
/// ignores it. The `sqlite` backend also keeps the server's configuration
/// (see [`WorldBackend::config`]).
#[derive(Clone)]
pub enum Storage {
    /// A built-in backend by name (see [`BUILTIN`]).
    Named(String),
    /// A backend supplied by the embedding program or a feature.
    Custom(Arc<dyn WorldBackend>),
}

impl Default for Storage {
    fn default() -> Self {
        Self::Named(vanilla::NAME.into())
    }
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => write!(f, "Storage::Named({name:?})"),
            Self::Custom(backend) => write!(f, "Storage::Custom({:?})", backend.name()),
        }
    }
}

/// Settings for opening a built-in backend; each backend reads the ones
/// that concern it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendOptions {
    pub sqlite_locking: sqlite::Locking,
}

impl Storage {
    /// Instantiates the backend for `world_path` (a directory or a file,
    /// depending on the backend; see [`Storage`]).
    pub fn open(&self, world_path: &Path) -> io::Result<Arc<dyn WorldBackend>> {
        self.open_with(world_path, BackendOptions::default())
    }

    pub fn open_with(
        &self,
        world_path: &Path,
        options: BackendOptions,
    ) -> io::Result<Arc<dyn WorldBackend>> {
        sqlite::set_idle_hook(crate::threads::TRIM_POLL, crate::threads::collect_on_trim);
        match self {
            Self::Custom(backend) => Ok(backend.clone()),
            Self::Named(name) => {
                let (_, open) = BUILTIN
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("unknown storage backend {name:?}"),
                        )
                    })?;
                open(world_path, options)
            }
        }
    }
}

/// Opens the world at `world_path` for a run that writes nothing to disk:
/// an existing world is read through an in-memory overlay that keeps the
/// run's own writes, and a missing one lives in an in-memory server file.
pub fn open_no_save(
    storage: &Storage,
    world_path: &Path,
    options: BackendOptions,
) -> io::Result<(
    Arc<dyn WorldBackend>,
    Box<dyn tuff_storage::backend::WorldLock>,
)> {
    let missing = match storage {
        Storage::Named(name) if name == vanilla::NAME => !world_path.join("level.dat").exists(),
        Storage::Named(name) if name == sqlite::NAME => !world_path.exists(),
        _ => false,
    };
    if missing {
        let mut memory = sqlite::SqliteBackend::open_in_memory()?;
        if matches!(storage, Storage::Named(name) if name == vanilla::NAME) {
            memory = memory.without_config();
        }
        crate::log::info!(
            "no world at {}; this run keeps its world in memory",
            world_path.display()
        );
        let backend: Arc<dyn WorldBackend> = Arc::new(memory);
        let lock = backend.lock()?;
        return Ok((backend, lock));
    }
    let inner: Arc<dyn WorldBackend> = match storage {
        Storage::Named(name) if name == sqlite::NAME => {
            Arc::new(sqlite::SqliteBackend::open_read_only(world_path)?)
        }
        _ => storage.open_with(world_path, options)?,
    };
    let backend: Arc<dyn WorldBackend> = Arc::new(tuff_storage::no_save::NoSaveBackend::new(inner));
    let lock = backend.lock()?;
    Ok((backend, lock))
}

type Open = fn(&Path, BackendOptions) -> io::Result<Arc<dyn WorldBackend>>;

/// The backends this crate ships, by name.
pub const BUILTIN: &[(&str, Open)] = &[
    (vanilla::NAME, |dir, _| {
        Ok(Arc::new(vanilla::VanillaBackend::new(dir)))
    }),
    (memory::NAME, |_, _| {
        Ok(Arc::new(memory::MemoryBackend::default()))
    }),
    (sqlite::NAME, |file, options| {
        Ok(Arc::new(sqlite::SqliteBackend::open_with(
            file,
            options.sqlite_locking,
        )?))
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn named_backends_resolve_and_unknown_names_fail() {
        let dir = tuff_storage::test_dir::TestDir::new();
        assert_eq!(
            Storage::default().open(dir.path()).unwrap().name(),
            "vanilla"
        );
        assert_eq!(
            Storage::Named("memory".into())
                .open(dir.path())
                .unwrap()
                .name(),
            "memory"
        );
        assert_eq!(
            Storage::Named("sqlite".into())
                .open(&dir.path().join("world.db"))
                .unwrap()
                .name(),
            "sqlite"
        );
        assert!(Storage::Named("sqlite".into()).open(dir.path()).is_err());
        assert!(Storage::Named("bogus".into()).open(dir.path()).is_err());
        let custom: Arc<dyn WorldBackend> = Arc::new(memory::MemoryBackend::default());
        assert!(Arc::ptr_eq(
            &Storage::Custom(custom.clone()).open(dir.path()).unwrap(),
            &custom
        ));
    }
}

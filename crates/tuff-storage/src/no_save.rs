//! Read-only views of a world and its configuration for `--no-save` runs.
//!
//! [`NoSaveBackend`] reads through to the wrapped backend and keeps every
//! write in an in-memory overlay, so a run sees its own changes (a chunk
//! unloaded and loaded again keeps its blocks) while nothing reaches the
//! wrapped storage. [`NoSaveConfig`] does the same for a [`ConfigStore`].

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use crate::admin::{BanEntry, NameAndId, OpEntry, ServerProperties, UserCacheEntry};
use crate::backend::{BlobCodec, Documents, RegionStore, WorldBackend, WorldLock};
use crate::backup::BackupSource;
use crate::config::{ConfigStore, MAX_FILE_LIST};
use crate::document::{Document, DocumentId};
use tuff_nbt::Compound;
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Written blobs by chunk; `None` records a removal.
type Blobs = HashMap<ChunkPos, Option<Vec<u8>>>;

#[derive(Default)]
struct RegionOverlay {
    chunks: Blobs,
    entities: Blobs,
    pois: Blobs,
}

type SharedOverlay = Arc<Mutex<RegionOverlay>>;

pub struct NoSaveBackend {
    inner: Arc<dyn WorldBackend>,
    documents: Mutex<HashMap<DocumentId, Compound>>,
    regions: Mutex<HashMap<(String, FileRegionPos), SharedOverlay>>,
    extra: Mutex<BTreeMap<String, Option<Vec<u8>>>>,
}

impl NoSaveBackend {
    pub fn new(inner: Arc<dyn WorldBackend>) -> Self {
        Self {
            inner,
            documents: Mutex::default(),
            regions: Mutex::default(),
            extra: Mutex::default(),
        }
    }
}

struct NoLock;

impl WorldLock for NoLock {}

impl Documents for NoSaveBackend {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        if let Some(root) = locked(&self.documents).get(&DocumentId::from(document)) {
            return Ok(Some(root.clone()));
        }
        self.inner.documents().read(document)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        locked(&self.documents).insert(document.into(), root);
        Ok(())
    }

    fn exists(&self, document: Document<'_>) -> bool {
        locked(&self.documents).contains_key(&DocumentId::from(document))
            || self.inner.documents().exists(document)
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        let mut ids = self.inner.documents().list()?;
        for id in locked(&self.documents).keys() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        Ok(ids)
    }
}

struct NoSaveRegion {
    inner: Box<dyn RegionStore>,
    overlay: Arc<Mutex<RegionOverlay>>,
}

fn overlaid(
    written: Option<&Option<Vec<u8>>>,
    read: impl FnOnce() -> io::Result<Option<Vec<u8>>>,
) -> io::Result<Option<Vec<u8>>> {
    match written {
        Some(blob) => Ok(blob.clone()),
        None => read(),
    }
}

impl RegionStore for NoSaveRegion {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let overlay = locked(&self.overlay);
        overlaid(overlay.chunks.get(&pos), || self.inner.read_chunk(pos))
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        locked(&self.overlay)
            .chunks
            .insert(pos, Some(blob.to_vec()));
        Ok(())
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        locked(&self.overlay).chunks.insert(pos, None);
        Ok(())
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let overlay = locked(&self.overlay);
        overlaid(overlay.entities.get(&pos), || self.inner.read_entities(pos))
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        locked(&self.overlay)
            .entities
            .insert(pos, blob.map(<[u8]>::to_vec));
        Ok(())
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let overlay = locked(&self.overlay);
        overlaid(overlay.pois.get(&pos), || self.inner.read_pois(pos))
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        locked(&self.overlay)
            .pois
            .insert(pos, blob.map(<[u8]>::to_vec));
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        let mut chunks = self.inner.list_chunks()?;
        let overlay = locked(&self.overlay);
        for (pos, blob) in overlay
            .chunks
            .iter()
            .chain(&overlay.entities)
            .chain(&overlay.pois)
        {
            if blob.is_some() && !chunks.contains(pos) {
                chunks.push(*pos);
            }
        }
        Ok(chunks)
    }
}

impl WorldBackend for NoSaveBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    /// Takes no lock: a lock file would be a write.
    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        Ok(Box::new(NoLock))
    }

    fn documents(&self) -> &dyn Documents {
        self
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        let overlay = locked(&self.regions)
            .entry((dimension.to_owned(), region))
            .or_default()
            .clone();
        Ok(Box::new(NoSaveRegion {
            inner: self.inner.open_region(dimension, region)?,
            overlay,
        }))
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.inner.blob_codec()
    }

    fn config(&self) -> Option<Arc<dyn ConfigStore>> {
        self.inner
            .config()
            .map(|store| Arc::new(NoSaveConfig::new(store)) as Arc<dyn ConfigStore>)
    }

    fn backup_source(&self) -> BackupSource {
        BackupSource::Unsupported
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.inner.list_dimensions()
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        let mut regions = self.inner.list_regions(dimension)?;
        for (dim, region) in locked(&self.regions).keys() {
            if dim == dimension && !regions.contains(region) {
                regions.push(*region);
            }
        }
        Ok(regions)
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        let mut paths = self.inner.list_extra_files()?;
        let extra = locked(&self.extra);
        paths.retain(|path| !matches!(extra.get(path), Some(None)));
        for (path, data) in extra.iter() {
            if data.is_some() && !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        Ok(paths)
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        match locked(&self.extra).get(path) {
            Some(Some(data)) => Ok(data.clone()),
            Some(None) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{path:?} was removed"),
            )),
            None => self.inner.read_extra_file(path),
        }
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        locked(&self.extra).insert(path.to_owned(), Some(data.to_vec()));
        Ok(())
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        locked(&self.extra).insert(path.to_owned(), None);
        Ok(())
    }
}

#[derive(Default)]
struct ConfigOverlay {
    properties: Option<ServerProperties>,
    ops: Option<Vec<OpEntry>>,
    whitelist: Option<Vec<NameAndId>>,
    bans: Option<Vec<BanEntry<NameAndId>>>,
    ip_bans: Option<Vec<BanEntry<String>>>,
    icon: Option<Option<Vec<u8>>>,
    settings: Option<Vec<(String, String)>>,
    user_cache: Option<Vec<UserCacheEntry>>,
    grants: BTreeMap<String, Option<String>>,
    files: BTreeMap<String, Option<Vec<u8>>>,
}

/// A [`ConfigStore`] that reads through to `inner` and keeps writes in
/// memory.
pub struct NoSaveConfig {
    inner: Arc<dyn ConfigStore>,
    overlay: Mutex<ConfigOverlay>,
}

impl NoSaveConfig {
    pub fn new(inner: Arc<dyn ConfigStore>) -> Self {
        Self {
            inner,
            overlay: Mutex::default(),
        }
    }
}

macro_rules! overlay_list {
    ($read:ident, $write:ident, $field:ident, $ty:ty) => {
        fn $read(&self) -> io::Result<Vec<$ty>> {
            match &locked(&self.overlay).$field {
                Some(list) => Ok(list.clone()),
                None => self.inner.$read(),
            }
        }

        fn $write(&self, list: &[$ty]) -> io::Result<()> {
            locked(&self.overlay).$field = Some(list.to_vec());
            Ok(())
        }
    };
}

impl ConfigStore for NoSaveConfig {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn read_properties(&self) -> io::Result<Option<ServerProperties>> {
        match &locked(&self.overlay).properties {
            Some(properties) => Ok(Some(properties.clone())),
            None => self.inner.read_properties(),
        }
    }

    fn write_properties(&self, properties: &ServerProperties) -> io::Result<()> {
        locked(&self.overlay).properties = Some(properties.clone());
        Ok(())
    }

    overlay_list!(read_ops, write_ops, ops, OpEntry);
    overlay_list!(read_whitelist, write_whitelist, whitelist, NameAndId);
    overlay_list!(read_bans, write_bans, bans, BanEntry<NameAndId>);
    overlay_list!(read_ip_bans, write_ip_bans, ip_bans, BanEntry<String>);
    overlay_list!(read_settings, write_settings, settings, (String, String));
    overlay_list!(
        read_user_cache,
        write_user_cache,
        user_cache,
        UserCacheEntry
    );

    fn read_icon(&self) -> io::Result<Option<Vec<u8>>> {
        match &locked(&self.overlay).icon {
            Some(icon) => Ok(icon.clone()),
            None => self.inner.read_icon(),
        }
    }

    fn write_icon(&self, png: Option<&[u8]>) -> io::Result<()> {
        locked(&self.overlay).icon = Some(png.map(<[u8]>::to_vec));
        Ok(())
    }

    fn read_plugin_grants(&self) -> io::Result<Vec<(String, String)>> {
        let mut grants: BTreeMap<String, String> =
            self.inner.read_plugin_grants()?.into_iter().collect();
        for (plugin, body) in &locked(&self.overlay).grants {
            match body {
                Some(body) => grants.insert(plugin.clone(), body.clone()),
                None => grants.remove(plugin),
            };
        }
        Ok(grants.into_iter().collect())
    }

    fn write_plugin_grant(&self, plugin: &str, body: Option<&str>) -> io::Result<()> {
        locked(&self.overlay)
            .grants
            .insert(plugin.to_owned(), body.map(str::to_owned));
        Ok(())
    }

    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        let mut paths = match self.inner.list_files(prefix) {
            Ok(paths) => paths,
            Err(err) if err.kind() == io::ErrorKind::Unsupported => Vec::new(),
            Err(err) => return Err(err),
        };
        let overlay = locked(&self.overlay);
        paths.retain(|path| !matches!(overlay.files.get(path), Some(None)));
        for (path, data) in &overlay.files {
            if data.is_some() && path.starts_with(prefix) && !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        paths.sort();
        paths.truncate(MAX_FILE_LIST);
        Ok(paths)
    }

    fn read_file(&self, path: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
        match locked(&self.overlay).files.get(path) {
            Some(Some(data)) if data.len() > max => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path:?} is over the {max} byte limit"),
            )),
            Some(data) => Ok(data.clone()),
            None => self.inner.read_file(path, max),
        }
    }

    fn write_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        crate::config::validate_file_path(path)?;
        locked(&self.overlay)
            .files
            .insert(path.to_owned(), Some(contents.to_vec()));
        Ok(())
    }

    fn append_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        crate::config::validate_file_path(path)?;
        let mut overlay = locked(&self.overlay);
        let mut data = match overlay.files.get(path) {
            Some(data) => data.clone().unwrap_or_default(),
            None => match self.inner.read_file(path, usize::MAX) {
                Ok(data) => data.unwrap_or_default(),
                Err(err) if err.kind() == io::ErrorKind::Unsupported => Vec::new(),
                Err(err) => return Err(err),
            },
        };
        data.extend_from_slice(contents);
        overlay.files.insert(path.to_owned(), Some(data));
        Ok(())
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        locked(&self.overlay).files.insert(path.to_owned(), None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryBackend;

    #[tuff_core::watchdog]
    #[test]
    fn writes_stay_in_the_overlay_and_read_back() {
        let inner: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
        let pos = ChunkPos::new(1, 2);
        let region = FileRegionPos::new(0, 0);
        inner
            .open_region("minecraft:overworld", region)
            .unwrap()
            .write_chunk(pos, b"old")
            .unwrap();
        let no_save = NoSaveBackend::new(inner.clone());
        let mut store = no_save.open_region("minecraft:overworld", region).unwrap();
        assert_eq!(store.read_chunk(pos).unwrap().as_deref(), Some(&b"old"[..]));
        store.write_chunk(pos, b"new").unwrap();
        drop(store);
        let mut reopened = no_save.open_region("minecraft:overworld", region).unwrap();
        assert_eq!(
            reopened.read_chunk(pos).unwrap().as_deref(),
            Some(&b"new"[..])
        );
        let mut original = inner.open_region("minecraft:overworld", region).unwrap();
        assert_eq!(
            original.read_chunk(pos).unwrap().as_deref(),
            Some(&b"old"[..])
        );
        no_save
            .documents()
            .write(Document::Level, Compound::new())
            .unwrap();
        assert!(no_save.documents().exists(Document::Level));
        assert!(!inner.documents().exists(Document::Level));
    }
}

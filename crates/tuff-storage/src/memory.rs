//! A backend that keeps everything in memory: worlds vanish when the process
//! ends. Useful for tests, throwaway servers, and as the smallest example of
//! a backend.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

use crate::backend::{BlobCodec, Documents, RegionStore, WorldBackend, WorldLock};
use crate::blobs::NbtBlobCodec;
use crate::document::{Document, DocumentId};
use tuff_nbt::Compound;
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

type RegionMap = HashMap<(String, FileRegionPos), Arc<Mutex<Region>>>;

pub const NAME: &str = "memory";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Level,
    SavedData(String),
    DimensionData(String, String),
    Player(u128),
    Stats(u128),
    Advancements(u128),
    PlayerData(String, u128),
}

impl Key {
    fn of(document: Document<'_>) -> Self {
        match document {
            Document::Level => Self::Level,
            Document::SavedData(id) => Self::SavedData(id.into()),
            Document::DimensionData { dimension, id } => {
                Self::DimensionData(dimension.into(), id.into())
            }
            Document::Player(uuid) => Self::Player(uuid),
            Document::Stats(uuid) => Self::Stats(uuid),
            Document::Advancements(uuid) => Self::Advancements(uuid),
            Document::PlayerData { id, uuid } => Self::PlayerData(id.into(), uuid),
        }
    }

    fn into_id(self) -> DocumentId {
        match self {
            Self::Level => DocumentId::Level,
            Self::SavedData(id) => DocumentId::SavedData(id),
            Self::DimensionData(dimension, id) => DocumentId::DimensionData { dimension, id },
            Self::Player(uuid) => DocumentId::Player(uuid),
            Self::Stats(uuid) => DocumentId::Stats(uuid),
            Self::Advancements(uuid) => DocumentId::Advancements(uuid),
            Self::PlayerData(id, uuid) => DocumentId::PlayerData { id, uuid },
        }
    }
}

type Blobs = HashMap<ChunkPos, Vec<u8>>;

#[derive(Default)]
struct Region {
    chunks: Blobs,
    entities: Blobs,
    pois: Blobs,
}

#[derive(Default)]
pub struct MemoryBackend {
    documents: Mutex<HashMap<Key, Compound>>,
    regions: Mutex<RegionMap>,
    locked: Arc<Mutex<bool>>,
    codec: Arc<NbtBlobCodec>,
    extra: Mutex<BTreeMap<String, Vec<u8>>>,
}

fn extra_path(path: &str) -> io::Result<&str> {
    let valid = !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if valid {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid extra file path {path:?}"),
        ))
    }
}

struct Lock(Arc<Mutex<bool>>);

impl Drop for Lock {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = false;
    }
}

impl WorldLock for Lock {}

impl Documents for MemoryBackend {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        let mut root = self
            .documents
            .lock()
            .unwrap()
            .get(&Key::of(document))
            .cloned();
        if let Some(root) = &mut root {
            crate::datafix::document(document, root)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
        Ok(root)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        self.documents
            .lock()
            .unwrap()
            .insert(Key::of(document), root);
        Ok(())
    }

    fn exists(&self, document: Document<'_>) -> bool {
        self.documents
            .lock()
            .unwrap()
            .contains_key(&Key::of(document))
    }

    fn remove(&self, document: Document<'_>) -> io::Result<()> {
        self.documents.lock().unwrap().remove(&Key::of(document));
        Ok(())
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        Ok(self
            .documents
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .map(Key::into_id)
            .collect())
    }
}

struct MemoryRegionStore(Arc<Mutex<Region>>);

impl RegionStore for MemoryRegionStore {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().chunks.get(&pos).cloned())
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().chunks.insert(pos, blob.to_vec());
        Ok(())
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        self.0.lock().unwrap().chunks.remove(&pos);
        Ok(())
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().entities.get(&pos).cloned())
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        let mut region = self.0.lock().unwrap();
        match blob {
            Some(blob) => region.entities.insert(pos, blob.to_vec()),
            None => region.entities.remove(&pos),
        };
        Ok(())
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().pois.get(&pos).cloned())
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        let mut region = self.0.lock().unwrap();
        match blob {
            Some(blob) => region.pois.insert(pos, blob.to_vec()),
            None => region.pois.remove(&pos),
        };
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        let region = self.0.lock().unwrap();
        let mut positions: Vec<ChunkPos> = region
            .chunks
            .keys()
            .chain(region.entities.keys())
            .chain(region.pois.keys())
            .copied()
            .collect();
        positions.sort();
        positions.dedup();
        Ok(positions)
    }
}

impl WorldBackend for MemoryBackend {
    fn name(&self) -> &str {
        NAME
    }

    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        let mut locked = self.locked.lock().unwrap();
        if *locked {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the in-memory world is already in use",
            ));
        }
        *locked = true;
        drop(locked);
        Ok(Box::new(Lock(self.locked.clone())))
    }

    fn documents(&self) -> &dyn Documents {
        self
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        let region = self
            .regions
            .lock()
            .unwrap()
            .entry((dimension.into(), region))
            .or_default()
            .clone();
        Ok(Box::new(MemoryRegionStore(region)))
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.codec.clone()
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        let regions = self.regions.lock().unwrap();
        let mut dimensions: Vec<String> = regions
            .iter()
            .filter(|(_, region)| {
                let region = region.lock().unwrap();
                !region.chunks.is_empty() || !region.entities.is_empty()
            })
            .map(|((d, _), _)| d.clone())
            .collect();
        dimensions.sort();
        dimensions.dedup();
        Ok(dimensions)
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        let regions = self.regions.lock().unwrap();
        Ok(regions
            .iter()
            .filter(|((d, _), region)| {
                if d != dimension {
                    return false;
                }
                let region = region.lock().unwrap();
                !region.chunks.is_empty() || !region.entities.is_empty()
            })
            .map(|((_, pos), _)| *pos)
            .collect())
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        Ok(self.extra.lock().unwrap().keys().cloned().collect())
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        self.extra
            .lock()
            .unwrap()
            .get(extra_path(path)?)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let path = extra_path(path)?;
        self.extra
            .lock()
            .unwrap()
            .insert(path.to_owned(), data.to_vec());
        Ok(())
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        self.extra.lock().unwrap().remove(extra_path(path)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn documents_and_regions_persist_within_the_backend() {
        let backend = MemoryBackend::default();
        backend
            .write(Document::Level, Compound::new().with("a", 1))
            .unwrap();
        assert!(backend.exists(Document::Level));
        assert!(!backend.exists(Document::Player(1)));
        assert_eq!(
            backend.read(Document::Level).unwrap().unwrap().get_int("a"),
            Some(1)
        );
        let region = FileRegionPos::new(1, 1);
        let pos = ChunkPos::new(33, 40);
        let mut store = backend.open_region("minecraft:overworld", region).unwrap();
        store.write_chunk(pos, b"blob").unwrap();
        store.write_entities(pos, Some(b"ents")).unwrap();
        let mut again = backend.open_region("minecraft:overworld", region).unwrap();
        assert_eq!(again.read_chunk(pos).unwrap().unwrap(), b"blob");
        again.write_entities(pos, None).unwrap();
        assert!(store.read_entities(pos).unwrap().is_none());
        let mut other = backend.open_region("minecraft:the_end", region).unwrap();
        assert!(other.read_chunk(pos).unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lock_is_exclusive_until_dropped() {
        let backend = MemoryBackend::default();
        let lock = backend.lock().unwrap();
        assert!(backend.lock().is_err());
        drop(lock);
        assert!(backend.lock().is_ok());
    }
}

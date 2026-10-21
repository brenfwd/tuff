//! The interface between the world actors and whatever persists a world.
//!
//! A backend answers three kinds of question, each on the thread that suits
//! it: [`Documents`] hold the world's small whole-file records (level data,
//! saved data, player files) and are read and written on the blocking pool;
//! a [`RegionStore`] moves opaque chunk blobs in and out of one region's
//! storage, also on the blocking pool with the store handed off and back; and
//! a [`BlobCodec`] turns those blobs into NBT on the rayon pool, so
//! decoding never blocks I/O and I/O never blocks decoding.
//!
//! Blobs are opaque to everything but the backend that produced them, so a
//! backend is free to choose any encoding, compression or layout.

use std::io;
use std::sync::Arc;

use crate::config::ConfigStore;
use crate::document::{Document, DocumentId};
use tuff_nbt::Compound;
use tuff_world_model::poi::Record;
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

/// Whole-record storage. Records are NBT compounds in memory; how they are
/// laid out on disk (or elsewhere) is the backend's business.
pub trait Documents: Send + Sync {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>>;
    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()>;
    fn exists(&self, document: Document<'_>) -> bool;
    /// Every document currently stored, in no particular order. Used by
    /// offline tools (world conversion) that must copy everything without
    /// knowing document identities ahead of time.
    fn list(&self) -> io::Result<Vec<DocumentId>>;
}

/// Held for as long as the world is open; dropping it releases the world.
pub trait WorldLock: Send + Sync {}

/// Chunk and entity blobs of one 32x32-chunk region. One store is owned by
/// one region actor, so implementations need no internal locking.
pub trait RegionStore: Send {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>>;
    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()>;
    /// Deletes the chunk blob (a missing one is fine).
    fn remove_chunk(&mut self, _pos: ChunkPos) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this storage cannot delete chunks",
        ))
    }
    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>>;
    /// `None` removes the chunk's entity record.
    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()>;
    /// A chunk's points of interest. A store that keeps none has their
    /// claims forgotten on unload; the points come back from the blocks.
    fn read_pois(&mut self, _pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// `None` removes the chunk's point of interest record.
    fn write_pois(&mut self, _pos: ChunkPos, _blob: Option<&[u8]>) -> io::Result<()> {
        Ok(())
    }
    /// Makes everything written so far durable.
    fn flush(&mut self) -> io::Result<()>;
    /// Every chunk position in this region with a stored chunk blob, an
    /// entity blob, a point of interest blob, or any of them. Used by offline tools that must enumerate a
    /// region's contents rather than knowing chunk positions ahead of time.
    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>>;
}

/// Converts between blobs and NBT compounds, the layer below the chunk
/// codec that knows the world's in-memory forms. Runs on the rayon pool,
/// possibly from many threads at once.
pub trait BlobCodec: Send + Sync {
    /// An opaque tag naming the blob layout this codec reads and writes
    /// (not the compression it happens to choose when encoding). Two
    /// codecs with equal tags can exchange blobs unchanged; offline tools
    /// use this to skip a decode/re-encode round trip when it would be a
    /// no-op.
    fn blob_format(&self) -> &'static str;
    /// A chunk blob as its whole saved compound, as stored.
    fn read_root(&self, _blob: &[u8]) -> Result<Compound, String> {
        Err(format!("{} cannot read raw chunk data", self.blob_format()))
    }
    fn write_root(&self, _root: Compound) -> Result<Vec<u8>, String> {
        Err(format!(
            "{} cannot write raw chunk data",
            self.blob_format()
        ))
    }
    /// A chunk's saved entities, each as its NBT compound.
    fn decode_entities(&self, blob: &[u8], pos: ChunkPos) -> Result<Vec<Compound>, String>;
    /// `None` when there is nothing to store, which removes the record.
    fn encode_entities(
        &self,
        pos: ChunkPos,
        entities: &[Compound],
    ) -> Result<Option<Vec<u8>>, String>;
    /// A chunk's saved points of interest, `None` when too old to trust.
    fn decode_pois(&self, _blob: &[u8], _pos: ChunkPos) -> Result<Option<Vec<Record>>, String> {
        Ok(None)
    }
    /// `None` when there is nothing to store, which removes the record.
    fn encode_pois(&self, _pos: ChunkPos, _records: &[Record]) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
}

/// A way of persisting a world.
pub trait WorldBackend: Send + Sync + 'static {
    fn name(&self) -> &str;
    /// Takes exclusive use of the world, failing if another process has it.
    fn lock(&self) -> io::Result<Box<dyn WorldLock>>;
    /// Brings data saved by an older game version into the layout this
    /// backend reads, once the world is locked. Default: nothing to do.
    fn upgrade(&self) -> io::Result<()> {
        Ok(())
    }
    /// Turns maintenance of a backend's secondary entity index on or off.
    /// Default: the backend has none.
    fn set_entity_index(&self, _enabled: bool) {}
    /// Ad hoc SQL against the world, for a backend that keeps it in SQLite.
    fn sql_console(&self) -> Option<crate::sqlite::console::SqlConsole> {
        None
    }
    fn documents(&self) -> &dyn Documents;
    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>>;
    fn blob_codec(&self) -> Arc<dyn BlobCodec>;
    /// Where the server keeps its configuration, for a backend that holds
    /// it alongside the world. `None` leaves it in the instance directory.
    fn config(&self) -> Option<Arc<dyn ConfigStore>> {
        None
    }
    /// What a backup copies. Default: nothing, so the world cannot be
    /// backed up.
    fn backup_source(&self) -> crate::backup::BackupSource {
        crate::backup::BackupSource::Unsupported
    }
    /// Every dimension with any region data, by resource location (e.g.
    /// `minecraft:overworld`). Used by offline tools; the running server
    /// gets its dimension list from world-gen settings instead.
    fn list_dimensions(&self) -> io::Result<Vec<String>>;
    /// Every region with any chunk or entity data in `dimension`.
    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>>;
    /// Paths, relative to whatever this backend calls its root, of data it
    /// does not model as a document or chunk/entity blob (e.g. a vanilla
    /// world's data packs or POI files). Offline tools carry these through
    /// unchanged instead of losing them silently. Default: none.
    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{} does not support extra file {path:?}", self.name()),
        ))
    }
    /// The [`Self::list_extra_files`] paths below the directory `prefix`
    /// (which ends in `/`).
    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        let mut paths = self.list_extra_files()?;
        paths.retain(|path| path.starts_with(prefix));
        Ok(paths)
    }
    /// [`Self::read_extra_file`], refusing a file over `max` bytes, for
    /// files a player may have supplied (data packs).
    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let data = self.read_extra_file(path)?;
        if data.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path:?} is over the {max} byte limit"),
            ));
        }
        Ok(data)
    }
    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let _ = data;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{} does not support extra file {path:?}", self.name()),
        ))
    }
    /// Deletes an extra file; a missing one is not an error.
    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{} does not support extra file {path:?}", self.name()),
        ))
    }
}

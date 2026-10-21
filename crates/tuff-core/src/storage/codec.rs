//! Chunk blobs as the world's in-memory forms: the chunk NBT schema over
//! the backend's blob layer.

use std::sync::Arc;

use crate::world::block_entity::BlockEntity;
use tuff_nbt::Compound;
use tuff_storage::backend::{BlobCodec, WorldBackend};
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::chunk_meta::{ChunkMeta, Decoded};
use tuff_world_model::poi::Record;
use tuff_world_model::pos::ChunkPos;

use crate::storage::chunk_nbt;

/// Converts between blobs and the world's in-memory forms. Runs on the
/// rayon pool, possibly from many threads at once.
pub trait ChunkCodec: Send + Sync {
    /// An opaque tag naming the blob layout this codec reads and writes
    /// (not the compression it happens to choose when encoding). Two
    /// codecs with equal tags can exchange blobs unchanged; offline tools
    /// use this to skip a decode/re-encode round trip when it would be a
    /// no-op.
    fn blob_format(&self) -> &'static str;
    fn decode_chunk(
        &self,
        blob: &[u8],
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Result<Decoded<BlockEntity>, String>;
    fn encode_chunk(
        &self,
        chunk: &Chunk,
        meta: &ChunkMeta<BlockEntity>,
        pos: ChunkPos,
        last_update: i64,
    ) -> Result<Vec<u8>, String>;
    /// [`Self::decode_chunk`] in a world whose biomes may include custom
    /// ones.
    fn decode_chunk_in(
        &self,
        blob: &[u8],
        pos: ChunkPos,
        shape: ChunkShape,
        biomes: &Biomes,
    ) -> Result<Decoded<BlockEntity>, String>;
    /// [`Self::encode_chunk`] in a world whose biomes may include custom
    /// ones.
    fn encode_chunk_in(
        &self,
        chunk: &Chunk,
        meta: &ChunkMeta<BlockEntity>,
        pos: ChunkPos,
        last_update: i64,
        biomes: &Biomes,
    ) -> Result<Vec<u8>, String>;
    /// A chunk's saved entities, each as its NBT compound.
    fn decode_entities(&self, blob: &[u8], pos: ChunkPos) -> Result<Vec<Compound>, String>;
    /// A chunk blob as its whole saved compound, for chunks still in
    /// generation that only the generator understands.
    fn decode_root(&self, blob: &[u8], shape: ChunkShape) -> Result<Compound, String>;
    fn encode_root(&self, root: Compound) -> Result<Vec<u8>, String>;
    /// `None` when there is nothing to store, which removes the record.
    fn encode_entities(
        &self,
        pos: ChunkPos,
        entities: &[Compound],
    ) -> Result<Option<Vec<u8>>, String>;
    /// A chunk's saved points of interest, `None` when too old to trust.
    fn decode_pois(&self, blob: &[u8], pos: ChunkPos) -> Result<Option<Vec<Record>>, String>;
    /// `None` when there is nothing to store, which removes the record.
    fn encode_pois(&self, pos: ChunkPos, records: &[Record]) -> Result<Option<Vec<u8>>, String>;
}

/// The chunk codec of every backend: the chunk NBT schema over its blobs.
pub struct NbtChunkCodec {
    blobs: Arc<dyn BlobCodec>,
}

impl NbtChunkCodec {
    pub fn new(blobs: Arc<dyn BlobCodec>) -> Self {
        Self { blobs }
    }
}

impl Default for NbtChunkCodec {
    fn default() -> Self {
        Self::new(Arc::new(tuff_storage::blobs::NbtBlobCodec::default()))
    }
}

impl ChunkCodec for NbtChunkCodec {
    fn blob_format(&self) -> &'static str {
        self.blobs.blob_format()
    }

    fn decode_chunk(
        &self,
        blob: &[u8],
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Result<Decoded<BlockEntity>, String> {
        chunk_nbt::decode(self.blobs.read_root(blob)?, pos, shape).map_err(|e| e.to_string())
    }

    fn encode_chunk(
        &self,
        chunk: &Chunk,
        meta: &ChunkMeta<BlockEntity>,
        pos: ChunkPos,
        last_update: i64,
    ) -> Result<Vec<u8>, String> {
        self.blobs
            .write_root(chunk_nbt::encode(chunk, meta, pos, last_update))
    }

    fn decode_chunk_in(
        &self,
        blob: &[u8],
        pos: ChunkPos,
        shape: ChunkShape,
        biomes: &Biomes,
    ) -> Result<Decoded<BlockEntity>, String> {
        chunk_nbt::decode_in(self.blobs.read_root(blob)?, pos, shape, biomes)
            .map_err(|e| e.to_string())
    }

    fn encode_chunk_in(
        &self,
        chunk: &Chunk,
        meta: &ChunkMeta<BlockEntity>,
        pos: ChunkPos,
        last_update: i64,
        biomes: &Biomes,
    ) -> Result<Vec<u8>, String> {
        self.blobs
            .write_root(chunk_nbt::encode_in(chunk, meta, pos, last_update, biomes))
    }

    fn decode_root(&self, blob: &[u8], shape: ChunkShape) -> Result<Compound, String> {
        let mut root = self.blobs.read_root(blob)?;
        tuff_storage::datafix::chunk_in(&mut root, shape.min_section(), shape.overworld)
            .map_err(|e| e.to_string())?;
        Ok(root)
    }

    fn encode_root(&self, root: Compound) -> Result<Vec<u8>, String> {
        self.blobs.write_root(root)
    }

    fn decode_entities(&self, blob: &[u8], pos: ChunkPos) -> Result<Vec<Compound>, String> {
        self.blobs.decode_entities(blob, pos)
    }

    fn encode_entities(
        &self,
        pos: ChunkPos,
        entities: &[Compound],
    ) -> Result<Option<Vec<u8>>, String> {
        self.blobs.encode_entities(pos, entities)
    }

    fn decode_pois(&self, blob: &[u8], pos: ChunkPos) -> Result<Option<Vec<Record>>, String> {
        self.blobs.decode_pois(blob, pos)
    }

    fn encode_pois(&self, pos: ChunkPos, records: &[Record]) -> Result<Option<Vec<u8>>, String> {
        self.blobs.encode_pois(pos, records)
    }
}

/// Gives a backend its chunk codec.
pub trait WorldBackendExt {
    fn chunk_codec(&self) -> Arc<dyn ChunkCodec>;
}

impl<T: WorldBackend + ?Sized> WorldBackendExt for T {
    fn chunk_codec(&self) -> Arc<dyn ChunkCodec> {
        Arc::new(NbtChunkCodec::new(self.blob_codec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::sqlite::SqliteBackend;
    use tuff_storage::test_dir::TestDir;
    use tuff_vanilla_data::Block;

    const OVERWORLD: &str = "minecraft:overworld";

    #[tuff_core::watchdog]
    #[test]
    fn chunks_round_trip_through_the_blob_codec() {
        let dir = TestDir::new();
        let backend = SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
        let codec = backend.chunk_codec();
        let pos = ChunkPos::new(2, -7);
        let ore = Block::Glass.default_state();
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(4, 10, 4, ore);
        let blob = codec
            .encode_chunk(&chunk, &ChunkMeta::generated(), pos, 0)
            .unwrap();
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        store.write_chunk(pos, &blob).unwrap();
        store.flush().unwrap();
        let read = store.read_chunk(pos).unwrap().unwrap();
        let Decoded::Chunk(decoded, _) = codec
            .decode_chunk(&read, pos, ChunkShape::OVERWORLD)
            .unwrap()
        else {
            panic!("saved chunk has terrain");
        };
        assert_eq!(decoded.get(4, 10, 4), ore);
    }
}

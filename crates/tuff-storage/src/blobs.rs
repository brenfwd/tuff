use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};
use tuff_world_model::poi::Record;
use tuff_world_model::pos::ChunkPos;

use crate::backend::BlobCodec;
use crate::lz4;
use crate::region_file::{ChunkCompression, RawChunk};

/// Upper bound on one decompressed chunk or entity chunk.
pub const MAX_CHUNK_NBT_BYTES: usize = 32 * 1024 * 1024;

/// Chunks as vanilla region NBT: a compression id byte followed by the
/// (compressed) named-tag bytes, exactly what a region file sector holds
/// after its length prefix.
#[derive(Default)]
pub struct NbtBlobCodec {
    /// `None` stores the NBT uncompressed.
    pub compression: Option<ChunkCompression>,
}

impl NbtBlobCodec {
    pub fn root(&self, blob: &[u8]) -> Result<Compound, String> {
        let raw = RawChunk::from_blob(blob).ok_or("empty chunk blob")?;
        let bytes = raw
            .decompress(MAX_CHUNK_NBT_BYTES)
            .map_err(|e| e.to_string())?;
        let limits = NbtLimits {
            max_bytes: MAX_CHUNK_NBT_BYTES,
            ..NbtLimits::TRUSTED
        };
        match NamedTag::read(&mut &bytes[..], limits)
            .map_err(|e| e.to_string())?
            .tag
        {
            Tag::Compound(root) => Ok(root),
            _ => Err("chunk root is not a compound".into()),
        }
    }

    pub fn blob(&self, root: Compound) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        NamedTag::new("", root)
            .write(&mut bytes)
            .map_err(|e| e.to_string())?;
        let raw = match self.compression {
            Some(ChunkCompression::Zlib) => RawChunk::compress(bytes).map_err(|e| e.to_string())?,
            Some(ChunkCompression::Gzip) => RawChunk {
                compression: ChunkCompression::Gzip,
                data: Compression::Gzip
                    .compress(bytes)
                    .map_err(|e| e.to_string())?,
            },
            Some(ChunkCompression::Lz4) => RawChunk {
                compression: ChunkCompression::Lz4,
                data: lz4::compress_stream(&bytes),
            },
            Some(ChunkCompression::None) | None => RawChunk {
                compression: ChunkCompression::None,
                data: bytes,
            },
        };
        Ok(raw.into_blob())
    }
}

impl BlobCodec for NbtBlobCodec {
    fn blob_format(&self) -> &'static str {
        "vanilla-nbt"
    }

    fn read_root(&self, blob: &[u8]) -> Result<Compound, String> {
        self.root(blob)
    }

    fn write_root(&self, root: Compound) -> Result<Vec<u8>, String> {
        self.blob(root)
    }

    fn decode_entities(&self, blob: &[u8], pos: ChunkPos) -> Result<Vec<Compound>, String> {
        crate::entities::decode_owned(self.root(blob)?, pos).map_err(|e| e.to_string())
    }

    fn encode_entities(
        &self,
        pos: ChunkPos,
        entities: &[Compound],
    ) -> Result<Option<Vec<u8>>, String> {
        crate::entities::encode(pos, entities)
            .map(|root| self.blob(root))
            .transpose()
    }

    fn decode_pois(&self, blob: &[u8], pos: ChunkPos) -> Result<Option<Vec<Record>>, String> {
        crate::poi::decode(&self.root(blob)?, pos).map_err(|e| e.to_string())
    }

    fn encode_pois(&self, _pos: ChunkPos, records: &[Record]) -> Result<Option<Vec<u8>>, String> {
        crate::poi::encode(records)
            .map(|root| self.blob(root))
            .transpose()
    }
}

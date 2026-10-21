use std::collections::HashMap;

use tuff_nbt::Compound;

use crate::BlockPos;
use crate::chunk::Chunk;

pub const STATUS_FULL: &str = "minecraft:full";

/// What is persisted alongside a chunk's blocks. `E` is the block entity
/// model the server owns; this crate only stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkMeta<E> {
    pub status: String,
    /// Top-level keys this server does not model.
    pub extra: Compound,
    /// Set once blocks change (or the chunk was generated here): stored
    /// heightmaps are stale, so they are dropped on save for vanilla to
    /// recompute. Light is saved whenever the chunk carries it.
    pub relight: bool,
    /// The block entities this server models, taken out of the chunk's
    /// `block_entities` list on load and written back into it on save.
    pub block_entities: HashMap<BlockPos, E>,
    /// Every other block entity, as saved.
    pub raw_block_entities: HashMap<BlockPos, Compound>,
    /// Set when the chunk was upgraded from a world without the deep layers
    /// and its blocks below y 0 are a placeholder fill; never persisted.
    pub below_zero_retrogen: bool,
}

impl<E> Default for ChunkMeta<E> {
    fn default() -> Self {
        Self {
            status: String::new(),
            extra: Compound::default(),
            relight: false,
            block_entities: HashMap::new(),
            raw_block_entities: HashMap::new(),
            below_zero_retrogen: false,
        }
    }
}

impl<E> ChunkMeta<E> {
    pub fn generated() -> Self {
        Self {
            status: STATUS_FULL.into(),
            relight: true,
            ..Self::default()
        }
    }

    pub fn block_changed(&mut self) {
        self.relight = true;
    }
}

#[derive(Debug)]
pub enum Decoded<E> {
    Chunk(Box<Chunk>, ChunkMeta<E>),
    /// The chunk exists but holds no terrain yet; its unmodeled data should be
    /// kept when this server generates the terrain.
    Ungenerated(Compound),
}

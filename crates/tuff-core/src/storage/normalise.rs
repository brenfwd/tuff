//! The world-save normalisation hook. A feature that keeps state a vanilla
//! reader cannot understand (extra keys, stand-in blocks, in-flight
//! entities) registers a [`SaveNormaliser`]; it sees every chunk and
//! entity record just before serialisation and rewrites a private copy, so
//! the live world is never touched.
//!
//! Runs on the owning region actor for periodic saves, `/save-all` and
//! shutdown, and in the offline converter. With nothing registered
//! [`SaveNormalisers::apply`] returns its input untouched with no clone.
//!
//! Normalisers run in ascending `order` (registration order breaks ties).
//! A normaliser that returns an error or panics is logged and its partial
//! edits discarded; the next one still runs, and the save is never lost.

use crate::world::block_entity::BlockEntity;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use tuff_nbt::Compound;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::chunk_meta::ChunkMeta;
use tuff_world_model::pos::ChunkPos;

/// What is about to be written for one chunk. `blocks` is `Some` when the
/// chunk record is being written (block entities live in the meta),
/// `entities` when the entity record is.
#[derive(Clone)]
pub struct SaveView {
    pub dimension: String,
    pub pos: ChunkPos,
    pub blocks: Option<(Chunk, ChunkMeta<BlockEntity>)>,
    pub entities: Option<Vec<Compound>>,
}

pub trait SaveNormaliser: Send + Sync {
    fn name(&self) -> &'static str;

    fn normalise(&self, view: &mut SaveView) -> Result<(), String>;
}

#[derive(Clone)]
pub struct SaveNormaliserEntry {
    pub order: i32,
    pub hook: Arc<dyn SaveNormaliser>,
}

#[derive(Clone, Default)]
pub struct SaveNormalisers(Arc<[SaveNormaliserEntry]>);

impl SaveNormalisers {
    pub fn new(mut entries: Vec<SaveNormaliserEntry>) -> Self {
        entries.sort_by_key(|entry| entry.order);
        Self(entries.into())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn apply(&self, mut view: SaveView) -> SaveView {
        for entry in self.0.iter() {
            let before = view.clone();
            let outcome = catch_unwind(AssertUnwindSafe(|| entry.hook.normalise(&mut view)));
            let failure = match outcome {
                Ok(Ok(())) => continue,
                Ok(Err(err)) => err,
                Err(_) => "panicked".to_owned(),
            };
            log::warn!(
                "save normaliser {} failed at {:?} in {}: {failure}",
                entry.hook.name(),
                view.pos,
                view.dimension
            );
            view = before;
        }
        view
    }
}

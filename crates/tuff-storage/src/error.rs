use thiserror::Error;
use tuff_world_model::pos::ChunkPos;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChunkNbtError {
    #[error("chunk was saved by a newer version (data version {0})")]
    NewerVersion(i32),
    #[error("chunk is stored at the wrong position {0:?}")]
    WrongPosition(ChunkPos),
    #[error("malformed chunk: {0}")]
    Malformed(&'static str),
    #[error(transparent)]
    Upgrade(#[from] crate::datafix::DataFixError),
}

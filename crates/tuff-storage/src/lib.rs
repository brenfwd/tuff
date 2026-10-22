//! World storage that needs only NBT, the world model and the game's data
//! tables: the data fixer for records saved by older game versions, the
//! backend interface and its vanilla folder, SQLite and in-memory backends,
//! the document save gate, backups, the config store and the admin records
//! it persists, and the lz4 and sqlar codecs. The chunk NBT schema, level
//! and player data live in core, which owns the block entity and item models
//! they map.

pub mod admin;
pub mod backend;
pub mod backup;
pub mod blobs;
pub mod chunk_tickets;
pub mod config;
pub mod datafix;
pub mod document;
pub mod entities;
pub mod error;
pub mod files;
pub mod gate;
pub mod journal;
pub use tuff_nbt::lz4;
pub mod memory;
pub mod migrate;
pub mod no_save;
pub mod poi;
pub mod region_file;
pub mod sqlar;
pub mod sqlite;
pub mod test_dir;
pub mod vanilla;

pub use document::{Document, DocumentId};

//! Zip archives: a writer that streams entries of any size (zip64, data
//! descriptors, deflate split into blocks compressed in parallel on rayon)
//! and a reader that treats every archive as hostile (bounded entry
//! counts and sizes, sizes and checksums checked while inflating, a
//! compression ratio cap, and, in strict mode, name validation that keeps
//! extraction inside its target directory).

mod read;
mod write;

#[cfg(test)]
mod tests;

pub use read::{Archive, Entry, EntryReader, Limits, Source, validate_name};
pub use write::{BLOCK, DosTime, Pipeline, Prepared, Writer, prepare, write_all};

pub(crate) const LOCAL_HEADER: u32 = 0x0403_4b50;
pub(crate) const CENTRAL_HEADER: u32 = 0x0201_4b50;
pub(crate) const END_RECORD: u32 = 0x0605_4b50;
pub(crate) const ZIP64_END_RECORD: u32 = 0x0606_4b50;
pub(crate) const ZIP64_LOCATOR: u32 = 0x0706_4b50;
pub(crate) const DESCRIPTOR: u32 = 0x0807_4b50;
pub(crate) const ZIP64_EXTRA: u16 = 0x0001;

pub(crate) const LOCAL_HEADER_LEN: u64 = 30;
pub const CENTRAL_HEADER_LEN: usize = 46;
pub const END_RECORD_LEN: u64 = 22;
pub(crate) const ZIP64_END_RECORD_LEN: u64 = 56;
pub(crate) const ZIP64_LOCATOR_LEN: u64 = 20;

pub(crate) const STORED: u16 = 0;
pub(crate) const DEFLATED: u16 = 8;

pub(crate) const FLAG_ENCRYPTED: u16 = 1;
pub(crate) const FLAG_DESCRIPTOR: u16 = 1 << 3;
pub(crate) const FLAG_UTF8: u16 = 1 << 11;

pub(crate) const VERSION: u16 = 20;
pub(crate) const VERSION_ZIP64: u16 = 45;

pub(crate) const U16_MAX: u64 = u16::MAX as u64;
pub(crate) const U32_MAX: u64 = u32::MAX as u64;

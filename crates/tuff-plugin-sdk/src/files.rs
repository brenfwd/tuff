// SPDX-License-Identifier: MIT
//! The plugin's own file store (`cap.files`) as futures. Paths are
//! relative and `/`-separated.

use crate::bindings::tuff::plugin::capability_types as cap;
use crate::bindings::tuff::plugin::files as host;
use crate::exec::{Call, Completion};

pub use cap::{Denied as Error, FileEntry as Entry};

/// Where a [`write_with`] lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Create or truncate, then write.
    Replace,
    Append,
    /// Write at this offset of an existing file.
    At(u64),
}

async fn finish(handle: Result<u32, Error>) -> Result<cap::FileResult, Error> {
    match Call::file(handle?).await {
        Completion::File(outcome) => outcome,
        _ => Err(Error::Failed("mismatched completion".into())),
    }
}

fn unexpected<T>() -> Result<T, Error> {
    Err(Error::Failed("unexpected result".into()))
}

/// Up to `len` bytes of `path` from `offset`.
pub async fn read_range(path: &str, offset: u64, len: u32) -> Result<Vec<u8>, Error> {
    match finish(host::read(path, offset, len)).await? {
        cap::FileResult::Data(data) => Ok(data),
        _ => unexpected(),
    }
}

/// The whole file, up to the host's read limit.
pub async fn read(path: &str) -> Result<Vec<u8>, Error> {
    read_range(path, 0, u32::MAX).await
}

pub async fn read_string(path: &str) -> Result<String, Error> {
    let bytes = read(path).await?;
    String::from_utf8(bytes).map_err(|_| Error::Failed("not utf-8".into()))
}

async fn expect_done(handle: Result<u32, Error>) -> Result<(), Error> {
    match finish(handle).await? {
        cap::FileResult::Done => Ok(()),
        _ => unexpected(),
    }
}

pub async fn write_with(path: &str, mode: Mode, data: &[u8]) -> Result<(), Error> {
    let mode = match mode {
        Mode::Replace => cap::WriteMode::Replace,
        Mode::Append => cap::WriteMode::Append,
        Mode::At(offset) => cap::WriteMode::At(offset),
    };
    expect_done(host::write(path, mode, data)).await
}

/// Creates or truncates `path`.
pub async fn write(path: &str, data: impl AsRef<[u8]>) -> Result<(), Error> {
    write_with(path, Mode::Replace, data.as_ref()).await
}

pub async fn append(path: &str, data: impl AsRef<[u8]>) -> Result<(), Error> {
    write_with(path, Mode::Append, data.as_ref()).await
}

pub async fn list_dir(path: &str) -> Result<Vec<Entry>, Error> {
    match finish(host::list_dir(path)).await? {
        cap::FileResult::Entries(entries) => Ok(entries),
        _ => unexpected(),
    }
}

pub async fn stat(path: &str) -> Result<Entry, Error> {
    match finish(host::stat(path)).await? {
        cap::FileResult::Entry(entry) => Ok(entry),
        _ => unexpected(),
    }
}

pub async fn remove(path: &str) -> Result<(), Error> {
    expect_done(host::remove(path)).await
}

pub async fn rename(from: &str, to: &str) -> Result<(), Error> {
    expect_done(host::rename(from, to)).await
}

pub async fn create_dir(path: &str) -> Result<(), Error> {
    expect_done(host::create_dir(path)).await
}

/// Bytes used and the quota, from the host's running count.
pub fn usage() -> (u64, u64) {
    host::usage()
}

//! A plugin's own file store: the directory `<plugin dir>/<id>/files/`.
//!
//! Every operation runs on a per-plugin worker thread, never on the host
//! actor: the guest submits an [`Op`] and gets a handle back, and the
//! result arrives later through the completion callback. The worker owns
//! the byte and entry accounting, so quota checks are atomic with the
//! change they guard. Paths are validated by [`validate`] and resolved
//! component by component without following symlinks.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use crate::wit::bindings::capability_types::{Denied, FileEntry, FileResult, Handle, WriteMode};

/// The longest path, in bytes.
pub const MAX_PATH: usize = 1024;
/// The deepest path, in segments.
pub const MAX_DEPTH: usize = 16;
/// Entries the usage scan visits at most.
const SCAN_BOUND: u64 = 1_000_000;

/// What a plugin may do with its store right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    NotRequested,
    NotGranted,
    /// Granted, with this many bytes of disk.
    Quota(u64),
}

impl Access {
    /// The access a policy gives right now.
    pub fn of(policy: &crate::caps::Policy) -> Access {
        use crate::caps::Rule;
        let requested = policy
            .requests()
            .iter()
            .any(|r| matches!(r.entry.rule, Rule::Files(_)));
        match (requested, policy.files_quota()) {
            (false, _) => Access::NotRequested,
            (true, None) => Access::NotGranted,
            (true, Some(bytes)) => Access::Quota(bytes),
        }
    }

    pub(crate) fn denied(self) -> Option<Denied> {
        match self {
            Access::NotRequested => Some(Denied::NotRequested("files".into())),
            Access::NotGranted => Some(Denied::NotGranted("files".into())),
            Access::Quota(_) => None,
        }
    }
}

/// One file operation. Paths are already validated.
#[derive(Debug)]
pub enum Op {
    Read {
        path: Vec<String>,
        offset: u64,
        len: u32,
    },
    Write {
        path: Vec<String>,
        mode: WriteMode,
        data: Vec<u8>,
    },
    List(Vec<String>),
    Stat(Vec<String>),
    Remove(Vec<String>),
    Rename(Vec<String>, Vec<String>),
    CreateDir(Vec<String>),
    /// Recount the store after something outside changed it.
    Rescan,
}

/// Checks a guest path and splits it into segments. `allow_root` lets the
/// empty path (the store's root) through, for listing and stat.
pub fn validate(path: &str, allow_root: bool) -> Result<Vec<String>, Denied> {
    let bad = |why: &str| Err(Denied::Malformed(format!("bad file path {path:?}: {why}")));
    if path.is_empty() {
        return match allow_root {
            true => Ok(Vec::new()),
            false => bad("empty"),
        };
    }
    if path.len() > MAX_PATH {
        return bad("too long");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_DEPTH {
        return bad("too deep");
    }
    for segment in &segments {
        if segment.is_empty() {
            return bad("empty segment");
        }
        if segment.starts_with('.') {
            return bad("a segment starts with a dot");
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return bad("segments may only use letters, digits, '.', '_' and '-'");
        }
    }
    Ok(segments.into_iter().map(str::to_owned).collect())
}

/// Counters the worker keeps and everyone else reads.
struct Shared {
    used: AtomicU64,
    entries: AtomicU64,
    /// Encoded [`Access`]: 0 not requested, 1 not granted, else quota + 2.
    access: AtomicU64,
    cancelled: AtomicBool,
    inflight: AtomicU32,
}

fn encode(access: Access) -> u64 {
    match access {
        Access::NotRequested => 0,
        Access::NotGranted => 1,
        Access::Quota(bytes) => bytes.saturating_add(2),
    }
}

fn decode(value: u64) -> Access {
    match value {
        0 => Access::NotRequested,
        1 => Access::NotGranted,
        n => Access::Quota(n - 2),
    }
}

/// Result of one operation, with the handle `submit` returned for it.
pub type Done = (Handle, Result<FileResult, Denied>);

type Notify = Arc<dyn Fn(Done) + Send + Sync>;

/// The limits a store enforces besides the byte quota.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Files and directories the store may hold.
    pub entries: u32,
    /// Bytes one read returns.
    pub read: u64,
    /// Operations in flight at once.
    pub pending: u32,
}

/// The guest-facing side of a plugin's store.
pub struct Files {
    root: PathBuf,
    bounds: Bounds,
    shared: Arc<Shared>,
    notify: Notify,
    tx: Mutex<Option<Sender<(Handle, Op)>>>,
    next: AtomicU32,
}

impl Files {
    /// A store at `root`. No thread runs until it is first granted or
    /// used; `notify` is called from the worker with each result.
    pub fn new(
        root: PathBuf,
        bounds: Bounds,
        notify: impl Fn(Done) + Send + Sync + 'static,
    ) -> Self {
        Self {
            root,
            bounds,
            shared: Arc::new(Shared {
                used: AtomicU64::new(0),
                entries: AtomicU64::new(0),
                access: AtomicU64::new(0),
                cancelled: AtomicBool::new(false),
                inflight: AtomicU32::new(0),
            }),
            notify: Arc::new(notify),
            tx: Mutex::new(None),
            next: AtomicU32::new(1),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Applies a changed policy; revoking takes effect for every operation
    /// not yet started.
    pub fn set_access(&self, access: Access) {
        self.shared.access.store(encode(access), Ordering::SeqCst);
        if matches!(access, Access::Quota(_)) {
            self.start();
        }
    }

    pub fn access(&self) -> Access {
        decode(self.shared.access.load(Ordering::SeqCst))
    }

    /// Bytes used and the quota.
    pub fn usage(&self) -> (u64, u64) {
        let quota = match self.access() {
            Access::Quota(bytes) => bytes,
            _ => 0,
        };
        (self.shared.used.load(Ordering::SeqCst), quota)
    }

    /// Entries (files and directories) the store holds.
    pub fn entries(&self) -> u64 {
        self.shared.entries.load(Ordering::SeqCst)
    }

    fn start(&self) -> Sender<(Handle, Op)> {
        let mut slot = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = slot.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = channel();
        let worker = Worker {
            root: self.root.clone(),
            bounds: self.bounds,
            shared: self.shared.clone(),
            notify: self.notify.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("plugin-files".into())
            .spawn(move || worker.run(rx));
        if let Err(err) = spawned {
            log::error!("cannot start a plugin file worker: {err}");
        }
        *slot = Some(tx.clone());
        tx
    }

    /// Queues `op`, returning its handle. Refuses without queueing when
    /// the plugin has no access or too many operations are in flight.
    pub fn submit(&self, op: Op) -> Result<Handle, Denied> {
        if let Some(denied) = self.access().denied() {
            return Err(denied);
        }
        let taken = self.shared.inflight.fetch_add(1, Ordering::SeqCst);
        if taken >= self.bounds.pending {
            self.shared.inflight.fetch_sub(1, Ordering::SeqCst);
            return Err(Denied::Quota("too many file operations in flight".into()));
        }
        let handle = self.next.fetch_add(1, Ordering::SeqCst);
        if self.start().send((handle, op)).is_err() {
            self.shared.inflight.fetch_sub(1, Ordering::SeqCst);
            return Err(Denied::Failed("the file worker stopped".into()));
        }
        Ok(handle)
    }

    /// Recounts the store in the background (after an outside change).
    pub fn rescan(&self) {
        if let Access::Quota(_) = self.access() {
            let _ = self.start().send((0, Op::Rescan));
        }
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::SeqCst);
    }
}

struct Worker {
    root: PathBuf,
    bounds: Bounds,
    shared: Arc<Shared>,
    notify: Notify,
}

impl Worker {
    fn run(self, rx: Receiver<(Handle, Op)>) {
        self.scan();
        while let Ok((handle, op)) = rx.recv() {
            if self.shared.cancelled.load(Ordering::SeqCst) {
                break;
            }
            if matches!(op, Op::Rescan) {
                self.scan();
                continue;
            }
            let outcome = match self.access() {
                Some(denied) => Err(denied),
                None => self.execute(op),
            };
            self.shared.inflight.fetch_sub(1, Ordering::SeqCst);
            if !self.shared.cancelled.load(Ordering::SeqCst) {
                (self.notify)((handle, outcome));
            }
        }
    }

    fn access(&self) -> Option<Denied> {
        decode(self.shared.access.load(Ordering::SeqCst)).denied()
    }

    fn quota(&self) -> u64 {
        match decode(self.shared.access.load(Ordering::SeqCst)) {
            Access::Quota(bytes) => bytes,
            _ => 0,
        }
    }

    fn scan(&self) {
        let (mut bytes, mut entries, mut seen) = (0u64, 0u64, 0u64);
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in read.flatten() {
                seen += 1;
                if seen > SCAN_BOUND {
                    break;
                }
                let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                entries += 1;
                if meta.is_dir() {
                    stack.push(entry.path());
                } else {
                    bytes += meta.len();
                }
            }
        }
        self.shared.used.store(bytes, Ordering::SeqCst);
        self.shared.entries.store(entries, Ordering::SeqCst);
    }

    /// The real path of `segments`, refusing any existing symlink on the way.
    fn resolve(&self, segments: &[String]) -> Result<PathBuf, Denied> {
        let mut path = self.root.clone();
        for segment in segments {
            path.push(segment);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(Denied::Malformed(format!(
                        "{segment:?} is a symbolic link; the store does not follow them"
                    )));
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(failed(err)),
            }
        }
        Ok(path)
    }

    fn exists(path: &Path) -> Option<fs::Metadata> {
        fs::symlink_metadata(path).ok()
    }

    fn parent_dir(&self, segments: &[String]) -> Result<PathBuf, Denied> {
        let full = self.resolve(segments)?;
        let parent = full.parent().map(Path::to_path_buf).unwrap_or_default();
        match Self::exists(&parent) {
            Some(meta) if meta.is_dir() => Ok(full),
            _ => Err(Denied::NotFound),
        }
    }

    fn charge(&self, bytes: u64, entries: u64) -> Result<(), Denied> {
        let used = self.shared.used.load(Ordering::SeqCst);
        if used.saturating_add(bytes) > self.quota() {
            return Err(Denied::Quota(format!(
                "the file store is limited to {} bytes",
                self.quota()
            )));
        }
        let count = self.shared.entries.load(Ordering::SeqCst);
        if count + entries > u64::from(self.bounds.entries) {
            return Err(Denied::Quota(format!(
                "the file store is limited to {} entries",
                self.bounds.entries
            )));
        }
        Ok(())
    }

    fn adjust(&self, used: i128, entries: i128) {
        let apply = |counter: &AtomicU64, delta: i128| {
            let now = counter.load(Ordering::SeqCst) as i128;
            counter.store((now + delta).max(0) as u64, Ordering::SeqCst);
        };
        apply(&self.shared.used, used);
        apply(&self.shared.entries, entries);
    }

    fn execute(&self, op: Op) -> Result<FileResult, Denied> {
        match op {
            Op::Read { path, offset, len } => self.read(&path, offset, len),
            Op::Write { path, mode, data } => self.write(&path, mode, &data),
            Op::List(path) => self.list(&path),
            Op::Stat(path) => {
                let full = self.resolve(&path)?;
                let meta = Self::exists(&full).ok_or(Denied::NotFound)?;
                Ok(FileResult::Entry(entry(&full, &path, &meta)))
            }
            Op::Remove(path) => self.remove(&path),
            Op::Rename(src, dst) => self.rename(&src, &dst),
            Op::CreateDir(path) => self.create_dir(&path),
            Op::Rescan => Ok(FileResult::Done),
        }
    }

    fn read(&self, path: &[String], offset: u64, len: u32) -> Result<FileResult, Denied> {
        let full = self.resolve(path)?;
        let meta = Self::exists(&full).ok_or(Denied::NotFound)?;
        if !meta.is_file() {
            return Err(Denied::Malformed("not a file".into()));
        }
        let want = u64::from(len).min(self.bounds.read);
        let mut file = File::open(&full).map_err(failed)?;
        file.seek(SeekFrom::Start(offset)).map_err(failed)?;
        let mut data = Vec::new();
        file.take(want).read_to_end(&mut data).map_err(failed)?;
        Ok(FileResult::Data(data))
    }

    fn write(&self, path: &[String], mode: WriteMode, data: &[u8]) -> Result<FileResult, Denied> {
        let _ = fs::create_dir_all(&self.root);
        let full = self.parent_dir(path)?;
        let existing = Self::exists(&full);
        if existing.as_ref().is_some_and(|m| !m.is_file()) {
            return Err(Denied::Malformed("not a file".into()));
        }
        let old = existing.as_ref().map_or(0, |m| m.len());
        let new_entry = u64::from(existing.is_none());
        let len = data.len() as u64;
        let new_size = match &mode {
            WriteMode::Replace => len,
            WriteMode::Append => old.saturating_add(len),
            WriteMode::At(offset) => {
                if existing.is_none() {
                    return Err(Denied::NotFound);
                }
                old.max(offset.saturating_add(len))
            }
        };
        self.charge(new_size.saturating_sub(old), new_entry)?;
        let mut options = OpenOptions::new();
        options.write(true);
        match &mode {
            WriteMode::Replace => options.create(true).truncate(true),
            WriteMode::Append => options.create(true).append(true),
            WriteMode::At(_) => &mut options,
        };
        let mut file = options.open(&full).map_err(failed)?;
        if let WriteMode::At(offset) = mode {
            file.seek(SeekFrom::Start(offset)).map_err(failed)?;
        }
        file.write_all(data).map_err(failed)?;
        let after = file.metadata().map_or(new_size, |m| m.len());
        self.adjust(i128::from(after) - i128::from(old), i128::from(new_entry));
        Ok(FileResult::Done)
    }

    fn list(&self, path: &[String]) -> Result<FileResult, Denied> {
        let full = self.resolve(path)?;
        let meta = Self::exists(&full);
        let read = match meta {
            Some(meta) if meta.is_dir() => fs::read_dir(&full).map_err(failed)?,
            Some(_) => return Err(Denied::Malformed("not a directory".into())),
            None if path.is_empty() => return Ok(FileResult::Entries(Vec::new())),
            None => return Err(Denied::NotFound),
        };
        let mut entries = Vec::new();
        for item in read.flatten() {
            let Ok(meta) = fs::symlink_metadata(item.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let mut child = path.to_vec();
            child.push(item.file_name().to_string_lossy().into_owned());
            entries.push(entry(&item.path(), &child, &meta));
            if entries.len() >= self.bounds.entries as usize {
                break;
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(FileResult::Entries(entries))
    }

    fn remove(&self, path: &[String]) -> Result<FileResult, Denied> {
        let full = self.resolve(path)?;
        let meta = Self::exists(&full).ok_or(Denied::NotFound)?;
        match meta.is_dir() {
            true => fs::remove_dir(&full).map_err(failed)?,
            false => fs::remove_file(&full).map_err(failed)?,
        }
        let bytes = if meta.is_dir() { 0 } else { meta.len() };
        self.adjust(-i128::from(bytes), -1);
        Ok(FileResult::Done)
    }

    fn rename(&self, src: &[String], dst: &[String]) -> Result<FileResult, Denied> {
        let from = self.resolve(src)?;
        let to = self.parent_dir(dst)?;
        if Self::exists(&from).is_none() {
            return Err(Denied::NotFound);
        }
        if Self::exists(&to).is_some() {
            return Err(Denied::Failed("the destination exists".into()));
        }
        if dst.starts_with(src) {
            return Err(Denied::Malformed(
                "cannot move a directory into itself".into(),
            ));
        }
        fs::rename(&from, &to).map_err(failed)?;
        Ok(FileResult::Done)
    }

    fn create_dir(&self, path: &[String]) -> Result<FileResult, Denied> {
        let _ = fs::create_dir_all(&self.root);
        let full = self.parent_dir(path)?;
        match Self::exists(&full) {
            Some(meta) if meta.is_dir() => return Ok(FileResult::Done),
            Some(_) => return Err(Denied::Failed("a file is in the way".into())),
            None => {}
        }
        self.charge(0, 1)?;
        fs::create_dir(&full).map_err(failed)?;
        self.adjust(0, 1);
        Ok(FileResult::Done)
    }
}

fn failed(err: std::io::Error) -> Denied {
    match err.kind() {
        std::io::ErrorKind::NotFound => Denied::NotFound,
        _ => Denied::Failed(err.to_string()),
    }
}

fn entry(_full: &Path, segments: &[String], meta: &fs::Metadata) -> FileEntry {
    FileEntry {
        name: segments.last().cloned().unwrap_or_default(),
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_ms: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64),
    }
}

/// Deletes a plugin's whole store (for `/tuff plugin purge`).
pub fn purge(root: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(root) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;

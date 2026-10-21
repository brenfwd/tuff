//! World backups. A folder world is copied file by file (every file but
//! `session.lock`) into `<level>-<yyyymmdd-hhmmss>.zip`, a valid world
//! folder once unzipped. A backend kept in one file (the `.mcserver`
//! server file) is frozen on disk and copied whole into
//! `<level>-<yyyymmdd-hhmmss>.<extension>`. Either way the consistent part
//! is short: the running server holds saving only while the folder is
//! staged (copied, which is a cheap clone on copy-on-write filesystems) or
//! the file frozen, and compresses or copies after saving resumes.
//!
//! The steps here are blocking and know nothing of a running world;
//! [`jobs`] runs them under a save hold, the CLI runs them on a locked
//! offline world. Partial output is `<name>.part` (and a
//! `.staging-<name>` folder), never listed and removed on failure or at
//! the next start.

pub mod restore;
pub mod space;

#[cfg(test)]
mod restore_tests;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::watch;

use tuff_zip::{DosTime, Pipeline, Writer};

pub const ZIP_EXTENSION: &str = "zip";
const PART_SUFFIX: &str = ".part";
const STAGING_PREFIX: &str = ".staging-";
const SESSION_LOCK: &str = "session.lock";
const COPY_BUFFER: usize = 1 << 20;

/// What a backup copies from a backend.
#[derive(Clone)]
pub enum BackupSource {
    /// A world folder, copied file by file while saving is held.
    Folder(PathBuf),
    /// A backend kept in one file it can freeze.
    File(Arc<dyn FileSnapshot>),
    Unsupported,
}

/// A backend in one file that can keep it unchanged on disk for a copy.
pub trait FileSnapshot: Send + Sync {
    /// The backup file's extension, without the dot.
    fn extension(&self) -> &'static str;
    /// Makes the file on disk complete and unchanging until the returned
    /// guard drops; writes meanwhile go elsewhere and stay readable. Called
    /// while saving is held.
    fn freeze(&self) -> io::Result<Box<dyn FrozenFile>>;
    /// Checks a finished copy.
    fn verify(&self, copy: &Path) -> io::Result<()>;
    /// The file itself, when a backup of it can be restored in its place.
    fn restorable(&self) -> Option<&Path> {
        None
    }
    /// Drops the extra files below any of the `prefixes` from a finished copy.
    fn exclude(&self, _copy: &Path, _prefixes: &[String]) -> io::Result<()> {
        Ok(())
    }
}

/// Extra-file directories holding data derived from the world, left out of
/// backups unless the operator asks to keep them.
const BUILT_IN_DERIVED: &[&str] = &["tuff/world-map/"];

static REGISTERED_DERIVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Marks an extra-file directory prefix as a rebuildable cache.
pub fn register_derived_prefix(prefix: &str) {
    let mut list = REGISTERED_DERIVED.lock().unwrap_or_else(|e| e.into_inner());
    if !list.iter().any(|p| p == prefix) {
        list.push(prefix.to_owned());
    }
}

/// Every extra-file prefix backups leave out by default.
pub fn derived_prefixes() -> Vec<String> {
    let mut all: Vec<String> = BUILT_IN_DERIVED.iter().map(|p| (*p).to_owned()).collect();
    let list = REGISTERED_DERIVED.lock().unwrap_or_else(|e| e.into_inner());
    for p in list.iter() {
        if !all.contains(p) {
            all.push(p.clone());
        }
    }
    all
}

pub trait FrozenFile: Send {
    fn path(&self) -> &Path;
    /// The backend is closing: a copy in progress must stop.
    fn interrupted(&self) -> bool;
    /// Bytes the copy must not read (another handle may hold an exclusive
    /// lock on them) and writes as zeros: the backend never stores data
    /// there.
    fn skip(&self) -> Option<Range<u64>> {
        None
    }
}

/// Reads a file but never the bytes in `skip`, which read as zeros.
struct SkipRange {
    file: File,
    pos: u64,
    len: u64,
    skip: Range<u64>,
}

impl Read for SkipRange {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let n = if self.skip.contains(&self.pos) {
            let n = (buf.len() as u64).min(self.skip.end.min(self.len) - self.pos) as usize;
            buf[..n].fill(0);
            self.file.seek(SeekFrom::Start(self.pos + n as u64))?;
            n
        } else {
            let room = if self.pos < self.skip.start {
                self.skip.start - self.pos
            } else {
                u64::MAX
            };
            let want = (buf.len() as u64).min(room) as usize;
            self.file.read(&mut buf[..want])?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Saving,
    Copying,
    Compressing,
    Verifying,
    Finishing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// A job's progress and cancel flag, shared with whoever watches it.
#[derive(Clone)]
pub struct Tracker {
    cancel: Arc<AtomicBool>,
    progress: Arc<watch::Sender<Progress>>,
}

impl Tracker {
    pub fn new() -> (Tracker, watch::Receiver<Progress>) {
        let (tx, rx) = watch::channel(Progress::default());
        (
            Tracker {
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(tx),
            },
            rx,
        )
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn check(&self) -> io::Result<()> {
        if self.cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the backup was cancelled",
            ));
        }
        Ok(())
    }

    pub fn progress(&self) -> Progress {
        self.progress.borrow().clone()
    }

    fn phase(&self, phase: Phase, files_total: u64, bytes_total: u64) {
        self.progress.send_replace(Progress {
            phase,
            files_done: 0,
            files_total,
            bytes_done: 0,
            bytes_total,
        });
    }

    fn add(&self, files: u64, bytes: u64) {
        self.progress.send_modify(|p| {
            p.files_done += files;
            p.bytes_done += bytes;
        });
    }
}

/// A file of a folder world, as found when the backup started.
#[derive(Clone, Debug)]
pub struct FileItem {
    /// Path below the world folder, `/`-separated.
    pub rel: String,
    pub size: u64,
    pub modified: DosTime,
}

/// Every regular file below `root` (symbolic links are not followed),
/// except `session.lock` at the top and anything under `skip`, sorted.
pub fn scan_folder(root: &Path, skip: &[PathBuf]) -> io::Result<Vec<FileItem>> {
    let mut files = Vec::new();
    walk(root, root, skip, &mut files)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

fn walk(root: &Path, dir: &Path, skip: &[PathBuf], out: &mut Vec<FileItem>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if skip.iter().any(|s| path.starts_with(s)) {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk(root, &path, skip, out)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let Some(parts) = rel
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
        else {
            log::warn!("backup skips {}: not a UTF-8 path", path.display());
            continue;
        };
        let rel = parts.join("/");
        if rel == SESSION_LOCK {
            continue;
        }
        out.push(FileItem {
            rel,
            size: meta.len(),
            modified: meta.modified().map_or(DosTime::MIN, DosTime::from_system),
        });
    }
    Ok(())
}

pub fn join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Copies `files` from `root` into the new folder `staging`.
pub fn stage_folder(
    root: &Path,
    files: &[FileItem],
    staging: &Path,
    tracker: &Tracker,
) -> io::Result<()> {
    tracker.phase(
        Phase::Copying,
        files.len() as u64,
        files.iter().map(|f| f.size).sum(),
    );
    fs::create_dir(staging)?;
    for file in files {
        tracker.check()?;
        let to = join(staging, &file.rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let copied = fs::copy(join(root, &file.rel), &to)?;
        tracker.add(1, copied);
    }
    Ok(())
}

struct Tracked<'a, R> {
    inner: R,
    tracker: &'a Tracker,
    interrupted: Option<&'a dyn FrozenFile>,
}

impl<R: Read> Read for Tracked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tracker.check()?;
        if self.interrupted.is_some_and(|f| f.interrupted()) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the world closed during the backup",
            ));
        }
        let n = self.inner.read(buf)?;
        self.tracker.add(0, n as u64);
        Ok(n)
    }
}

/// Zips `files` from `root` into the new file `out`, each under `prefix`
/// (a folder name ending in `/`). Returns the archive's size.
pub fn zip_folder(
    root: &Path,
    files: &[FileItem],
    prefix: &str,
    out: &Path,
    level: u32,
    tracker: &Tracker,
) -> io::Result<u64> {
    tracker.phase(
        Phase::Compressing,
        files.len() as u64,
        files.iter().map(|f| f.size).sum(),
    );
    let file = File::create_new(out)?;
    let mut pipeline = Pipeline::new(
        Writer::new(BufWriter::with_capacity(COPY_BUFFER, file)),
        level,
    );
    let newest = files
        .iter()
        .map(|f| f.modified)
        .max_by_key(|t| (t.date, t.time))
        .unwrap_or(DosTime::MIN);
    pipeline.add_directory(prefix, newest)?;
    for item in files {
        tracker.check()?;
        let reader = Tracked {
            inner: File::open(join(root, &item.rel))?,
            tracker,
            interrupted: None,
        };
        pipeline.add_reader(&format!("{prefix}{}", item.rel), item.modified, reader)?;
        tracker.add(1, 0);
    }
    let file = pipeline
        .finish()?
        .into_inner()
        .map_err(|err| err.into_error())?;
    file.sync_all()?;
    Ok(file.metadata()?.len())
}

/// Copies `from` into the new file `to`, reporting progress and stopping
/// when cancelled or when `frozen` is interrupted.
pub fn copy_file(
    from: &Path,
    to: &Path,
    tracker: &Tracker,
    frozen: Option<&dyn FrozenFile>,
) -> io::Result<u64> {
    let source = File::open(from)?;
    let len = source.metadata()?.len();
    tracker.phase(Phase::Copying, 1, len);
    let inner: Box<dyn Read> = match frozen.and_then(|f| f.skip()) {
        Some(skip) => Box::new(SkipRange {
            file: source,
            pos: 0,
            len,
            skip,
        }),
        None => Box::new(source),
    };
    let mut reader = Tracked {
        inner,
        tracker,
        interrupted: frozen,
    };
    let mut out = File::create_new(to)?;
    let mut buf = vec![0; COPY_BUFFER];
    let mut copied = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        copied += n as u64;
    }
    out.sync_all()?;
    tracker.add(1, 0);
    Ok(copied)
}

/// A level name made safe for file names.
pub fn file_level(level: &str) -> String {
    let cleaned: String = level
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() {
        "world".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// `<level>-<yyyymmdd-hhmmss>[-n].<extension>`, the time in UTC.
pub fn file_name(level: &str, unix_secs: u64, n: u32, extension: &str) -> String {
    let (y, m, d) = crate::admin::civil_from_days((unix_secs / 86_400) as i64);
    let s = unix_secs % 86_400;
    let suffix = if n > 1 {
        format!("-{n}")
    } else {
        String::new()
    };
    format!(
        "{level}-{y:04}{m:02}{d:02}-{:02}{:02}{:02}{suffix}.{extension}",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// The creation time (unix seconds) a backup name of `level` encodes.
pub fn parse_name(level: &str, name: &str) -> Option<u64> {
    parse_parts(level, name).map(|(secs, _)| secs)
}

/// The creation time and the same-second counter of a backup name.
fn parse_parts(level: &str, name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix(level)?.strip_prefix('-')?;
    let (stem, extension) = rest.rsplit_once('.')?;
    if extension.is_empty() || !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let (date, rest) = stem.split_at_checked(8)?;
    let rest = rest.strip_prefix('-')?;
    let (time, n) = rest.split_at_checked(6)?;
    let n = if n.is_empty() {
        1
    } else {
        let n = n.strip_prefix('-')?;
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok()?
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(date) || !digits(time) {
        return None;
    }
    let num = |s: &str| s.parse::<u32>().ok();
    let (y, m, d) = (num(&date[..4])?, num(&date[4..6])?, num(&date[6..])?);
    let (hh, mm, ss) = (num(&time[..2])?, num(&time[2..4])?, num(&time[4..])?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let days = crate::admin::days_from_civil(i64::from(y), m, d);
    let secs = u64::try_from(days * 86_400 + i64::from(hh * 3600 + mm * 60 + ss)).ok()?;
    Some((secs, n))
}

/// A finished backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub size: u64,
    pub created_unix: u64,
}

/// The finished backups of `level` in `dir`, newest first. Partial files
/// and anything not named like a backup are left out.
pub fn list(dir: &Path, level: &str) -> io::Result<Vec<BackupInfo>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(created_unix) = parse_name(level, &name) else {
            continue;
        };
        let meta = entry.metadata()?;
        if meta.is_file() {
            found.push(BackupInfo {
                name,
                size: meta.len(),
                created_unix,
            });
        }
    }
    found.sort_by_key(|b| std::cmp::Reverse((parse_parts(level, &b.name), b.name.clone())));
    Ok(found)
}

/// The path of the finished backup `name` of `level`, if it is one.
pub fn resolve(dir: &Path, level: &str, name: &str) -> Option<PathBuf> {
    parse_name(level, name)?;
    let path = dir.join(name);
    path.is_file().then_some(path)
}

/// How many backups to keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// Newest backups kept; 0 keeps all.
    pub keep: usize,
    pub max_age: Option<Duration>,
}

/// Deletes the backups `retention` no longer keeps, never `protect`.
/// Returns the deleted names.
pub fn prune(
    dir: &Path,
    level: &str,
    retention: Retention,
    now_unix: u64,
    protect: &str,
) -> io::Result<Vec<String>> {
    let mut deleted = Vec::new();
    for (i, backup) in list(dir, level)?.into_iter().enumerate() {
        if backup.name == protect {
            continue;
        }
        let surplus = retention.keep > 0 && i >= retention.keep;
        let expired = retention
            .max_age
            .is_some_and(|age| now_unix.saturating_sub(backup.created_unix) > age.as_secs());
        if surplus || expired {
            fs::remove_file(dir.join(&backup.name))?;
            deleted.push(backup.name);
        }
    }
    Ok(deleted)
}

/// Removes partial output of `level` a crashed job left in `dir`.
pub fn clean_leftovers(dir: &Path, level: &str) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(part) = name.strip_suffix(PART_SUFFIX)
            && parse_name(level, part).is_some()
        {
            fs::remove_file(entry.path())?;
        } else if let Some(staged) = name.strip_prefix(STAGING_PREFIX)
            && parse_name(level, staged).is_some()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One backup to make.
#[derive(Clone)]
pub struct Plan {
    pub source: BackupSource,
    /// Where backups go; created when missing.
    pub dir: PathBuf,
    pub level: String,
    /// Deflate level for folder worlds.
    pub compression: u32,
    /// Stage a folder world before compressing it.
    pub staged: bool,
    /// Keep the rebuildable caches ([`derived_prefixes`]) in the backup.
    pub include_caches: bool,
}

/// The names and paths a [`Plan`] writes to.
#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub path: PathBuf,
    pub part: PathBuf,
    pub staging: PathBuf,
}

impl Plan {
    /// Picks a free name for a backup made now.
    pub fn target(&self) -> io::Result<Target> {
        let extension = match &self.source {
            BackupSource::Folder(_) => ZIP_EXTENSION,
            BackupSource::File(file) => file.extension(),
            BackupSource::Unsupported => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "this world's storage cannot be backed up",
                ));
            }
        };
        fs::create_dir_all(&self.dir)?;
        let level = file_level(&self.level);
        let now = now_unix();
        for n in 1..1000 {
            let name = file_name(&level, now, n, extension);
            let path = self.dir.join(&name);
            let part = self.dir.join(format!("{name}{PART_SUFFIX}"));
            let staging = self.dir.join(format!("{STAGING_PREFIX}{name}"));
            if !path.exists() && !part.exists() && !staging.exists() {
                return Ok(Target {
                    name,
                    path,
                    part,
                    staging,
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free backup name",
        ))
    }
}

/// What the consistent step leaves for the rest of the job.
pub enum Snapshot {
    /// The archive is written to the part file.
    Written,
    /// The world is copied to the staging folder.
    Staged(Vec<FileItem>),
    Frozen(Box<dyn FrozenFile>),
}

/// The step that needs the world still: stage or zip a folder world,
/// freeze a file one.
pub fn snapshot(plan: &Plan, target: &Target, tracker: &Tracker) -> io::Result<Snapshot> {
    match &plan.source {
        BackupSource::Folder(root) => {
            let skip: Vec<PathBuf> = [plan.dir.canonicalize().ok(), Some(plan.dir.clone())]
                .into_iter()
                .flatten()
                .collect();
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let mut files = scan_folder(&root, &skip)?;
            if !plan.include_caches {
                let excluded = derived_prefixes();
                files.retain(|f| !excluded.iter().any(|p| f.rel.starts_with(p.as_str())));
            }
            if plan.staged {
                stage_folder(&root, &files, &target.staging, tracker)?;
                Ok(Snapshot::Staged(files))
            } else {
                zip_folder(
                    &root,
                    &files,
                    &zip_prefix(&plan.level),
                    &target.part,
                    plan.compression,
                    tracker,
                )?;
                Ok(Snapshot::Written)
            }
        }
        BackupSource::File(file) => {
            tracker.phase(Phase::Copying, 1, 0);
            Ok(Snapshot::Frozen(file.freeze()?))
        }
        BackupSource::Unsupported => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this world's storage cannot be backed up",
        )),
    }
}

fn zip_prefix(level: &str) -> String {
    format!("{}/", file_level(level))
}

/// The step that may run while the world saves again: compress the
/// staging folder or copy the frozen file, then verify and publish the
/// backup under its final name. Returns its size.
pub fn finish(
    plan: &Plan,
    target: &Target,
    snapshot: Snapshot,
    tracker: &Tracker,
) -> io::Result<u64> {
    match snapshot {
        Snapshot::Written => {}
        Snapshot::Staged(files) => {
            zip_folder(
                &target.staging,
                &files,
                &zip_prefix(&plan.level),
                &target.part,
                plan.compression,
                tracker,
            )?;
            fs::remove_dir_all(&target.staging)?;
        }
        Snapshot::Frozen(frozen) => {
            copy_file(frozen.path(), &target.part, tracker, Some(&*frozen))?;
            drop(frozen);
            if let BackupSource::File(file) = &plan.source {
                if !plan.include_caches {
                    file.exclude(&target.part, &derived_prefixes())?;
                }
                tracker.phase(Phase::Verifying, 1, 0);
                file.verify(&target.part)?;
            }
        }
    }
    tracker.check()?;
    tracker.phase(Phase::Finishing, 0, 0);
    fs::rename(&target.part, &target.path)?;
    if let Ok(dir) = File::open(&plan.dir) {
        let _ = dir.sync_all();
    }
    Ok(fs::metadata(&target.path)?.len())
}

/// Removes whatever a failed job left behind.
pub fn discard(target: &Target) {
    let _ = fs::remove_file(&target.part);
    let _ = fs::remove_dir_all(&target.staging);
}

/// Makes a backup of a world no server is running (the caller holds its
/// lock), returning its name and size.
pub fn run_offline(plan: &Plan, tracker: &Tracker) -> io::Result<(String, u64)> {
    let target = plan.target()?;
    let result = snapshot(plan, &target, tracker)
        .and_then(|snapshot| finish(plan, &target, snapshot, tracker));
    match result {
        Ok(size) => Ok((target.name, size)),
        Err(err) => {
            discard(&target);
            Err(err)
        }
    }
}

/// One stored record of a world, for comparing worlds.
#[derive(Debug, PartialEq)]
pub enum Record {
    Document(tuff_nbt::Compound),
    Blob(Vec<u8>),
}

/// Every document and chunk/entity blob of `backend`, keyed by what it
/// is, so a backup can be checked against its source record by record.
pub fn world_records(
    backend: &dyn crate::backend::WorldBackend,
) -> io::Result<std::collections::BTreeMap<String, Record>> {
    let mut records = std::collections::BTreeMap::new();
    for id in backend.documents().list()? {
        if let Some(root) = backend.documents().read(id.borrow())? {
            records.insert(format!("{id:?}"), Record::Document(root));
        }
    }
    for dimension in backend.list_dimensions()? {
        for region in backend.list_regions(&dimension)? {
            let mut store = backend.open_region(&dimension, region)?;
            for pos in store.list_chunks()? {
                if let Some(blob) = store.read_chunk(pos)? {
                    records.insert(format!("{dimension} chunk {pos:?}"), Record::Blob(blob));
                }
                if let Some(blob) = store.read_entities(pos)? {
                    records.insert(format!("{dimension} entities {pos:?}"), Record::Blob(blob));
                }
            }
        }
    }
    Ok(records)
}

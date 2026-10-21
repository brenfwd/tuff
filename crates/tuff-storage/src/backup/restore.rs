//! Checking backups that may come from outside, and restoring one without
//! touching the live world. A restore is staged next to the world while
//! the server runs (`.<world>.restore`, unpacked and checked, committed by
//! the marker `.<world>.restore.json` written last) and applied at the next
//! start, before the world is opened: the current world moves aside to
//! `<world>-before-restore-<yyyymmdd-hhmmss>` and the staged one takes its
//! place by rename.
//!
//! Zip limits: entries are checked strictly (names, links, duplicates,
//! overlaps) and the unpacked total is capped and enforced while
//! inflating, which is what stops a bomb. The ratio cap sits at deflate's
//! own ceiling (about 1032:1), so a real world holding a large file of one
//! repeated byte is accepted while a header claiming more than deflate can
//! produce is refused before anything is inflated.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tuff_vanilla_data::version::DATA_VERSION;

use super::{BackupInfo, ZIP_EXTENSION, file_name, now_unix, space};
use crate::sqlite;
use tuff_nbt::{NamedTag, NbtLimits, Tag};
use tuff_zip::{Archive, Limits};

/// No deflate stream inflates by more than this (258 bytes from 2 bits).
pub const DEFLATE_MAX_RATIO: u64 = 1032;
/// Largest `level.dat` read from a backup.
pub const MAX_LEVEL_DAT: u64 = 16 << 20;
const LEVEL_NBT: NbtLimits = NbtLimits {
    max_depth: 64,
    max_bytes: 32 << 20,
};
const MAX_ENTRIES: usize = 1_000_000;
const MAX_MARKER: u64 = 64 * 1024;
/// Room kept free beyond what a restore writes.
const SPACE_MARGIN: u64 = 64 << 20;
const UPLOAD_PREFIX: &str = ".upload-";
const UPLOAD_SUFFIX: &str = ".part";
const LEVEL_DAT: &str = "level.dat";

/// How a world or a backup is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A world folder, backed up as a zip archive.
    Folder,
    /// A `.mcserver` server file, backed up as a copy.
    ServerFile,
}

impl Kind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Folder => ZIP_EXTENSION,
            Self::ServerFile => sqlite::EXTENSION,
        }
    }
}

/// Why a backup cannot be checked or restored.
#[derive(Debug)]
pub enum RestoreError {
    /// The backup is malformed, hostile or from a newer version.
    Invalid(String),
    /// The backup is of another kind than the world.
    Mismatch(String),
    NoSpace {
        needed: u64,
        free: u64,
    },
    Io(io::Error),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) | Self::Mismatch(why) => f.write_str(why),
            Self::NoSpace { needed, free } => write!(
                f,
                "not enough free disk space: {needed} bytes needed, {free} free"
            ),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RestoreError {}

impl From<io::Error> for RestoreError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn invalid(why: impl std::fmt::Display) -> RestoreError {
    RestoreError::Invalid(why.to_string())
}

/// What checking a backup found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Checked {
    pub kind: Kind,
    /// The folder the world sits in inside a zip (`""` at the top).
    pub root: String,
    pub data_version: i32,
    pub files: u64,
    /// Bytes the world takes once unpacked.
    pub unpacked: u64,
}

/// The reader limits for a zip world unpacking to at most `max_unpacked`.
pub const fn zip_limits(max_unpacked: u64) -> Limits {
    Limits {
        max_entries: MAX_ENTRIES,
        max_entry_bytes: max_unpacked,
        max_total_bytes: max_unpacked,
        max_ratio: Some(DEFLATE_MAX_RATIO),
        strict: true,
    }
}

/// The kind of backup `path` holds, from its first bytes.
pub fn sniff(path: &Path) -> Result<Kind, RestoreError> {
    let mut header = [0u8; 16];
    let mut file = File::open(path)?;
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..])? {
            0 => break,
            n => read += n,
        }
    }
    let header = &header[..read];
    if header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06") {
        Ok(Kind::Folder)
    } else if sqlite::inspect::is_database(header) {
        Ok(Kind::ServerFile)
    } else {
        Err(invalid(
            "the file is neither a zip archive nor a server file",
        ))
    }
}

/// The data version a `level.dat` (gzip or plain NBT) was saved at,
/// refusing one from a newer version than this server.
pub fn level_version(bytes: &[u8]) -> Result<i32, RestoreError> {
    let (named, _) = NamedTag::from_bytes(bytes, LEVEL_NBT)
        .map_err(|err| invalid(format!("level.dat is unreadable: {err}")))?;
    let Tag::Compound(root) = named.tag else {
        return Err(invalid("level.dat does not hold a compound"));
    };
    let data = root
        .get_compound("Data")
        .ok_or_else(|| invalid("level.dat has no Data compound"))?;
    let version = data.get_int("DataVersion").unwrap_or(0);
    if version > DATA_VERSION {
        return Err(invalid(format!(
            "the world was saved by a newer version (data version {version}, this server supports {DATA_VERSION})"
        )));
    }
    Ok(version)
}

/// The one folder (`""` or `"<name>/"`) holding `level.dat` that every
/// entry lies in.
fn world_root(archive: &Archive<File>) -> Result<String, RestoreError> {
    let mut roots = archive
        .entries()
        .iter()
        .filter(|e| !e.is_dir())
        .filter_map(|e| {
            if e.name == LEVEL_DAT {
                return Some(String::new());
            }
            let (dir, file) = e.name.split_once('/')?;
            (file == LEVEL_DAT).then(|| format!("{dir}/"))
        });
    let root = match (roots.next(), roots.next()) {
        (Some(root), None) => root,
        (None, _) => {
            return Err(invalid(
                "the archive holds no world: level.dat is neither at the top nor in one folder",
            ));
        }
        (Some(_), Some(_)) => return Err(invalid("the archive holds more than one world")),
    };
    if let Some(stray) = archive
        .entries()
        .iter()
        .find(|e| !e.name.starts_with(&root))
    {
        return Err(invalid(format!(
            "{:?} lies outside the world folder {root:?}",
            stray.name
        )));
    }
    Ok(root)
}

fn open_zip(path: &Path, max_unpacked: u64) -> Result<(Archive<File>, String), RestoreError> {
    let archive = Archive::open(File::open(path)?, zip_limits(max_unpacked)).map_err(|err| {
        match err.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => invalid(err),
            _ => RestoreError::Io(err),
        }
    })?;
    let root = world_root(&archive)?;
    Ok((archive, root))
}

fn unpacked(archive: &Archive<File>) -> (u64, u64) {
    archive
        .entries()
        .iter()
        .filter(|e| !e.is_dir())
        .fold((0, 0), |(files, bytes), e| (files + 1, bytes + e.size))
}

fn read_error(err: io::Error) -> RestoreError {
    match err.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => invalid(err),
        _ => RestoreError::Io(err),
    }
}

/// Checks the backup at `path` completely: a zip is inflated entry by
/// entry (sizes, checksums and the unpacked cap enforced as it goes), a
/// server file is integrity-checked; either way the world's level data
/// must be readable and not newer than this server.
pub fn inspect(path: &Path, max_unpacked: u64) -> Result<Checked, RestoreError> {
    match sniff(path)? {
        Kind::Folder => inspect_zip(path, max_unpacked),
        Kind::ServerFile => inspect_server_file(path),
    }
}

fn inspect_zip(path: &Path, max_unpacked: u64) -> Result<Checked, RestoreError> {
    let (archive, root) = open_zip(path, max_unpacked)?;
    let level_name = format!("{root}{LEVEL_DAT}");
    let mut level = None;
    let mut buf = vec![0; 64 * 1024];
    for entry in archive.entries().iter().filter(|e| !e.is_dir()) {
        let mut reader = archive.reader(entry).map_err(read_error)?;
        if entry.name == level_name {
            if entry.size > MAX_LEVEL_DAT {
                return Err(invalid("level.dat is too large"));
            }
            let mut bytes = Vec::with_capacity(entry.size as usize);
            reader.read_to_end(&mut bytes).map_err(read_error)?;
            level = Some(bytes);
            continue;
        }
        while reader.read(&mut buf).map_err(read_error)? > 0 {}
    }
    let data_version = level_version(&level.ok_or_else(|| invalid("level.dat is missing"))?)?;
    let (files, unpacked) = unpacked(&archive);
    Ok(Checked {
        kind: Kind::Folder,
        root,
        data_version,
        files,
        unpacked,
    })
}

fn inspect_server_file(path: &Path) -> Result<Checked, RestoreError> {
    let level = sqlite::inspect::inspect(path)
        .map_err(invalid)?
        .ok_or_else(|| invalid("the server file holds no world"))?;
    Ok(Checked {
        kind: Kind::ServerFile,
        root: String::new(),
        data_version: level_version(&level)?,
        files: 1,
        unpacked: fs::metadata(path)?.len(),
    })
}

/// A restore waiting for the next start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    /// The backup's name.
    pub backup: String,
    pub by: String,
    pub staged_unix: u64,
    pub kind: Kind,
}

/// Where a restore of `world` is staged: the unpacked world, the folder
/// or file being written, and the marker that commits it.
#[derive(Clone, Debug)]
pub struct StagePaths {
    pub staged: PathBuf,
    pub partial: PathBuf,
    pub marker: PathBuf,
}

pub fn stage_paths(world: &Path) -> io::Result<StagePaths> {
    let name = world
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "..")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} does not name a world", world.display()),
            )
        })?;
    let sibling = |suffix: &str| world.with_file_name(format!(".{name}{suffix}"));
    Ok(StagePaths {
        staged: sibling(".restore"),
        partial: sibling(".restore.partial"),
        marker: sibling(".restore.json"),
    })
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn read_marker(marker: &Path) -> Option<Staged> {
    let file = File::open(marker).ok()?;
    let mut text = Vec::new();
    file.take(MAX_MARKER).read_to_end(&mut text).ok()?;
    serde_json::from_slice(&text).ok()
}

/// The restore staged for `world`, if a complete one is.
pub fn staged(world: &Path) -> Option<Staged> {
    let paths = stage_paths(world).ok()?;
    let found = read_marker(&paths.marker)?;
    fs::symlink_metadata(&paths.staged).ok()?;
    Some(found)
}

/// Drops a staged restore of `world`, complete or not.
pub fn unstage(world: &Path) -> io::Result<()> {
    let paths = stage_paths(world)?;
    remove_any(&paths.marker)?;
    remove_any(&paths.partial)?;
    remove_any(&paths.staged)
}

fn check_space(dir: &Path, bytes: u64) -> Result<(), RestoreError> {
    let needed = bytes
        .saturating_add(bytes / 10)
        .saturating_add(SPACE_MARGIN);
    match space::available(dir) {
        Some(free) if free < needed => Err(RestoreError::NoSpace { needed, free }),
        _ => Ok(()),
    }
}

fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Unpacks or copies the backup at `backup` next to `world` (a world of
/// `kind`), checking it as it goes, and commits it for the next start.
/// Replaces a restore staged earlier. `progress` hears bytes written out
/// of the total and may stop the staging by failing.
pub fn stage(
    backup: &Path,
    name: &str,
    world: &Path,
    kind: Kind,
    max_unpacked: u64,
    by: &str,
    mut progress: impl FnMut(u64, u64) -> io::Result<()>,
) -> Result<Staged, RestoreError> {
    let found = sniff(backup)?;
    if found != kind {
        return Err(RestoreError::Mismatch(match kind {
            Kind::Folder => "this world is a folder: restore a .zip backup".to_owned(),
            Kind::ServerFile => {
                "this world is a server file: restore a .mcserver backup".to_owned()
            }
        }));
    }
    let paths = stage_paths(world)?;
    unstage(world)?;
    let result = match kind {
        Kind::Folder => stage_zip(backup, &paths.partial, max_unpacked, &mut progress),
        Kind::ServerFile => stage_server_file(backup, &paths.partial, &mut progress),
    };
    if let Err(err) = result {
        let _ = remove_any(&paths.partial);
        return Err(err);
    }
    fs::rename(&paths.partial, &paths.staged)?;
    let staged = Staged {
        backup: name.to_owned(),
        by: by.to_owned(),
        staged_unix: now_unix(),
        kind,
    };
    let temp = paths.marker.with_extension("json.tmp");
    let written = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(&serde_json::to_vec(&staged).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temp, &paths.marker)
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        let _ = remove_any(&paths.staged);
        return Err(err.into());
    }
    sync_dir(parent(world));
    Ok(staged)
}

fn sync_dir(dir: &Path) {
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

fn stage_zip(
    backup: &Path,
    into: &Path,
    max_unpacked: u64,
    progress: &mut impl FnMut(u64, u64) -> io::Result<()>,
) -> Result<(), RestoreError> {
    let (archive, root) = open_zip(backup, max_unpacked)?;
    let (_, total) = unpacked(&archive);
    check_space(parent(into), total)?;
    fs::create_dir(into)?;
    let mut done = 0;
    let mut buf = vec![0; 64 * 1024];
    for entry in archive.entries() {
        let rel = &entry.name[root.len()..];
        if rel.is_empty() {
            continue;
        }
        let path: PathBuf = rel
            .trim_end_matches('/')
            .split('/')
            .fold(into.to_path_buf(), |path, part| path.join(part));
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut out = File::create_new(&path)?;
        let mut reader = archive.reader(entry).map_err(read_error)?;
        loop {
            let n = reader.read(&mut buf).map_err(read_error)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total)?;
        }
        out.sync_all()?;
    }
    let level = into.join(LEVEL_DAT);
    if fs::metadata(&level)?.len() > MAX_LEVEL_DAT {
        return Err(invalid("level.dat is too large"));
    }
    level_version(&fs::read(level)?)?;
    Ok(())
}

fn stage_server_file(
    backup: &Path,
    into: &Path,
    progress: &mut impl FnMut(u64, u64) -> io::Result<()>,
) -> Result<(), RestoreError> {
    let total = fs::metadata(backup)?.len();
    check_space(parent(into), total)?;
    let mut from = File::open(backup)?;
    let mut out = File::create_new(into)?;
    let mut buf = vec![0; 1 << 20];
    let mut done = 0;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total)?;
    }
    out.sync_all()?;
    drop(out);
    inspect_server_file(into)?;
    Ok(())
}

/// What applying a staged restore did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub backup: String,
    /// Where the world it replaced went, if there was one.
    pub previous: Option<PathBuf>,
}

fn aside(world: &Path, kind: Kind) -> io::Result<PathBuf> {
    let name = world
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad world path"))?;
    let (stem, extension) = match kind {
        Kind::Folder => (name, None),
        Kind::ServerFile => match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
            _ => (name, None),
        },
    };
    let now = now_unix();
    for n in 1..1000 {
        let named = file_name(&format!("{stem}-before-restore"), now, n, "x");
        let base = named.strip_suffix(".x").unwrap_or(&named);
        let candidate = world.with_file_name(match extension {
            Some(ext) => format!("{base}.{ext}"),
            None => base.to_owned(),
        });
        if fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the replaced world",
    ))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Applies the restore staged for `world`, if there is a complete one.
/// `probe` runs first when a world exists and must fail if it is in use
/// (it takes and releases the world's lock). Safe to re-run after an
/// interruption at any point. Leftovers of an incomplete staging are
/// removed.
pub fn apply_staged(
    world: &Path,
    probe: impl FnOnce() -> io::Result<()>,
) -> io::Result<Option<Applied>> {
    let paths = stage_paths(world)?;
    remove_any(&paths.partial)?;
    let Some(staged) = read_marker(&paths.marker) else {
        remove_any(&paths.marker)?;
        remove_any(&paths.staged)?;
        return Ok(None);
    };
    if fs::symlink_metadata(&paths.staged).is_err() {
        log::warn!(
            "the restore of {} staged for {} is gone; nothing restored",
            staged.backup,
            world.display()
        );
        remove_any(&paths.marker)?;
        return Ok(None);
    }
    let previous = if fs::symlink_metadata(world).is_ok() {
        probe()?;
        let to = aside(world, staged.kind)?;
        fs::rename(world, &to)?;
        for suffix in ["-wal", "-shm"] {
            let from = with_suffix(world, suffix);
            if from.exists() {
                fs::rename(&from, with_suffix(&to, suffix))?;
            }
        }
        Some(to)
    } else {
        None
    };
    fs::rename(&paths.staged, world)?;
    fs::remove_file(&paths.marker)?;
    sync_dir(parent(world));
    Ok(Some(Applied {
        backup: staged.backup,
        previous,
    }))
}

/// A fresh temporary file for an upload into `dir`.
pub fn upload_path(dir: &Path) -> PathBuf {
    dir.join(format!(
        "{UPLOAD_PREFIX}{:016x}{UPLOAD_SUFFIX}",
        rand::random::<u64>()
    ))
}

/// Removes uploads a stopped server left unfinished in `dir`.
pub fn clean_uploads(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(UPLOAD_PREFIX) && name.ends_with(UPLOAD_SUFFIX) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Lists a checked upload at `temp` as a backup of `level` named for now.
pub fn publish_upload(dir: &Path, level: &str, temp: &Path, kind: Kind) -> io::Result<BackupInfo> {
    let now = now_unix();
    for n in 1..1000 {
        let name = file_name(level, now, n, kind.extension());
        let path = dir.join(&name);
        let part = dir.join(format!("{name}.part"));
        if path.exists() || part.exists() {
            continue;
        }
        fs::rename(temp, &path)?;
        sync_dir(dir);
        return Ok(BackupInfo {
            size: fs::metadata(&path)?.len(),
            name,
            created_unix: now,
        });
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free backup name",
    ))
}

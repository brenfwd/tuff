//! Adding, listing and removing a world's own packs (`datapacks/`) through
//! its backend, so a folder world and a server file work alike. Front ends
//! (the `datapack` subcommand today) turn their input into an [`Upload`]
//! and call these functions; everything they need to refuse is refused
//! here.
//!
//! Uploads are untrusted. They are read within the loader's caps, must hold
//! a readable `pack.mcmeta` at their root, and may only name files below
//! their own pack; a zip is fully inflated and checksummed once before it
//! is stored. A pack is stored as given: a zip as `datapacks/<name>.zip`, a
//! folder as files below `datapacks/<name>/`.
//!
//! These functions change storage directly, so they are for worlds no
//! server is running ([`OfflineWorld`] takes the world's lock). A running
//! server keeps its selection in memory and saves it over `level.dat`; it
//! must add files through [`add`] and then reload through
//! [`super::DataPacks`] instead of calling [`enable`].

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use super::{
    DIRECTORY, DataPackConfig, Files, MAX_ARCHIVE, MAX_ENTRIES, MAX_FILE, MAX_TOTAL, METADATA,
    PackMeta, directory_files, read_archive, read_meta,
};
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::backend::WorldLock;
use tuff_storage::document::Document;
use tuff_storage::sqlite::{self, Locking, SqliteBackend};
use tuff_storage::vanilla::VanillaBackend;

/// The longest pack name accepted for a new pack.
pub const MAX_NAME: usize = 64;
const MAX_PATH: usize = 512;
const MAX_DEPTH: usize = 32;
const ZIP_SUFFIX: &str = ".zip";
const LEVEL_FILE: &str = "level.dat";

#[derive(Debug)]
pub enum StoreError {
    /// A pack name, or a file name inside a pack, that is not allowed.
    Name(String),
    /// Input that is not a usable pack.
    Invalid(String),
    /// Input over a size, file count or depth limit.
    TooLarge(String),
    Exists(String),
    NotFound(String),
    /// Another process, usually a running server, holds the world.
    Locked(String),
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(message)
            | Self::Invalid(message)
            | Self::TooLarge(message)
            | Self::Exists(message)
            | Self::NotFound(message)
            | Self::Locked(message) => f.write_str(message),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

fn too_large(message: impl Into<String>) -> StoreError {
    StoreError::TooLarge(message.into())
}

/// A pack handed in to be stored.
#[derive(Debug, Clone)]
pub enum Upload {
    /// A zip archive's bytes.
    Zip(Vec<u8>),
    /// A folder's files, by `/`-separated path relative to the pack root.
    Files(Vec<(String, Vec<u8>)>),
}

impl Upload {
    /// Reads a zip archive or a pack folder from the local file system,
    /// within the same caps as any other upload. Symbolic links are
    /// refused so nothing outside the folder is picked up.
    pub fn read_path(path: &Path) -> Result<Self, StoreError> {
        let meta = fs::symlink_metadata(path).map_err(|err| {
            StoreError::NotFound(format!("cannot read {}: {err}", path.display()))
        })?;
        if meta.file_type().is_symlink() {
            let target = fs::metadata(path)?;
            if target.is_dir() {
                return Err(invalid(format!(
                    "{} is a symbolic link; give the folder itself",
                    path.display()
                )));
            }
        }
        if fs::metadata(path)?.is_dir() {
            let mut files = Vec::new();
            let mut total = 0;
            walk(path, "", 0, &mut files, &mut total)?;
            Ok(Self::Files(files))
        } else {
            read_capped(path, MAX_ARCHIVE).map(Self::Zip)
        }
    }

    fn is_zip(&self) -> bool {
        matches!(self, Self::Zip(_))
    }
}

fn read_capped(path: &Path, max: usize) -> Result<Vec<u8>, StoreError> {
    let file = File::open(path)?;
    let declared = file.metadata()?.len();
    let too_big = || too_large(format!("{} is over the {max} byte limit", path.display()));
    if declared > max as u64 {
        return Err(too_big());
    }
    let mut data = Vec::with_capacity(declared as usize);
    file.take(max as u64 + 1).read_to_end(&mut data)?;
    if data.len() > max {
        return Err(too_big());
    }
    Ok(data)
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    files: &mut Vec<(String, Vec<u8>)>,
    total: &mut usize,
) -> Result<(), StoreError> {
    if depth > MAX_DEPTH {
        return Err(too_large(format!(
            "{} is nested more than {MAX_DEPTH} folders deep",
            dir.display()
        )));
    }
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(StoreError::Name(format!(
                "{} is not a UTF-8 file name",
                path.display()
            )));
        };
        let relative = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid(format!(
                "{} is a symbolic link, which packs may not hold",
                path.display()
            )));
        } else if kind.is_dir() {
            walk(&path, &format!("{relative}/"), depth + 1, files, total)?;
        } else if kind.is_file() {
            if files.len() >= MAX_ENTRIES {
                return Err(too_large(format!("pack has over {MAX_ENTRIES} files")));
            }
            let data = read_capped(&path, MAX_FILE)?;
            *total += data.len();
            if *total > MAX_ARCHIVE {
                return Err(too_large(format!(
                    "pack is over the {MAX_ARCHIVE} byte limit"
                )));
            }
            files.push((relative, data));
        }
    }
    Ok(())
}

/// Whether `name` is allowed for a new pack: ASCII letters, digits and
/// `_ - . +`, not starting with a dot, and not a name some file system
/// reserves.
pub fn valid_name(name: &str) -> bool {
    let reserved = {
        let upper = name.to_ascii_uppercase();
        let stem = upper.split('.').next().unwrap_or("");
        matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    };
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
        && !reserved
}

/// A usable pack name made from a file or folder name: disallowed
/// characters become `_`, and the result is cut to [`MAX_NAME`].
pub fn suggested_name(path: &Path) -> Option<String> {
    let base = path.file_name()?.to_string_lossy();
    let mut name: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem_len = name.strip_suffix(ZIP_SUFFIX).map_or(name.len(), str::len);
    let keep = MAX_NAME - ZIP_SUFFIX.len();
    if stem_len > keep {
        let suffix = name[stem_len..].to_owned();
        name.truncate(keep);
        name.push_str(&suffix);
    }
    let name = name
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_owned();
    (!name.is_empty()).then_some(name)
}

/// The stored name of a pack called `name`: a zip always ends in `.zip`,
/// a folder never does.
pub fn stored_name(name: &str, upload: &Upload) -> Result<String, StoreError> {
    let name = name.strip_prefix("file/").unwrap_or(name);
    let name = if upload.is_zip() && !name.ends_with(ZIP_SUFFIX) {
        format!("{name}{ZIP_SUFFIX}")
    } else {
        name.to_owned()
    };
    if !upload.is_zip() && name.ends_with(ZIP_SUFFIX) {
        return Err(StoreError::Name(format!(
            "a folder pack's name may not end in {ZIP_SUFFIX}: {name:?}"
        )));
    }
    if !valid_name(&name) {
        return Err(StoreError::Name(format!(
            "invalid pack name {name:?}: use up to {MAX_NAME} ASCII letters, digits and _ - . +, \
             not starting or ending with a dot"
        )));
    }
    Ok(name)
}

/// Whether `path` may be stored inside a pack: relative, `/`-separated,
/// with no empty, `.` or `..` segment and no backslash, colon or control
/// character.
pub fn valid_file_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

struct Checked {
    meta: PackMeta,
    files: usize,
    bytes: usize,
}

fn wrapped_root(names: &[&str]) -> Option<String> {
    let first = names.first()?.split_once('/')?.0;
    names
        .iter()
        .all(|name| name.split_once('/').is_some_and(|(top, _)| top == first))
        .then(|| first.to_owned())
        .filter(|top| names.contains(&format!("{top}/{METADATA}").as_str()))
}

fn missing_meta(names: &[&str]) -> StoreError {
    match wrapped_root(names) {
        Some(top) => invalid(format!(
            "no {METADATA} at the pack root; the pack is inside {top}/, so pack that folder's \
             contents instead"
        )),
        None => invalid(format!("no {METADATA} at the pack root")),
    }
}

fn check_names(names: &[&str]) -> Result<(), StoreError> {
    match names.iter().find(|name| !valid_file_path(name)) {
        Some(bad) => Err(StoreError::Name(format!(
            "pack holds a file with a disallowed path: {bad:?}"
        ))),
        None => Ok(()),
    }
}

fn parse_meta(bytes: &[u8]) -> Result<PackMeta, StoreError> {
    PackMeta::parse(bytes).map_err(|err| invalid(format!("unreadable {METADATA}: {err}")))
}

/// The strict reader's structural checks (links, duplicate and case-folded
/// names, overlapping entries); sizes are enforced while each entry is
/// read within the loader's caps.
fn structure_limits() -> tuff_zip::Limits {
    tuff_zip::Limits {
        max_entries: MAX_ENTRIES,
        max_entry_bytes: u64::MAX,
        max_total_bytes: u64::MAX,
        max_ratio: None,
        strict: true,
    }
}

/// The feature flags every world has.
pub const WORLD_FEATURES: &[&str] = &["minecraft:vanilla"];

/// The feature flags `meta` needs that this world lacks; a pack needing
/// any cannot be enabled.
pub fn missing_features(meta: &PackMeta) -> Vec<&str> {
    meta.features
        .iter()
        .map(String::as_str)
        .filter(|flag| !WORLD_FEATURES.contains(flag))
        .collect()
}

fn check(upload: &Upload) -> Result<Checked, StoreError> {
    match upload {
        Upload::Zip(bytes) => {
            if bytes.len() > MAX_ARCHIVE {
                return Err(too_large(format!(
                    "archive is over the {MAX_ARCHIVE} byte limit"
                )));
            }
            let archive = tuff_datapack::zip::Archive::open(bytes, MAX_ENTRIES)
                .map_err(|err| invalid(format!("unreadable zip archive: {err}")))?;
            let names = Files::names(&archive);
            check_names(&names)?;
            tuff_zip::Archive::open(bytes.as_slice(), structure_limits())
                .map_err(|err| invalid(format!("unsafe zip archive: {err}")))?;
            if !archive.contains(METADATA) {
                return Err(missing_meta(&names));
            }
            let mut budget = MAX_TOTAL;
            for name in &names {
                let limit = MAX_FILE.min(budget);
                if archive.size(name).is_some_and(|size| size > limit as u64) {
                    return Err(too_large(format!(
                        "{name:?} is over the {limit} byte limit"
                    )));
                }
                let data = archive
                    .read(name, limit)
                    .map_err(|err| invalid(format!("unreadable zip archive: {err}")))?;
                budget -= data.len();
            }
            let meta = read_meta(&archive)
                .map_err(|err| invalid(format!("unreadable {METADATA}: {err}")))?;
            Ok(Checked {
                meta,
                files: names.len(),
                bytes: bytes.len(),
            })
        }
        Upload::Files(files) => {
            if files.len() > MAX_ENTRIES {
                return Err(too_large(format!("pack has over {MAX_ENTRIES} files")));
            }
            let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
            check_names(&names)?;
            let mut seen = std::collections::HashSet::new();
            let mut bytes = 0usize;
            for (name, data) in files {
                if !seen.insert(name.as_str()) {
                    return Err(StoreError::Name(format!("pack lists {name:?} twice")));
                }
                if data.len() > MAX_FILE {
                    return Err(too_large(format!(
                        "{name:?} is over the {MAX_FILE} byte limit"
                    )));
                }
                bytes += data.len();
            }
            if bytes > MAX_ARCHIVE {
                return Err(too_large(format!(
                    "pack is over the {MAX_ARCHIVE} byte limit"
                )));
            }
            let meta = files
                .iter()
                .find(|(name, _)| name == METADATA)
                .ok_or_else(|| missing_meta(&names))?;
            Ok(Checked {
                meta: parse_meta(&meta.1)?,
                files: files.len(),
                bytes,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    Zip,
}

/// Whether the world's saved selection enables a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Enabled,
    Disabled,
    /// Not in the saved selection, so the server enables it on its next
    /// start.
    New,
}

/// A pack stored in the world.
#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// The metadata, or why it cannot be read.
    pub meta: Result<PackMeta, String>,
    pub selection: Selection,
    paths: Vec<String>,
}

impl Entry {
    /// The id the server and `/datapack` know it by.
    pub fn id(&self) -> String {
        format!("file/{}", self.name)
    }

    /// The stored files: one for a zip, every file for a folder.
    pub fn files(&self) -> usize {
        self.paths.len()
    }
}

fn scan(backend: &dyn WorldBackend) -> io::Result<Vec<Entry>> {
    let paths = match backend.list_extra_files_under(DIRECTORY) {
        Ok(paths) => paths,
        Err(err) if err.kind() == io::ErrorKind::Unsupported => Vec::new(),
        Err(err) => return Err(err),
    };
    let mut entries: Vec<Entry> = Vec::new();
    for path in paths {
        let Some(rest) = path.strip_prefix(DIRECTORY) else {
            continue;
        };
        let (name, kind) = match rest.split_once('/') {
            Some((name, _)) => (name, Kind::Directory),
            None if rest.ends_with(ZIP_SUFFIX) => (rest, Kind::Zip),
            None => continue,
        };
        if name.is_empty() {
            continue;
        }
        match entries
            .iter_mut()
            .find(|entry| entry.name == name && entry.kind == kind)
        {
            Some(entry) => entry.paths.push(path),
            None => entries.push(Entry {
                name: name.to_owned(),
                kind,
                meta: Err(String::new()),
                selection: Selection::New,
                paths: vec![path],
            }),
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn entry_meta(backend: &dyn WorldBackend, entry: &Entry) -> Result<PackMeta, String> {
    let meta = match entry.kind {
        Kind::Directory => directory_files(backend, &format!("{DIRECTORY}{}/", entry.name))
            .and_then(|files| {
                if !Files::names(&files).contains(&METADATA) {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no {METADATA}"),
                    ));
                }
                read_meta(&files)
            }),
        Kind::Zip => read_archive(backend, &entry.paths[0]).and_then(|bytes| {
            let archive = tuff_datapack::zip::Archive::open(&bytes, MAX_ENTRIES)?;
            read_meta(&archive)
        }),
    };
    meta.map_err(|err| err.to_string())
}

fn find(backend: &dyn WorldBackend, name: &str) -> io::Result<Option<Entry>> {
    Ok(scan(backend)?.into_iter().find(|entry| entry.name == name))
}

/// Every pack stored in the world, by name, with its metadata and whether
/// the saved selection enables it.
pub fn list(backend: &dyn WorldBackend) -> Result<Vec<Entry>, StoreError> {
    let config = saved_selection(backend)?;
    let mut entries = scan(backend)?;
    for entry in &mut entries {
        entry.meta = entry_meta(backend, entry);
        let id = entry.id();
        entry.selection = match &config {
            Some(config) if config.enabled.contains(&id) => Selection::Enabled,
            Some(config) if config.disabled.contains(&id) => Selection::Disabled,
            _ => Selection::New,
        };
    }
    Ok(entries)
}

/// What [`add`] stored.
#[derive(Debug)]
pub struct Added {
    pub name: String,
    pub meta: PackMeta,
    pub files: usize,
    pub bytes: usize,
    /// Whether a pack of the same name was replaced.
    pub replaced: bool,
}

impl Added {
    pub fn id(&self) -> String {
        format!("file/{}", self.name)
    }
}

fn delete(backend: &dyn WorldBackend, paths: &[String]) -> io::Result<()> {
    for path in paths {
        backend.remove_extra_file(path)?;
    }
    Ok(())
}

/// Validates `upload` and stores it as the pack `name` (see
/// [`stored_name`]). An existing pack of that name is refused unless
/// `replace`. Nothing is written unless the whole upload is valid; a write
/// that fails part way removes what it wrote.
pub fn add(
    backend: &dyn WorldBackend,
    name: &str,
    upload: &Upload,
    replace: bool,
) -> Result<Added, StoreError> {
    let name = stored_name(name, upload)?;
    let checked = check(upload)?;
    let existing = find(backend, &name)?;
    if existing.is_some() && !replace {
        return Err(StoreError::Exists(format!(
            "pack file/{name} already exists"
        )));
    }
    if let Some(existing) = &existing {
        delete(backend, &existing.paths)?;
    }
    let mut written = Vec::new();
    let result = match upload {
        Upload::Zip(bytes) => {
            let path = format!("{DIRECTORY}{name}");
            written.push(path.clone());
            backend.write_extra_file(&path, bytes)
        }
        Upload::Files(files) => files.iter().try_for_each(|(file, data)| {
            let path = format!("{DIRECTORY}{name}/{file}");
            written.push(path.clone());
            backend.write_extra_file(&path, data)
        }),
    };
    if let Err(err) = result {
        let _ = delete(backend, &written);
        return Err(err.into());
    }
    Ok(Added {
        name,
        meta: checked.meta,
        files: checked.files,
        bytes: checked.bytes,
        replaced: existing.is_some(),
    })
}

/// Deletes the pack `name` (`file/` prefix optional) and returns what it
/// was. The saved selection is left alone: the server drops a missing
/// pack from it on its next start.
pub fn remove(backend: &dyn WorldBackend, name: &str) -> Result<Entry, StoreError> {
    let name = name.strip_prefix("file/").unwrap_or(name);
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(StoreError::Name(format!("invalid pack name {name:?}")));
    }
    let entry = find(backend, name)?
        .ok_or_else(|| StoreError::NotFound(format!("no pack file/{name} in the world")))?;
    delete(backend, &entry.paths)?;
    Ok(entry)
}

/// The pack `name` as one zip archive, with the file name to save it as: a
/// zip pack's own bytes, or a folder pack's files zipped on the fly
/// (deterministically), within the loader's caps.
pub fn archive(backend: &dyn WorldBackend, name: &str) -> Result<(String, Vec<u8>), StoreError> {
    let name = name.strip_prefix("file/").unwrap_or(name);
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(StoreError::Name(format!("invalid pack name {name:?}")));
    }
    let entry = find(backend, name)?
        .ok_or_else(|| StoreError::NotFound(format!("no pack file/{name} in the world")))?;
    match entry.kind {
        Kind::Zip => Ok((
            entry.name.clone(),
            read_archive(backend, &entry.paths[0]).map_err(|err| {
                if err.kind() == io::ErrorKind::InvalidData {
                    too_large(err.to_string())
                } else {
                    err.into()
                }
            })?,
        )),
        Kind::Directory => {
            let prefix = format!("{DIRECTORY}{}/", entry.name);
            let mut files = Vec::with_capacity(entry.paths.len());
            let mut total = 0usize;
            for path in &entry.paths {
                let data = backend.read_extra_file_capped(path, MAX_FILE)?;
                total += data.len();
                if total > MAX_TOTAL {
                    return Err(too_large(format!(
                        "pack is over the {MAX_TOTAL} byte limit"
                    )));
                }
                let relative = path.strip_prefix(&prefix).unwrap_or(path).to_owned();
                files.push((relative, data));
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
            let entries: Vec<(&str, &[u8])> = files
                .iter()
                .map(|(name, data)| (name.as_str(), data.as_slice()))
                .collect();
            let bytes = tuff_zip::write_all(&entries, tuff_zip::DosTime::MIN, 6, u64::MAX)?;
            Ok((format!("{}{ZIP_SUFFIX}", entry.name), bytes))
        }
    }
}

fn level_selection(root: &Compound) -> Option<DataPackConfig> {
    let packs = root.get_compound("Data")?.get_compound("DataPacks")?;
    let names = |key: &str| -> Vec<String> {
        packs
            .get_list(key)
            .map(|list| {
                list.iter()
                    .filter_map(|tag| tag.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(DataPackConfig {
        enabled: names("Enabled"),
        disabled: names("Disabled"),
    })
}

/// The world's saved selection, `None` when its level data has none yet
/// (every pack is then enabled on the first start).
pub fn saved_selection(backend: &dyn WorldBackend) -> io::Result<Option<DataPackConfig>> {
    Ok(backend
        .documents()
        .read(Document::Level)?
        .as_ref()
        .and_then(level_selection))
}

/// Puts `id` at the top of the saved selection and out of the disabled
/// list, as `/datapack enable` would. False when the world has no level
/// data yet, in which case the pack is enabled on the first start anyway.
pub fn enable(backend: &dyn WorldBackend, id: &str) -> Result<bool, StoreError> {
    backend.upgrade()?;
    let documents = backend.documents();
    let Some(mut root) = documents.read(Document::Level)? else {
        return Ok(false);
    };
    let mut config = level_selection(&root).unwrap_or_default();
    let Some(data) = root.get_compound_mut("Data") else {
        return Ok(false);
    };
    config.disabled.retain(|other| other != id);
    if !config.enabled.iter().any(|other| other == id) {
        config.enabled.push(id.to_owned());
    }
    let list = |names: &[String]| Tag::List(names.iter().map(|n| Tag::from(n.as_str())).collect());
    data.insert(
        "DataPacks",
        Compound::new()
            .with("Enabled", list(&config.enabled))
            .with("Disabled", list(&config.disabled)),
    );
    documents.write(Document::Level, root)?;
    Ok(true)
}

fn locked_or(err: io::Error) -> StoreError {
    if err.kind() == io::ErrorKind::WouldBlock {
        StoreError::Locked(format!("{err}; stop the server first"))
    } else {
        StoreError::Io(err)
    }
}

/// A world opened for offline changes, holding its lock so no server can
/// start on it meanwhile. A folder is a vanilla world (it must hold
/// `level.dat`); a file is a server file.
pub struct OfflineWorld {
    lock: Option<Box<dyn WorldLock>>,
    guard: Option<File>,
    backend: Arc<dyn WorldBackend>,
}

impl OfflineWorld {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let meta = fs::metadata(path).map_err(|err| {
            StoreError::NotFound(format!("no world at {}: {err}", path.display()))
        })?;
        if meta.is_dir() {
            if !path.join(LEVEL_FILE).is_file() {
                let hint = [
                    crate::world::DEFAULT_LEVEL_NAME.to_owned(),
                    format!("{}.{}", crate::world::DEFAULT_LEVEL_NAME, sqlite::EXTENSION),
                ]
                .into_iter()
                .map(|candidate| path.join(candidate))
                .find(|candidate| candidate.exists())
                .map(|candidate| format!(" (did you mean {}?)", candidate.display()))
                .unwrap_or_default();
                return Err(StoreError::NotFound(format!(
                    "{} is not a world: it has no {LEVEL_FILE}{hint}",
                    path.display()
                )));
            }
            let backend: Arc<dyn WorldBackend> = Arc::new(VanillaBackend::new(path));
            let lock = backend.lock().map_err(locked_or)?;
            crate::storage::recover_journal(&*backend, path).map_err(StoreError::Io)?;
            return Ok(Self {
                lock: Some(lock),
                guard: None,
                backend,
            });
        }
        if meta.len() == 0 {
            return Err(StoreError::NotFound(format!(
                "{} is empty, not a server file",
                path.display()
            )));
        }
        let guard = lock_guard(path)?;
        let backend: Arc<dyn WorldBackend> =
            Arc::new(SqliteBackend::open_with(path, Locking::Exclusive).map_err(locked_or)?);
        let lock = backend.lock().map_err(locked_or)?;
        crate::storage::recover_journal(&*backend, path).map_err(StoreError::Io)?;
        Ok(Self {
            lock: Some(lock),
            guard,
            backend,
        })
    }

    pub fn backend(&self) -> &dyn WorldBackend {
        &*self.backend
    }
}

impl Drop for OfflineWorld {
    fn drop(&mut self) {
        drop(self.lock.take());
        drop(self.guard.take());
    }
}

/// Holds the lock file a server in `normal` locking mode keeps beside its
/// server file, when there is one.
fn lock_guard(file: &Path) -> Result<Option<File>, StoreError> {
    let path = sqlite::lock_path(file);
    let guard = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(guard) => guard,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    guard.try_lock().map_err(|_| {
        StoreError::Locked(format!(
            "{} is already in use by another server; stop the server first",
            file.display()
        ))
    })?;
    Ok(Some(guard))
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

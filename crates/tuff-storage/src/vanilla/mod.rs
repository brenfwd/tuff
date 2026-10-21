//! The vanilla backend: a world directory in the game's own 26.x layout
//! (`level.dat`, saved data files, per-dimension region and entity files,
//! player files), so worlds move between this server and the real one.

pub mod config;
pub mod symlinks;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::{BlobCodec, Documents, RegionStore, WorldBackend, WorldLock};
use crate::blobs::NbtBlobCodec;
use crate::document::{Document, DocumentId};
use crate::region_file::{ChunkCompression, RawChunk, RegionFile, parse_region_file_name};
use tuff_nbt::Compound;
use tuff_world_model::pos::{ChunkPos, FileRegionPos};

pub const NAME: &str = "vanilla";
use crate::files::{read_nbt_file, safe_replace, write_nbt_file};
const SESSION_LOCK: &str = "session.lock";
/// The save journal's directory inside the world (see [`crate::journal`]).
pub const JOURNAL_DIR: &str = "tuff-journal";
/// Vanilla writes a snowman (U+2603) into `session.lock`.
const SESSION_LOCK_CONTENT: &[u8] = b"\xE2\x98\x83";

/// Paths within one world directory (e.g. `<instance>/world`).
#[derive(Debug, Clone)]
pub struct WorldPaths {
    root: PathBuf,
}

impl WorldPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn level_dat(&self) -> PathBuf {
        self.root.join("level.dat")
    }

    /// `data/<namespace>/<path>.dat` for world-wide saved data.
    pub fn saved_data(&self, id: &str) -> PathBuf {
        resolve_id(&self.root.join("data"), id, ".dat")
    }

    pub fn dimension(&self, dimension: &str) -> PathBuf {
        resolve_id(&self.root.join("dimensions"), dimension, "")
    }

    /// Saved data scoped to one dimension, e.g. its world border.
    pub fn dimension_data(&self, dimension: &str, id: &str) -> PathBuf {
        resolve_id(&self.dimension(dimension).join("data"), id, ".dat")
    }

    pub fn region_dir(&self, dimension: &str) -> PathBuf {
        self.dimension(dimension).join("region")
    }

    pub fn entities_dir(&self, dimension: &str) -> PathBuf {
        self.dimension(dimension).join("entities")
    }

    pub fn poi_dir(&self, dimension: &str) -> PathBuf {
        self.dimension(dimension).join("poi")
    }

    pub fn player_data_dir(&self) -> PathBuf {
        self.root.join("players").join("data")
    }

    pub fn advancements_file(&self, uuid: u128) -> PathBuf {
        self.root
            .join("players")
            .join("advancements")
            .join(format!("{}.json", player_file_stem(uuid)))
    }

    pub fn stats_file(&self, uuid: u128) -> PathBuf {
        self.root
            .join("players")
            .join("stats")
            .join(format!("{}.json", player_file_stem(uuid)))
    }

    /// Vanilla names player files after the hyphenated UUID.
    pub fn plugin_player_file(&self, id: &str, uuid: u128) -> PathBuf {
        self.root
            .join("data")
            .join("players")
            .join(id)
            .join(format!("{}.dat", player_file_stem(uuid)))
    }

    pub fn player_file(&self, uuid: u128, suffix: &str) -> PathBuf {
        self.player_data_dir()
            .join(format!("{}{suffix}", player_file_stem(uuid)))
    }
}

pub fn player_file_stem(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `namespace:path` resolved as `<base>/<namespace>/<path><suffix>`; a bare
/// path uses the `minecraft` namespace.
fn resolve_id(base: &Path, id: &str, suffix: &str) -> PathBuf {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    base.join(namespace).join(format!("{path}{suffix}"))
}

/// Lists every dimension directory (`<namespace>/<name>`, one level deep,
/// matching how vanilla itself only ever nests one level under
/// `dimensions/`) as a `namespace:name` id.
fn list_dimension_dirs(root: &Path) -> io::Result<Vec<String>> {
    let mut dims = Vec::new();
    let Ok(namespaces) = fs::read_dir(root.join("dimensions")) else {
        return Ok(dims);
    };
    for ns_entry in namespaces {
        let ns_entry = ns_entry?;
        if !ns_entry.file_type()?.is_dir() {
            continue;
        }
        let namespace = ns_entry.file_name().to_string_lossy().into_owned();
        if namespace == "players" {
            continue;
        }
        let Ok(names) = fs::read_dir(ns_entry.path()) else {
            continue;
        };
        for name_entry in names {
            let name_entry = name_entry?;
            if name_entry.file_type()?.is_dir() {
                let name = name_entry.file_name().to_string_lossy().into_owned();
                dims.push(format!("{namespace}:{name}"));
            }
        }
    }
    dims.sort();
    Ok(dims)
}

/// Every `namespace:id` (with `.dat` stripped) of files under `base`, one
/// namespace directory deep then any nesting, matching what [`resolve_id`]
/// with `.dat` produces.
fn list_resource_files(base: &Path) -> io::Result<Vec<String>> {
    let mut ids = Vec::new();
    let Ok(namespaces) = fs::read_dir(base) else {
        return Ok(ids);
    };
    for ns_entry in namespaces {
        let ns_entry = ns_entry?;
        if !ns_entry.file_type()?.is_dir() {
            continue;
        }
        let namespace = ns_entry.file_name().to_string_lossy().into_owned();
        let ns_path = ns_entry.path();
        walk_resource_files(&ns_path, &ns_path, &namespace, &mut ids)?;
    }
    Ok(ids)
}

fn walk_resource_files(
    root: &Path,
    dir: &Path,
    namespace: &str,
    ids: &mut Vec<String>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk_resource_files(root, &path, namespace, ids)?;
        } else if let Some(rel) = path
            .strip_prefix(root)
            .ok()
            .and_then(|rel| rel.to_str())
            .and_then(|rel| rel.strip_suffix(".dat"))
        {
            ids.push(format!("{namespace}:{}", rel.replace('\\', "/")));
        }
    }
    Ok(())
}

/// Copies every stored chunk, entity and point-of-interest record of the
/// world into the tables of a fresh server file, for read-only queries.
fn import_regions(paths: &WorldPaths, file: &Path) -> io::Result<()> {
    let mut conn = crate::sqlite::schema::create_import_file(file)?;
    let sql = |err: rusqlite::Error| io::Error::other(err.to_string());
    let tx = conn.transaction().map_err(sql)?;
    import_documents(paths, &tx)?;
    for dimension in list_dimension_dirs(paths.root())? {
        for (table, dir) in [
            ("chunks", paths.region_dir(&dimension)),
            ("entities", paths.entities_dir(&dimension)),
            ("pois", paths.poi_dir(&dimension)),
        ] {
            let mut insert = tx
                .prepare_cached(&format!(
                    "INSERT OR REPLACE INTO {table}(dimension, x, z, data) VALUES (?1, ?2, ?3, ?4)"
                ))
                .map_err(sql)?;
            for region in list_region_files(&dir)? {
                let Some(mut mca) = RegionFile::open(&dir, region.x, region.z, false)? else {
                    continue;
                };
                for pos in mca.positions(region.x, region.z) {
                    match mca.read(pos) {
                        Ok(Some(raw)) => {
                            insert
                                .execute(rusqlite::params![
                                    dimension,
                                    pos.x,
                                    pos.z,
                                    raw.into_blob()
                                ])
                                .map_err(sql)?;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            log::warn!("skipping damaged chunk {pos:?} in {dimension}: {err}")
                        }
                    }
                }
            }
        }
    }
    tx.commit().map_err(sql)
}

fn import_documents(paths: &WorldPaths, tx: &rusqlite::Transaction<'_>) -> io::Result<()> {
    let sql = |err: rusqlite::Error| io::Error::other(err.to_string());
    let backend = VanillaBackend::new(paths.root());
    let mut insert = tx
        .prepare_cached("INSERT OR REPLACE INTO documents(kind, key, data) VALUES (?1, ?2, ?3)")
        .map_err(sql)?;
    for id in Documents::list(&backend)? {
        let document = id.borrow();
        let root = match Documents::read(&backend, document) {
            Ok(Some(root)) => root,
            Ok(None) => continue,
            Err(err) => {
                log::warn!("skipping unreadable document {id:?}: {err}");
                continue;
            }
        };
        let data = tuff_nbt::NamedTag::new("", root)
            .to_bytes(tuff_nbt::Compression::None)
            .map_err(io::Error::other)?;
        let (kind, key) = crate::sqlite::document_key(document);
        insert
            .execute(rusqlite::params![kind, key, data])
            .map_err(sql)?;
    }
    Ok(())
}

/// Region positions with an `r.<x>.<z>.mca` file in `dir`.
fn list_region_files(dir: &Path) -> io::Result<Vec<FileRegionPos>> {
    let mut regions = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(regions);
    };
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str()
            && let Some((x, z)) = parse_region_file_name(name)
        {
            regions.push(FileRegionPos::new(x, z));
        }
    }
    Ok(regions)
}

/// Top-level relative path segments this backend models as documents or
/// region/entity stores; anything else under the world root is an "extra"
/// file a conversion must carry through unchanged.
fn is_modeled_path(rel: &Path) -> bool {
    let mut components = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match components.next().as_deref() {
        Some("level.dat" | "level.dat_old" | "session.lock" | JOURNAL_DIR) => true,
        Some("data") => true,
        Some("players") => matches!(
            components.next().as_deref(),
            Some("data" | "stats" | "advancements")
        ),
        Some("dimensions") => {
            // <namespace>/<name>/<kind>/...
            let _namespace = components.next();
            let _name = components.next();
            matches!(
                components.next().as_deref(),
                Some("region" | "entities" | "poi" | "data")
            )
        }
        _ => false,
    }
}

fn list_extra_paths(root: &Path) -> io::Result<Vec<String>> {
    let mut extras = Vec::new();
    if root.exists() {
        walk_extra_paths(root, root, &mut extras)?;
    }
    extras.sort();
    Ok(extras)
}

fn walk_extra_paths(root: &Path, dir: &Path, extras: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap();
        if is_modeled_path(rel) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            walk_extra_paths(root, &path, extras)?;
        } else {
            extras.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// `path` as a path below the world root, refusing anything that could
/// leave it.
fn relative_path(path: &str) -> io::Result<&Path> {
    let rel = Path::new(path);
    if rel
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)) || c == std::path::Component::CurDir)
    {
        Ok(rel)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{path:?} leaves the world directory"),
        ))
    }
}

/// Regular files below `dir`, relative to `root`; symbolic links are
/// skipped so nothing outside the world is read.
fn walk_regular_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            walk_regular_files(root, &path, out)?;
        } else if kind.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Holds the world's `session.lock` open and locked so that a second server
/// instance cannot use the world at the same time.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

impl SessionLock {
    pub fn acquire(world: &Path) -> io::Result<Self> {
        fs::create_dir_all(world)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(world.join(SESSION_LOCK))?;
        file.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is already in use by another server", world.display()),
            )
        })?;
        file.set_len(0)?;
        file.write_all(SESSION_LOCK_CONTENT)?;
        file.flush()?;
        Ok(Self { _file: file })
    }
}

impl WorldLock for SessionLock {}

/// Largest JSON document read, so a corrupt file cannot exhaust memory.
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;

/// Reads a JSON document as a compound: objects become compounds, whole
/// numbers ints (longs past the int range), other numbers doubles.
fn read_json_file(path: &Path) -> io::Result<Option<Compound>> {
    let bytes = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_JSON_BYTES => {
            return Err(io::Error::other(format!("{} is too large", path.display())));
        }
        Ok(_) => std::fs::read(path)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    match json_to_tag(&value) {
        tuff_nbt::Tag::Compound(root) => Ok(Some(root)),
        _ => Err(io::Error::other(format!(
            "{} is not an object",
            path.display()
        ))),
    }
}

fn json_to_tag(value: &serde_json::Value) -> tuff_nbt::Tag {
    use serde_json::Value;
    use tuff_nbt::Tag;
    match value {
        Value::Object(map) => {
            let mut compound = Compound::new();
            for (key, value) in map {
                compound.insert(key.as_str(), json_to_tag(value));
            }
            Tag::Compound(compound)
        }
        Value::Array(items) => Tag::List(items.iter().map(json_to_tag).collect()),
        Value::Number(n) => match n.as_i64() {
            Some(v) => i32::try_from(v).map_or(Tag::Long(v), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(text) => Tag::String(text.clone()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Null => Tag::Compound(Compound::new()),
    }
}

fn tag_to_json(tag: &tuff_nbt::Tag, bytes_as_bools: bool) -> serde_json::Value {
    use serde_json::Value;
    use tuff_nbt::Tag;
    match tag {
        Tag::Compound(compound) => Value::Object(
            compound
                .iter()
                .map(|(key, value)| (key.to_owned(), tag_to_json(value, bytes_as_bools)))
                .collect(),
        ),
        Tag::List(items) => Value::Array(
            items
                .iter()
                .map(|item| tag_to_json(item, bytes_as_bools))
                .collect(),
        ),
        Tag::Byte(v) if bytes_as_bools => Value::from(*v != 0),
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => Value::from(*v),
        Tag::Double(v) => Value::from(*v),
        Tag::String(text) => Value::from(text.as_str()),
        other => Value::from(other.to_string()),
    }
}

/// Writes a compound as pretty-printed JSON, the way the game writes its
/// JSON documents. JSON booleans are read as bytes; `bytes_as_bools`
/// writes bytes back as booleans for documents that only use bytes that way.
fn write_json_file(path: &Path, root: &Compound, bytes_as_bools: bool) -> io::Result<()> {
    let value = tag_to_json(&tuff_nbt::Tag::Compound(root.clone()), bytes_as_bools);
    let text = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    safe_replace(path, text.as_bytes())
}

pub struct VanillaBackend {
    paths: WorldPaths,
    codec: Arc<NbtBlobCodec>,
}

impl VanillaBackend {
    pub fn new(world_dir: impl Into<PathBuf>) -> Self {
        Self {
            paths: WorldPaths::new(world_dir),
            codec: Arc::new(NbtBlobCodec {
                compression: Some(ChunkCompression::Zlib),
            }),
        }
    }

    pub fn paths(&self) -> &WorldPaths {
        &self.paths
    }

    fn path(&self, document: Document<'_>) -> PathBuf {
        match document {
            Document::Level => self.paths.level_dat(),
            Document::SavedData(id) => self.paths.saved_data(id),
            Document::DimensionData { dimension, id } => self.paths.dimension_data(dimension, id),
            Document::Player(uuid) => self.paths.player_file(uuid, ".dat"),
            Document::Stats(uuid) => self.paths.stats_file(uuid),
            Document::Advancements(uuid) => self.paths.advancements_file(uuid),
            Document::PlayerData { id, uuid } => self.paths.plugin_player_file(id, uuid),
        }
    }

    fn read_nbt(&self, document: Document<'_>, path: &Path) -> io::Result<Option<Compound>> {
        match read_nbt_file(path) {
            Ok(Some(root)) => Ok(Some(root)),
            Ok(None) if matches!(document, Document::Player(_)) => {
                read_nbt_file(&path.with_extension("dat_old"))
            }
            Ok(None) => Ok(None),
            Err(err) if matches!(document, Document::Player(_)) => {
                log::warn!("failed to load {}: {err}", path.display());
                keep_corrupted_player_copy(path);
                read_nbt_file(&path.with_extension("dat_old"))
            }
            Err(err) if matches!(document, Document::Level) => {
                log::warn!("failed to load {}: {err}", path.display());
                restore_level_from_old(path).ok_or(err)
            }
            Err(err) => Err(err),
        }
    }
}

fn keep_corrupted_player_copy(path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("player");
    let copy = path.with_file_name(format!("{stem}_corrupted_{stamp}.dat"));
    if let Err(err) = fs::copy(path, &copy) {
        log::warn!("could not keep a copy of {}: {err}", path.display());
    }
}

fn restore_level_from_old(path: &Path) -> Option<Option<Compound>> {
    let old = path.with_extension("dat_old");
    let root = read_nbt_file(&old).ok().flatten()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let corrupted = path.with_extension(format!("dat_corrupted_{stamp}"));
    if let Err(err) = fs::rename(path, &corrupted).and_then(|()| fs::copy(&old, path).map(drop)) {
        log::warn!(
            "could not restore {} from {}: {err}",
            path.display(),
            old.display()
        );
    } else {
        log::warn!(
            "restored {} from {}; the unreadable file was kept as {}",
            path.display(),
            old.display(),
            corrupted.display()
        );
    }
    Some(Some(root))
}

impl Documents for VanillaBackend {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        let path = self.path(document);
        let mut root = if matches!(document, Document::Stats(_) | Document::Advancements(_)) {
            read_json_file(&path)?
        } else {
            self.read_nbt(document, &path)?
        };
        if let Some(root) = &mut root {
            crate::datafix::document(document, root)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
        Ok(root)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        if matches!(document, Document::Stats(_) | Document::Advancements(_)) {
            let bools = matches!(document, Document::Advancements(_));
            return write_json_file(&self.path(document), &root, bools);
        }
        write_nbt_file(&self.path(document), root)
    }

    fn exists(&self, document: Document<'_>) -> bool {
        self.path(document).exists()
    }

    fn remove(&self, document: Document<'_>) -> io::Result<()> {
        match fs::remove_file(self.path(document)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        let mut ids = Vec::new();
        if self.paths.level_dat().exists() {
            ids.push(DocumentId::Level);
        }
        for id in list_resource_files(&self.paths.root.join("data"))? {
            ids.push(DocumentId::SavedData(id));
        }
        for dimension in list_dimension_dirs(&self.paths.root)? {
            let data_dir = self.paths.dimension(&dimension).join("data");
            for id in list_resource_files(&data_dir)? {
                ids.push(DocumentId::DimensionData {
                    dimension: dimension.clone(),
                    id,
                });
            }
        }
        let uuids_in = |dir: PathBuf, ext: &str| -> io::Result<Vec<u128>> {
            let mut uuids = Vec::new();
            let Ok(entries) = fs::read_dir(&dir) else {
                return Ok(uuids);
            };
            for entry in entries {
                let entry = entry?;
                if let Some(stem) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(ext))
                    && let Some(uuid) = crate::admin::parse_uuid_text(stem)
                {
                    uuids.push(uuid);
                }
            }
            Ok(uuids)
        };
        if let Ok(plugins) = fs::read_dir(self.paths.root.join("data").join("players")) {
            for plugin in plugins {
                let plugin = plugin?;
                let Some(id) = plugin.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                for uuid in uuids_in(plugin.path(), ".dat")? {
                    ids.push(DocumentId::PlayerData {
                        id: id.clone(),
                        uuid,
                    });
                }
            }
        }
        for uuid in uuids_in(self.paths.player_data_dir(), ".dat")? {
            ids.push(DocumentId::Player(uuid));
        }
        for uuid in uuids_in(self.paths.root.join("players").join("stats"), ".json")? {
            ids.push(DocumentId::Stats(uuid));
        }
        for uuid in uuids_in(
            self.paths.root.join("players").join("advancements"),
            ".json",
        )? {
            ids.push(DocumentId::Advancements(uuid));
        }
        Ok(ids)
    }
}

impl WorldBackend for VanillaBackend {
    fn name(&self) -> &str {
        NAME
    }

    fn backup_source(&self) -> crate::backup::BackupSource {
        crate::backup::BackupSource::Folder(self.paths.root().to_path_buf())
    }

    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        symlinks::validate_world_dir(self.paths.root())?;
        Ok(Box::new(SessionLock::acquire(self.paths.root())?))
    }

    fn upgrade(&self) -> io::Result<()> {
        crate::datafix::layout::upgrade_world(self.paths.root()).map(drop)
    }

    fn sql_console(&self) -> Option<crate::sqlite::console::SqlConsole> {
        let paths = self.paths.clone();
        Some(crate::sqlite::console::SqlConsole::importing(Arc::new(
            move |file| import_regions(&paths, file),
        )))
    }

    fn documents(&self) -> &dyn Documents {
        self
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        Ok(Box::new(VanillaRegionStore {
            chunk_dir: self.paths.region_dir(dimension),
            entity_dir: self.paths.entities_dir(dimension),
            poi_dir: self.paths.poi_dir(dimension),
            region,
            chunks: None,
            entities: None,
            pois: None,
        }))
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.codec.clone()
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        list_dimension_dirs(&self.paths.root)
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        let mut regions = list_region_files(&self.paths.region_dir(dimension))?;
        regions.extend(list_region_files(&self.paths.entities_dir(dimension))?);
        regions.extend(list_region_files(&self.paths.poi_dir(dimension))?);
        regions.sort();
        regions.dedup();
        Ok(regions)
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        list_extra_paths(&self.paths.root)
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.paths.root.join(path))
    }

    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        let dir = self.paths.root.join(relative_path(prefix)?);
        let mut paths = Vec::new();
        if dir.is_dir() {
            walk_regular_files(&self.paths.root, &dir, &mut paths)?;
        }
        paths.sort();
        Ok(paths)
    }

    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        let target = self.paths.root.join(relative_path(path)?);
        let file = File::open(&target)?;
        let declared = file.metadata()?.len();
        if declared > max as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path:?} is over the {max} byte limit"),
            ));
        }
        let mut data = Vec::with_capacity(declared as usize);
        file.take(max as u64 + 1).read_to_end(&mut data)?;
        if data.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path:?} is over the {max} byte limit"),
            ));
        }
        Ok(data)
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let target = self.paths.root.join(relative_path(path)?);
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(target, data)
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        let target = self.paths.root.join(relative_path(path)?);
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        }
        let mut dir = target.parent();
        while let Some(parent) = dir.filter(|d| *d != self.paths.root) {
            if fs::remove_dir(parent).is_err() {
                break;
            }
            dir = parent.parent();
        }
        Ok(())
    }
}

/// One region's `.mca` files, opened lazily: reading a region that was never
/// written creates nothing.
pub struct VanillaRegionStore {
    chunk_dir: PathBuf,
    entity_dir: PathBuf,
    poi_dir: PathBuf,
    region: FileRegionPos,
    chunks: Option<RegionFile>,
    entities: Option<RegionFile>,
    pois: Option<RegionFile>,
}

fn open<'a>(
    file: &'a mut Option<RegionFile>,
    dir: &Path,
    region: FileRegionPos,
    create: bool,
) -> io::Result<Option<&'a mut RegionFile>> {
    if file.is_none() {
        *file = RegionFile::open(dir, region.x, region.z, create)?;
    }
    Ok(file.as_mut())
}

fn read(
    file: &mut Option<RegionFile>,
    dir: &Path,
    region: FileRegionPos,
    pos: ChunkPos,
) -> io::Result<Option<Vec<u8>>> {
    let Some(file) = open(file, dir, region, false)? else {
        return Ok(None);
    };
    Ok(file
        .read(pos)
        .map_err(io::Error::other)?
        .map(RawChunk::into_blob))
}

fn write(
    file: &mut Option<RegionFile>,
    dir: &Path,
    region: FileRegionPos,
    pos: ChunkPos,
    blob: Option<&[u8]>,
) -> io::Result<()> {
    match blob {
        Some(blob) => {
            let raw = RawChunk::from_blob(blob).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "blob is not a vanilla chunk")
            })?;
            open(file, dir, region, true)?
                .expect("created")
                .write(pos, &raw)
        }
        None => match open(file, dir, region, false)? {
            Some(file) => file.clear(pos),
            None => Ok(()),
        },
    }
}

impl RegionStore for VanillaRegionStore {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        read(&mut self.chunks, &self.chunk_dir, self.region, pos)
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        write(
            &mut self.chunks,
            &self.chunk_dir,
            self.region,
            pos,
            Some(blob),
        )
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        write(&mut self.chunks, &self.chunk_dir, self.region, pos, None)
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        read(&mut self.entities, &self.entity_dir, self.region, pos)
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        write(&mut self.entities, &self.entity_dir, self.region, pos, blob)
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        read(&mut self.pois, &self.poi_dir, self.region, pos)
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        write(&mut self.pois, &self.poi_dir, self.region, pos, blob)
    }

    fn flush(&mut self) -> io::Result<()> {
        for file in [&mut self.chunks, &mut self.entities, &mut self.pois]
            .into_iter()
            .flatten()
        {
            file.sync()?;
        }
        Ok(())
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        let mut positions = Vec::new();
        if let Some(file) = open(&mut self.chunks, &self.chunk_dir, self.region, false)? {
            positions.extend(file.positions(self.region.x, self.region.z));
        }
        for (file, dir) in [
            (&mut self.entities, &self.entity_dir),
            (&mut self.pois, &self.poi_dir),
        ] {
            if let Some(file) = open(file, dir, self.region, false)? {
                for pos in file.positions(self.region.x, self.region.z) {
                    if !positions.contains(&pos) {
                        positions.push(pos);
                    }
                }
            }
        }
        Ok(positions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::MAX_FILE_NBT_BYTES;
    use crate::test_dir::TestDir;
    use tuff_nbt::Compression;
    use tuff_world_model::poi::Record;

    #[tuff_core::watchdog]
    #[test]
    fn the_sql_console_reads_region_files_and_refuses_writes() {
        use crate::sqlite::console::{Cancel, Limits, WriteAction};
        let dir = TestDir::new();
        let paths = WorldPaths::new(dir.path());
        for (sub, x, z) in [
            (paths.region_dir("minecraft:overworld"), 33, -1),
            (paths.poi_dir("minecraft:overworld"), 0, 0),
        ] {
            fs::create_dir_all(&sub).unwrap();
            let mut mca = RegionFile::open(&sub, x >> 5, z >> 5, true)
                .unwrap()
                .unwrap();
            let raw = RawChunk::compress(vec![10, 0, 0, 0]).unwrap();
            mca.write(ChunkPos::new(x, z), &raw).unwrap();
            mca.sync().unwrap();
        }
        let backend = VanillaBackend::new(dir.path());
        let mut level = tuff_nbt::Compound::default();
        level.insert("Probe", tuff_nbt::Tag::Int(1));
        Documents::write(&backend, Document::Level, level).unwrap();
        let console = backend.sql_console().unwrap();
        assert!(!console.can_write());
        let out = console
            .query(
                "SELECT dimension, x, z FROM chunks".into(),
                0,
                10,
                Limits::default(),
                Cancel::new(),
            )
            .unwrap();
        assert_eq!(out.rows.len(), 1);
        let count = console
            .query(
                "SELECT count(*) FROM pois".into(),
                0,
                10,
                Limits::default(),
                Cancel::new(),
            )
            .unwrap();
        assert_eq!(count.rows.len(), 1);
        let docs = console
            .query(
                "SELECT kind FROM documents".into(),
                0,
                10,
                Limits::default(),
                Cancel::new(),
            )
            .unwrap();
        assert_eq!(docs.rows.len(), 1);
        assert!(
            console
                .write(
                    "DELETE FROM chunks".into(),
                    WriteAction::Preview,
                    Limits::default()
                )
                .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn paths_follow_vanilla_layout() {
        let paths = WorldPaths::new("/srv/world");
        assert_eq!(
            paths.region_dir("minecraft:overworld"),
            Path::new("/srv/world/dimensions/minecraft/overworld/region")
        );
        assert_eq!(
            paths.saved_data("minecraft:world_clocks"),
            Path::new("/srv/world/data/minecraft/world_clocks.dat")
        );
        assert_eq!(
            paths.dimension_data("minecraft:overworld", "world_border"),
            Path::new("/srv/world/dimensions/minecraft/overworld/data/minecraft/world_border.dat")
        );
        assert_eq!(
            paths.player_file(0x069a79f444e94726a5befca90e38aaf5, ".dat"),
            Path::new("/srv/world/players/data/069a79f4-44e9-4726-a5be-fca90e38aaf5.dat")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn session_lock_is_exclusive() {
        let dir = TestDir::new();
        let lock = SessionLock::acquire(dir.path()).unwrap();
        assert_eq!(
            fs::read(dir.path().join(SESSION_LOCK)).unwrap(),
            SESSION_LOCK_CONTENT
        );
        assert!(SessionLock::acquire(dir.path()).is_err());
        drop(lock);
        SessionLock::acquire(dir.path()).unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn safe_replace_keeps_previous_version() {
        let dir = TestDir::new();
        let target = dir.path().join("level.dat");
        safe_replace(&target, b"one").unwrap();
        safe_replace(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(fs::read(dir.path().join("level.dat_old")).unwrap(), b"one");
    }

    #[tuff_core::watchdog]
    #[test]
    fn unreadable_player_dat_is_kept_as_corrupted_copy() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let player = Document::Player(7);
        write_nbt_file(
            &backend.paths.player_file(7, ".dat"),
            Compound::new().with("a", 1i32),
        )
        .unwrap();
        let file = backend.paths.player_file(7, ".dat");
        fs::write(&file, b"garbage").unwrap();
        assert!(backend.read(player).unwrap().is_none());
        let dir_of = file.parent().unwrap();
        let kept = fs::read_dir(dir_of)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains("_corrupted_") && name.ends_with(".dat")
            });
        assert!(kept);
    }

    #[tuff_core::watchdog]
    #[test]
    fn corrupt_level_dat_is_restored_from_level_dat_old() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let level = dir.path().join("level.dat");
        write_nbt_file(&level, Compound::new().with("a", 1i32)).unwrap();
        write_nbt_file(&level, Compound::new().with("a", 2i32)).unwrap();
        fs::write(&level, b"garbage").unwrap();
        let root = backend.read(Document::Level).unwrap().unwrap();
        assert_eq!(root.get_int("a"), Some(1));
        assert_eq!(
            read_nbt_file(&level).unwrap().unwrap().get_int("a"),
            Some(1)
        );
        let kept = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("level.dat_corrupted_")
            });
        assert!(kept);
    }

    #[tuff_core::watchdog]
    #[test]
    fn corrupt_level_dat_without_backup_is_an_error() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        fs::write(dir.path().join("level.dat"), b"garbage").unwrap();
        assert!(backend.read(Document::Level).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn nbt_files_round_trip_and_reject_bombs() {
        let dir = TestDir::new();
        let path = dir.path().join("data.dat");
        assert!(read_nbt_file(&path).unwrap().is_none());
        write_nbt_file(&path, Compound::new().with("a", 1i32)).unwrap();
        assert_eq!(read_nbt_file(&path).unwrap().unwrap().get_int("a"), Some(1));

        let mut raw = vec![10, 0, 0, 7, 0, 1, b'b'];
        let len = (MAX_FILE_NBT_BYTES + 1) as i32;
        raw.extend_from_slice(&len.to_be_bytes());
        raw.resize(raw.len() + MAX_FILE_NBT_BYTES + 1, 0);
        raw.push(0);
        fs::write(&path, Compression::Gzip.compress(raw).unwrap()).unwrap();
        assert!(read_nbt_file(&path).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn statistics_are_pretty_json_under_players_stats() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let uuid = 0x069a79f444e94726a5befca90e38aaf5;
        let stats = Compound::new()
            .with(
                "stats",
                Compound::new().with(
                    "minecraft:custom",
                    Compound::new().with("minecraft:play_time", 1200),
                ),
            )
            .with("DataVersion", 5023);
        backend.write(Document::Stats(uuid), stats.clone()).unwrap();
        let path = dir
            .path()
            .join("players/stats/069a79f4-44e9-4726-a5be-fca90e38aaf5.json");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"minecraft:play_time\": 1200"), "{text}");
        assert_eq!(backend.read(Document::Stats(uuid)).unwrap(), Some(stats));
        assert_eq!(backend.read(Document::Stats(1)).unwrap(), None);
        fs::write(&path, "[1, 2]").unwrap();
        assert!(backend.read(Document::Stats(uuid)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn player_documents_fall_back_to_old_file() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let player = Document::Player(5);
        assert!(backend.read(player).unwrap().is_none());
        backend.write(player, Compound::new().with("a", 1)).unwrap();
        backend.write(player, Compound::new().with("a", 2)).unwrap();
        fs::write(backend.paths.player_file(5, ".dat"), b"garbage").unwrap();
        assert_eq!(backend.read(player).unwrap().unwrap().get_int("a"), Some(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn region_store_round_trips_blobs_and_clears_entities() {
        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let region = FileRegionPos::new(0, -1);
        let pos = ChunkPos::new(3, -30);
        let mut store = backend.open_region("minecraft:the_nether", region).unwrap();
        assert!(store.read_chunk(pos).unwrap().is_none());
        assert!(!dir.path().join("dimensions").exists());
        let blob = RawChunk::compress(vec![1, 2, 3]).unwrap().into_blob();
        store.write_chunk(pos, &blob).unwrap();
        store.write_entities(pos, Some(&blob)).unwrap();
        store.flush().unwrap();
        assert_eq!(store.read_chunk(pos).unwrap().unwrap(), blob);
        store.write_entities(pos, None).unwrap();
        assert!(store.read_entities(pos).unwrap().is_none());
        assert!(
            dir.path()
                .join("dimensions/minecraft/the_nether/region/r.0.-1.mca")
                .exists()
        );
        assert!(store.write_chunk(pos, &[]).is_err());
        store.remove_chunk(pos).unwrap();
        assert!(store.read_chunk(pos).unwrap().is_none());
        store.remove_chunk(pos).unwrap();
        let mut absent = backend
            .open_region("minecraft:the_nether", FileRegionPos::new(5, 5))
            .unwrap();
        absent.remove_chunk(ChunkPos::new(160, 160)).unwrap();
        assert!(
            !dir.path()
                .join("dimensions/minecraft/the_nether/region/r.5.5.mca")
                .exists()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn points_of_interest_live_in_the_poi_folder_and_are_not_extra_files() {
        use tuff_vanilla_data::registry::PointOfInterestType;

        let dir = TestDir::new();
        let backend = VanillaBackend::new(dir.path());
        let pos = ChunkPos::new(40, 2);
        let records = vec![Record::new(
            tuff_world_model::BlockPos::new(645, 64, 40),
            PointOfInterestType::Meeting,
        )];
        let codec = backend.blob_codec();
        let blob = codec.encode_pois(pos, &records).unwrap().unwrap();
        let mut store = backend
            .open_region("minecraft:overworld", pos.file_region())
            .unwrap();
        store.write_pois(pos, Some(&blob)).unwrap();
        store.flush().unwrap();
        assert!(
            dir.path()
                .join("dimensions/minecraft/overworld/poi/r.1.0.mca")
                .is_file()
        );
        assert_eq!(store.list_chunks().unwrap(), vec![pos]);
        assert_eq!(
            backend.list_regions("minecraft:overworld").unwrap(),
            vec![pos.file_region()]
        );
        assert!(backend.list_extra_files().unwrap().is_empty());
        let read = store.read_pois(pos).unwrap().unwrap();
        assert_eq!(codec.decode_pois(&read, pos).unwrap(), Some(records));
        store.write_pois(pos, None).unwrap();
        assert!(store.read_pois(pos).unwrap().is_none());
    }
}

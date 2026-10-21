//! Offline conversion between world backends: everything a
//! [`WorldBackend`] and its [`ConfigStore`] can enumerate is copied from a
//! source to a destination through those traits ([`convert_world`],
//! [`convert_config`]), so this works for any pair of backends (including a
//! feature's own), not a vanilla-to-sqlite special case.
//!
//! [`convert_offline`] is the safe entry point a CLI uses: it takes the
//! source's world lock (so it can never be modified), refuses an existing
//! destination, and builds the destination at a temporary path that is
//! renamed into place only once every record has copied successfully,
//! leaving nothing half-written on failure.

use std::fs;
use std::io;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::storage::BackendOptions;
use crate::storage::Storage;
use crate::storage::codec::{ChunkCodec, WorldBackendExt};
use crate::storage::normalise::{SaveNormaliser, SaveNormaliserEntry, SaveNormalisers, SaveView};
use crate::world::DEFAULT_LEVEL_NAME;
use tuff_storage::backend::WorldBackend;
use tuff_storage::config::ConfigStore;
use tuff_storage::document::DocumentId;
use tuff_storage::sqlite;
use tuff_storage::vanilla::config::VanillaConfig;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::chunk_meta::Decoded;
use tuff_world_model::pos::ChunkPos;

type CopiedChunk = io::Result<(ChunkPos, Option<Vec<u8>>, Option<Vec<u8>>)>;

type ChunkBlobs = (ChunkPos, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

/// Counts of what a conversion moved, for the operator's benefit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConvertReport {
    pub documents: usize,
    pub chunks: usize,
    pub entity_records: usize,
    pub poi_records: usize,
    pub config_items: usize,
    pub extra_files: usize,
    pub dimensions: usize,
}

/// Copies every document, chunk, entity record and extra file from `source`
/// to `destination`. Config (properties, ops, whitelist, bans, icon) is
/// copied separately via [`ConfigStore::import`] since not every
/// `WorldBackend` carries its own config store.
pub fn convert_world(
    source: &dyn WorldBackend,
    destination: &dyn WorldBackend,
) -> io::Result<ConvertReport> {
    convert_world_with(
        source,
        destination,
        &SaveNormalisers::default(),
        Biomes::vanilla(),
    )
}

/// [`convert_world`] with save normalisers applied to every chunk and
/// entity record (forcing a decode and re-encode even for equal formats).
pub fn convert_world_with(
    source: &dyn WorldBackend,
    destination: &dyn WorldBackend,
    normalisers: &SaveNormalisers,
    biomes: &Biomes,
) -> io::Result<ConvertReport> {
    let mut report = ConvertReport::default();
    copy_documents(source, destination, &mut report)?;
    copy_regions(source, destination, (normalisers, biomes), &mut report)?;
    copy_extra_files(source, destination, &mut report)?;
    Ok(report)
}

/// Copies the server configuration (properties, user lists, icon) and
/// counts the items moved.
pub fn convert_config(
    source: &dyn ConfigStore,
    destination: &dyn ConfigStore,
) -> io::Result<usize> {
    destination.import(source)?;
    let mut count = 0;
    if source.read_properties()?.is_some() {
        count += 1;
    }
    let lists = source.read_lists()?;
    count += lists.ops.len() + lists.whitelist.len() + lists.bans.len() + lists.ip_bans.len();
    if source.read_icon()?.is_some() {
        count += 1;
    }
    count += source.read_settings()?.len();
    Ok(count)
}

/// One side of an offline conversion. `path` means what it means when
/// running the server with the same backend (see `src/main.rs`'s `Args`):
/// for `vanilla` the instance directory (world at `<path>/world`,
/// configuration alongside it); for `sqlite` the server file itself.
#[derive(Debug, Clone)]
pub struct ConvertEndpoint {
    pub backend: &'static str,
    pub path: PathBuf,
    pub options: BackendOptions,
}

impl ConvertEndpoint {
    fn is_vanilla(&self) -> bool {
        self.backend != sqlite::NAME
    }

    pub fn world_path(&self) -> PathBuf {
        if self.is_vanilla() {
            self.path.join(DEFAULT_LEVEL_NAME)
        } else {
            self.path.clone()
        }
    }

    fn open(&self) -> io::Result<std::sync::Arc<dyn WorldBackend>> {
        Storage::Named(self.backend.to_owned()).open_with(&self.world_path(), self.options)
    }

    fn config(&self, backend: &dyn WorldBackend) -> std::sync::Arc<dyn ConfigStore> {
        backend
            .config()
            .unwrap_or_else(|| std::sync::Arc::new(VanillaConfig::new(&self.path)))
    }
}

fn remove_any(path: &std::path::Path) {
    let _ = fs::remove_dir_all(path);
    let _ = fs::remove_file(path);
}

struct Rewrite {
    erase_cache: bool,
}

impl SaveNormaliser for Rewrite {
    fn name(&self) -> &'static str {
        "force-upgrade"
    }

    fn normalise(&self, view: &mut SaveView) -> Result<(), String> {
        if self.erase_cache
            && let Some((_, meta)) = view.blocks.as_mut()
        {
            meta.block_changed();
        }
        Ok(())
    }
}

/// Normalisers that make a conversion decode and re-encode every chunk and
/// entity record, even between equal formats; with `erase_cache` the cached
/// lighting and heightmaps are dropped to be recomputed on load.
pub fn rewrite_normalisers(erase_cache: bool) -> SaveNormalisers {
    SaveNormalisers::new(vec![SaveNormaliserEntry {
        order: 0,
        hook: std::sync::Arc::new(Rewrite { erase_cache }),
    }])
}

/// Converts `source` into `destination`, refusing to touch `source` and
/// refusing to overwrite an existing `destination`. Everything is built at
/// a temporary sibling path first and renamed into place only once the
/// whole conversion succeeds; a failure leaves neither the temporary path
/// nor a partial destination behind.
pub fn convert_offline(
    source: &ConvertEndpoint,
    destination: &ConvertEndpoint,
) -> io::Result<ConvertReport> {
    convert_offline_with(
        source,
        destination,
        &SaveNormalisers::default(),
        &Biomes::preserving(),
    )
}

/// [`convert_offline`] that decodes and re-encodes every chunk and entity
/// record (the "force upgrade"), dropping cached lighting and heightmaps
/// when `erase_cache` is set.
pub fn convert_offline_rewriting(
    source: &ConvertEndpoint,
    destination: &ConvertEndpoint,
    erase_cache: bool,
) -> io::Result<ConvertReport> {
    convert_offline_with(
        source,
        destination,
        &rewrite_normalisers(erase_cache),
        &Biomes::preserving(),
    )
}

/// The `--force_upgrade` of a world in place: rewrites every chunk and
/// entity record into a staging copy next to the world, then swaps it in.
/// The previous world is kept beside it as `<name>.pre-upgrade`, which must
/// not exist yet. A world that does not exist yet has nothing to upgrade.
pub fn force_upgrade_in_place(
    source: &ConvertEndpoint,
    erase_cache: bool,
) -> io::Result<ConvertReport> {
    let world = source.world_path();
    if !world.exists() {
        return Ok(ConvertReport::default());
    }
    let sibling = |suffix: &str| {
        let name = world
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("world");
        world.with_file_name(format!("{name}.{suffix}"))
    };
    let backup = sibling("pre-upgrade");
    if backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; move it away before upgrading again",
                backup.display()
            ),
        ));
    }
    let staging = ConvertEndpoint {
        backend: source.backend,
        path: sibling("upgrading"),
        options: source.options,
    };
    let report = convert_offline_with(
        source,
        &staging,
        &rewrite_normalisers(erase_cache),
        &Biomes::preserving(),
    )?;
    fs::rename(&world, &backup)?;
    if let Err(err) = fs::rename(staging.world_path(), &world) {
        let _ = fs::rename(&backup, &world);
        remove_any(&staging.path);
        return Err(err);
    }
    if staging.path != world {
        remove_any(&staging.path);
    }
    Ok(report)
}

pub fn convert_offline_with(
    source: &ConvertEndpoint,
    destination: &ConvertEndpoint,
    normalisers: &SaveNormalisers,
    biomes: &Biomes,
) -> io::Result<ConvertReport> {
    if destination.path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; conversion refuses to overwrite it",
                destination.path.display()
            ),
        ));
    }
    let parent = destination
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent)?;
    let temp_name = format!(
        ".{}.converting-{}",
        destination
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("output"),
        std::process::id()
    );
    let temp_path = parent.join(temp_name);
    remove_any(&temp_path);
    let temp = ConvertEndpoint {
        backend: destination.backend,
        path: temp_path.clone(),
        options: destination.options,
    };

    let outcome = run_conversion(source, &temp, normalisers, biomes);
    match outcome {
        Ok(report) => {
            fs::rename(&temp_path, &destination.path)?;
            Ok(report)
        }
        Err(err) => {
            remove_any(&temp_path);
            Err(err)
        }
    }
}

fn run_conversion(
    source: &ConvertEndpoint,
    destination: &ConvertEndpoint,
    normalisers: &SaveNormalisers,
    biomes: &Biomes,
) -> io::Result<ConvertReport> {
    let source_backend = source
        .open()
        .map_err(|err| io::Error::other(format!("opening source world: {err}")))?;
    let _source_lock = source_backend
        .lock()
        .map_err(|err| io::Error::new(err.kind(), format!("source world is in use: {err}")))?;
    super::recover_journal(&*source_backend, &source.world_path()).map_err(|err| {
        io::Error::new(err.kind(), format!("recovering source world save: {err}"))
    })?;
    source_backend
        .upgrade()
        .map_err(|err| io::Error::new(err.kind(), format!("upgrading source world: {err}")))?;
    let source_config = source.config(&*source_backend);

    if destination.is_vanilla() {
        fs::create_dir_all(destination.world_path())?;
    }
    let destination_backend = destination
        .open()
        .map_err(|err| io::Error::other(format!("creating destination world: {err}")))?;
    let destination_config = destination.config(&*destination_backend);

    let mut report =
        convert_world_with(&*source_backend, &*destination_backend, normalisers, biomes)?;
    report.config_items = convert_config(&*source_config, &*destination_config)?;
    Ok(report)
}

fn copy_documents(
    source: &dyn WorldBackend,
    destination: &dyn WorldBackend,
    report: &mut ConvertReport,
) -> io::Result<()> {
    let source_docs = source.documents();
    let destination_docs = destination.documents();
    for id in source_docs.list()? {
        // `DocumentId::borrow` matches every variant with no catch-all arm,
        // so a `Document` variant added without updating it (and every
        // backend's `list`) fails to compile; nothing further needs to
        // branch on the variant here.
        let document = id.borrow();
        if let Some(root) = source_docs.read(document)? {
            destination_docs.write(document, root)?;
            report.documents += 1;
        }
        // Keep `id` alive for its type to matter to the reader above.
        let _: &DocumentId = &id;
    }
    Ok(())
}

fn copy_regions(
    source: &dyn WorldBackend,
    destination: &dyn WorldBackend,
    (normalisers, biomes): (&SaveNormalisers, &Biomes),
    report: &mut ConvertReport,
) -> io::Result<()> {
    let source_codec = source.chunk_codec();
    let destination_codec = destination.chunk_codec();
    let same_format = source_codec.blob_format() == destination_codec.blob_format();
    let dimensions = source.list_dimensions()?;
    report.dimensions = dimensions.len();
    let mut region_lists = Vec::with_capacity(dimensions.len());
    for dimension in dimensions {
        let regions = source.list_regions(&dimension)?;
        region_lists.push((dimension, regions));
    }
    let mut progress = Progress::new(region_lists.iter().map(|(_, r)| r.len()).sum());
    for (dimension, regions) in region_lists {
        for region in regions {
            let mut source_store = source.open_region(&dimension, region)?;
            let mut destination_store = destination.open_region(&dimension, region)?;
            let positions = source_store.list_chunks()?;
            let blobs: Vec<ChunkBlobs> = positions
                .into_iter()
                .map(|pos| {
                    Ok((
                        pos,
                        source_store.read_chunk(pos)?,
                        source_store.read_entities(pos)?,
                        source_store.read_pois(pos)?,
                    ))
                })
                .collect::<io::Result<_>>()?;
            type Converted = (ChunkPos, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);
            let converted: Vec<io::Result<Converted>> = blobs
                .into_par_iter()
                .map(|(pos, chunk_blob, entity_blob, poi_blob)| {
                    let (pos, chunk_out, entities_out) = convert_chunk_blobs(
                        pos,
                        chunk_blob,
                        entity_blob,
                        same_format,
                        (&dimension, normalisers, biomes),
                        &*source_codec,
                        &*destination_codec,
                    )?;
                    let pois_out = convert_poi_blob(
                        pos,
                        poi_blob,
                        same_format,
                        &*source_codec,
                        &*destination_codec,
                    )?;
                    Ok((pos, chunk_out, entities_out, pois_out))
                })
                .collect();
            for result in converted {
                let (pos, chunk_out, entities_out, pois_out) = result?;
                if let Some(blob) = chunk_out {
                    destination_store.write_chunk(pos, &blob)?;
                    report.chunks += 1;
                }
                if let Some(blob) = entities_out {
                    destination_store.write_entities(pos, Some(&blob))?;
                    report.entity_records += 1;
                }
                if let Some(blob) = pois_out {
                    destination_store.write_pois(pos, Some(&blob))?;
                    report.poi_records += 1;
                }
            }
            destination_store.flush()?;
            progress.region_done();
        }
    }
    Ok(())
}

const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

struct Progress {
    total: usize,
    done: usize,
    started: std::time::Instant,
    last_logged: std::time::Instant,
}

impl Progress {
    fn new(total: usize) -> Self {
        let now = std::time::Instant::now();
        Self {
            total,
            done: 0,
            started: now,
            last_logged: now,
        }
    }

    fn region_done(&mut self) {
        self.done += 1;
        let now = std::time::Instant::now();
        if self.done == self.total || now.duration_since(self.last_logged) >= PROGRESS_INTERVAL {
            self.last_logged = now;
            log::info!(
                "{}",
                progress_line(self.done, self.total, now - self.started)
            );
        }
    }
}

fn progress_line(done: usize, total: usize, elapsed: std::time::Duration) -> String {
    let percent = (done * 100).checked_div(total).unwrap_or(100);
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 { done as f64 / secs } else { 0.0 };
    let remaining = total.saturating_sub(done);
    let eta = if rate > 0.0 {
        format!("{}s left", (remaining as f64 / rate).ceil() as u64)
    } else {
        "unknown time left".to_owned()
    };
    format!("Converting world: {percent}% ({done}/{total} regions, {rate:.1} regions/s, {eta})")
}

/// Either copies a chunk's blobs unchanged (same blob format) or decodes
/// them through `source_codec` and re-encodes through `destination_codec`.
///
/// Entity records are plain NBT compounds regardless of blob format, so
/// they always take the generic decode/encode path when formats differ (it
/// is exact either way). Fully generated chunks likewise round-trip through
/// the backend-neutral `Chunk`/`ChunkMeta` types; only the original chunk's
/// `LastUpdate` game tick cannot be recovered on that path (no shipped
/// codec differs from another, so this never runs in practice). A chunk
/// still mid-generation (`Decoded::Ungenerated`) has no codec-neutral
/// representation at all, so converting one between two differently shaped
/// codecs is refused with a clear error rather than silently dropping it.
fn dimension_shape(dimension: &str) -> ChunkShape {
    match dimension {
        "minecraft:the_nether" | "minecraft:the_end" => ChunkShape::new(0, 256),
        "minecraft:overworld" => ChunkShape::OVERWORLD,
        _ => ChunkShape::OVERWORLD.with_overworld(false),
    }
}

pub(crate) fn convert_chunk_blobs(
    pos: ChunkPos,
    chunk_blob: Option<Vec<u8>>,
    entity_blob: Option<Vec<u8>>,
    same_format: bool,
    (dimension, normalisers, biomes): (&str, &SaveNormalisers, &Biomes),
    source_codec: &dyn ChunkCodec,
    destination_codec: &dyn ChunkCodec,
) -> CopiedChunk {
    let copy = same_format && normalisers.is_empty();
    let mut chunk_out = None;
    let mut chunk_state = None;
    match chunk_blob {
        None => {}
        Some(blob) if copy => chunk_out = Some(blob),
        Some(blob) => {
            let decoded = source_codec
                .decode_chunk_in(&blob, pos, dimension_shape(dimension), biomes)
                .map_err(io::Error::other)?;
            match decoded {
                Decoded::Chunk(chunk, meta) => chunk_state = Some((*chunk, meta)),
                Decoded::Ungenerated(_) if same_format => chunk_out = Some(blob),
                Decoded::Ungenerated(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "{pos:?} is still generating (no terrain yet), which cannot be \
                             converted between differing chunk blob formats"
                        ),
                    ));
                }
            }
        }
    }
    let mut entity_state = None;
    let mut entities_out = None;
    match entity_blob {
        None => {}
        Some(blob) if copy => entities_out = Some(blob),
        Some(blob) => {
            entity_state = Some(
                source_codec
                    .decode_entities(&blob, pos)
                    .map_err(io::Error::other)?,
            );
        }
    }
    if chunk_state.is_some() || entity_state.is_some() {
        let view = normalisers.apply(SaveView {
            dimension: dimension.to_owned(),
            pos,
            blocks: chunk_state,
            entities: entity_state,
        });
        if let Some((chunk, meta)) = view.blocks {
            chunk_out = Some(
                destination_codec
                    .encode_chunk_in(&chunk, &meta, pos, 0, biomes)
                    .map_err(io::Error::other)?,
            );
        }
        if let Some(compounds) = view.entities {
            entities_out = destination_codec
                .encode_entities(pos, &compounds)
                .map_err(io::Error::other)?;
        }
    }
    Ok((pos, chunk_out, entities_out))
}

/// A chunk's points of interest, copied as they are or decoded and encoded
/// again for a differing destination format.
fn convert_poi_blob(
    pos: ChunkPos,
    blob: Option<Vec<u8>>,
    same_format: bool,
    source_codec: &dyn ChunkCodec,
    destination_codec: &dyn ChunkCodec,
) -> io::Result<Option<Vec<u8>>> {
    let Some(blob) = blob else {
        return Ok(None);
    };
    if same_format {
        return Ok(Some(blob));
    }
    match source_codec
        .decode_pois(&blob, pos)
        .map_err(io::Error::other)?
    {
        Some(records) => destination_codec
            .encode_pois(pos, &records)
            .map_err(io::Error::other),
        None => Ok(None),
    }
}

fn copy_extra_files(
    source: &dyn WorldBackend,
    destination: &dyn WorldBackend,
    report: &mut ConvertReport,
) -> io::Result<()> {
    for path in source.list_extra_files()? {
        let data = source.read_extra_file(&path)?;
        destination.write_extra_file(&path, &data)?;
        report.extra_files += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use tuff_nbt::Compound;
    use tuff_storage::config::fixtures::{sample_lists, sample_properties};
    use tuff_storage::document::Document;
    use tuff_storage::memory::MemoryBackend;
    use tuff_storage::sqlite::SqliteBackend;
    use tuff_storage::test_dir::TestDir;
    use tuff_storage::vanilla::{self, VanillaBackend};
    use tuff_world_model::pos::FileRegionPos;

    const OVERWORLD: &str = "minecraft:overworld";
    const NETHER: &str = "minecraft:the_nether";
    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/vanilla-26.3"
    );

    fn sample_world(backend: &dyn WorldBackend) {
        backend
            .documents()
            .write(Document::Level, Compound::new().with("a", 1i32))
            .unwrap();
        backend
            .documents()
            .write(
                Document::SavedData("minecraft:world_clocks"),
                Compound::new().with("b", 2i32),
            )
            .unwrap();
        backend
            .documents()
            .write(
                Document::DimensionData {
                    dimension: OVERWORLD,
                    id: "minecraft:world_border",
                },
                Compound::new().with("c", 3i32),
            )
            .unwrap();
        backend
            .documents()
            .write(Document::Player(1), Compound::new().with("d", 4i32))
            .unwrap();
        backend
            .documents()
            .write(Document::Stats(1), Compound::new().with("e", 5i32))
            .unwrap();
        backend
            .documents()
            .write(Document::Advancements(1), Compound::new().with("f", 6i32))
            .unwrap();
        backend
            .documents()
            .write(
                Document::PlayerData {
                    id: "mine",
                    uuid: 1,
                },
                Compound::new().with("g", 7i32),
            )
            .unwrap();

        let pos = ChunkPos::new(3, 40);
        let region = pos.file_region();
        let mut overworld = backend.open_region(OVERWORLD, region).unwrap();
        let codec = backend.chunk_codec();
        let blob = codec
            .encode_entities(pos, &[Compound::new().with("id", "minecraft:cow")])
            .unwrap()
            .unwrap();
        overworld.write_entities(pos, Some(&blob)).unwrap();
        overworld.flush().unwrap();

        let mut nether = backend
            .open_region(NETHER, FileRegionPos::new(-1, 0))
            .unwrap();
        let far_pos = ChunkPos::new(-31, 5);
        let far_blob = codec
            .encode_entities(far_pos, &[Compound::new().with("id", "minecraft:ghast")])
            .unwrap()
            .unwrap();
        nether.write_entities(far_pos, Some(&far_blob)).unwrap();
        nether.flush().unwrap();
    }

    fn assert_world_equal(source: &dyn WorldBackend, destination: &dyn WorldBackend) {
        let mut source_ids = source.documents().list().unwrap();
        let mut destination_ids = destination.documents().list().unwrap();
        source_ids.sort_by_key(|id| format!("{id:?}"));
        destination_ids.sort_by_key(|id| format!("{id:?}"));
        assert_eq!(source_ids, destination_ids);
        for id in &source_ids {
            assert_eq!(
                source.documents().read(id.borrow()).unwrap(),
                destination.documents().read(id.borrow()).unwrap(),
                "{id:?}"
            );
        }

        let mut source_dims = source.list_dimensions().unwrap();
        let mut destination_dims = destination.list_dimensions().unwrap();
        source_dims.sort();
        destination_dims.sort();
        assert_eq!(source_dims, destination_dims);
        for dimension in source_dims {
            let mut source_regions = source.list_regions(&dimension).unwrap();
            let mut destination_regions = destination.list_regions(&dimension).unwrap();
            source_regions.sort();
            destination_regions.sort();
            assert_eq!(source_regions, destination_regions, "{dimension}");
            for region in source_regions {
                let mut source_store = source.open_region(&dimension, region).unwrap();
                let mut destination_store = destination.open_region(&dimension, region).unwrap();
                let mut source_positions = source_store.list_chunks().unwrap();
                let mut destination_positions = destination_store.list_chunks().unwrap();
                source_positions.sort();
                destination_positions.sort();
                assert_eq!(
                    source_positions, destination_positions,
                    "{dimension} {region:?}"
                );
                for pos in source_positions {
                    let source_entities = source
                        .chunk_codec()
                        .decode_entities(
                            &source_store.read_entities(pos).unwrap().unwrap_or_default(),
                            pos,
                        )
                        .unwrap_or_default();
                    let destination_entities = destination
                        .chunk_codec()
                        .decode_entities(
                            &destination_store
                                .read_entities(pos)
                                .unwrap()
                                .unwrap_or_default(),
                            pos,
                        )
                        .unwrap_or_default();
                    assert_eq!(source_entities, destination_entities, "{pos:?}");
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn progress_lines_show_percent_rate_and_time_left() {
        let line = progress_line(25, 100, std::time::Duration::from_secs(10));
        assert_eq!(
            line,
            "Converting world: 25% (25/100 regions, 2.5 regions/s, 30s left)"
        );
        assert!(progress_line(0, 0, std::time::Duration::ZERO).contains("100%"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn memory_to_sqlite_and_back_round_trips_documents_and_entities() {
        let source = MemoryBackend::default();
        sample_world(&source);

        let dir = TestDir::new();
        let sqlite = SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
        let report = convert_world(&source, &sqlite).unwrap();
        assert_eq!(report.documents, 7);
        assert_eq!(report.entity_records, 2);
        assert_eq!(report.dimensions, 2);
        assert_world_equal(&source, &sqlite);

        let back = MemoryBackend::default();
        let report = convert_world(&sqlite, &back).unwrap();
        assert_eq!(report.documents, 7);
        assert_eq!(report.entity_records, 2);
        assert_world_equal(&source, &back);
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_and_sqlite_share_a_blob_format_so_chunks_copy_without_reencoding() {
        let vanilla_dir = TestDir::new();
        let vanilla = VanillaBackend::new(vanilla_dir.path());
        let sqlite_dir = TestDir::new();
        let sqlite = SqliteBackend::open(sqlite_dir.path().join("world.mcserver")).unwrap();
        assert_eq!(
            vanilla.chunk_codec().blob_format(),
            sqlite.chunk_codec().blob_format()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn config_round_trips_through_convert_config() {
        let source_dir = TestDir::new();
        let destination_dir = TestDir::new();
        let source = tuff_storage::vanilla::config::VanillaConfig::new(source_dir.path());
        let destination = tuff_storage::vanilla::config::VanillaConfig::new(destination_dir.path());
        source.write_properties(&sample_properties()).unwrap();
        let lists = sample_lists();
        source.write_ops(&lists.ops).unwrap();
        source.write_whitelist(&lists.whitelist).unwrap();
        source.write_bans(&lists.bans).unwrap();
        source.write_ip_bans(&lists.ip_bans).unwrap();
        source
            .write_icon(Some(&crate::status::tests::png_header(16, 16)))
            .unwrap();
        let settings = tuff_storage::config::fixtures::sample_settings();
        source.write_settings(&settings).unwrap();

        let count = convert_config(&source, &destination).unwrap();
        assert_eq!(
            count,
            1 + lists.ops.len()
                + lists.whitelist.len()
                + lists.bans.len()
                + lists.ip_bans.len()
                + 1
                + settings.len()
        );
        assert_eq!(destination.read_settings().unwrap(), settings);
        assert_eq!(
            destination.read_properties().unwrap(),
            source.read_properties().unwrap()
        );
        assert_eq!(destination.read_lists().unwrap(), lists);
        assert_eq!(
            destination.read_icon().unwrap(),
            source.read_icon().unwrap()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn extra_files_are_carried_through_sqlar() {
        let vanilla_dir = TestDir::new();
        let vanilla = VanillaBackend::new(vanilla_dir.path());
        vanilla
            .write_extra_file("datapacks/custom/pack.mcmeta", b"{\"pack\":{}}")
            .unwrap();
        vanilla
            .write_extra_file(
                "dimensions/minecraft/overworld/notes/r.0.0.txt",
                b"poi-data",
            )
            .unwrap();

        let sqlite_dir = TestDir::new();
        let sqlite = SqliteBackend::open(sqlite_dir.path().join("world.mcserver")).unwrap();
        let mut report = ConvertReport::default();
        copy_extra_files(&vanilla, &sqlite, &mut report).unwrap();
        assert_eq!(report.extra_files, 2);
        let mut listed = sqlite.list_extra_files().unwrap();
        listed.sort();
        assert_eq!(
            listed,
            vec![
                "datapacks/custom/pack.mcmeta".to_owned(),
                "dimensions/minecraft/overworld/notes/r.0.0.txt".to_owned(),
            ]
        );
        assert_eq!(
            sqlite
                .read_extra_file("datapacks/custom/pack.mcmeta")
                .unwrap(),
            b"{\"pack\":{}}"
        );

        let back_dir = TestDir::new();
        let back = VanillaBackend::new(back_dir.path());
        let mut report = ConvertReport::default();
        copy_extra_files(&sqlite, &back, &mut report).unwrap();
        assert_eq!(report.extra_files, 2);
        assert_eq!(
            std::fs::read(back_dir.path().join("datapacks/custom/pack.mcmeta")).unwrap(),
            b"{\"pack\":{}}"
        );
        assert_eq!(
            std::fs::read(
                back_dir
                    .path()
                    .join("dimensions/minecraft/overworld/notes/r.0.0.txt")
            )
            .unwrap(),
            b"poi-data"
        );
    }

    fn sample_pois() -> Vec<tuff_world_model::poi::Record> {
        use tuff_vanilla_data::registry::PointOfInterestType;
        vec![tuff_world_model::poi::Record {
            free: 0,
            ..tuff_world_model::poi::Record::new(
                tuff_world_model::BlockPos::new(165, 70, 166),
                PointOfInterestType::Home,
            )
        }]
    }

    /// Builds a sample vanilla instance at `instance`: world data (from the
    /// real 26.3 fixtures) under `<instance>/world`, configuration directly
    /// under `<instance>`, matching how `ConvertEndpoint` interprets a
    /// vanilla path.
    fn build_sample_vanilla_world(instance: &Path) {
        use tuff_storage::vanilla::config::VanillaConfig;

        let root = instance.join(crate::world::DEFAULT_LEVEL_NAME);
        std::fs::create_dir_all(&root).unwrap();
        let paths = vanilla::WorldPaths::new(&root);
        std::fs::copy(Path::new(FIXTURES).join("level.dat"), paths.level_dat()).unwrap();
        for (file, id) in [
            ("world_clocks.dat", "minecraft:world_clocks"),
            ("world_gen_settings.dat", "minecraft:world_gen_settings"),
        ] {
            let target = paths.saved_data(id);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(Path::new(FIXTURES).join(file), target).unwrap();
        }
        let border = paths.dimension_data(OVERWORLD, "world_border");
        std::fs::create_dir_all(border.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(FIXTURES).join("world_border.dat"), border).unwrap();
        let region_dir = paths.region_dir(OVERWORLD);
        std::fs::create_dir_all(&region_dir).unwrap();
        std::fs::copy(
            Path::new(FIXTURES).join("r.0.0.deflate.mca"),
            region_dir.join("r.0.0.mca"),
        )
        .unwrap();

        let uuid = 0x069a_79f4_44e9_4726_a5be_fca9_0e38_aaf5u128;
        let backend = VanillaBackend::new(&root);
        backend
            .documents()
            .write(
                Document::Player(uuid),
                Compound::new().with("Health", 20.0f32),
            )
            .unwrap();
        backend
            .documents()
            .write(
                Document::Stats(uuid),
                Compound::new()
                    .with("stats", Compound::new())
                    .with("DataVersion", 5023),
            )
            .unwrap();
        backend
            .documents()
            .write(
                Document::Advancements(uuid),
                Compound::new().with("DataVersion", 5023),
            )
            .unwrap();
        backend
            .write_extra_file("datapacks/custom/pack.mcmeta", b"{\"pack\":{}}")
            .unwrap();
        backend
            .write_extra_file(
                "dimensions/minecraft/overworld/notes/r.0.0.txt",
                b"poi-bytes",
            )
            .unwrap();
        let pos = ChunkPos::new(10, 10);
        let poi_blob = backend
            .chunk_codec()
            .encode_pois(pos, &sample_pois())
            .unwrap()
            .unwrap();
        backend
            .open_region(OVERWORLD, pos.file_region())
            .unwrap()
            .write_pois(pos, Some(&poi_blob))
            .unwrap();

        let config = VanillaConfig::new(instance);
        config.write_properties(&sample_properties()).unwrap();
        let lists = sample_lists();
        config.write_ops(&lists.ops).unwrap();
        config.write_whitelist(&lists.whitelist).unwrap();
        config.write_bans(&lists.bans).unwrap();
        config.write_ip_bans(&lists.ip_bans).unwrap();
        config
            .write_icon(Some(&crate::status::tests::png_header(16, 16)))
            .unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn convert_offline_refuses_an_existing_destination() {
        let source_dir = TestDir::new();
        let destination_dir = TestDir::new();
        let source = ConvertEndpoint {
            backend: vanilla::NAME,
            path: source_dir.path().join("instance"),
            options: BackendOptions::default(),
        };
        // TestDir already created this directory, so it "exists".
        let destination = ConvertEndpoint {
            backend: vanilla::NAME,
            path: destination_dir.path().to_path_buf(),
            options: BackendOptions::default(),
        };
        let err = convert_offline(&source, &destination).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn convert_offline_refuses_a_locked_source_and_leaves_it_untouched() {
        let source_dir = TestDir::new();
        let source_path = source_dir.path().join("instance");
        build_sample_vanilla_world(&source_path);
        let before = std::fs::read(
            source_path
                .join(crate::world::DEFAULT_LEVEL_NAME)
                .join("level.dat"),
        )
        .unwrap();

        let backend = VanillaBackend::new(source_path.join(crate::world::DEFAULT_LEVEL_NAME));
        let _lock = backend.lock().unwrap();

        let destination_dir = TestDir::new();
        let destination_path = destination_dir.path().join("out");
        let source = ConvertEndpoint {
            backend: vanilla::NAME,
            path: source_path.clone(),
            options: BackendOptions::default(),
        };
        let destination = ConvertEndpoint {
            backend: vanilla::NAME,
            path: destination_path.clone(),
            options: BackendOptions::default(),
        };
        let err = convert_offline(&source, &destination).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(!destination_path.exists());
        assert_eq!(
            std::fs::read(
                source_path
                    .join(crate::world::DEFAULT_LEVEL_NAME)
                    .join("level.dat")
            )
            .unwrap(),
            before
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn convert_offline_round_trips_a_real_world_through_sqlite_and_back() {
        let source_dir = TestDir::new();
        let source_path = source_dir.path().join("instance");
        build_sample_vanilla_world(&source_path);
        let settings = tuff_storage::config::fixtures::sample_settings();
        tuff_storage::vanilla::config::VanillaConfig::new(&source_path)
            .write_settings(&settings)
            .unwrap();

        let mid_dir = TestDir::new();
        let sqlite_path = mid_dir.path().join("nested").join("world.mcserver");
        let source = ConvertEndpoint {
            backend: vanilla::NAME,
            path: source_path.clone(),
            options: BackendOptions::default(),
        };
        let mid = ConvertEndpoint {
            backend: sqlite::NAME,
            path: sqlite_path.clone(),
            options: BackendOptions::default(),
        };
        let report = convert_offline(&source, &mid).unwrap();
        assert_eq!(report.documents, 7);
        assert_eq!(report.chunks, 2);
        assert_eq!(report.poi_records, 1);
        assert_eq!(report.extra_files, 2);
        assert_eq!(report.dimensions, 1);
        assert!(report.config_items > 0);
        assert!(sqlite_path.is_file());

        let back_dir = TestDir::new();
        let back_path = back_dir.path().join("instance2");
        let back = ConvertEndpoint {
            backend: vanilla::NAME,
            path: back_path.clone(),
            options: BackendOptions::default(),
        };
        let report2 = convert_offline(&mid, &back).unwrap();
        assert_eq!(report2.documents, report.documents);
        assert_eq!(report2.chunks, report.chunks);
        assert_eq!(report2.extra_files, report.extra_files);

        let source_backend =
            VanillaBackend::new(source_path.join(crate::world::DEFAULT_LEVEL_NAME));
        let back_backend = VanillaBackend::new(back_path.join(crate::world::DEFAULT_LEVEL_NAME));
        assert_world_equal(&source_backend, &back_backend);

        let source_config = tuff_storage::vanilla::config::VanillaConfig::new(&source_path);
        let back_config = tuff_storage::vanilla::config::VanillaConfig::new(&back_path);
        assert_eq!(
            source_config.read_properties().unwrap(),
            back_config.read_properties().unwrap()
        );
        assert_eq!(
            source_config.read_lists().unwrap(),
            back_config.read_lists().unwrap()
        );
        assert_eq!(back_config.read_settings().unwrap(), settings);
        assert_eq!(
            source_config.read_icon().unwrap(),
            back_config.read_icon().unwrap()
        );

        let mut source_extra = source_backend.list_extra_files().unwrap();
        let mut back_extra = back_backend.list_extra_files().unwrap();
        source_extra.sort();
        back_extra.sort();
        assert_eq!(source_extra, back_extra);
        for path in &source_extra {
            assert_eq!(
                source_backend.read_extra_file(path).unwrap(),
                back_backend.read_extra_file(path).unwrap(),
                "{path}"
            );
        }

        let pos = ChunkPos::new(10, 10);
        let mut source_region = source_backend
            .open_region(OVERWORLD, pos.file_region())
            .unwrap();
        let mut back_region = back_backend
            .open_region(OVERWORLD, pos.file_region())
            .unwrap();
        let source_blob = source_region.read_chunk(pos).unwrap().unwrap();
        let back_blob = back_region.read_chunk(pos).unwrap().unwrap();
        let codec = source_backend.chunk_codec();
        let back_pois = back_region.read_pois(pos).unwrap().unwrap();
        assert_eq!(
            codec.decode_pois(&back_pois, pos).unwrap(),
            Some(sample_pois())
        );
        let source_decoded = codec
            .decode_chunk(&source_blob, pos, ChunkShape::OVERWORLD)
            .unwrap();
        let back_decoded = codec
            .decode_chunk(&back_blob, pos, ChunkShape::OVERWORLD)
            .unwrap();
        match (source_decoded, back_decoded) {
            (Decoded::Chunk(source_chunk, source_meta), Decoded::Chunk(back_chunk, back_meta)) => {
                assert_eq!(source_chunk, back_chunk);
                assert_eq!(source_meta, back_meta);
            }
            _ => panic!("expected generated terrain"),
        }
    }

    struct AddEntity;

    impl crate::storage::normalise::SaveNormaliser for AddEntity {
        fn name(&self) -> &'static str {
            "add_entity"
        }

        fn normalise(&self, view: &mut SaveView) -> Result<(), String> {
            let dimension = view.dimension.clone();
            if let Some(entities) = view.entities.as_mut() {
                entities.push(Compound::new().with("id", dimension));
            }
            Ok(())
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn conversion_applies_save_normalisers_to_entity_records() {
        use crate::storage::normalise::SaveNormaliserEntry;
        let source = MemoryBackend::default();
        sample_world(&source);
        let normalisers = SaveNormalisers::new(vec![SaveNormaliserEntry {
            order: 0,
            hook: std::sync::Arc::new(AddEntity),
        }]);
        let plain = MemoryBackend::default();
        convert_world(&source, &plain).unwrap();
        let normalised = MemoryBackend::default();
        convert_world_with(&source, &normalised, &normalisers, Biomes::vanilla()).unwrap();
        for (backend, extra) in [(&plain, 0), (&normalised, 1)] {
            for (dimension, pos, base) in [
                (OVERWORLD, ChunkPos::new(3, 40), "minecraft:cow"),
                (NETHER, ChunkPos::new(-31, 5), "minecraft:ghast"),
            ] {
                let mut store = backend.open_region(dimension, pos.file_region()).unwrap();
                let blob = store.read_entities(pos).unwrap().unwrap();
                let ids: Vec<String> = backend
                    .chunk_codec()
                    .decode_entities(&blob, pos)
                    .unwrap()
                    .iter()
                    .map(|c| c.get_str("id").unwrap().to_owned())
                    .collect();
                assert_eq!(ids.len(), 1 + extra);
                assert_eq!(ids[0], base);
                if extra == 1 {
                    assert_eq!(ids[1], dimension);
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn force_upgrade_in_place_swaps_the_world_and_keeps_the_old_one() {
        let dir = TestDir::new();
        let instance = dir.path().join("instance");
        build_sample_vanilla_world(&instance);
        let source = ConvertEndpoint {
            backend: vanilla::NAME,
            path: instance.clone(),
            options: BackendOptions::default(),
        };
        let world = source.world_path();
        let report = force_upgrade_in_place(&source, true).unwrap();
        assert_eq!(report.chunks, 2);
        assert!(world.is_dir());
        let backup = instance.join(format!("{}.pre-upgrade", crate::world::DEFAULT_LEVEL_NAME));
        assert!(backup.is_dir());
        assert!(
            !instance
                .join(format!("{}.upgrading", crate::world::DEFAULT_LEVEL_NAME))
                .exists()
        );
        assert_world_equal(&VanillaBackend::new(&backup), &VanillaBackend::new(&world));
        let err = force_upgrade_in_place(&source, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[tuff_core::watchdog]
    #[test]
    fn force_upgrade_in_place_ignores_a_missing_world() {
        let dir = TestDir::new();
        let source = ConvertEndpoint {
            backend: vanilla::NAME,
            path: dir.path().join("fresh"),
            options: BackendOptions::default(),
        };
        let report = force_upgrade_in_place(&source, false).unwrap();
        assert_eq!(report, ConvertReport::default());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_forced_rewrite_forces_reencoding_and_erase_cache_marks_chunks_for_relight() {
        use tuff_vanilla_data::Block;
        use tuff_world_model::chunk::Chunk;
        use tuff_world_model::chunk_meta::ChunkMeta;

        assert!(!rewrite_normalisers(false).is_empty());
        let pos = ChunkPos::new(0, 0);
        let view = || SaveView {
            dimension: OVERWORLD.to_owned(),
            pos,
            blocks: Some((
                Chunk::uniform(
                    ChunkShape::OVERWORLD,
                    Block::Air.default_state(),
                    tuff_world_model::chunk::plains(),
                ),
                ChunkMeta::default(),
            )),
            entities: None,
        };
        let relight = |erase| {
            let out = rewrite_normalisers(erase).apply(view());
            out.blocks.unwrap().1.relight
        };
        assert!(!relight(false));
        assert!(relight(true));
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_biomes_survive_a_decode_and_encode_conversion() {
        use crate::storage::codec::NbtChunkCodec;
        use crate::world::custom_dimension::tests::{collect, definition};
        use tuff_vanilla_data::Block;
        use tuff_world_model::chunk::Chunk;
        use tuff_world_model::chunk_meta::ChunkMeta;

        let sky = r#"{"has_precipitation": false, "temperature": 0.5, "downfall": 0.0,
            "effects": {}}"#;
        let custom = collect(vec![definition(
            "ex:sky",
            "minecraft:overworld",
            "",
            &[("ex:cloud", sky)],
        )]);
        let biomes = custom.biomes();
        let cloud = biomes.id("ex:cloud").unwrap();
        let codec = NbtChunkCodec::new(std::sync::Arc::new(tuff_storage::blobs::NbtBlobCodec {
            compression: None,
        }));
        let pos = ChunkPos::new(0, 0);
        let chunk = Chunk::uniform(ChunkShape::OVERWORLD, Block::Air.default_state(), cloud);
        let blob = codec
            .encode_chunk_in(&chunk, &ChunkMeta::generated(), pos, 0, biomes)
            .unwrap();
        let convert = |biomes: &Biomes| {
            let (_, out, _) = convert_chunk_blobs(
                pos,
                Some(blob.clone()),
                None,
                false,
                (OVERWORLD, &SaveNormalisers::default(), biomes),
                &codec,
                &codec,
            )
            .unwrap();
            let decoded = codec
                .decode_chunk_in(&out.unwrap(), pos, ChunkShape::OVERWORLD, biomes)
                .unwrap();
            match decoded {
                Decoded::Chunk(chunk, _) => chunk.biome(0, 0, 0),
                Decoded::Ungenerated(_) => panic!("expected terrain"),
            }
        };
        assert_eq!(convert(biomes), cloud);
        assert_eq!(
            convert(Biomes::vanilla()),
            tuff_world_model::chunk::plains()
        );
        let preserving = Biomes::preserving();
        let (_, out, _) = convert_chunk_blobs(
            pos,
            Some(blob.clone()),
            None,
            false,
            (OVERWORLD, &SaveNormalisers::default(), &preserving),
            &codec,
            &codec,
        )
        .unwrap();
        match codec
            .decode_chunk_in(&out.unwrap(), pos, ChunkShape::OVERWORLD, biomes)
            .unwrap()
        {
            Decoded::Chunk(chunk, _) => assert_eq!(chunk.biome(0, 0, 0), cloud),
            Decoded::Ungenerated(_) => panic!("expected terrain"),
        }
    }
}

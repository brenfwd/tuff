use std::path::{Path, PathBuf};

use tuff_core::storage::chunk_nbt;
use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};
use tuff_storage::region_file::RegionFile;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::chunk_meta::Decoded;
use tuff_world_model::pos::ChunkPos;

pub const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/worldgen/vanilla"
);
pub const VARIANTS: [&str; 4] = ["noise", "surface", "carvers", "full"];
/// The force-loaded chunks of `scripts/worldgen-fixture.sh`: a few spots,
/// then eroded badlands, frozen and deep frozen ocean, mangrove swamp, deep
/// dark, jagged peaks, ice spikes, mushroom fields, desert, dripstone
/// caves, warm ocean and deep dark; in the nether basalt deltas, crimson
/// forest, warped forest and soul sand valley; in the end a spike, end
/// highlands, small end islands, the arrival platform and a gateway; then
/// chunks holding the middle of ruined portals of every kind (land,
/// underground, ocean, giant in a mountain, jungle, swamp, desert, nether
/// among nether fossils), a nether chunk whose terrain adapts to a
/// nether fossil; then a pillager outpost, plains (over a trial chamber),
/// snowy, taiga and savanna village centers, and in the nether a fortress
/// chunk with a blaze spawner and a chest, and treasure, units, hoglin
/// stable and bridge bastions; then a stronghold portal room, a stronghold
/// library and chest corridor, a desert pyramid, a desert village, a jungle
/// temple, a swamp hut, an igloo with a basement, an ocean and a beached
/// shipwreck, buried treasure, and warm and cold ocean ruins; an end city
/// ship and a woodland mansion; two ocean monument starts.
pub const CENTERS: [(&str, [i32; 2]); 64] = [
    ("overworld", [0, 0]),
    ("overworld", [100, -50]),
    ("overworld", [-200, 300]),
    ("overworld", [-563, -125]),
    ("overworld", [360, -552]),
    ("overworld", [24, -96]),
    ("overworld", [-96, -184]),
    ("overworld", [192, -152]),
    ("overworld", [-96, 56]),
    ("overworld", [-48, -176]),
    ("overworld", [16, -80]),
    ("overworld", [-56, 112]),
    ("overworld", [16, 560]),
    ("overworld", [-12, -90]),
    ("overworld", [40, 640]),
    ("overworld", [-156, -186]),
    ("the_nether", [0, 0]),
    ("the_nether", [30, -20]),
    ("the_nether", [-6, 6]),
    ("the_nether", [12, 18]),
    ("the_nether", [-18, 6]),
    ("the_nether", [0, 12]),
    ("the_end", [0, 0]),
    ("the_end", [100, 10]),
    ("the_end", [-3, -1]),
    ("the_end", [-12, -72]),
    ("the_end", [-66, 0]),
    ("the_end", [6, 0]),
    ("the_end", [89, 4]),
    ("overworld", [19, 18]),
    ("overworld", [1, 61]),
    ("overworld", [-25, 15]),
    ("overworld", [-111, -33]),
    ("overworld", [-147, 8]),
    ("overworld", [-345, 0]),
    ("overworld", [21, 569]),
    ("the_nether", [15, -40]),
    ("the_nether", [13, -40]),
    ("overworld", [70, 136]),
    ("overworld", [39, 51]),
    ("overworld", [-16, -58]),
    ("overworld", [-48, -82]),
    ("overworld", [125, -188]),
    ("the_nether", [22, -10]),
    ("the_nether", [13, 1]),
    ("the_nether", [31, -10]),
    ("the_nether", [-16, -29]),
    ("the_nether", [15, -60]),
    ("overworld", [109, 43]),
    ("overworld", [-10, -113]),
    ("overworld", [17, 597]),
    ("overworld", [22, 580]),
    ("overworld", [-151, 18]),
    ("overworld", [-255, -86]),
    ("overworld", [-9, -59]),
    ("overworld", [-13, -20]),
    ("overworld", [28, 10]),
    ("overworld", [27, -41]),
    ("overworld", [-18, 48]),
    ("overworld", [-13, 1]),
    ("the_end", [6, 98]),
    ("overworld", [118, 15]),
    ("overworld", [76, -76]),
    ("overworld", [101, -88]),
];
/// Chunks kept from the full run only for their structure starts: where
/// the structures of the centers start. They are not generated fully.
pub const STARTS_KEPT: [(&str, [i32; 2]); 24] = [
    ("overworld", [-4, 0]),
    ("overworld", [-55, 109]),
    ("overworld", [-24, 15]),
    ("overworld", [-112, -33]),
    ("overworld", [-147, 9]),
    ("overworld", [-344, 0]),
    ("overworld", [22, 569]),
    ("the_nether", [10, -40]),
    ("the_nether", [12, -40]),
    ("overworld", [-22, 11]),
    ("overworld", [195, -153]),
    ("the_nether", [-6, 9]),
    ("the_nether", [21, -8]),
    ("the_nether", [12, 0]),
    ("the_nether", [33, -9]),
    ("the_nether", [-16, -27]),
    ("the_nether", [15, -59]),
    ("overworld", [40, 51]),
    ("overworld", [-15, -57]),
    ("overworld", [-48, -81]),
    ("overworld", [126, -188]),
    ("overworld", [108, 44]),
    ("overworld", [-9, -113]),
    ("the_end", [6, 103]),
];
pub(super) const KEPT_KEYS: [&str; 9] = [
    "DataVersion",
    "xPos",
    "yPos",
    "zPos",
    "Status",
    "sections",
    "Heightmaps",
    "structures",
    "block_entities",
];

/// The chunks kept for a variant: every center, plus the 3x3 around the
/// overworld origin in the full run.
pub fn kept(variant: &str) -> Vec<(&'static str, ChunkPos)> {
    let mut chunks: Vec<(&str, ChunkPos)> = CENTERS
        .iter()
        .map(|(dim, [x, z])| (*dim, ChunkPos::new(*x, *z)))
        .collect();
    if variant == "full" {
        for dz in -1..=1 {
            for dx in -1..=1 {
                if dx != 0 || dz != 0 {
                    chunks.push(("overworld", ChunkPos::new(dx, dz)));
                }
            }
        }
    }
    chunks
}

pub(super) fn read_root(dir: &Path, pos: ChunkPos) -> Option<Compound> {
    let region = pos.file_region();
    let mut file = RegionFile::open(dir, region.x, region.z, false).ok()??;
    let raw = file.read(pos).ok()??;
    let bytes = raw.decompress(1 << 24).unwrap();
    match NamedTag::read(&mut &bytes[..], NbtLimits::TRUSTED)
        .unwrap()
        .tag
    {
        Tag::Compound(root) => Some(root),
        _ => panic!("chunk root is not a compound"),
    }
}

fn fixture_file(variant: &str) -> PathBuf {
    Path::new(FIXTURE_DIR).join(format!("{variant}.nbt"))
}

pub(super) fn key(dimension: &str, pos: ChunkPos) -> String {
    format!("{dimension} {} {}", pos.x, pos.z)
}

pub fn shape(dimension: &str) -> ChunkShape {
    match dimension {
        "overworld" => ChunkShape::OVERWORLD,
        _ => ChunkShape::new(0, 256),
    }
}

pub(super) fn variant_chunks(variant: &str) -> Compound {
    let data = std::fs::read(fixture_file(variant)).expect("worldgen fixture");
    let raw = Compression::Gzip.decompress(&data, 1 << 28).unwrap();
    match NamedTag::read(&mut &raw[..], NbtLimits::TRUSTED)
        .unwrap()
        .tag
    {
        Tag::Compound(root) => root,
        _ => panic!("fixture root is not a compound"),
    }
}

/// A vanilla chunk from the fixture, with its raw NBT.
pub fn load(variant: &str, dimension: &str, pos: ChunkPos) -> (Chunk, Compound) {
    let chunks = variant_chunks(variant);
    let root = chunks
        .get_compound(&key(dimension, pos))
        .unwrap_or_else(|| panic!("no {variant} {dimension} chunk {pos:?} in the fixture"))
        .clone();
    let Decoded::Chunk(chunk, _) = chunk_nbt::decode(root.clone(), pos, shape(dimension)).unwrap()
    else {
        panic!("{variant} {dimension} {pos:?} holds no terrain");
    };
    (*chunk, root)
}

#[tuff_core::watchdog]
#[test]
#[ignore = "run by scripts/worldgen-fixture.sh"]
fn trim_worldgen_fixture() {
    let work = std::env::var("MC_WORLDGEN_FIXTURE_WORK").expect("MC_WORLDGEN_FIXTURE_WORK");
    let _ = std::fs::remove_dir_all(FIXTURE_DIR);
    std::fs::create_dir_all(FIXTURE_DIR).unwrap();
    for variant in VARIANTS {
        let mut chunks = Compound::new();
        for (dimension, pos) in kept(variant) {
            let source = Path::new(&work)
                .join(variant)
                .join("world/dimensions/minecraft")
                .join(dimension)
                .join("region");
            let root = read_root(&source, pos)
                .unwrap_or_else(|| panic!("vanilla did not save {variant} {dimension} {pos:?}"));
            assert_eq!(
                root.get_str("Status"),
                Some("minecraft:full"),
                "{variant} {dimension} {pos:?}"
            );
            let mut kept = Compound::new();
            for key in KEPT_KEYS {
                if let Some(value) = root.get(key) {
                    kept.insert(key, value.clone());
                }
            }
            if let Some(Tag::List(sections)) = kept.get("sections").cloned() {
                let stripped: Vec<Tag> = sections
                    .into_iter()
                    .map(|section| match section {
                        Tag::Compound(mut c) => {
                            c.remove("SkyLight");
                            c.remove("BlockLight");
                            Tag::Compound(c)
                        }
                        other => other,
                    })
                    .collect();
                kept.insert("sections", Tag::List(stripped));
            }
            chunks.insert(key(dimension, pos), kept);
        }
        if variant == "full" {
            for (dimension, [x, z]) in STARTS_KEPT {
                let pos = ChunkPos::new(x, z);
                let source = Path::new(&work)
                    .join(variant)
                    .join("world/dimensions/minecraft")
                    .join(dimension)
                    .join("region");
                let root = read_root(&source, pos)
                    .unwrap_or_else(|| panic!("vanilla did not save {dimension} {pos:?}"));
                let mut kept = Compound::new();
                for key in ["DataVersion", "Status", "structures"] {
                    if let Some(value) = root.get(key) {
                        kept.insert(key, value.clone());
                    }
                }
                chunks.insert(format!("starts {}", key(dimension, pos)), kept);
            }
        }
        let mut bytes = Vec::new();
        NamedTag::new("", chunks).write(&mut bytes).unwrap();
        std::fs::write(
            fixture_file(variant),
            Compression::Gzip.compress(bytes).unwrap(),
        )
        .unwrap();
    }
}

use std::sync::{Arc, OnceLock};

use tuff_vanilla_data::worldgen::NoiseSettings;

use crate::biome_source::{BiomeSource, MultiNoise, TheEnd};
use crate::generator::NoiseGenerator;
use crate::proto::ProtoChunk;
use tuff_world_model::chunk::biome_name;

pub fn generator(dimension: &str) -> &'static NoiseGenerator {
    static GENERATORS: OnceLock<[NoiseGenerator; 3]> = OnceLock::new();
    let all = GENERATORS.get_or_init(|| {
        let make = |settings: &str, source: Arc<dyn BiomeSource>| {
            NoiseGenerator::new(NoiseSettings::get(settings).unwrap(), source, 1)
        };
        [
            make(
                "minecraft:overworld",
                Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
            ),
            make(
                "minecraft:nether",
                Arc::new(MultiNoise::preset("minecraft:nether").unwrap()),
            )
            .without_sky_light(),
            make("minecraft:end", Arc::new(TheEnd::new())),
        ]
    });
    match dimension {
        "overworld" => &all[0],
        "the_nether" => &all[1],
        _ => &all[2],
    }
}

/// Maps `items` on plain threads instead of the rayon pool. The chunk
/// caches below block while a chunk generates, and generation itself uses
/// rayon: a rayon worker waiting inside one cache could pick up another
/// item that needs the same chunk and wait on itself forever.
pub fn map_on_threads<T: Sync, R: Send>(items: &[T], map: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(items.len())
        .max(1);
    let mut results: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let _fixtures = tuff_item::item::conservation::fixture_thread();
                    tuff_datapack::tags::built_in_scope(|| {
                        let mut done = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(index) else {
                                return done;
                            };
                            done.push((index, map(item)));
                        }
                    })
                })
            })
            .collect();
        for worker in workers {
            for (index, result) in worker.join().unwrap() {
                results[index] = Some(result);
            }
        }
    });
    results.into_iter().map(Option::unwrap).collect()
}

pub fn diff(ours: &ProtoChunk, theirs: &Chunk, limit: usize) -> (usize, usize, Vec<String>) {
    let shape = ours.shape;
    let mut blocks = 0;
    let mut biomes = 0;
    let mut lines = Vec::new();
    for y in shape.min_y..shape.min_y + shape.height {
        for z in 0..16 {
            for x in 0..16 {
                let (a, b) = (ours.get(x, y, z), theirs.get(x, y, z));
                if a != b {
                    blocks += 1;
                    if lines.len() < limit {
                        lines.push(format!("block {x} {y} {z}: ours {a:?} vanilla {b:?}"));
                    }
                }
            }
        }
    }
    for qy in (shape.min_y >> 2)..((shape.min_y + shape.height) >> 2) {
        for qz in 0..4 {
            for qx in 0..4 {
                let a = ours.biome(qx, qy, qz);
                let b = theirs.biome(qx * 4, qy * 4, qz * 4);
                if a != b {
                    biomes += 1;
                    if lines.len() < limit {
                        lines.push(format!(
                            "biome {qx} {qy} {qz}: ours {} vanilla {}",
                            biome_name(a),
                            biome_name(b)
                        ));
                    }
                }
            }
        }
    }
    (blocks, biomes, lines)
}
/// The structures starting in a chunk, computed once per test run.
pub fn starts(dimension: &str, pos: ChunkPos) -> Vec<Arc<crate::structure::StructureStart>> {
    type Cache =
        std::collections::HashMap<(String, ChunkPos), Vec<Arc<crate::structure::StructureStart>>>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (dimension.to_string(), pos);
    if let Some(starts) = cache.lock().unwrap().get(&key) {
        return starts.clone();
    }
    let starts = generator(dimension).structure_starts(pos, shape(dimension));
    cache.lock().unwrap().insert(key, starts.clone());
    starts
}

/// The terrain of one chunk with its structure starts and references,
/// generated once per test run.
pub fn structured_terrain(dimension: &str, pos: ChunkPos) -> ProtoChunk {
    assert!(
        rayon::current_thread_index().is_none(),
        "the terrain cache blocks; call it off the rayon pool (see map_on_threads)"
    );
    type Cache = std::collections::HashMap<(String, ChunkPos), Arc<OnceLock<ProtoChunk>>>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    let cell = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry((dimension.to_string(), pos))
        .or_default()
        .clone();
    cell.get_or_init(|| generate_structured_terrain(dimension, pos))
        .clone()
}

fn generate_structured_terrain(dimension: &str, pos: ChunkPos) -> ProtoChunk {
    let generator = generator(dimension);
    let radius = crate::structure::REFERENCE_RADIUS;
    let mut near = std::collections::HashMap::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let p = ChunkPos::new(pos.x + dx, pos.z + dz);
            near.insert(p, starts(dimension, p));
        }
    }
    let references = generator
        .structures()
        .references(pos, |p| near.get(&p).map_or(&[][..], Vec::as_slice));
    generator.terrain(pos, shape(dimension), near[&pos].clone(), references)
}

/// The 5x5 chunks of terrain around `center`.
pub fn terrain(
    dimension: &str,
    center: ChunkPos,
) -> std::collections::HashMap<ChunkPos, ProtoChunk> {
    let mut chunks = std::collections::HashMap::new();
    for dz in -2..=2 {
        for dx in -2..=2 {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            chunks.insert(pos, structured_terrain(dimension, pos));
        }
    }
    chunks
}

/// Gives the chunks their own copies of the structure starts that reach
/// them. Pieces keep placement progress (a settled height, a placed
/// spawner), and the cached terrain shares its starts between decorations.
fn refresh_starts(dimension: &str, chunks: &mut std::collections::HashMap<ChunkPos, ProtoChunk>) {
    use crate::structure::StructureStart;

    let generator = generator(dimension);
    let mut fresh: std::collections::HashMap<(ChunkPos, &'static str), Arc<StructureStart>> =
        Default::default();
    let mut wanted: Vec<ChunkPos> = chunks
        .values()
        .flat_map(|c| {
            c.structure_starts
                .iter()
                .chain(c.structure_references.iter().flat_map(|r| r.starts.iter()))
                .map(|s| s.chunk)
        })
        .collect();
    wanted.sort_by_key(|p| (p.x, p.z));
    wanted.dedup();
    for pos in wanted {
        for start in generator.structure_starts(pos, shape(dimension)) {
            fresh.insert((pos, start.structure.name), start);
        }
    }
    let swap = |s: &mut Arc<StructureStart>| {
        if let Some(new) = fresh.get(&(s.chunk, s.structure.name)) {
            *s = new.clone();
        }
    };
    for chunk in chunks.values_mut() {
        chunk.structure_starts.iter_mut().for_each(swap);
        for refs in &mut chunk.structure_references {
            refs.starts.iter_mut().for_each(swap);
        }
    }
}

/// The 3x3 around a chunk in row order from the north-west.
pub const ROW_ORDER: [(i32, i32); 9] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (0, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// Decorates the 3x3 around `center` in `order` (offsets from `center`)
/// and returns the centre chunk once it and then its neighbours went live:
/// a neighbour's first fluid ticks can flow into the centre.
pub fn decorate_in_order(
    dimension: &str,
    center: ChunkPos,
    mut chunks: std::collections::HashMap<ChunkPos, ProtoChunk>,
    order: &[(i32, i32)],
) -> ProtoChunk {
    use crate::feature::level::Region;

    let generator = generator(dimension);
    refresh_starts(dimension, &mut chunks);
    for (dx, dz) in order {
        let middle = ChunkPos::new(center.x + dx, center.z + dz);
        let mut window = Vec::new();
        for wz in -1..=1 {
            for wx in -1..=1 {
                window.push(
                    chunks
                        .remove(&ChunkPos::new(middle.x + wx, middle.z + wz))
                        .unwrap(),
                );
            }
        }
        let mut region = Region::new(generator, middle, window);
        generator.decorate(&mut region);
        for chunk in region.into_chunks() {
            chunks.insert(chunk.pos, chunk);
        }
    }
    chunks.get_mut(&center).unwrap().post_process();
    go_live(dimension, center, &mut chunks);
    for (dx, dz) in ROW_ORDER {
        if (dx, dz) != (0, 0) {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            chunks.get_mut(&pos).unwrap().post_process();
            go_live(dimension, pos, &mut chunks);
        }
    }
    chunks.remove(&center).unwrap()
}

/// The decorated chunks as the live world sees them while the centre
/// chunk's post-processing runs. Nothing scheduled ever runs: the fixtures
/// are tick-frozen.
struct LiveWindow<'a> {
    chunks: &'a mut std::collections::HashMap<ChunkPos, ProtoChunk>,
    settings: tuff_core::world::fluid_api::Settings,
    random: tuff_item::random::LegacyRandom,
    /// Only computed when a marked block depends on it.
    light: Option<tuff_world_model::light::LevelLight>,
}

/// The chunks as the light engine reads them.
struct LightWindow(
    std::collections::HashMap<ChunkPos, (Chunk, tuff_world_model::light::SkyLightSources)>,
);

impl tuff_world_model::light::ChunkSource for LightWindow {
    fn chunk(&self, pos: ChunkPos) -> Option<tuff_world_model::light::LightChunk<'_>> {
        self.0.get(&pos).map(
            |(blocks, sky_sources)| tuff_world_model::light::LightChunk {
                blocks,
                sky_sources,
            },
        )
    }
}

/// Lights the window the way the game lights chunks before they go live.
fn light_window(
    dimension: &str,
    chunks: &std::collections::HashMap<ChunkPos, ProtoChunk>,
) -> tuff_world_model::light::LevelLight {
    let window = LightWindow(
        chunks
            .iter()
            .map(|(pos, proto)| {
                let chunk = proto.clone().into_chunk();
                let sources = tuff_world_model::light::SkyLightSources::from_chunk(&chunk);
                (*pos, (chunk, sources))
            })
            .collect(),
    );
    let mut light =
        tuff_world_model::light::LevelLight::new(shape(dimension), dimension != "the_nether");
    let mut positions: Vec<ChunkPos> = window.0.keys().copied().collect();
    positions.sort_by_key(|p| (p.z, p.x));
    for &pos in &positions {
        light.initialize_light(&window, pos, false);
    }
    for &pos in &positions {
        light.light_chunk(&window, pos, false);
    }
    light
}

impl LiveWindow<'_> {
    fn put(&mut self, pos: tuff_world_model::BlockPos, state: tuff_vanilla_data::BlockState) {
        if let Some(chunk) = self.chunks.get_mut(&ChunkPos::new(pos.x >> 4, pos.z >> 4))
            && chunk.shape.contains_y(pos.y)
        {
            chunk.set(pos.x & 15, pos.y, pos.z & 15, state);
        }
    }
}

impl tuff_core::world::fluid_api::FluidWorld for LiveWindow<'_> {
    fn block(&self, pos: tuff_world_model::BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        let chunk = self.chunks.get(&ChunkPos::new(pos.x >> 4, pos.z >> 4))?;
        chunk
            .shape
            .contains_y(pos.y)
            .then(|| chunk.get(pos.x & 15, pos.y, pos.z & 15))
    }

    fn settings(&self) -> tuff_core::world::fluid_api::Settings {
        self.settings
    }

    fn spread_to(
        &mut self,
        pos: tuff_world_model::BlockPos,
        direction: tuff_vanilla_data::block::Facing,
        fluid: tuff_vanilla_data::fluid::FluidState,
    ) {
        if let Some(state) = self.block(pos) {
            tuff_feature_builtin_vanilla_blocks::world::fluid::apply_spread(
                self, pos, state, direction, fluid,
            );
        }
    }

    fn random(&mut self, bound: u32) -> u32 {
        use tuff_item::random::RandomSource;
        self.random.next_int_bounded(bound.max(1) as i32) as u32
    }

    fn set(&mut self, pos: tuff_world_model::BlockPos, state: tuff_vanilla_data::BlockState) {
        use tuff_feature_builtin_vanilla_blocks::world::fluid;
        use tuff_vanilla_common::placement::{UPDATE_ORDER, step, update_shape};

        self.put(pos, state);
        let mut queue = std::collections::VecDeque::from([(pos, true)]);
        let mut budget = 512;
        while let Some((at, direct)) = queue.pop_front() {
            let Some(source) = self.block(at) else {
                continue;
            };
            for direction in UPDATE_ORDER {
                if budget == 0 {
                    return;
                }
                budget -= 1;
                let target = step(at, direction);
                let Some(old) = self.block(target) else {
                    continue;
                };
                if !old.fluid_state().is_empty() {
                    if direct {
                        fluid::liquid_changed(self, target, old);
                    }
                    if let Some(now) = self.block(target) {
                        fluid::shape_changed(self, target, now, source);
                    }
                }
                let Some(old) = self.block(target) else {
                    continue;
                };
                if tuff_world_model::chunk::is_air(old) {
                    continue;
                }
                let level = |p| {
                    self.block(p)
                        .unwrap_or(tuff_vanilla_data::Block::Air.default_state())
                };
                let new = update_shape(old, target, direction.opposite(), source, &level);
                if new != old {
                    let new = if tuff_world_model::chunk::is_air(new) {
                        tuff_vanilla_common::placement::remains(old)
                    } else {
                        new
                    };
                    self.put(target, new);
                    queue.push_back((target, false));
                }
            }
        }
    }

    fn schedule(
        &mut self,
        _: tuff_world_model::BlockPos,
        _: tuff_vanilla_data::fluid::Fluid,
        _: u32,
    ) {
    }

    fn wash_away(&mut self, _: tuff_world_model::BlockPos, _: tuff_vanilla_data::BlockState) {}

    fn place_liquid(
        &mut self,
        pos: tuff_world_model::BlockPos,
        placement: &'static tuff_vanilla_data::fluid::LiquidPlacement,
    ) {
        self.put(pos, placement.result);
    }

    fn fizz(&mut self, _: tuff_world_model::BlockPos) {}

    fn raw_brightness(&self, pos: tuff_world_model::BlockPos) -> Option<u8> {
        use tuff_world_model::light::{LightLayer, Pos};
        let light = self.light.as_ref()?;
        let at = Pos::new(pos.x, pos.y, pos.z);
        Some(
            light
                .light_value(LightLayer::Sky, at)
                .max(light.light_value(LightLayer::Block, at)),
        )
    }
}

/// Runs the post-processing the centre chunk gets as vanilla promotes it
/// to a live chunk: the first fluid tick and the shape update of every
/// marked position, section by section.
pub(super) fn go_live(
    dimension: &str,
    center: ChunkPos,
    chunks: &mut std::collections::HashMap<ChunkPos, ProtoChunk>,
) {
    use tuff_world_model::BlockPos;

    let mut positions = chunks[&center].post_processing.clone();
    positions.sort_by_key(|&(_, y, _)| y >> 4);
    let dimension_kind = format!("minecraft:{dimension}");
    let needs_light = positions.iter().any(|&(x, y, z)| {
        let chunk = &chunks[&center];
        chunk.shape.contains_y(y)
            && chunk.get(x & 15, y, z & 15).block().kind() == tuff_vanilla_data::BlockKind::Mushroom
    });
    let light = needs_light.then(|| light_window(dimension, chunks));
    let mut world = LiveWindow {
        light,
        chunks,
        settings: tuff_core::world::fluid_api::Settings {
            fast_lava: tuff_vanilla_data::fluid::fast_lava(&dimension_kind),
            water_source_conversion: true,
            lava_source_conversion: false,
        },
        random: tuff_item::random::LegacyRandom::new(0),
    };
    for (x, y, z) in positions {
        let pos = BlockPos::new(x, y, z);
        if let Some(shaped) =
            tuff_feature_builtin_vanilla_blocks::world::fluid::post_process(&mut world, pos)
        {
            world.put(pos, shaped);
        }
    }
    world
        .chunks
        .get_mut(&center)
        .unwrap()
        .post_processing
        .clear();
}

/// Generates the 5x5 terrain around `center`, decorates its 3x3 in row
/// order, and returns the centre chunk. A force-loaded vanilla chunk only
/// needs its direct neighbours decorated, so the ring beyond stays bare.
/// Vanilla decorates those nine in a scheduling-dependent order; row order
/// matches it for most fixture chunks.
pub fn decorated(dimension: &str, center: ChunkPos) -> ProtoChunk {
    decorate_in_order(dimension, center, terrain(dimension, center), &ROW_ORDER)
}

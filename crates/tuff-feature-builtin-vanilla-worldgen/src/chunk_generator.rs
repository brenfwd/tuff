//! Vanilla's noise generator as a core `ChunkGenerator`, and the
//! structure-start blocks and steps its pipeline runs through.

use std::sync::{Arc, OnceLock};

use crate::biome_source::{BiomeSourceConfig, BiomeSources};
use crate::feature::level::Region;
use crate::generator::{NoiseGenerator, Stages};
use crate::proto::ProtoChunk;
use crate::structure::{StructureStart, structures_nbt};
use tuff_core::storage::level::{DimensionSettings, FlatSettings};
use tuff_core::world::generator::{
    ChunkGenerator, ChunkPipeline, Finished, PipelineContext, WorldGenerators,
};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

/// Structure starts are computed for square blocks of chunks this wide.
pub const STARTS_BLOCK: i32 = 8;

/// The structure starts of every chunk of one block of
/// [`STARTS_BLOCK`] x [`STARTS_BLOCK`] chunks, computed from the seed alone.
#[derive(Default)]
pub struct StartsBlock {
    /// The block's coordinates (chunk coordinates divided by the width).
    pub block: (i32, i32),
    /// Per chunk, x fastest.
    pub starts: Vec<Vec<Arc<StructureStart>>>,
}

impl StartsBlock {
    pub fn of(pos: ChunkPos) -> (i32, i32) {
        (
            pos.x.div_euclid(STARTS_BLOCK),
            pos.z.div_euclid(STARTS_BLOCK),
        )
    }

    pub fn chunks(block: (i32, i32)) -> impl Iterator<Item = ChunkPos> {
        (0..STARTS_BLOCK).flat_map(move |dz| {
            (0..STARTS_BLOCK).map(move |dx| {
                ChunkPos::new(block.0 * STARTS_BLOCK + dx, block.1 * STARTS_BLOCK + dz)
            })
        })
    }

    pub fn get(&self, pos: ChunkPos) -> Option<&[Arc<StructureStart>]> {
        if Self::of(pos) != self.block {
            return None;
        }
        let (dx, dz) = (
            pos.x.rem_euclid(STARTS_BLOCK),
            pos.z.rem_euclid(STARTS_BLOCK),
        );
        self.starts
            .get((dz * STARTS_BLOCK + dx) as usize)
            .map(Vec::as_slice)
    }

    /// The blocks whose starts can reach `pos`.
    pub fn around(pos: ChunkPos) -> impl Iterator<Item = (i32, i32)> {
        let radius = crate::structure::REFERENCE_RADIUS;
        let (x0, z0) = Self::of(ChunkPos::new(pos.x - radius, pos.z - radius));
        let (x1, z1) = Self::of(ChunkPos::new(pos.x + radius, pos.z + radius));
        (z0..=z1).flat_map(move |z| (x0..=x1).map(move |x| (x, z)))
    }
}

/// The steps of a generator that decorates across chunk borders.
pub trait StagedGenerator: Send + Sync {
    /// The structure starts of one block of chunks (see [`StartsBlock`]).
    fn starts(&self, block: (i32, i32), _shape: ChunkShape) -> StartsBlock {
        StartsBlock {
            block,
            starts: Vec::new(),
        }
    }
    /// Terrain of one chunk: everything that needs no neighbours. `starts`
    /// holds the blocks [`StartsBlock::around`] names.
    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, starts: &[Arc<StartsBlock>]) -> ProtoChunk;
    /// A chunk read back from storage past its terrain, given the same
    /// structure starts its terrain was built with.
    fn resume(&self, proto: ProtoChunk, _starts: &[Arc<StartsBlock>]) -> ProtoChunk {
        proto
    }
    /// Places the features of `center` into the 3x3 window around it
    /// (row by row from the north-west) and returns the window.
    fn decorate(&self, center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk>;
    /// A chunk whose neighbours are all decorated, as the world stores it,
    /// with the block entity data its features left.
    fn finish(&self, proto: ProtoChunk) -> Finished;
    /// Drops caches that are rebuilt on demand; the world is idle.
    fn shrink(&self) {}
}

fn biome_config(settings: &DimensionSettings) -> Option<BiomeSourceConfig> {
    Some(BiomeSourceConfig {
        kind: settings.biome_source.clone()?,
        preset: settings.biome_preset.clone(),
        biome: settings.biome.clone(),
        biomes: settings.biome_list.clone(),
        scale: settings.biome_scale,
        points: settings.biome_points.clone(),
    })
}

pub fn vanilla(settings: &DimensionSettings, seed: i64) -> Option<Vanilla> {
    if settings.generator != "minecraft:noise" {
        return None;
    }
    let noise = tuff_vanilla_data::worldgen::NoiseSettings::get(settings.settings.as_deref()?)?;
    let config = biome_config(settings)?;
    BiomeSources::builtin().create_in(&config, crate::loaded::biomes::PackBiomes::lenient())?;
    Some(Vanilla {
        noise,
        bound: OnceLock::new(),
        config: Some(config),
        flat: None,
        seed,
        structures: settings.generate_structures,
        sky_light: tuff_vanilla_data::dimension_type::DimensionType::get(&settings.kind)
            .is_none_or(|kind| kind.has_skylight),
        disabled: OnceLock::new(),
        generator: OnceLock::new(),
    })
}

/// The spawn of a new world with the vanilla overworld.
pub fn overworld_spawn(seed: i64) -> BlockPos {
    let settings = DimensionSettings::vanilla();
    let overworld = settings
        .iter()
        .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
        .expect("vanilla has an overworld");
    for_settings(overworld, seed).spawn()
}

/// The vanilla generators as core sees them.
pub struct VanillaGenerators;

impl WorldGenerators for VanillaGenerators {
    fn generator(
        &self,
        settings: &DimensionSettings,
        seed: i64,
    ) -> Option<Arc<dyn ChunkGenerator>> {
        match settings.generator.as_str() {
            "minecraft:flat" => {
                let flat = settings.flat.clone().unwrap_or_default();
                return crate::flat::Flat::for_world(&flat, seed, settings.generate_structures)
                    .map(|generator| Arc::new(generator) as Arc<dyn ChunkGenerator>);
            }
            "minecraft:debug" => {
                return Some(Arc::new(crate::flat::Debug::new()));
            }
            _ => {}
        }
        vanilla(settings, seed).map(|generator| Arc::new(generator) as Arc<dyn ChunkGenerator>)
    }

    fn new_world_spawn(&self, seed: i64) -> BlockPos {
        overworld_spawn(seed)
    }
}

/// The generator for a dimension's settings, or a void.
pub fn for_settings(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    VanillaGenerators
        .generator(settings, seed)
        .unwrap_or_else(|| Arc::new(tuff_core::world::generator::Void))
}

/// Vanilla's noise-based generator, built on first use.
pub struct Vanilla {
    noise: &'static tuff_vanilla_data::worldgen::NoiseSettings,
    /// The settings in effect: `noise` relinked against the data packs
    /// once bound, else `noise` itself.
    bound: OnceLock<(
        &'static tuff_vanilla_data::worldgen::NoiseSettings,
        crate::loaded::Options,
    )>,
    config: Option<BiomeSourceConfig>,
    flat: Option<FlatSettings>,
    seed: i64,
    /// The world's `generate_structures` option.
    structures: bool,
    sky_light: bool,
    disabled: OnceLock<tuff_core::world::generator::DisabledStages>,
    generator: OnceLock<NoiseGenerator>,
}

impl Vanilla {
    /// The staged generator of a flat world; `None` when a layer or the
    /// biome is unknown.
    pub fn flat(settings: &FlatSettings, seed: i64, structures: bool) -> Option<Vanilla> {
        let noise = tuff_vanilla_data::worldgen::NoiseSettings::get("minecraft:overworld")?;
        crate::flat::Flat::layer_states(settings)?;
        Some(Vanilla {
            noise,
            bound: OnceLock::new(),
            config: None,
            flat: Some(settings.clone()),
            seed,
            structures,
            sky_light: true,
            disabled: OnceLock::new(),
            generator: OnceLock::new(),
        })
    }

    pub fn generator(&self) -> &NoiseGenerator {
        self.generator.get_or_init(|| {
            let disabled = self.disabled.get().copied().unwrap_or_default();
            self.build_generator().with_disabled(disabled)
        })
    }

    fn build_generator(&self) -> NoiseGenerator {
        {
            if let Some(flat) = &self.flat {
                let options = self.bound().1.clone();
                let biome = options
                    .biomes()
                    .id(&flat.biome)
                    .unwrap_or_else(tuff_world_model::chunk::plains);
                let layers = crate::flat::Flat::layer_states(flat).unwrap_or_default();
                let terrain = crate::generator::FlatTerrain {
                    top_layers: crate::flat::Flat::top_layers(&layers),
                    layers,
                    biome,
                    features: flat.features,
                    lakes: flat.lakes,
                    structure_sets: flat.structure_overrides.clone(),
                };
                return NoiseGenerator::with_options(
                    self.settings(),
                    Arc::new(crate::biome_source::Fixed(biome)),
                    self.seed,
                    options,
                )
                .with_flat(terrain);
            }
            let config = self
                .config
                .as_ref()
                .expect("noise generators have a biome source");
            let (settings, options) = self.bound();
            let sources = BiomeSources::builtin();
            let source = sources
                .create_in(config, options.biomes())
                .unwrap_or_else(|| {
                    log::error!(
                        "biome source {} names an unknown biome or parameter list; using plains for it",
                        config.kind
                    );
                    sources
                        .create_in(config, crate::loaded::biomes::PackBiomes::lenient())
                        .expect("biome source checked at construction")
                });
            let generator =
                NoiseGenerator::with_options(settings, source, self.seed, options.clone());
            if self.sky_light {
                generator
            } else {
                generator.without_sky_light()
            }
        }
    }
}

impl Vanilla {
    fn bound(
        &self,
    ) -> &(
        &'static tuff_vanilla_data::worldgen::NoiseSettings,
        crate::loaded::Options,
    ) {
        self.bound
            .get_or_init(|| (self.noise, crate::loaded::Options::default()))
    }

    fn locates_structures(&self) -> bool {
        let disabled = self.generator().disabled();
        self.structures && !disabled.structures && !disabled.features
    }

    fn settings(&self) -> &'static tuff_vanilla_data::worldgen::NoiseSettings {
        self.bound().0
    }
}

impl ChunkGenerator for Vanilla {
    fn variant(&self, seed: Option<i64>, biome: Option<&str>) -> Option<Arc<dyn ChunkGenerator>> {
        let bound = self
            .bound
            .get()
            .cloned()
            .unwrap_or_else(|| (self.noise, crate::loaded::Options::default()));
        let mut config = self.config.clone();
        let mut flat = self.flat.clone();
        if let Some(biome) = biome {
            bound.1.biomes().id(biome)?;
            if let Some(flat) = &mut flat {
                flat.biome = biome.to_owned();
            }
            if let Some(config) = &mut config {
                *config = BiomeSourceConfig {
                    kind: "minecraft:fixed".to_owned(),
                    preset: None,
                    biome: Some(biome.to_owned()),
                    biomes: Vec::new(),
                    scale: config.scale,
                    points: Vec::new(),
                };
            }
        }
        let variant = Vanilla {
            noise: self.noise,
            bound: OnceLock::from(bound),
            config,
            flat,
            seed: seed.unwrap_or(self.seed),
            structures: self.structures,
            sky_light: self.sky_light,
            disabled: self.disabled.clone(),
            generator: OnceLock::new(),
        };
        Some(Arc::new(variant))
    }

    fn bind(&self, data: &tuff_core::world::generator::GeneratorData<'_>) {
        if self.disabled.set(data.disabled).is_err() {
            log::warn!("a generator was bound twice; its first debug stage switches stay");
        }
        if self.flat.is_some() {
            match crate::loaded::flat_options(data.registries) {
                Ok(Some(options)) => {
                    if self.bound.set((self.noise, options)).is_err() {
                        log::warn!("a flat generator was used before data packs loaded");
                    }
                }
                Ok(None) => {}
                Err(message) => log::error!("cannot use data pack worldgen entries: {message}"),
            }
            return;
        }
        match crate::loaded::resolve(data.registries, self.noise.name, data.compile_loaded) {
            None => {}
            Some(Ok(bound)) => {
                if self.bound.set(bound).is_err() {
                    log::warn!(
                        "noise settings {} were used before data packs loaded; the built-in ones stay",
                        self.noise.name
                    );
                }
            }
            Some(Err(err)) => log::error!(
                "cannot use the data pack noise settings {}, keeping the built-in ones: {err}",
                self.noise.name
            ),
        }
    }

    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        self.generator()
            .generate(pos, shape, Stages::ALL)
            .into_chunk()
    }

    fn retrogen_below_zero(&self, pos: ChunkPos, shape: ChunkShape, chunk: &mut Chunk) -> bool {
        if self.flat.is_some() {
            return false;
        }
        let terrain = self
            .generator()
            .generate(pos, shape, Stages::ALL)
            .into_chunk();
        for z in 0..16 {
            for x in 0..16 {
                for y in shape.min_y..0 {
                    chunk.set(x, y, z, terrain.get(x, y, z));
                }
            }
        }
        true
    }

    fn sea_level(&self) -> i32 {
        self.settings().sea_level
    }

    fn spawn(&self) -> BlockPos {
        self.generator().spawn(ChunkShape::OVERWORLD)
    }

    fn pipeline(self: Arc<Self>, cx: PipelineContext) -> Option<Arc<dyn ChunkPipeline>> {
        Some(Arc::new(crate::pipeline::GenerationActor::spawn(self, cx)))
    }

    fn spawns_creatures(&self) -> bool {
        !self.settings().disable_mob_generation
    }

    fn nearest_ring_structure(&self, structures: &[&str], from: BlockPos) -> Option<BlockPos> {
        if !self.structures {
            return None;
        }
        let generator = self.generator();
        generator
            .structures()
            .nearest_ring_structure(generator, structures, from)
    }

    fn locate_structure(
        &self,
        structures: &[&str],
        from: BlockPos,
        shape: ChunkShape,
    ) -> Option<(BlockPos, &'static str)> {
        if !self.locates_structures() {
            return None;
        }
        crate::locate::structure(self.generator(), structures, from, shape)
    }

    fn locate_structure_within(
        &self,
        structures: &[&str],
        from: BlockPos,
        radius: i32,
        shape: ChunkShape,
        skip: Option<tuff_core::world::generator::SkipChunk<'_>>,
    ) -> Option<(BlockPos, &'static str)> {
        if !self.locates_structures() {
            return None;
        }
        crate::locate::structure_within(self.generator(), structures, from, radius, shape, skip)
    }

    fn plan_structure(
        &self,
        structure: &str,
        chunk: ChunkPos,
        shape: ChunkShape,
    ) -> Option<Arc<dyn tuff_core::world::features::PlannedStructure>> {
        let generator = self.generator();
        let structures = generator.structures();
        let index = structures.index_of(structure)?;
        let start = structures.forced_start(generator, index, chunk, shape)?;
        Some(Arc::new(crate::place::StartPlan(start)))
    }

    fn plan_jigsaw(
        &self,
        pool: &str,
        target: &str,
        max_depth: i32,
        pos: BlockPos,
        shape: ChunkShape,
        keep_jigsaws: bool,
    ) -> Option<Arc<dyn tuff_core::world::features::PlannedStructure>> {
        let generator = self.generator();
        let pieces = generator.structures().assemble_jigsaw(
            generator,
            (pool, target, max_depth),
            pos,
            shape,
            keep_jigsaws,
        )?;
        let boxes: Vec<_> = pieces.iter().map(|p| p.bounding_box()).collect();
        let bounds = crate::structure::geometry::encapsulating_boxes(&boxes)?;
        Some(Arc::new(crate::place::PiecesPlan {
            pieces,
            bounds,
            reference: pos,
        }))
    }

    fn biomes_at(&self, positions: &[BlockPos], _shape: ChunkShape) -> Vec<Option<&'static str>> {
        crate::locate::biomes_at(self.generator(), positions)
            .into_iter()
            .map(Some)
            .collect()
    }

    fn locate_biome(
        &self,
        biomes: &[&str],
        from: BlockPos,
        shape: ChunkShape,
    ) -> Option<(BlockPos, &'static str)> {
        crate::locate::biome(self.generator(), biomes, from, shape)
    }
}

impl StagedGenerator for Vanilla {
    fn starts(&self, block: (i32, i32), shape: ChunkShape) -> StartsBlock {
        if !self.structures {
            return StartsBlock {
                block,
                starts: Vec::new(),
            };
        }
        let generator = self.generator();
        StartsBlock {
            block,
            starts: StartsBlock::chunks(block)
                .map(|pos| generator.structure_starts(pos, shape))
                .collect(),
        }
    }

    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, starts: &[Arc<StartsBlock>]) -> ProtoChunk {
        let generator = self.generator();
        let lookup = |p: ChunkPos| -> &[Arc<StructureStart>] {
            starts.iter().find_map(|b| b.get(p)).unwrap_or(&[])
        };
        let references = generator.structures().references(pos, lookup);
        generator.terrain(pos, shape, lookup(pos).to_vec(), references)
    }

    fn resume(&self, mut proto: ProtoChunk, starts: &[Arc<StartsBlock>]) -> ProtoChunk {
        let lookup = |p: ChunkPos| -> &[Arc<StructureStart>] {
            starts.iter().find_map(|b| b.get(p)).unwrap_or(&[])
        };
        proto.structure_references = self.generator().structures().references(proto.pos, lookup);
        proto.structure_starts = lookup(proto.pos).to_vec();
        if let Some(saved) = proto.saved_starts.take() {
            for start in &proto.structure_starts {
                if let Some(tag) = saved.get_compound(start.structure.name) {
                    start.load_progress(tag);
                }
            }
        }
        proto
    }

    fn decorate(&self, center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        let generator = self.generator();
        let mut region = Region::new(generator, center, window);
        generator.decorate(&mut region);
        region.into_chunks()
    }

    fn finish(&self, mut proto: ProtoChunk) -> Finished {
        proto.post_process();
        let block_entities = std::mem::take(&mut proto.block_entities)
            .into_values()
            .collect();
        let structures = structures_nbt(&proto.structure_starts, &proto.structure_references);
        let post_processing = proto
            .post_processing
            .iter()
            .map(|&(x, y, z)| BlockPos::new(x, y, z))
            .collect();
        let fluid_ticks = std::mem::take(&mut proto.fluid_ticks);
        let block_ticks = std::mem::take(&mut proto.block_ticks);
        let entities = std::mem::take(&mut proto.entities);
        Finished {
            chunk: proto.into_chunk(),
            block_entities,
            structures: Some(structures),
            post_processing,
            fluid_ticks,
            block_ticks,
            entities,
        }
    }

    fn shrink(&self) {
        if let Some(generator) = self.generator.get() {
            generator.shrink();
        }
        crate::structure::pool::clear_cache();
        crate::structure::template::clear_cache();
    }
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Block;

    use super::*;
    use tuff_test_support::placeholder::*;

    const OVERWORLD: ChunkShape = ChunkShape::OVERWORLD;
    const NETHER: ChunkShape = ChunkShape::new(0, 256);

    #[tuff_core::watchdog]
    #[test]
    fn a_new_worlds_spawn_is_where_vanilla_puts_it() {
        assert_eq!(
            overworld_spawn(1),
            BlockPos::new(160, 70, 160),
            "the seed 1 fixture world's level.dat"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn deterministic_and_seeded() {
        let a = Overworld::new(1);
        let b = Overworld::new(1);
        let c = Overworld::new(2);
        let pos = ChunkPos::new(3, -7);
        assert_eq!(a.generate(pos, OVERWORLD), b.generate(pos, OVERWORLD));
        assert!((0..64).any(|i| a.height(i * 7, i * 3) != c.height(i * 7, i * 3)));
        assert_eq!(
            Nether::new(1).generate(pos, NETHER),
            Nether::new(1).generate(pos, NETHER)
        );
        assert_eq!(
            End::new(1).generate(pos, NETHER),
            End::new(1).generate(pos, NETHER)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn columns_are_layered() {
        let generator = Overworld::new(1);
        let pos = ChunkPos::new(0, 0);
        let chunk = generator.generate(pos, OVERWORLD);
        for (x, z) in [(0, 0), (5, 9), (15, 15)] {
            let height = generator.height(x, z);
            assert_eq!(chunk.get(x, OVERWORLD.min_y, z).block(), Block::Bedrock);
            assert_eq!(
                chunk.get(x, height - Overworld::SOIL_DEPTH - 1, z).block(),
                Block::Stone
            );
            let top = chunk.get(x, height, z).block();
            assert!(matches!(top, Block::GrassBlock | Block::Sand));
            let above = chunk.get(x, height + 1, z).block();
            assert!(matches!(above, Block::Air | Block::Water));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn terrain_varies_and_floods_below_sea_level() {
        let generator = Overworld::new(1);
        let heights: Vec<i32> = (0..400).map(|i| generator.height(i * 16, i * 11)).collect();
        let (min, max) = (heights.iter().min().unwrap(), heights.iter().max().unwrap());
        assert!(max - min > 20, "{min}..{max}");
        let (x, z) = (0..4000)
            .map(|i| (i * 13, i * 5))
            .find(|&(x, z)| generator.height(x, z) < Overworld::SEA_LEVEL - 2)
            .expect("some water");
        let chunk = generator.generate(ChunkPos::new(x >> 4, z >> 4), OVERWORLD);
        assert_eq!(
            chunk.get(x, Overworld::SEA_LEVEL - 1, z).block(),
            Block::Water
        );
        let spawn = generator.spawn();
        assert!(spawn.y >= Overworld::SEA_LEVEL);
    }

    #[tuff_core::watchdog]
    #[test]
    fn overworld_follows_the_requested_shape() {
        let generator = Overworld::new(1);
        let shape = ChunkShape::new(-128, 512);
        let chunk = generator.generate(ChunkPos::new(1, 1), shape);
        assert_eq!(chunk.shape(), shape);
        assert_eq!(chunk.get(0, -128, 0).block(), Block::Bedrock);
        assert_eq!(chunk.get(0, -127, 0).block(), Block::Stone);
        assert_eq!(chunk.get(0, 383, 0).block(), Block::Air);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nether_has_bedrock_bounds_lava_seas_and_caverns() {
        let generator = Nether::new(1);
        let mut lava = 0;
        let mut air = 0;
        for cx in -2..2 {
            for cz in -2..2 {
                let pos = ChunkPos::new(cx, cz);
                let chunk = generator.generate(pos, NETHER);
                assert_eq!(chunk.shape(), NETHER);
                for x in 0..16 {
                    for z in 0..16 {
                        assert_eq!(chunk.get(x, 0, z).block(), Block::Bedrock);
                        assert_eq!(chunk.get(x, Nether::ROOF, z).block(), Block::Bedrock);
                        assert_eq!(chunk.get(x, 200, z).block(), Block::Air);
                        for y in 1..Nether::ROOF {
                            match chunk.get(x, y, z).block() {
                                Block::Lava => {
                                    assert!(y < Nether::SEA_LEVEL);
                                    lava += 1;
                                }
                                Block::Air => {
                                    assert!(y >= Nether::SEA_LEVEL);
                                    air += 1;
                                }
                                Block::Netherrack => {}
                                other => panic!("unexpected {other:?} at {y}"),
                            }
                        }
                    }
                }
            }
        }
        assert!(air > 0);
        let (x, z) = (0..4000)
            .map(|i| (i * 13, i * 5))
            .find(|&(x, z)| generator.floor_height(x, z) < Nether::SEA_LEVEL - 2)
            .expect("a lava sea somewhere");
        let sea = generator.generate(ChunkPos::new(x >> 4, z >> 4), NETHER);
        assert_eq!(sea.get(x, Nether::SEA_LEVEL - 1, z).block(), Block::Lava);
        assert_eq!(sea.get(x, Nether::SEA_LEVEL, z).block(), Block::Air);
        assert!(lava > 0 || generator.floor_height(0, 0) >= Nether::SEA_LEVEL);
        let spawn = generator.spawn();
        assert!(spawn.y > 0 && spawn.y < Nether::ROOF);
    }

    #[tuff_core::watchdog]
    #[test]
    fn end_has_an_island_and_the_spawn_platform() {
        let generator = End::new(1);
        let center = generator.generate(ChunkPos::new(0, 0), NETHER);
        let (top, bottom) = generator.island(0, 0).unwrap();
        assert_eq!(center.get(0, top, 0).block(), Block::EndStone);
        assert_eq!(center.get(0, bottom, 0).block(), Block::EndStone);
        assert_eq!(center.get(0, top + 1, 0).block(), Block::Air);
        assert_eq!(center.get(0, bottom - 1, 0).block(), Block::Air);
        assert!(generator.island(400, 400).is_none());
        let far = generator.generate(ChunkPos::new(25, 25), NETHER);
        assert!(far.sections().all(|s| s.has_only_air()));
        for dx in -2..=2 {
            for dz in -2..=2 {
                let (x, z) = (End::PLATFORM.x + dx, End::PLATFORM.z + dz);
                let platform =
                    generator.generate(ChunkPos::of_block(BlockPos::new(x, 0, z)), NETHER);
                assert_eq!(platform.get(x, End::PLATFORM.y, z).block(), Block::Obsidian);
                assert_eq!(platform.get(x, End::PLATFORM.y + 1, z).block(), Block::Air);
            }
        }
        let edge = generator.generate(ChunkPos::of_block(BlockPos::new(97, 0, 0)), NETHER);
        assert_eq!(edge.get(97, End::PLATFORM.y, 0).block(), Block::Air);
        assert_eq!(generator.spawn(), BlockPos::new(100, 49, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn without_generate_structures_nothing_starts_or_is_located() {
        let mut settings = DimensionSettings::vanilla()
            .into_iter()
            .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
            .unwrap();
        let hut = ChunkPos::new(-255, -86);
        let block = StartsBlock::of(hut);
        let from = BlockPos::new(hut.x * 16, 64, hut.z * 16);
        let hut_id = ["minecraft:swamp_hut"];
        let with = vanilla(&settings, 1).unwrap();
        assert!(
            with.starts(block, OVERWORLD)
                .get(hut)
                .is_some_and(|s| !s.is_empty())
        );
        assert!(with.locate_structure(&hut_id, from, OVERWORLD).is_some());
        settings.generate_structures = false;
        let without = vanilla(&settings, 1).unwrap();
        assert!(
            without
                .starts(block, OVERWORLD)
                .get(hut)
                .is_none_or(<[_]>::is_empty)
        );
        assert!(without.locate_structure(&hut_id, from, OVERWORLD).is_none());
        assert!(
            without
                .nearest_ring_structure(&["minecraft:stronghold"], from)
                .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_generators_fall_back_to_void() {
        let mut modded = DimensionSettings::vanilla().remove(0);
        modded.generator = "modded:islands".into();
        assert!(VanillaGenerators.generator(&modded, 1).is_none());
        let void = super::for_settings(&modded, 1);
        let chunk = void.generate(ChunkPos::new(0, 0), OVERWORLD);
        assert!(chunk.sections().all(|s| s.has_only_air()));
        assert!(
            DimensionSettings::vanilla()
                .iter()
                .all(|settings| VanillaGenerators.generator(settings, 1).is_some())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_sea_levels() {
        let settings = DimensionSettings::vanilla();
        let level = |key: &str| {
            let dimension = settings.iter().find(|d| d.key == key).unwrap();
            super::for_settings(dimension, 1).sea_level()
        };
        assert_eq!(level(tuff_core::storage::level::OVERWORLD), 63);
        assert_eq!(level(tuff_core::storage::level::THE_NETHER), 32);
    }

    #[tuff_core::watchdog]
    #[test]
    fn variants_take_another_seed_or_one_biome() {
        let settings = DimensionSettings::vanilla();
        let overworld = settings
            .iter()
            .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
            .unwrap();
        let generator = super::for_settings(overworld, 1);
        let pos = ChunkPos::new(0, 0);
        let surface = |chunk: &Chunk| -> Vec<_> {
            (0..16)
                .flat_map(|x| (-64..320).map(move |y| chunk.get(x, y, 0)))
                .collect()
        };
        let base = generator.generate(pos, OVERWORLD);
        let same = generator
            .variant(Some(1), None)
            .unwrap()
            .generate(pos, OVERWORLD);
        assert_eq!(surface(&base), surface(&same));
        let other = generator
            .variant(Some(2), None)
            .unwrap()
            .generate(pos, OVERWORLD);
        assert_ne!(surface(&base), surface(&other));
        let desert = tuff_world_model::chunk::biome_id("minecraft:desert").unwrap();
        let forced = generator
            .variant(None, Some("minecraft:desert"))
            .unwrap()
            .generate(pos, OVERWORLD);
        assert_eq!(forced.biome(0, 64, 0), desert);
        assert_eq!(forced.biome(15, 100, 15), desert);
        assert!(generator.variant(None, Some("minecraft:nowhere")).is_none());
    }

    /// Serial terrain, decoration and finishing of a fixed window, best of
    /// three, so changes to block storage can be compared on a busy machine.
    /// Run with `cargo test --release -p vanilla-worldgen --lib -- --ignored
    /// --nocapture staged_generation_serial`.
    #[tuff_core::watchdog]
    #[test]
    #[ignore = "benchmark"]
    fn staged_generation_serial() {
        use std::collections::HashMap;
        use std::time::{Duration, Instant};
        let settings = DimensionSettings::vanilla()
            .into_iter()
            .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
            .unwrap();
        let side = 8;
        let mut best = [Duration::MAX; 3];
        for _ in 0..3 {
            let generator = vanilla(&settings, 1).unwrap();
            let origin = ChunkPos::new(-4, -4);
            generator.terrain(ChunkPos::new(-5000, -5000), OVERWORLD, &[]);
            let positions: Vec<ChunkPos> = (0..side)
                .flat_map(|z| (0..side).map(move |x| ChunkPos::new(origin.x + x, origin.z + z)))
                .collect();
            let mut blocks = HashMap::new();
            for pos in &positions {
                for block in StartsBlock::around(*pos) {
                    blocks
                        .entry(block)
                        .or_insert_with(|| Arc::new(generator.starts(block, OVERWORLD)));
                }
            }
            let starts: Vec<Arc<StartsBlock>> = blocks.into_values().collect();
            let started = Instant::now();
            let mut chunks: HashMap<ChunkPos, ProtoChunk> = positions
                .iter()
                .map(|pos| (*pos, generator.terrain(*pos, OVERWORLD, &starts)))
                .collect();
            let terrain = started.elapsed();
            let started = Instant::now();
            for z in 1..side - 1 {
                for x in 1..side - 1 {
                    let center = ChunkPos::new(origin.x + x, origin.z + z);
                    let window: Vec<ChunkPos> = (-1..=1)
                        .flat_map(|dz| {
                            (-1..=1).map(move |dx| ChunkPos::new(center.x + dx, center.z + dz))
                        })
                        .collect();
                    let protos = window.iter().map(|p| chunks.remove(p).unwrap()).collect();
                    for (pos, proto) in window.iter().zip(generator.decorate(center, protos)) {
                        chunks.insert(*pos, proto);
                    }
                }
            }
            let decoration = started.elapsed();
            let started = Instant::now();
            for z in 2..side - 2 {
                for x in 2..side - 2 {
                    let pos = ChunkPos::new(origin.x + x, origin.z + z);
                    std::hint::black_box(generator.finish(chunks.remove(&pos).unwrap()));
                }
            }
            let finishing = started.elapsed();
            for (best, now) in best.iter_mut().zip([terrain, decoration, finishing]) {
                *best = (*best).min(now);
            }
        }
        eprintln!(
            "terrain {:?} for {} chunks, decoration {:?} for {} windows, finishing {:?} for {} chunks",
            best[0],
            side * side,
            best[1],
            (side - 2) * (side - 2),
            best[2],
            (side - 4) * (side - 4)
        );
    }

    /// Anti-xray obfuscation and chunk packet encoding of generated terrain
    /// in each mode, best of fifteen. Run with `cargo test --release -p
    /// vanilla-worldgen --lib -- --ignored --nocapture anti_xray_encode_serial`.
    #[tuff_core::watchdog]
    #[test]
    #[ignore = "benchmark"]
    fn anti_xray_encode_serial() {
        use std::time::{Duration, Instant};
        use tuff_core::protocol::play::LevelChunkWithLight;
        use tuff_core::world::anti_xray::{self, AROUND, Cache, DIMENSIONS, Mode, Plan};
        let settings = DimensionSettings::vanilla()
            .into_iter()
            .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
            .unwrap();
        let generator = vanilla(&settings, 1).unwrap();
        let side = 10;
        let square = |from: i32, to: i32| -> Vec<ChunkPos> {
            (from..to)
                .flat_map(|z| (from..to).map(move |x| ChunkPos::new(x, z)))
                .collect()
        };
        let mut protos: std::collections::HashMap<ChunkPos, ProtoChunk> = square(0, side)
            .into_iter()
            .map(|pos| (pos, generator.terrain(pos, OVERWORLD, &[])))
            .collect();
        for center in square(1, side - 1) {
            let window: Vec<ChunkPos> = (-1..=1)
                .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(center.x + dx, center.z + dz)))
                .collect();
            let taken = window.iter().map(|p| protos.remove(p).unwrap()).collect();
            for (pos, proto) in window.iter().zip(generator.decorate(center, taken)) {
                protos.insert(*pos, proto);
            }
        }
        let chunks: std::collections::HashMap<ChunkPos, Chunk> = square(2, side - 2)
            .into_iter()
            .map(|pos| (pos, generator.finish(protos.remove(&pos).unwrap()).chunk))
            .collect();
        let inner = square(3, side - 3);
        let (_, hidden, replacement) = DIMENSIONS[0];
        let plan = |mode| {
            Arc::new(
                Plan::new(
                    mode,
                    64,
                    2,
                    &anti_xray::parse_blocks(hidden).unwrap(),
                    &anti_xray::parse_blocks(replacement).unwrap(),
                    7,
                )
                .unwrap(),
            )
        };
        for (label, plan) in [
            ("off", None),
            ("hide", Some(plan(Mode::Hide))),
            ("fill", Some(plan(Mode::Fill))),
        ] {
            let mut best = [Duration::MAX; 2];
            let mut bytes = 0;
            for _ in 0..15 {
                let mut spent = [Duration::ZERO; 2];
                bytes = 0;
                for pos in &inner {
                    let chunk = &chunks[pos];
                    let started = Instant::now();
                    let sent = plan.as_ref().map(|plan| {
                        let around = AROUND.map(|(dx, dz)| {
                            chunks.get(&ChunkPos::new(pos.x + dx, pos.z + dz)).cloned()
                        });
                        anti_xray::obfuscate(chunk, *pos, &around, plan, &mut Cache::default()).0
                    });
                    spent[0] += started.elapsed();
                    let started = Instant::now();
                    let packet = tuff_core::world::framed(&LevelChunkWithLight(
                        sent.as_ref().unwrap_or(chunk).encode(pos.x, pos.z, &[]),
                    ));
                    spent[1] += started.elapsed();
                    bytes += packet.len();
                }
                for (best, now) in best.iter_mut().zip(spent) {
                    *best = (*best).min(now);
                }
            }
            let n = inner.len() as u32;
            eprintln!(
                "{label}: obfuscate {:?}/chunk, encode {:?}/chunk, {} bytes/chunk",
                best[0] / n,
                best[1] / n,
                bytes / inner.len()
            );
        }
    }
}

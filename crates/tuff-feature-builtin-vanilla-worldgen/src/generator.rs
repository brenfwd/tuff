//! The noise-based chunk generator: biomes, then terrain density with
//! aquifers, then material rules (surface), then carvers.

use std::sync::Arc;

use crate::beardifier::Beardifier;
use crate::structure::{References, StructureStart, Structures};

use tuff_vanilla_data::worldgen::NoiseSettings;

use crate::aquifer::{Aquifer, AquiferSamplers, FluidPicker, NoiseAquifer};
use crate::biome_source::BiomeSource;
use crate::biome_zoom::{SampledBiomes, StoredBiomes, obfuscate_seed};
use crate::cache::ChunkCache;
use crate::carver::{CarvingMask, carvable, is_grassy};
use crate::density::{Context, DensityVolume};
use crate::proto::{HeightmapKind, ProtoChunk};
use crate::state::RandomState;
use crate::surface::{GenContext, MaterialSystem};
use tuff_core::world::generator::DisabledStages;
use tuff_item::random::WorldgenRandom;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

/// Which stages run; the stage-by-stage tests stop early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stages {
    pub surface: bool,
    pub carvers: bool,
}

impl Stages {
    pub const ALL: Stages = Stages {
        surface: true,
        carvers: true,
    };

    fn without(self, disabled: DisabledStages) -> Stages {
        Stages {
            surface: self.surface && !disabled.surface,
            carvers: self.carvers && !disabled.carvers,
        }
    }
}

/// What turns a generator into a flat world: its layers (bottom first, one
/// state per block from the world floor) and which extras are placed.
pub struct FlatTerrain {
    pub layers: Vec<BlockState>,
    pub biome: u16,
    pub features: bool,
    pub lakes: bool,
    /// Layers placed by the top layer modification step instead of the
    /// terrain: block index from the world floor and state.
    pub top_layers: Vec<(i32, BlockState)>,
    pub structure_sets: Option<Vec<String>>,
}

pub struct NoiseGenerator {
    flat: Option<FlatTerrain>,
    pub settings: &'static NoiseSettings,
    pub state: RandomState,
    pub aquifers: Option<AquiferSamplers>,
    pub picker: FluidPicker,
    pub biome_source: Arc<dyn BiomeSource>,
    pub surface: MaterialSystem,
    pub zoom_seed: i64,
    pub span: GenContext,
    /// Whether the dimension has sky light: features then see full
    /// brightness, as no chunk being decorated is lit yet.
    pub sky_light: bool,
    possible_biomes: std::sync::OnceLock<Vec<u16>>,
    decorator: std::sync::OnceLock<crate::feature::Decorator>,
    features: Option<Arc<crate::loaded::features::FeatureSources>>,
    structures: std::sync::OnceLock<Structures>,
    /// Chunk biome columns as `fill_chunk` gives them: a chunk's surface
    /// rules read its neighbours' biomes, and each neighbour is generated
    /// once but looked at up to eight times.
    biome_columns: ChunkCache<Arc<[u16]>>,
    /// The biome a chunk's carvers start in, read by every chunk within
    /// eight of it.
    carver_biomes: ChunkCache<u16>,
    disabled: DisabledStages,
}

impl NoiseGenerator {
    pub fn new(
        settings: &'static NoiseSettings,
        biome_source: Arc<dyn BiomeSource>,
        seed: i64,
    ) -> Self {
        Self::with_options(
            settings,
            biome_source,
            seed,
            crate::loaded::Options::default(),
        )
    }

    /// A generator for settings decoded from data packs.
    pub fn with_options(
        settings: &'static NoiseSettings,
        biome_source: Arc<dyn BiomeSource>,
        seed: i64,
        options: crate::loaded::Options,
    ) -> Self {
        let features = options.features.clone();
        let mut state = RandomState::with_options(settings, seed, options);
        let aquifers = settings.aquifers.as_ref().map(|a| AquiferSamplers {
            barrier: state.compile(a.barrier),
            floodedness: state.compile(a.fluid_level_floodedness),
            spread: state.compile(a.fluid_level_spread),
            lava: state.compile(a.lava),
            exclusion: state.compile(a.exclusion),
            surface_level: state.compile(a.surface_level),
            random: state.random_factory("minecraft:aquifer"),
        });
        let surface = MaterialSystem::new(
            &mut state,
            settings.material_rule,
            settings.default_block,
            settings.sea_level,
        );
        Self {
            flat: None,
            surface,
            zoom_seed: obfuscate_seed(seed),
            sky_light: true,
            possible_biomes: std::sync::OnceLock::new(),
            decorator: std::sync::OnceLock::new(),
            features,
            structures: std::sync::OnceLock::new(),
            biome_columns: ChunkCache::new(4096),
            carver_biomes: ChunkCache::new(1 << 16),
            disabled: DisabledStages::default(),
            span: GenContext {
                min_y: settings.min_y,
                height: settings.height,
                sea_level: settings.sea_level,
            },
            settings,
            picker: FluidPicker {
                sea_level: settings.sea_level,
                default_fluid: settings.default_fluid,
                fluids: true,
            },
            state,
            aquifers,
            biome_source,
        }
    }

    /// The generator of a flat world: the layers replace the noise.
    /// Switches the debug-disabled stages off for every chunk.
    pub fn with_disabled(mut self, disabled: DisabledStages) -> Self {
        self.disabled = disabled;
        self.picker.fluids = !disabled.fluid_generation;
        self.surface.ore_veins = !disabled.ore_veins;
        self
    }

    pub fn disabled(&self) -> DisabledStages {
        self.disabled
    }

    pub fn with_flat(mut self, flat: FlatTerrain) -> Self {
        self.flat = Some(flat);
        self
    }

    fn flat_column(
        &self,
        flat: &FlatTerrain,
        shape: ChunkShape,
        stop: Option<HeightmapKind>,
    ) -> (i32, Vec<BlockState>) {
        let air = tuff_vanilla_data::Block::Air.default_state();
        let mut column = flat.layers.clone();
        for &(i, _) in &flat.top_layers {
            column[i as usize] = air;
        }
        column.resize(shape.height as usize, air);
        let lowest = stop
            .and_then(|kind| column.iter().rposition(|s| kind.is_opaque(*s)))
            .unwrap_or(0);
        column.drain(..lowest);
        (shape.min_y + lowest as i32, column)
    }

    fn generate_flat(&self, flat: &FlatTerrain, pos: ChunkPos, shape: ChunkShape) -> ProtoChunk {
        let mut chunk = ProtoChunk::new(pos, shape);
        self.fill_biomes(&mut chunk);
        let air = tuff_vanilla_data::Block::Air.default_state();
        for (i, state) in flat.layers.iter().enumerate().take(shape.height as usize) {
            if *state == air || flat.top_layers.iter().any(|&(l, _)| l == i as i32) {
                continue;
            }
            let y = shape.min_y + i as i32;
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_raw(x, y, z, *state);
                }
            }
        }
        chunk.finish_terrain();
        chunk
    }

    /// Empties the memo tables; they fill again as chunks are generated.
    pub fn shrink(&self) {
        self.biome_columns.clear();
        self.carver_biomes.clear();
    }

    /// The generator for a dimension without sky light.
    pub fn without_sky_light(mut self) -> Self {
        self.sky_light = false;
        self
    }

    pub fn fill_biomes(&self, chunk: &mut ProtoChunk) {
        let biomes = self.chunk_biomes(chunk.pos, chunk.shape);
        chunk.set_biomes(biomes.to_vec());
    }

    /// The biomes of a chunk column the way a new chunk is filled, indexed
    /// `qy * 16 + qz * 4 + qx` from the shape's lowest quart. Memoized per
    /// position for the shape the generator is used with.
    pub fn chunk_biomes(&self, pos: ChunkPos, shape: ChunkShape) -> Arc<[u16]> {
        let fill = || -> Arc<[u16]> {
            let mut ctx = Context::builder().caches().build();
            self.biome_source
                .fill_chunk(
                    &self.state.router,
                    &mut ctx,
                    [pos.x, pos.z],
                    shape.min_y >> 2,
                    shape.height >> 2,
                )
                .into()
        };
        let biomes = self.biome_columns.get_or_insert_with(pos, fill);
        if biomes.len() == 16 * (shape.height / 4) as usize {
            biomes
        } else {
            fill()
        }
    }

    /// The world's pack-added biomes and parameter lists.
    pub fn biomes(&self) -> &crate::loaded::biomes::PackBiomes {
        self.state.options().biomes()
    }

    /// The biome data generation reads for `id`, pack-added ones included.
    pub fn biome_data(&self, id: u16) -> &'static tuff_vanilla_data::biome::Biome {
        self.biomes().data(id)
    }

    fn carvers_of(&self, biome: u16) -> &[(usize, Arc<dyn crate::carver::CarverKind>)] {
        match self.biomes().added(biome) {
            Some(added) => &added.carvers,
            None => crate::carver::carvers_of(biome),
        }
    }

    fn possible_biomes(&self) -> &[u16] {
        self.possible_biomes
            .get_or_init(|| self.biome_source.possible_biomes())
    }

    /// The features of every biome this generator can produce, in the
    /// game's order; built on first use.
    pub fn decorator(&self) -> &crate::feature::Decorator {
        self.decorator.get_or_init(|| match &self.flat {
            Some(flat) => {
                let void_gen = flat
                    .layers
                    .iter()
                    .all(|s| tuff_world_model::chunk::is_air(*s));
                let void_biome = tuff_world_model::chunk::biome_id("minecraft:the_void")
                    .is_some_and(|id| id == flat.biome);
                let features = flat.features && (!void_gen || void_biome);
                crate::feature::Decorator::flat(
                    flat.biome,
                    self.biome_data(flat.biome),
                    features,
                    flat.lakes,
                    &flat.top_layers,
                    self.features.clone(),
                )
            }
            None => crate::feature::Decorator::new(self.possible_biomes(), self.features.clone()),
        })
    }

    /// The structures this generator places, built on first use.
    pub fn structures(&self) -> &Structures {
        self.structures.get_or_init(|| {
            let options = self.state.options();
            let structures = Structures::with_sources(
                self.possible_biomes(),
                self.state.seed,
                options.structures.clone(),
                options.biomes.clone(),
            );
            match self.flat.as_ref().and_then(|f| f.structure_sets.as_deref()) {
                Some(sets) => structures.restricted_to(sets),
                None => structures,
            }
        })
    }

    /// The structures that start in `pos`.
    pub fn structure_starts(&self, pos: ChunkPos, shape: ChunkShape) -> Vec<Arc<StructureStart>> {
        if self.disabled.structures {
            return Vec::new();
        }
        self.structures().starts(self, pos, shape)
    }

    /// Terrain of a chunk that `references` reach: the structures adapt the
    /// noise around them, and the chunk keeps its own `starts` and the
    /// references for decoration and saving.
    pub fn terrain(
        &self,
        pos: ChunkPos,
        shape: ChunkShape,
        starts: Vec<Arc<StructureStart>>,
        references: Vec<References>,
    ) -> ProtoChunk {
        let beardifier = Beardifier::for_chunk(&references, pos);
        let mut chunk = self.generate_bound(pos, shape, Stages::ALL, &beardifier);
        chunk.structure_starts = starts;
        chunk.structure_references = references;
        chunk
    }

    /// Decorates the centre chunk of a window with its structures and the
    /// features of the biomes around it.
    pub fn decorate(&self, region: &mut crate::feature::level::Region) {
        if self.disabled.features {
            return;
        }
        let present = crate::feature::biomes_in(region, self.possible_biomes());
        let center = region.center;
        let references = region
            .chunk(center.x, center.z)
            .map(|c| c.structure_references.clone())
            .unwrap_or_default();
        self.decorator().decorate(
            region,
            self.state.seed,
            &present,
            Some((self.structures(), &references)),
        );
    }

    /// A new world's spawn: the best chunk by the spawn targets, then the
    /// first column a player can stand in of the chunks spiralling out
    /// from it (at height 64 in its centre if none has one).
    pub fn spawn(&self, shape: ChunkShape) -> tuff_world_model::BlockPos {
        use tuff_world_model::BlockPos;
        let mut state = RandomState::new(self.settings, self.state.seed);
        let targets = crate::spawn::targets(&mut state, self.settings);
        let (x, z) = if targets.is_empty() {
            (0, 0)
        } else {
            crate::spawn::find(&targets)
        };
        let origin = ChunkPos::new(x >> 4, z >> 4);
        for (dx, dz) in crate::spawn_search::spiral() {
            let pos = ChunkPos::new(origin.x + dx, origin.z + dz);
            let chunk = self.generate(pos, shape, Stages::ALL);
            let (min_x, min_z) = (pos.x * 16, pos.z * 16);
            let heights = |kind, x: i32, z: i32| chunk.height_of(kind, x - min_x, z - min_z) + 1;
            let block = |at: BlockPos| {
                (ChunkPos::of_block(at) == pos).then(|| chunk.get(at.x - min_x, at.y, at.z - min_z))
            };
            if let Some(found) =
                crate::spawn_search::spawn_in_chunk(min_x, min_z, shape.min_y, &heights, &block)
            {
                return found;
            }
        }
        BlockPos::new(origin.x * 16 + 8, 64, origin.z * 16 + 8)
    }

    /// The part of the chunk the noise covers.
    fn noise_volume(&self, pos: ChunkPos, shape: ChunkShape) -> Option<DensityVolume> {
        let min_y = self.settings.min_y.max(shape.min_y);
        let max_y = (self.settings.min_y + self.settings.height).min(shape.min_y + shape.height);
        let height = max_y - min_y;
        (height > 0)
            .then(|| DensityVolume::blocks([16, height, 16], [pos.x << 4, min_y, pos.z << 4]))
    }

    /// The block states of one column from the noise alone (no surface or
    /// carvers), bottom up, as structures probe the terrain before any
    /// chunk exists. Stops early at the first block `stop` counts, from
    /// the top. The first value is the column's lowest y.
    fn noise_column(
        &self,
        x: i32,
        z: i32,
        shape: ChunkShape,
        stop: Option<HeightmapKind>,
    ) -> (i32, Vec<BlockState>) {
        if let Some(flat) = &self.flat {
            return self.flat_column(flat, shape, stop);
        }
        let min_y = self.settings.min_y.max(shape.min_y);
        let max_y = (self.settings.min_y + self.settings.height).min(shape.min_y + shape.height);
        let height = max_y - min_y;
        if height <= 0 {
            return (shape.min_y, Vec::new());
        }
        let volume = DensityVolume::blocks([1, height, 1], [x, min_y, z]);
        let mut ctx = Context::builder().caches().build();
        let mut aquifer = match &self.aquifers {
            Some(samplers) => Aquifer::Noise(Box::new(NoiseAquifer::new(
                &mut ctx,
                samplers,
                &volume,
                self.picker,
            ))),
            None => Aquifer::Disabled(self.picker),
        };
        let mut density = ctx.acquire(volume.size());
        self.state
            .router
            .final_density
            .sample_volume(&mut ctx, &mut density, &volume);
        let mut column = vec![tuff_vanilla_data::Block::Air.default_state(); height as usize];
        let mut lowest = 0;
        for y in (0..volume.size_y).rev() {
            let by = volume.block_y(y);
            let d = density[volume.index(0, y, 0)];
            let state = aquifer
                .compute_substance(&mut ctx, x, by, z, d as f64)
                .unwrap_or(self.settings.default_block);
            column[y as usize] = state;
            lowest = y;
            if stop.is_some_and(|kind| kind.is_opaque(state)) {
                break;
            }
        }
        ctx.release(density);
        column.drain(..lowest as usize);
        (min_y + lowest, column)
    }

    /// The whole noise column at `x`, `z`, bottom up from the returned y.
    pub fn base_column(&self, x: i32, z: i32, shape: ChunkShape) -> (i32, Vec<BlockState>) {
        self.noise_column(x, z, shape, None)
    }

    /// One above the highest block of the noise column that `kind` counts,
    /// or the bottom of the world if none does.
    pub fn base_height(&self, x: i32, z: i32, kind: HeightmapKind, shape: ChunkShape) -> i32 {
        let (bottom, column) = self.noise_column(x, z, shape, Some(kind));
        match column.first() {
            Some(state) if kind.is_opaque(*state) => bottom + 1,
            _ => shape.min_y,
        }
    }

    pub fn generate(&self, pos: ChunkPos, shape: ChunkShape, stages: Stages) -> ProtoChunk {
        self.generate_bound(pos, shape, stages, &Beardifier::default())
    }

    fn generate_bound(
        &self,
        pos: ChunkPos,
        shape: ChunkShape,
        stages: Stages,
        beardifier: &Beardifier,
    ) -> ProtoChunk {
        if let Some(flat) = &self.flat {
            return self.generate_flat(flat, pos, shape);
        }
        let stages = stages.without(self.disabled);
        let mut chunk = ProtoChunk::new(pos, shape);
        self.fill_biomes(&mut chunk);
        let Some(volume) = self.noise_volume(pos, shape) else {
            return chunk;
        };
        let mut ctx = if beardifier.is_empty() {
            Context::builder().caches().build()
        } else {
            Context::builder().caches().beardifier(beardifier).build()
        };
        let mut aquifer = match &self.aquifers {
            Some(samplers) => Aquifer::Noise(Box::new(NoiseAquifer::new(
                &mut ctx,
                samplers,
                &volume,
                self.picker,
            ))),
            None => Aquifer::Disabled(self.picker),
        };
        self.fill_noise(&mut ctx, &mut aquifer, &volume, &mut chunk);
        let mut stored = StoredBiomes::new(self, shape, pos, self.chunk_biomes(pos, shape));
        let span = GenContext {
            min_y: self.span.min_y.max(shape.min_y),
            height: self.span.height.min(shape.height),
            sea_level: self.span.sea_level,
        };
        self.surface.build_surface(
            &mut ctx,
            span,
            &mut chunk,
            &mut stored,
            &volume,
            stages.surface,
        );
        if stages.carvers {
            self.carve(&mut ctx, span, &mut aquifer, &mut chunk);
        }
        chunk.finish_terrain();
        chunk
    }

    /// The carver biomes of the 17 by 17 chunks around `pos`, x-major.
    fn carver_biomes_around(&self, pos: ChunkPos, last: &mut Option<usize>) -> [u16; 289] {
        let mut positions = [pos; 289];
        for (n, at) in positions.iter_mut().enumerate() {
            *at = ChunkPos::new(pos.x + n as i32 / 17 - 8, pos.z + n as i32 % 17 - 8);
        }
        let mut found = [None; 289];
        self.carver_biomes.get_many(&positions, &mut found);
        let mut misses = Vec::new();
        let mut out = [0u16; 289];
        for (n, slot) in found.iter().enumerate() {
            out[n] = match slot {
                Some(biome) => *biome,
                None => {
                    let source = positions[n];
                    let mut ctx = Context::uncached();
                    let biome = self.biome_source.biome_at(
                        &self.state.router,
                        &mut ctx,
                        last,
                        [source.x << 2, 0, source.z << 2],
                    );
                    misses.push((source, biome));
                    biome
                }
            };
        }
        self.carver_biomes.insert_many(misses);
        out
    }

    fn carve(
        &self,
        ctx: &mut Context,
        span: GenContext,
        aquifer: &mut Aquifer,
        chunk: &mut ProtoChunk,
    ) {
        let pos = chunk.pos;
        let max_y = span.min_y + span.height - 1 - 7;
        let mut mask = CarvingMask::new(span.min_y + 1, max_y);
        let mut random = WorldgenRandom::legacy(0);
        let mut last = None;
        let sources = self.carver_biomes_around(pos, &mut last);
        for (n, biome) in sources.iter().enumerate() {
            let source = ChunkPos::new(pos.x + n as i32 / 17 - 8, pos.z + n as i32 % 17 - 8);
            for (index, carver) in self.carvers_of(*biome) {
                random.set_large_feature_seed(
                    self.state.seed.wrapping_add(*index as i64),
                    source.x,
                    source.z,
                );
                if carver.is_start_chunk(&mut random) {
                    carver.carve(span, &mut random, pos, source, &mut mask);
                }
            }
        }
        if mask.is_empty() {
            return;
        }
        let mut point_rules = None;
        let mut biomes = SampledBiomes {
            generator: self,
            last: None,
            cell: crate::biome_zoom::ZoomCell::empty(),
        };
        for (x, z, bottom, top) in mask.runs() {
            let mut has_grass = false;
            let (wx, wz) = (chunk.min_x() + x, chunk.min_z() + z);
            let mut y = top;
            while y >= bottom {
                let state = chunk.get(x, y, z);
                if carvable(state) {
                    if is_grassy(state) {
                        has_grass = true;
                    }
                    if let Some(new) = aquifer.compute_substance(ctx, wx, y, wz, 0.0) {
                        chunk.set(x, y, z, new);
                        let fluid = matches!(new.block(), Block::Water | Block::Lava);
                        if aquifer.should_schedule_fluid_update() && fluid {
                            chunk.post_processing.push((wx, y, wz));
                        }
                        if has_grass
                            && chunk.get(x, y - 1, z).block() == Block::Dirt
                            && let Some(top) = point_rules
                                .get_or_insert_with(|| self.surface.point_rules(span))
                                .top_material(ctx, &mut biomes, chunk, wx, y - 1, wz, fluid)
                        {
                            chunk.set(x, y - 1, z, top);
                            if matches!(top.block(), Block::Water | Block::Lava) {
                                chunk.post_processing.push((wx, y - 1, wz));
                            }
                        }
                    }
                }
                y -= 1;
            }
        }
        if let Some(rules) = point_rules {
            rules.release(ctx);
        }
    }

    fn fill_noise(
        &self,
        ctx: &mut Context,
        aquifer: &mut Aquifer,
        volume: &DensityVolume,
        chunk: &mut ProtoChunk,
    ) {
        let mut density = ctx.acquire(volume.size());
        self.state
            .router
            .final_density
            .sample_volume(ctx, &mut density, volume);
        let tags = tuff_datapack::tags::current();
        let flags = tags.state_flags();
        for z in 0..volume.size_z {
            let bz = volume.block_z(z);
            for x in 0..volume.size_x {
                let bx = volume.block_x(x);
                let mut column = crate::proto::ColumnFill::default();
                for y in (0..volume.size_y).rev() {
                    let by = volume.block_y(y);
                    let d = density[volume.index(x, y, z)];
                    let state = aquifer
                        .compute_substance(ctx, bx, by, bz, d as f64)
                        .unwrap_or(self.settings.default_block);
                    if state != tuff_vanilla_data::Block::Air.default_state() {
                        chunk.fill_block(&mut column, x, by, z, state, flags);
                        if aquifer.should_schedule_fluid_update()
                            && matches!(
                                state.block(),
                                tuff_vanilla_data::Block::Water | tuff_vanilla_data::Block::Lava
                            )
                        {
                            chunk.post_processing.push((bx, by, bz));
                        }
                    }
                }
            }
        }
        ctx.release(density);
    }
}

//! The flat and debug generators: terrain from a layer list or from the
//! block state registry, with no noise, features or structures.

use std::sync::Arc;

use crate::chunk_generator::Vanilla;
use tuff_core::storage::level::FlatSettings;
use tuff_core::world::generator::{ChunkGenerator, ChunkPipeline, PipelineContext};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{
    BIOME_VOLUME, Chunk, ChunkShape, SECTION_VOLUME, Section, biome_id, is_air, plains,
};
use tuff_world_model::pos::ChunkPos;

fn section_of(
    shape: ChunkShape,
    index: usize,
    biome: u16,
    state_at: impl Fn(usize, usize, i32) -> BlockState,
) -> Section {
    let base = shape.min_y + index as i32 * 16;
    let mut blocks = Box::new([Block::Air.default_state(); SECTION_VOLUME]);
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                blocks[(y << 8) | (z << 4) | x] = state_at(x, z, base + y as i32);
            }
        }
    }
    Section::new(blocks, Box::new([biome; BIOME_VOLUME]))
}

pub struct Flat {
    biome: std::sync::atomic::AtomicU16,
    /// The biome the settings name, resolved again once data packs load.
    biome_name: String,
    layers: Vec<BlockState>,
    staged: Option<Arc<Vanilla>>,
    /// The lowest y of the dimension, once bound.
    min_y: std::sync::atomic::AtomicI32,
}

impl Flat {
    /// `None` when a layer names an unknown block.
    pub fn new(settings: &FlatSettings) -> Option<Self> {
        Some(Self {
            biome: std::sync::atomic::AtomicU16::new(
                biome_id(&settings.biome).unwrap_or_else(plains),
            ),
            biome_name: settings.biome.clone(),
            layers: Self::layer_states(settings)?,
            staged: None,
            min_y: std::sync::atomic::AtomicI32::new(ChunkShape::OVERWORLD.min_y),
        })
    }

    /// A flat world that also places features, lakes and structures when
    /// its settings ask for any.
    pub fn for_world(settings: &FlatSettings, seed: i64, structures: bool) -> Option<Self> {
        let mut flat = Self::new(settings)?;
        let structure_sets = structures
            && settings
                .structure_overrides
                .as_ref()
                .is_none_or(|sets| !sets.is_empty());
        let top_layers = !Self::top_layers(&flat.layers).is_empty();
        if settings.features || settings.lakes || structure_sets || top_layers {
            flat.staged = Vanilla::flat(settings, seed, structures).map(Arc::new);
        }
        Some(flat)
    }

    /// One state per block of the layers, bottom first; `None` when a
    /// layer names an unknown block.
    pub fn layer_states(settings: &FlatSettings) -> Option<Vec<BlockState>> {
        let mut layers = Vec::new();
        for (block, height) in &settings.layers {
            let state = Block::from_name(block)?.default_state();
            layers.extend(std::iter::repeat_n(state, *height as usize));
        }
        Some(layers)
    }

    /// The layers that are not solid enough for terrain: the game places
    /// them with a feature after everything else.
    pub fn top_layers(layers: &[BlockState]) -> Vec<(i32, BlockState)> {
        layers
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !is_air(**s) && !crate::proto::HeightmapKind::MotionBlocking.is_opaque(**s)
            })
            .map(|(i, s)| (i as i32, *s))
            .collect()
    }

    fn state_at(&self, shape: ChunkShape, y: i32) -> BlockState {
        usize::try_from(y - shape.min_y)
            .ok()
            .and_then(|i| self.layers.get(i))
            .copied()
            .unwrap_or_else(|| Block::Air.default_state())
    }
}

impl ChunkGenerator for Flat {
    fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let biome = self.biome.load(std::sync::atomic::Ordering::Relaxed);
        let sections = (0..shape.sections())
            .map(|i| section_of(shape, i, biome, |_, _, y| self.state_at(shape, y)))
            .collect();
        Chunk::from_sections(shape, sections, None)
    }

    fn sea_level(&self) -> i32 {
        self.min_y() + 1
    }

    fn is_flat(&self) -> bool {
        true
    }

    fn bind(&self, data: &tuff_core::world::generator::GeneratorData<'_>) {
        self.min_y
            .store(data.shape.min_y, std::sync::atomic::Ordering::Relaxed);
        if biome_id(&self.biome_name).is_none()
            && let Some(id) = data
                .registries
                .get(crate::loaded::biomes::BIOME)
                .and_then(|biomes| biomes.index_of(&crate::json::qualify(&self.biome_name)))
        {
            self.biome
                .store(id as u16, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(staged) = &self.staged {
            staged.bind(data);
        }
    }

    fn pipeline(self: Arc<Self>, cx: PipelineContext) -> Option<Arc<dyn ChunkPipeline>> {
        self.staged.clone()?.pipeline(cx)
    }

    fn nearest_ring_structure(&self, structures: &[&str], from: BlockPos) -> Option<BlockPos> {
        self.staged
            .as_ref()?
            .nearest_ring_structure(structures, from)
    }

    fn locate_structure(
        &self,
        structures: &[&str],
        from: BlockPos,
        shape: ChunkShape,
    ) -> Option<(BlockPos, &'static str)> {
        self.staged
            .as_ref()?
            .locate_structure(structures, from, shape)
    }

    fn locate_structure_within(
        &self,
        structures: &[&str],
        from: BlockPos,
        radius: i32,
        shape: ChunkShape,
        skip: Option<tuff_core::world::generator::SkipChunk<'_>>,
    ) -> Option<(BlockPos, &'static str)> {
        self.staged
            .as_ref()?
            .locate_structure_within(structures, from, radius, shape, skip)
    }

    fn spawn(&self) -> BlockPos {
        let top = self
            .layers
            .iter()
            .rposition(|s| !is_air(*s))
            .map_or(0, |i| i as i32 + 1);
        BlockPos::new(0, self.min_y() + top, 0)
    }
}

impl Flat {
    fn min_y(&self) -> i32 {
        self.min_y.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub struct Debug {
    states: Vec<BlockState>,
    width: i32,
    height: i32,
}

impl Debug {
    pub fn new() -> Self {
        let states: Vec<BlockState> = (0..BlockState::COUNT as u32)
            .filter_map(BlockState::from_id)
            .collect();
        let width = (states.len() as f64).sqrt().ceil() as i32;
        let height = (states.len() as f64 / f64::from(width)).ceil() as i32;
        Self {
            states,
            width,
            height,
        }
    }

    fn state_at(&self, x: i32, z: i32) -> Option<BlockState> {
        if x <= 0 || z <= 0 || x % 2 == 0 || z % 2 == 0 {
            return None;
        }
        let (gx, gz) = (x / 2, z / 2);
        if gx > self.width || gz > self.height {
            return None;
        }
        self.states.get((gx * self.width + gz) as usize).copied()
    }
}

impl Default for Debug {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkGenerator for Debug {
    fn is_debug(&self) -> bool {
        true
    }

    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let barrier = Block::Barrier.default_state();
        let air = Block::Air.default_state();
        let sections = (0..shape.sections())
            .map(|i| {
                section_of(shape, i, plains(), |x, z, y| match y {
                    60 => barrier,
                    70 => self
                        .state_at(pos.x * 16 + x as i32, pos.z * 16 + z as i32)
                        .unwrap_or(air),
                    _ => air,
                })
            })
            .collect();
        Chunk::from_sections(shape, sections, None)
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 70, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_generator::{StagedGenerator, StartsBlock};

    #[tuff_core::watchdog]
    #[test]
    fn flat_and_debug_generators_report_their_client_flags() {
        let flat = Flat::new(&FlatSettings::default()).unwrap();
        assert!(flat.is_flat() && !flat.is_debug());
        let debug = Debug::new();
        assert!(debug.is_debug() && !debug.is_flat());
    }

    #[tuff_core::watchdog]
    #[test]
    fn default_flat_layers_stack_from_the_world_floor() {
        let flat = Flat::new(&FlatSettings::default()).unwrap();
        let chunk = flat.generate(ChunkPos { x: 0, z: 0 }, ChunkShape::OVERWORLD);
        let at = |y| chunk.get(3, y, 5);
        assert_eq!(at(-64), Block::Bedrock.default_state());
        assert_eq!(at(-62), Block::Dirt.default_state());
        assert_eq!(at(-61), Block::GrassBlock.default_state());
        assert_eq!(at(-60), Block::Air.default_state());
        assert_eq!(flat.spawn().y, -60);
    }

    #[tuff_core::watchdog]
    #[test]
    fn debug_world_has_a_barrier_floor_and_the_states_in_a_grid() {
        let debug = Debug::new();
        let chunk = debug.generate(ChunkPos { x: 0, z: 0 }, ChunkShape::OVERWORLD);
        assert_eq!(chunk.get(0, 60, 0), Block::Barrier.default_state());
        assert_eq!(chunk.get(3, 70, 1), debug.states[debug.width as usize]);
        assert_eq!(chunk.get(2, 70, 1), Block::Air.default_state());
    }

    #[tuff_core::watchdog]
    #[test]
    fn level_type_flat_selects_the_flat_generator() {
        let mut level = tuff_core::storage::level::LevelData::load(
            &tuff_storage::memory::MemoryBackend::default(),
            || 1,
            |_| BlockPos::new(0, 64, 0),
        )
        .unwrap();
        assert!(level.set_new_world_type("flat", "{}"));
        let overworld = level
            .dimension(tuff_core::storage::level::OVERWORLD)
            .unwrap();
        let generator = crate::chunk_generator::for_settings(overworld, 1);
        assert_eq!(generator.spawn(), BlockPos::new(0, -60, 0));
    }

    fn staged(settings: &FlatSettings, structures: bool) -> Vanilla {
        Vanilla::flat(settings, 42, structures).unwrap()
    }

    fn start_count(generator: &Vanilla, radius: i32) -> usize {
        let mut total = 0;
        for bz in -radius..=radius {
            for bx in -radius..=radius {
                let block = generator.starts((bx, bz), ChunkShape::OVERWORLD);
                total += StartsBlock::chunks((bx, bz))
                    .filter_map(|p| block.get(p))
                    .map(|starts| starts.len())
                    .sum::<usize>();
            }
        }
        total
    }

    fn decorated_center(generator: &Vanilla, center: ChunkPos) -> crate::proto::ProtoChunk {
        let shape = ChunkShape::OVERWORLD;
        let blocks: Vec<Arc<StartsBlock>> = StartsBlock::around(center)
            .map(|b| Arc::new(generator.starts(b, shape)))
            .collect();
        let window = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .map(|(dx, dz)| {
                generator.terrain(ChunkPos::new(center.x + dx, center.z + dz), shape, &blocks)
            })
            .collect();
        generator.decorate(center, window).remove(4)
    }

    fn top_non_layer_blocks(chunk: &crate::proto::ProtoChunk, layers: i32) -> usize {
        let base = ChunkShape::OVERWORLD.min_y + layers;
        let mut count = 0;
        for y in base..base + 12 {
            for z in 0..16 {
                for x in 0..16 {
                    if !is_air(chunk.get(x, y, z)) {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    #[tuff_core::watchdog]
    #[test]
    fn settings_round_trip_features_lakes_and_overrides() {
        let json = serde_json::json!({
            "features": true,
            "lakes": true,
            "structure_overrides": ["minecraft:villages"],
            "layers": [{"block": "minecraft:stone", "height": 2}],
        });
        let settings = FlatSettings::from_json(&json).unwrap();
        assert!(settings.features && settings.lakes);
        assert_eq!(
            settings.structure_overrides,
            Some(vec!["minecraft:villages".to_owned()])
        );
        let back = FlatSettings::from_nbt(&settings.to_nbt()).unwrap();
        assert_eq!(back, settings);
        let open = FlatSettings::from_json(&serde_json::json!({})).unwrap();
        assert_eq!(open.structure_overrides, None);
        assert_eq!(
            FlatSettings::default().structure_overrides.unwrap().len(),
            2
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn default_preset_places_villages_and_overrides_restrict_them() {
        let default = staged(&FlatSettings::default(), true);
        assert!(start_count(&default, 6) > 0);
        let none = FlatSettings {
            structure_overrides: Some(Vec::new()),
            ..FlatSettings::default()
        };
        assert_eq!(start_count(&staged(&none, true), 6), 0);
        assert_eq!(start_count(&staged(&FlatSettings::default(), false), 6), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn non_opaque_layers_are_filled_in_by_the_top_layer_step() {
        let settings = FlatSettings {
            layers: vec![
                ("minecraft:bedrock".into(), 1),
                ("minecraft:snow".into(), 1),
                ("minecraft:stone".into(), 1),
                ("minecraft:snow".into(), 1),
            ],
            structure_overrides: Some(Vec::new()),
            ..FlatSettings::default()
        };
        let generator = staged(&settings, true);
        let center = ChunkPos::new(0, 0);
        let shape = ChunkShape::OVERWORLD;
        let terrain = generator.terrain(center, shape, &[]);
        let snow = Block::Snow.default_state();
        let floor = shape.min_y;
        assert!(is_air(terrain.get(3, floor + 1, 4)));
        assert_eq!(terrain.get(3, floor + 2, 4), Block::Stone.default_state());
        let chunk = decorated_center(&generator, center);
        assert_eq!(chunk.get(3, floor + 1, 4), snow);
        assert_eq!(chunk.get(3, floor + 2, 4), Block::Stone.default_state());
        assert_eq!(chunk.get(3, floor + 3, 4), snow);
        assert!(is_air(chunk.get(3, floor + 4, 4)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn features_decorate_the_flat_surface() {
        let plain = FlatSettings {
            structure_overrides: Some(Vec::new()),
            ..FlatSettings::default()
        };
        let decorated = FlatSettings {
            features: true,
            ..plain.clone()
        };
        let mut plain_blocks = 0;
        let mut feature_blocks = 0;
        for x in 0..4 {
            let center = ChunkPos::new(x, 0);
            plain_blocks +=
                top_non_layer_blocks(&decorated_center(&staged(&plain, true), center), 4);
            feature_blocks +=
                top_non_layer_blocks(&decorated_center(&staged(&decorated, true), center), 4);
        }
        assert_eq!(plain_blocks, 0);
        assert!(feature_blocks > 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_all_air_flat_world_places_no_biome_features_unless_the_void_biome() {
        let plains = plains();
        let air = |biome: &str| FlatSettings {
            biome: biome.into(),
            layers: vec![("minecraft:air".into(), 3)],
            features: true,
            structure_overrides: Some(Vec::new()),
            ..FlatSettings::default()
        };
        let void_gen = staged(&air("minecraft:plains"), true);
        assert!(
            !void_gen
                .generator()
                .decorator()
                .has_feature(plains, "minecraft:patch_grass_plain")
        );
        let solid = FlatSettings {
            features: true,
            ..FlatSettings::default()
        };
        assert!(
            staged(&solid, true)
                .generator()
                .decorator()
                .has_feature(plains, "minecraft:patch_grass_plain")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn lakes_add_the_lava_lakes_without_the_biome_features() {
        let plains = plains();
        let lakes = crate::feature::Decorator::flat(
            plains,
            tuff_vanilla_data::biome::Biome::by_id(plains).unwrap(),
            false,
            true,
            &[],
            None,
        );
        assert!(lakes.has_feature(plains, "minecraft:lake_lava_surface"));
        assert!(lakes.has_feature(plains, "minecraft:lake_lava_underground"));
        assert!(!lakes.has_feature(plains, "minecraft:patch_grass_plain"));
        let none = crate::feature::Decorator::flat(
            plains,
            tuff_vanilla_data::biome::Biome::by_id(plains).unwrap(),
            false,
            false,
            &[],
            None,
        );
        assert!(!none.has_feature(plains, "minecraft:lake_lava_surface"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn flat_features_follow_a_pack_biome_feature_list() {
        let plains = plains();
        let mut sources = crate::loaded::features::FeatureSources::default();
        sources
            .insert_biome_id(
                plains,
                &serde_json::json!({"features": [[], [], [], [], [], [], [], [], [], ["minecraft:flower_plains"]]}),
            )
            .unwrap();
        let decorator = crate::feature::Decorator::flat(
            plains,
            tuff_vanilla_data::biome::Biome::by_id(plains).unwrap(),
            true,
            false,
            &[],
            Some(std::sync::Arc::new(sources)),
        );
        assert!(decorator.has_feature(plains, "minecraft:flower_plains"));
        assert!(!decorator.has_feature(plains, "minecraft:patch_grass_plain"));
    }
}

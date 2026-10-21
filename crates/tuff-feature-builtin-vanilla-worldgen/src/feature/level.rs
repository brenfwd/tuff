//! The world as features see it: a 3x3 window of chunks being generated
//! around the chunk that is decorated. Writes outside the window are
//! refused like the game refuses them; reads outside it see nothing.

use std::collections::HashMap;
use std::sync::Arc;

use tuff_vanilla_data::worldgen::VerticalAnchor;
use tuff_vanilla_data::{Block, BlockState};

use crate::biome_zoom::zoom;
use crate::generator::NoiseGenerator;
use crate::proto::{HeightmapKind, ProtoChunk, ScheduledTick};
use crate::surface::GenContext;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Everything a feature may touch.
pub struct Region<'g> {
    /// The noise generator, absent when features grow in a live world
    /// (saplings, bone meal) whose chunks came from elsewhere.
    generator: Option<&'g NoiseGenerator>,
    pub center: ChunkPos,
    chunks: Vec<ProtoChunk>,
    outside_biomes: HashMap<ChunkPos, Arc<[u16]>>,
    pub span: GenContext,
    /// The generator's own vertical span, before clipping to the chunks.
    generator_span: GenContext,
    seed: i64,
    zoom_seed: i64,
    random: Option<tuff_item::random::Random>,
    ocean_floor_ceiling: Option<Option<i32>>,
}

impl<'g> Region<'g> {
    /// `chunks` are the 3x3 around `center`, row by row from the north-west.
    pub fn new(generator: &'g NoiseGenerator, center: ChunkPos, chunks: Vec<ProtoChunk>) -> Self {
        let mut region = Self::live(
            center,
            chunks,
            generator.span,
            generator.state.seed,
            generator.zoom_seed,
        );
        region.generator = Some(generator);
        region
    }

    /// A window over live chunks, for features grown outside world
    /// generation: biome lookups outside the window see the nearest chunk
    /// and the biome placement filter passes everything.
    pub fn live(
        center: ChunkPos,
        chunks: Vec<ProtoChunk>,
        generator_span: GenContext,
        seed: i64,
        zoom_seed: i64,
    ) -> Self {
        assert_eq!(chunks.len(), 9, "a region is 3x3 chunks");
        let shape = chunks[4].shape;
        let span = GenContext {
            min_y: generator_span.min_y.max(shape.min_y),
            height: generator_span.height.min(shape.height),
            sea_level: generator_span.sea_level,
        };
        Self {
            generator: None,
            center,
            chunks,
            outside_biomes: HashMap::new(),
            span,
            generator_span,
            seed,
            zoom_seed,
            random: None,
            ocean_floor_ceiling: None,
        }
    }

    /// Whether `state` can stay at `pos` while the window is generated.
    /// Light is not known yet, so a dimension with sky light reads as full
    /// daylight: mushrooms only take to ground that overrides it.
    pub fn can_survive(&self, state: BlockState, pos: BlockPos) -> bool {
        if self.generator.is_some_and(|g| g.sky_light)
            && state.block().kind() == tuff_vanilla_data::BlockKind::Mushroom
        {
            let below = self.block(BlockPos::new(pos.x, pos.y - 1, pos.z)).block();
            return tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT,
                below,
            );
        }
        tuff_vanilla_common::placement::can_survive(state, pos, &|p| self.block(p))
    }

    /// The world seed.
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// The generator's vertical span.
    pub fn generator_span(&self) -> GenContext {
        self.generator_span
    }

    /// Whether the biome placement filter lets `top` place in `biome`.
    pub fn biome_has_feature(&self, biome: u16, top: &str) -> bool {
        self.generator
            .is_none_or(|generator| generator.decorator().has_feature(biome, top))
    }

    /// A placed feature by name, parsed with the generator's registry (or
    /// the shared one for live worlds).
    pub fn placed_feature(
        &self,
        name: &str,
    ) -> crate::json::Result<std::sync::Arc<crate::feature::PlacedFeature>> {
        self.placed_feature_value(&serde_json::Value::String(name.to_string()))
    }

    /// A placed feature given as a name or an inline definition.
    pub fn placed_feature_value(
        &self,
        value: &serde_json::Value,
    ) -> crate::json::Result<std::sync::Arc<crate::feature::PlacedFeature>> {
        match self.generator {
            Some(generator) => generator.decorator().registry.lock().unwrap().placed(value),
            None => crate::feature::live_registry()
                .lock()
                .unwrap()
                .placed(value),
        }
    }

    /// The window's own random, positioned at the centre chunk, which some
    /// structure pieces draw from.
    pub fn random(&mut self) -> &mut tuff_item::random::Random {
        let center = self.center;
        let seed = self.seed;
        let generator = self.generator;
        self.random.get_or_insert_with(|| match generator {
            Some(generator) => generator
                .state
                .random_factory("minecraft:worldgen_region_random")
                .at(center.x << 4, 0, center.z << 4),
            None => tuff_item::random::Random::Legacy(tuff_item::random::LegacyRandom::new(
                seed ^ ((center.x as i64) << 32) ^ center.z as i64,
            )),
        })
    }

    /// The block entity data waiting at `pos`.
    pub fn block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound> {
        self.chunk_mut(pos.x >> 4, pos.z >> 4)?
            .block_entities
            .get_mut(&(pos.x, pos.y, pos.z))
    }

    pub fn into_chunks(self) -> Vec<ProtoChunk> {
        self.chunks
    }

    fn slot(&self, cx: i32, cz: i32) -> Option<usize> {
        let (dx, dz) = (cx - self.center.x, cz - self.center.z);
        ((-1..=1).contains(&dx) && (-1..=1).contains(&dz)).then(|| ((dz + 1) * 3 + dx + 1) as usize)
    }

    pub fn chunk(&self, cx: i32, cz: i32) -> Option<&ProtoChunk> {
        self.slot(cx, cz).map(|i| &self.chunks[i])
    }

    pub fn chunk_mut(&mut self, cx: i32, cz: i32) -> Option<&mut ProtoChunk> {
        self.slot(cx, cz).map(move |i| &mut self.chunks[i])
    }

    pub fn min_y(&self) -> i32 {
        self.chunks[4].shape.min_y
    }

    pub fn max_y(&self) -> i32 {
        let shape = self.chunks[4].shape;
        shape.min_y + shape.height - 1
    }

    /// An upper bound of what `height` returns for the ocean floor
    /// anywhere, when no chunk of the window can still change that map
    /// (all are past terrain and have it); `None` otherwise.
    pub fn ocean_floor_ceiling(&mut self) -> Option<i32> {
        if self.ocean_floor_ceiling.is_none() {
            let min_y = self.min_y();
            let ceiling = self
                .chunks
                .iter()
                .map(|c| c.settled_ocean_floor_ceiling())
                .try_fold(min_y, |top, c| c.map(|c| top.max(c)));
            self.ocean_floor_ceiling = Some(ceiling);
        }
        self.ocean_floor_ceiling.unwrap()
    }

    pub fn is_outside_build_height(&self, y: i32) -> bool {
        y < self.min_y() || y > self.max_y()
    }

    pub fn sea_level(&self) -> i32 {
        self.span.sea_level
    }

    /// The data of biome `id`, pack-added biomes of the generator included.
    pub fn biome_data(&self, id: u16) -> &'static tuff_vanilla_data::biome::Biome {
        match self.generator {
            Some(generator) => generator.biome_data(id),
            None => crate::loaded::biomes::PackBiomes::none().data(id),
        }
    }

    pub fn resolve(&self, anchor: VerticalAnchor) -> i32 {
        self.span.resolve(anchor)
    }

    pub fn block(&self, pos: BlockPos) -> BlockState {
        match self.chunk(pos.x >> 4, pos.z >> 4) {
            Some(chunk) => chunk.get(pos.x & 15, pos.y, pos.z & 15),
            None => Block::Air.default_state(),
        }
    }

    pub fn is_within_write_zone(&self, pos: BlockPos) -> bool {
        self.slot(pos.x >> 4, pos.z >> 4).is_some()
    }

    /// Writes a block, refusing positions outside the window.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState) -> bool {
        self.set_block_flags(pos, state, true)
    }

    /// Writes a block; `post_process` off skips scheduling the block's
    /// later shape update, as trees write theirs.
    pub fn set_block_flags(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        post_process: bool,
    ) -> bool {
        let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) else {
            return false;
        };
        chunk.set(pos.x & 15, pos.y, pos.z & 15, state);
        if state.block().block_entity().is_none() {
            chunk.block_entities.remove(&(pos.x, pos.y, pos.z));
        }
        if post_process && let Some(post) = post_process_position(state, pos) {
            self.mark_post_processing(post);
        }
        true
    }

    /// Writes straight into the section, leaving heightmaps untouched, as
    /// ore placement does.
    pub fn set_block_raw(&mut self, pos: BlockPos, state: BlockState) -> bool {
        if self.is_outside_build_height(pos.y) {
            return false;
        }
        let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) else {
            return false;
        };
        chunk.set_raw(pos.x & 15, pos.y, pos.z & 15, state);
        true
    }

    pub fn mark_post_processing(&mut self, pos: BlockPos) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            chunk.post_processing.push((pos.x, pos.y, pos.z));
        }
    }

    /// Schedules a tick of `fluid` (its id) at `pos`, `delay` ticks after
    /// the chunk goes live.
    pub fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: &'static str, delay: i32) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            let tick = ScheduledTick {
                kind: fluid,
                pos: (pos.x, pos.y, pos.z),
                delay,
            };
            ScheduledTick::schedule(&mut chunk.fluid_ticks, tick);
        }
    }

    /// Schedules a tick of the block at `pos`.
    pub fn schedule_block_tick(&mut self, pos: BlockPos, block: Block, delay: i32) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            let tick = ScheduledTick {
                kind: block.name(),
                pos: (pos.x, pos.y, pos.z),
                delay,
            };
            ScheduledTick::schedule(&mut chunk.block_ticks, tick);
        }
    }

    /// Schedules the fluid in `state` at `pos` to tick as soon as the chunk
    /// is live, if it holds one.
    pub fn schedule_own_fluid_tick(&mut self, pos: BlockPos, state: BlockState) {
        if let Some(fluid) = crate::feature::predicate::fluid(state).id() {
            self.schedule_fluid_tick(pos, fluid, 0);
        }
    }

    /// Adds an entity (its saved data, with `Pos`) to the chunk holding
    /// `pos`.
    pub fn add_entity(&mut self, pos: BlockPos, data: Compound) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            chunk.entities.push(data);
        }
    }

    pub fn remove_block_entity(&mut self, pos: BlockPos) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            chunk.block_entities.remove(&(pos.x, pos.y, pos.z));
        }
    }

    /// Stores block entity data for a block a feature placed.
    pub fn set_block_entity(&mut self, pos: BlockPos, data: Compound) {
        if let Some(chunk) = self.chunk_mut(pos.x >> 4, pos.z >> 4) {
            chunk.set_block_entity(pos, data);
        }
    }

    /// The game's `getHeight` for features: one above the top matching
    /// block.
    pub fn height(&mut self, kind: HeightmapKind, x: i32, z: i32) -> i32 {
        match self.chunk_mut(x >> 4, z >> 4) {
            Some(chunk) => chunk.height(kind, x, z) + 1,
            None => self.min_y(),
        }
    }

    fn noise_biome(&mut self, qx: i32, qy: i32, qz: i32) -> u16 {
        let (cx, cz) = (qx >> 2, qz >> 2);
        if let Some(chunk) = self.chunk(cx, cz) {
            return chunk.biome(qx & 3, qy, qz & 3);
        }
        let shape = self.chunks[4].shape;
        let Some(generator) = self.generator else {
            let dx = (cx - self.center.x).clamp(-1, 1);
            let dz = (cz - self.center.z).clamp(-1, 1);
            let nearest = &self.chunks[((dz + 1) * 3 + dx + 1) as usize];
            return nearest.biome(qx & 3, qy, qz & 3);
        };
        let biomes = self
            .outside_biomes
            .entry(ChunkPos::new(cx, cz))
            .or_insert_with(|| generator.chunk_biomes(ChunkPos::new(cx, cz), shape));
        let min = shape.min_y >> 2;
        let qy = qy.clamp(min, min + shape.height / 4 - 1) - min;
        biomes[(qy * 16 + (qz & 3) * 4 + (qx & 3)) as usize]
    }

    /// The biome at a block, through the zoom.
    pub fn biome(&mut self, pos: BlockPos) -> u16 {
        let [qx, qy, qz] = zoom(self.zoom_seed, pos.x, pos.y, pos.z);
        self.noise_biome(qx, qy, qz)
    }

    /// Whether any block entity data waits at `pos`.
    pub fn has_block_entity(&self, pos: BlockPos) -> bool {
        self.chunk(pos.x >> 4, pos.z >> 4)
            .is_some_and(|c| c.block_entity(pos).is_some())
    }
}

/// Blocks whose placement schedules a later shape update of themselves or
/// the block above.
fn post_process_position(state: BlockState, pos: BlockPos) -> Option<BlockPos> {
    match state.block() {
        Block::BrownMushroom | Block::RedMushroom => Some(pos),
        Block::SoulSand | Block::MagmaBlock => Some(BlockPos::new(pos.x, pos.y + 1, pos.z)),
        _ => None,
    }
}

//! Block and sky light, computed exactly as the game computes it: the same
//! sources, propagation order, section bookkeeping and edge cases, so the
//! results match what a vanilla server stores and sends.
//!
//! [`LevelLight`] holds the light of every chunk added to it and reads blocks
//! through a [`ChunkSource`], so it can be driven from any owner of chunk
//! data (a region actor, or a plain map in tests). Chunks the source does not
//! return are treated as solid bedrock, like unloaded chunks.

mod data_layer;
mod engine;
mod sky_sources;
mod storage;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use tuff_vanilla_data::light::Direction;
use tuff_vanilla_data::{Block, BlockState};

use crate::chunk::{Chunk, ChunkShape};
use crate::pos::ChunkPos;

pub use data_layer::{DataLayer, LAYER_BYTES};
pub use sky_sources::{NEGATIVE_INFINITY, SkyLightSources};

use engine::LayerEngine;

const AIR: BlockState = Block::Air.default_state();

/// Lowest section y that holds light: one below the world.
pub const fn min_light_section(shape: ChunkShape) -> i32 {
    shape.min_section() - 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Pos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn section(self) -> SectionPos {
        SectionPos::new(self.x >> 4, self.y >> 4, self.z >> 4)
    }

    pub fn column(self) -> ChunkPos {
        ChunkPos::new(self.x >> 4, self.z >> 4)
    }

    fn section_relative(self) -> (i32, i32, i32) {
        (self.x & 15, self.y & 15, self.z & 15)
    }

    fn offset(self, direction: Direction) -> Self {
        let (dx, dy, dz) = direction.offset();
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SectionPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl SectionPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn column(self) -> ChunkPos {
        ChunkPos::new(self.x, self.z)
    }

    pub const fn block_to_section(coordinate: i32) -> i32 {
        coordinate >> 4
    }

    pub const fn section_to_block(section: i32) -> i32 {
        section << 4
    }

    pub const fn section_relative(coordinate: i32) -> i32 {
        coordinate & 15
    }

    pub const fn of_block(x: i32, y: i32, z: i32) -> Self {
        Self::new(x >> 4, y >> 4, z >> 4)
    }

    /// Packed as 22 bits of x, 20 of y and 22 of z, high to low.
    pub const fn as_long(self) -> i64 {
        ((self.x as i64 & 0x3F_FFFF) << 42)
            | (self.y as i64 & 0xF_FFFF)
            | ((self.z as i64 & 0x3F_FFFF) << 20)
    }

    pub const fn from_long(packed: i64) -> Self {
        Self::new(
            (packed >> 42) as i32,
            ((packed << 44) >> 44) as i32,
            ((packed << 22) >> 42) as i32,
        )
    }

    fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

/// Whether replacing `old` with `new` can change light, so the block needs a
/// light check and its column's sky sources an update.
pub fn has_different_light_properties(old: BlockState, new: BlockState) -> bool {
    old != new
        && (old.light_dampening() != new.light_dampening()
            || old.light_emission() != new.light_emission()
            || !old.is_empty_light_shape()
            || !new.is_empty_light_shape())
}

#[derive(Clone, Copy)]
pub struct LightChunk<'a> {
    pub blocks: &'a Chunk,
    pub sky_sources: &'a SkyLightSources,
}

/// Where the engine reads blocks from.
pub trait ChunkSource {
    fn chunk(&self, pos: ChunkPos) -> Option<LightChunk<'_>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightLayer {
    Sky,
    Block,
}

/// One light section as the network and storage formats see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionLight {
    /// No light is stored for the section.
    Missing,
    /// Stored, and zero everywhere.
    Empty,
    Data(Box<[u8; LAYER_BYTES]>),
}

impl SectionLight {
    fn of(layer: Option<&DataLayer>) -> Self {
        match layer {
            None => Self::Missing,
            Some(layer) if layer.is_empty() => Self::Empty,
            Some(layer) => Self::Data(layer.to_bytes()),
        }
    }
}

/// A chunk's light sections, from [`min_light_section`] up: one per
/// block section plus one below and one above the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkLight {
    pub sky: Vec<SectionLight>,
    pub block: Vec<SectionLight>,
}

impl ChunkLight {
    /// The light of `layer` at a block of this chunk (`x` and `z` taken
    /// within it), or `None` when its section stores none.
    pub fn value(
        &self,
        shape: ChunkShape,
        layer: LightLayer,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<u8> {
        let sections = match layer {
            LightLayer::Sky => &self.sky,
            LightLayer::Block => &self.block,
        };
        let index = usize::try_from((y >> 4) - min_light_section(shape)).ok()?;
        match sections.get(index)? {
            SectionLight::Missing => None,
            SectionLight::Empty => Some(0),
            SectionLight::Data(data) => {
                let i = (((y & 15) as usize) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize;
                Some(data[i >> 1] >> ((i & 1) * 4) & 0x0F)
            }
        }
    }
}

/// The light of a dimension.
pub struct LevelLight {
    shape: ChunkShape,
    block: LayerEngine,
    sky: Option<LayerEngine>,
}

impl LevelLight {
    pub fn new(shape: ChunkShape, has_sky_light: bool) -> Self {
        Self {
            shape,
            block: LayerEngine::new(false, min_light_section(shape), shape.light_sections()),
            sky: has_sky_light
                .then(|| LayerEngine::new(true, min_light_section(shape), shape.light_sections())),
        }
    }

    pub fn shape(&self) -> ChunkShape {
        self.shape
    }

    pub fn has_sky_light(&self) -> bool {
        self.sky.is_some()
    }

    fn engines(&mut self) -> impl Iterator<Item = &mut LayerEngine> {
        std::iter::once(&mut self.block).chain(self.sky.as_mut())
    }

    fn engine(&self, layer: LightLayer) -> Option<&LayerEngine> {
        match layer {
            LightLayer::Block => Some(&self.block),
            LightLayer::Sky => self.sky.as_ref(),
        }
    }

    /// Re-evaluates light at `pos` on the next [`Self::run_light_updates`].
    pub fn check_block(&mut self, pos: Pos) {
        self.engines().for_each(|engine| engine.check_block(pos));
    }

    /// Records whether a section now holds only air.
    pub fn update_section_status(&mut self, section: SectionPos, empty: bool) {
        self.engines()
            .for_each(|engine| engine.storage.update_section_status(section, empty));
    }

    pub fn set_light_enabled(&mut self, source: &dyn ChunkSource, column: ChunkPos, enabled: bool) {
        self.engines()
            .for_each(|engine| engine.set_light_enabled(source, column, enabled));
    }

    pub fn propagate_light_sources(&mut self, source: &dyn ChunkSource, column: ChunkPos) {
        self.engines()
            .for_each(|engine| engine.propagate_light_sources(source, column));
    }

    /// Supplies stored light for a section, applied once the section stores light.
    pub fn queue_section_data(
        &mut self,
        layer: LightLayer,
        section: SectionPos,
        data: Option<DataLayer>,
    ) {
        let engine = match layer {
            LightLayer::Block => Some(&mut self.block),
            LightLayer::Sky => self.sky.as_mut(),
        };
        if let Some(engine) = engine {
            engine.storage.queue_section_data(section, data);
        }
    }

    pub fn retain_data(&mut self, column: ChunkPos, retain: bool) {
        self.engines()
            .for_each(|engine| engine.storage.retain_data(column, retain));
    }

    pub fn has_light_work(&self) -> bool {
        self.block.has_light_work() || self.sky.as_ref().is_some_and(|s| s.has_light_work())
    }

    /// Propagates every pending change. Returns the number of queue entries processed.
    pub fn run_light_updates(&mut self, source: &dyn ChunkSource) -> usize {
        self.engines()
            .map(|engine| engine.run_light_updates(source))
            .sum()
    }

    /// Starts tracking a chunk's sections. `lighted` means its light is
    /// already known (queued with [`Self::queue_section_data`]).
    pub fn initialize_light(&mut self, source: &dyn ChunkSource, column: ChunkPos, lighted: bool) {
        let min_section = self.shape.min_section();
        if let Some(chunk) = source.chunk(column) {
            for (index, section) in chunk.blocks.sections().enumerate() {
                if !section.has_only_air() {
                    self.update_section_status(
                        SectionPos::new(column.x, min_section + index as i32, column.z),
                        false,
                    );
                }
            }
        }
        self.run_light_updates(source);
        self.set_light_enabled(source, column, lighted);
        self.retain_data(column, false);
    }

    /// Emits the chunk's light sources unless its light is already known.
    /// Light only spreads into chunks that are already initialized, so the
    /// game lights a chunk only once its eight neighbours are initialized.
    pub fn light_chunk(&mut self, source: &dyn ChunkSource, column: ChunkPos, lighted: bool) {
        if !lighted {
            self.propagate_light_sources(source, column);
        }
        self.run_light_updates(source);
    }

    /// Stops tracking a chunk; the source should no longer return it.
    pub fn remove_chunk(&mut self, source: &dyn ChunkSource, column: ChunkPos) {
        self.retain_data(column, false);
        self.set_light_enabled(source, column, false);
        let shape = self.shape;
        for index in 0..shape.light_sections() as i32 {
            let section = SectionPos::new(column.x, min_light_section(shape) + index, column.z);
            self.queue_section_data(LightLayer::Block, section, None);
            self.queue_section_data(LightLayer::Sky, section, None);
        }
        for index in 0..shape.sections() as i32 {
            self.update_section_status(
                SectionPos::new(column.x, shape.min_section() + index, column.z),
                true,
            );
        }
        self.run_light_updates(source);
    }

    pub fn light_on_in_column(&self, column: ChunkPos) -> bool {
        self.block.storage.light_on_in_column(column)
            && self
                .sky
                .as_ref()
                .is_none_or(|sky| sky.storage.light_on_in_column(column))
    }

    pub fn light_value(&self, layer: LightLayer, pos: Pos) -> u8 {
        self.engine(layer)
            .map_or(0, |engine| engine.storage.light_value(pos))
    }

    pub fn section_light(&self, layer: LightLayer, section: SectionPos) -> SectionLight {
        SectionLight::of(
            self.engine(layer)
                .and_then(|engine| engine.layer_data(section)),
        )
    }

    pub fn chunk_light(&self, column: ChunkPos) -> ChunkLight {
        let shape = self.shape;
        let sections = |layer| {
            (0..shape.light_sections() as i32)
                .map(|index| {
                    self.section_light(
                        layer,
                        SectionPos::new(column.x, min_light_section(shape) + index, column.z),
                    )
                })
                .collect()
        };
        ChunkLight {
            sky: sections(LightLayer::Sky),
            block: sections(LightLayer::Block),
        }
    }

    /// Light sections whose light changed since the last call, including the
    /// neighbours of changed blocks, as a bit per light section (from
    /// [`min_light_section`]) for each column.
    pub fn take_changed_sections(&mut self) -> HashMap<ChunkPos, u64> {
        let mut changed = self.block.storage.take_affected();
        if let Some(sky) = &mut self.sky {
            for (column, mask) in sky.storage.take_affected() {
                *changed.entry(column).or_default() |= mask;
            }
        }
        changed
    }
}

#[cfg(test)]
mod section_pos_tests {
    use super::SectionPos;

    #[tuff_core::watchdog]
    #[test]
    fn section_pos_round_trips_through_long() {
        for pos in [
            SectionPos::new(0, 0, 0),
            SectionPos::new(-1, -4, 1),
            SectionPos::new(1_875_000, 19, -1_875_000),
            SectionPos::new(-2_097_152, -524_288, 2_097_151),
        ] {
            assert_eq!(SectionPos::from_long(pos.as_long()), pos);
        }
        assert_eq!(
            SectionPos::new(1, 2, 3).as_long(),
            (1 << 42) | 2 | (3 << 20)
        );
        assert_eq!(SectionPos::of_block(-1, 16, 31), SectionPos::new(-1, 1, 1));
    }
}

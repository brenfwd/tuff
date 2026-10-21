//! A chunk being generated: paletted block sections, a flat biome array and
//! the game's heightmaps, converted to a [`Chunk`] once done.

use std::collections::BTreeMap;
use std::sync::Arc;

use tuff_vanilla_data::{Block, BlockState};

use crate::structure::{References, StructureStart};

use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{BIOME_VOLUME, Chunk, ChunkShape, Section};
use tuff_world_model::pos::ChunkPos;

use tuff_datapack::tags::{FLAG_BLOCKS_MOTION, FLAG_BLOCKS_MOTION_NO_LEAVES};

pub use tuff_core::world::heightmap_kind::{HeightmapKind, has_fluid};

/// The `TagSet::state_flags` bits that make a state count for `kind`.
fn kind_mask(kind: HeightmapKind) -> u8 {
    match kind {
        HeightmapKind::WorldSurfaceWg | HeightmapKind::WorldSurface => BlockState::FLAG_NOT_AIR,
        HeightmapKind::OceanFloorWg | HeightmapKind::OceanFloor => FLAG_BLOCKS_MOTION,
        HeightmapKind::MotionBlocking => FLAG_BLOCKS_MOTION | BlockState::FLAG_HEIGHT_FLUID,
        HeightmapKind::MotionBlockingNoLeaves => {
            FLAG_BLOCKS_MOTION_NO_LEAVES | BlockState::FLAG_HEIGHT_FLUID
        }
    }
}

/// Where the noise fill is in one column: it writes each column once, top
/// down, so each worldgen heightmap is set by the first state that counts
/// for it and never again.
#[derive(Default)]
pub struct ColumnFill {
    surface_done: bool,
    floor_done: bool,
}

/// Which heightmaps block writes keep current: the worldgen ones while
/// terrain is built, the final ones afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Terrain,
    Decoration,
}

pub use tuff_core::world::generator::ScheduledTick;

#[derive(Clone)]
pub struct ProtoChunk {
    pub pos: ChunkPos,
    pub shape: ChunkShape,
    pub stage: Stage,
    /// Their biomes are only filled in by [`ProtoChunk::into_chunk`].
    sections: Vec<Section>,
    biomes: Vec<u16>,
    /// First available y per column, or `None` until primed.
    heightmaps: [Option<Box<[i32; 256]>>; 6],
    /// Whether every block write so far kept the worldgen heightmaps
    /// current, so they equal what priming them would give.
    worldgen_maps_exact: bool,
    /// Positions whose fluid should be ticked once the chunk is live.
    pub post_processing: Vec<(i32, i32, i32)>,
    /// Fluid and block ticks features and structures scheduled, for the
    /// live chunk to run.
    pub fluid_ticks: Vec<ScheduledTick>,
    pub block_ticks: Vec<ScheduledTick>,
    /// Block entity data features and structures left, by position.
    pub block_entities: BTreeMap<(i32, i32, i32), Compound>,
    /// Entities structures placed, as saved entity data.
    pub entities: Vec<Compound>,
    /// Structures that start in this chunk.
    pub structure_starts: Vec<Arc<StructureStart>>,
    /// Structures from this chunk and its neighbours that reach it.
    pub structure_references: Vec<References>,
    /// The `starts` compound of the saved chunk this one resumed from.
    pub saved_starts: Option<Compound>,
}

impl ProtoChunk {
    pub fn new(pos: ChunkPos, shape: ChunkShape) -> Self {
        let air = Block::Air.default_state();
        let mut chunk = Self {
            pos,
            shape,
            stage: Stage::Terrain,
            sections: vec![Section::uniform(air, 0); shape.sections()],
            biomes: vec![0; 16 * (shape.height as usize / 4)],
            heightmaps: Default::default(),
            worldgen_maps_exact: true,
            post_processing: Vec::new(),
            fluid_ticks: Vec::new(),
            block_ticks: Vec::new(),
            block_entities: BTreeMap::new(),
            entities: Vec::new(),
            structure_starts: Vec::new(),
            structure_references: Vec::new(),
            saved_starts: None,
        };
        for kind in HeightmapKind::WORLDGEN {
            chunk.heightmaps[kind.index()] = Some(Box::new([shape.min_y; 256]));
        }
        chunk
    }

    /// A chunk in generation read back from storage, past its terrain:
    /// `sections` from the bottom up and `biomes` section by section,
    /// heightmaps primed on first use unless set.
    pub fn resumed(
        pos: ChunkPos,
        shape: ChunkShape,
        sections: Vec<Section>,
        biomes: Vec<u16>,
    ) -> Self {
        assert_eq!(sections.len(), shape.sections());
        assert_eq!(biomes.len(), shape.sections() * BIOME_VOLUME);
        Self {
            pos,
            shape,
            stage: Stage::Decoration,
            sections,
            biomes,
            heightmaps: Default::default(),
            worldgen_maps_exact: false,
            post_processing: Vec::new(),
            fluid_ticks: Vec::new(),
            block_ticks: Vec::new(),
            block_entities: BTreeMap::new(),
            entities: Vec::new(),
            structure_starts: Vec::new(),
            structure_references: Vec::new(),
            saved_starts: None,
        }
    }

    /// The first free y of every column, x fastest, if the heightmap is
    /// kept.
    pub fn heightmap(&self, kind: HeightmapKind) -> Option<&[i32; 256]> {
        self.heightmaps[kind.index()].as_deref()
    }

    pub fn set_heightmap(&mut self, kind: HeightmapKind, heights: [i32; 256]) {
        self.worldgen_maps_exact = false;
        self.heightmaps[kind.index()] = Some(Box::new(heights));
    }

    /// The section and the index within it of chunk-relative `x`, `z` and
    /// absolute `y` inside the chunk.
    #[inline]
    fn locate(&self, x: i32, y: i32, z: i32) -> (usize, usize) {
        let local = y - self.shape.min_y;
        (
            (local >> 4) as usize,
            (((local & 15) << 8) | (z << 4) | x) as usize,
        )
    }

    pub fn min_x(&self) -> i32 {
        self.pos.x << 4
    }

    pub fn min_z(&self) -> i32 {
        self.pos.z << 4
    }

    /// The block at chunk-relative `x`, `z` and absolute `y`.
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        if !self.shape.contains_y(y) {
            return Block::VoidAir.default_state();
        }
        let (section, index) = self.locate(x, y, z);
        self.sections[section].get(index)
    }

    /// Sets a block and updates the heightmaps of the current stage.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> BlockState {
        if !self.shape.contains_y(y) {
            return Block::VoidAir.default_state();
        }
        let section = ((y - self.shape.min_y) >> 4) as usize;
        if state.block() == Block::Air && self.section_is_air(section) {
            return state;
        }
        let old = self.write(x, y, z, state);
        let kinds: &[HeightmapKind] = match self.stage {
            Stage::Terrain => &HeightmapKind::WORLDGEN,
            Stage::Decoration => &HeightmapKind::FINAL,
        };
        let column = (z << 4 | x) as usize;
        let mut lowest_first = i32::MAX;
        let mut missing = false;
        for kind in kinds {
            match &self.heightmaps[kind.index()] {
                Some(heights) => lowest_first = lowest_first.min(heights[column]),
                None => missing = true,
            }
        }
        if !missing && y <= lowest_first - 2 {
            return old;
        }
        let tags = tuff_datapack::tags::current();
        let flags = tags.state_flags();
        let state_flags = flags[state.id() as usize];
        let mut absent = Vec::new();
        for kind in kinds {
            if self.heightmaps[kind.index()].is_some() {
                self.update_heightmap(*kind, x, y, z, state_flags, flags);
            } else {
                absent.push(*kind);
            }
        }
        if !absent.is_empty() {
            self.prime_with(&absent, flags);
        }
        old
    }

    fn section_is_air(&self, section: usize) -> bool {
        self.sections[section].has_only_air()
    }

    fn write(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> BlockState {
        let (section, index) = self.locate(x, y, z);
        self.sections[section].set(index, state)
    }

    /// Sets without touching the heightmaps, as ore placement writes.
    pub fn set_raw(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        self.worldgen_maps_exact = false;
        self.write(x, y, z, state);
    }

    /// The noise fill's write: a state at `y` of a column whose blocks
    /// are written once, top down, into a chunk whose worldgen heightmaps
    /// are still at the bottom. `flags` is `TagSet::state_flags`.
    #[inline]
    pub fn fill_block(
        &mut self,
        column: &mut ColumnFill,
        x: i32,
        y: i32,
        z: i32,
        state: BlockState,
        flags: &[u8],
    ) {
        self.write(x, y, z, state);
        let state_flags = flags[state.id() as usize];
        let index = (z << 4 | x) as usize;
        if !column.surface_done && state_flags & BlockState::FLAG_NOT_AIR != 0 {
            column.surface_done = true;
            let heights = self.heightmaps[HeightmapKind::WorldSurfaceWg.index()]
                .as_mut()
                .expect("heightmap exists");
            debug_assert_eq!(heights[index], self.shape.min_y);
            heights[index] = y + 1;
        }
        if !column.floor_done && state_flags & FLAG_BLOCKS_MOTION != 0 {
            column.floor_done = true;
            let heights = self.heightmaps[HeightmapKind::OceanFloorWg.index()]
                .as_mut()
                .expect("heightmap exists");
            debug_assert_eq!(heights[index], self.shape.min_y);
            heights[index] = y + 1;
        }
    }

    fn update_heightmap(
        &mut self,
        kind: HeightmapKind,
        x: i32,
        y: i32,
        z: i32,
        state_flags: u8,
        flags: &[u8],
    ) {
        let column = (z << 4 | x) as usize;
        let first = self.heightmaps[kind.index()]
            .as_ref()
            .expect("heightmap exists")[column];
        if y <= first - 2 {
            return;
        }
        let mask = kind_mask(kind);
        if state_flags & mask != 0 {
            if y >= first {
                self.heightmaps[kind.index()].as_mut().unwrap()[column] = y + 1;
            }
            return;
        }
        if first - 1 == y {
            let mut height = self.shape.min_y;
            for by in (self.shape.min_y..y).rev() {
                if flags[self.get(x, by, z).id() as usize] & mask != 0 {
                    height = by + 1;
                    break;
                }
            }
            self.heightmaps[kind.index()].as_mut().unwrap()[column] = height;
        }
    }

    /// Computes heightmaps from the blocks, as the game primes them.
    pub fn prime(&mut self, kinds: &[HeightmapKind]) {
        let tags = tuff_datapack::tags::current();
        self.prime_with(kinds, tags.state_flags());
    }

    fn prime_with(&mut self, kinds: &[HeightmapKind], flags: &[u8]) {
        assert!(kinds.len() <= 8);
        let mut masks = [0u8; 8];
        for (mask, kind) in masks.iter_mut().zip(kinds) {
            *mask = kind_mask(*kind);
        }
        for kind in kinds {
            self.heightmaps[kind.index()] = Some(Box::new([self.shape.min_y; 256]));
        }
        let top = match self.highest_filled_section() {
            Some(section) => self.shape.section_base(section) + 16,
            None => self.shape.min_y,
        };
        let all = ((1u16 << kinds.len()) - 1) as u8;
        for x in 0..16 {
            for z in 0..16 {
                let column = (z << 4 | x) as usize;
                let mut pending = all;
                let mut y = top - 1;
                while y >= self.shape.min_y && pending != 0 {
                    let state = self.get(x, y, z);
                    if state.block() != Block::Air {
                        let state_flags = flags[state.id() as usize];
                        for (i, kind) in kinds.iter().enumerate() {
                            if pending >> i & 1 == 1 && state_flags & masks[i] != 0 {
                                self.heightmaps[kind.index()].as_mut().unwrap()[column] = y + 1;
                                pending &= !(1 << i);
                            }
                        }
                    }
                    y -= 1;
                }
            }
        }
    }

    /// The game's `getHeight`: the top matching block's y (one below the
    /// first free position), priming the heightmap if needed.
    pub fn height(&mut self, kind: HeightmapKind, x: i32, z: i32) -> i32 {
        if self.heightmaps[kind.index()].is_none() {
            self.prime(&[kind]);
        }
        self.height_of(kind, x, z)
    }

    /// The highest ocean floor value `height` can give for this chunk, if
    /// the map exists and block writes no longer update it.
    pub fn settled_ocean_floor_ceiling(&self) -> Option<i32> {
        if self.stage != Stage::Decoration {
            return None;
        }
        let heights = self.heightmaps[HeightmapKind::OceanFloorWg.index()].as_ref()?;
        heights.iter().copied().max()
    }

    /// Like [`ProtoChunk::height`] for a heightmap known to exist.
    pub fn height_of(&self, kind: HeightmapKind, x: i32, z: i32) -> i32 {
        self.heightmaps[kind.index()]
            .as_ref()
            .expect("heightmap exists")[((z & 15) << 4 | (x & 15)) as usize]
            - 1
    }

    /// The index of the highest section holding a non-air block.
    pub fn highest_filled_section(&self) -> Option<usize> {
        (0..self.shape.sections())
            .rev()
            .find(|&index| !self.section_is_air(index))
    }

    pub fn set_block_entity(&mut self, pos: BlockPos, mut data: Compound) {
        data.insert("x", pos.x);
        data.insert("y", pos.y);
        data.insert("z", pos.z);
        self.block_entities.insert((pos.x, pos.y, pos.z), data);
    }

    pub fn block_entity(&self, pos: BlockPos) -> Option<&Compound> {
        self.block_entities.get(&(pos.x, pos.y, pos.z))
    }

    pub fn set_biomes(&mut self, biomes: Vec<u16>) {
        assert_eq!(biomes.len(), self.biomes.len());
        self.biomes = biomes;
    }

    /// The biome at chunk-relative quart `qx`, `qz` and absolute quart `qy`,
    /// clamped to the chunk's height like the game's.
    pub fn biome(&self, qx: i32, qy: i32, qz: i32) -> u16 {
        let min = self.shape.min_y >> 2;
        let max = min + self.shape.height / 4 - 1;
        let qy = qy.clamp(min, max) - min;
        self.biomes[(qy * 16 + (qz & 3) * 4 + (qx & 3)) as usize]
    }

    pub fn biomes(&self) -> &[u16] {
        &self.biomes
    }

    /// Ends terrain generation: from now on block writes keep the final
    /// heightmaps, primed here from the terrain.
    pub fn finish_terrain(&mut self) {
        self.stage = Stage::Decoration;
        let tags = tuff_datapack::tags::current();
        let flags = tags.state_flags();
        if !self.derive_final_heightmaps(flags) {
            self.prime_with(&HeightmapKind::FINAL, flags);
        }
    }

    /// Fills the final heightmaps from the worldgen ones, which every
    /// write kept current: the surface and ocean floor are the same maps,
    /// and the top block counting for motion blocking is at or below the
    /// highest non-air block. False if the worldgen maps cannot be trusted
    /// or a tag counts air for motion, which priming skips.
    fn derive_final_heightmaps(&mut self, flags: &[u8]) -> bool {
        let air_counts = [Block::Air, Block::CaveAir, Block::VoidAir]
            .iter()
            .any(|air| {
                flags[air.default_state().id() as usize]
                    & (FLAG_BLOCKS_MOTION | FLAG_BLOCKS_MOTION_NO_LEAVES)
                    != 0
            });
        let (Some(surface), Some(floor)) = (
            self.heightmap(HeightmapKind::WorldSurfaceWg),
            self.heightmap(HeightmapKind::OceanFloorWg),
        ) else {
            return false;
        };
        if !self.worldgen_maps_exact || air_counts {
            return false;
        }
        let (surface, floor) = (Box::new(*surface), Box::new(*floor));
        let min_y = self.shape.min_y;
        let mut motion = Box::new([min_y; 256]);
        let mut no_leaves = Box::new([min_y; 256]);
        let motion_mask = kind_mask(HeightmapKind::MotionBlocking);
        let no_leaves_mask = kind_mask(HeightmapKind::MotionBlockingNoLeaves);
        for x in 0..16 {
            for z in 0..16 {
                let column = (z << 4 | x) as usize;
                let mut motion_open = true;
                let mut no_leaves_open = true;
                let mut y = surface[column] - 1;
                while y >= min_y && (motion_open || no_leaves_open) {
                    let state_flags = flags[self.get(x, y, z).id() as usize];
                    if motion_open && state_flags & motion_mask != 0 {
                        motion[column] = y + 1;
                        motion_open = false;
                    }
                    if no_leaves_open && state_flags & no_leaves_mask != 0 {
                        no_leaves[column] = y + 1;
                        no_leaves_open = false;
                    }
                    y -= 1;
                }
            }
        }
        self.heightmaps[HeightmapKind::OceanFloor.index()] = Some(floor);
        self.heightmaps[HeightmapKind::WorldSurface.index()] = Some(surface);
        self.heightmaps[HeightmapKind::MotionBlocking.index()] = Some(motion);
        self.heightmaps[HeightmapKind::MotionBlockingNoLeaves.index()] = Some(no_leaves);
        true
    }

    /// Runs the part of the post-processing a chunk gets as it becomes
    /// live that stays inside the chunk: water sources marked for it grow
    /// bubble columns above magma and soul sand. The marks stay for the
    /// live chunk, which runs the rest (the first fluid tick and shape
    /// updates, which read neighbours).
    pub fn post_process(&mut self) {
        let mut positions = self.post_processing.clone();
        positions.sort_by_key(|&(_, y, _)| y >> 4);
        for (x, y, z) in positions {
            let (x, z) = (x & 15, z & 15);
            let state = self.get(x, y, z);
            if !is_full_water_source(state) {
                continue;
            }
            let below = self.get(x, y - 1, z);
            let column = match below.block() {
                Block::BubbleColumn => below,
                block
                    if tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::ENABLES_BUBBLE_COLUMN_PUSH_UP,
                        block,
                    ) =>
                {
                    Block::BubbleColumn
                        .default_state()
                        .with("drag", "false")
                        .unwrap()
                }
                block
                    if tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::ENABLES_BUBBLE_COLUMN_DRAG_DOWN,
                        block,
                    ) =>
                {
                    Block::BubbleColumn
                        .default_state()
                        .with("drag", "true")
                        .unwrap()
                }
                _ => state,
            };
            let mut y = y;
            while self.shape.contains_y(y) {
                let here = self.get(x, y, z);
                if here.block() != Block::BubbleColumn && !is_full_water_source(here) {
                    break;
                }
                self.set(x, y, z, column);
                y += 1;
            }
        }
    }

    /// A live chunk as a decorated proto chunk, for features grown in the
    /// world.
    pub fn from_chunk(pos: ChunkPos, chunk: &Chunk) -> Self {
        let shape = chunk.shape();
        let mut proto = Self::new(pos, shape);
        proto.sections = chunk.sections().cloned().collect();
        for (index, section) in chunk.sections().enumerate() {
            for i in 0..BIOME_VOLUME {
                proto.biomes[index * BIOME_VOLUME + i] = section.biome(i);
            }
        }
        proto.heightmaps = Default::default();
        proto.worldgen_maps_exact = false;
        proto.finish_terrain();
        proto
    }

    pub fn into_chunk(self) -> Chunk {
        let sections = self
            .sections
            .into_iter()
            .zip(self.biomes.as_chunks::<BIOME_VOLUME>().0)
            .map(|(mut section, biomes)| {
                let mut grid = Box::new([0u16; BIOME_VOLUME]);
                grid.copy_from_slice(biomes.as_slice());
                section.set_biomes(grid);
                section
            })
            .collect();
        Chunk::from_sections(self.shape, sections, None)
    }
}

fn is_full_water_source(state: BlockState) -> bool {
    state.block() == Block::Water && state.get("level") == Some("0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn section_emptiness_follows_every_write_path() {
        let shape = ChunkShape::OVERWORLD;
        let pos = ChunkPos::new(0, 0);
        let stone = Block::Stone.default_state();
        let mut chunk = ProtoChunk::new(pos, shape);
        assert_eq!(chunk.highest_filled_section(), None);

        chunk.set_raw(1, 40, 2, stone);
        let section = ((40 - shape.min_y) >> 4) as usize;
        assert_eq!(chunk.highest_filled_section(), Some(section));
        chunk.set(1, 40, 2, Block::CaveAir.default_state());
        assert_eq!(chunk.highest_filled_section(), None);

        chunk.set(3, 40, 3, stone);
        chunk.set(3, 41, 3, stone);
        chunk.set(3, 40, 3, Block::Air.default_state());
        assert_eq!(chunk.highest_filled_section(), Some(section));
        let mut expected = ProtoChunk::new(pos, shape);
        expected.set(3, 41, 3, stone);
        assert_eq!(
            chunk.height_of(HeightmapKind::WorldSurfaceWg, 3, 3),
            expected.height_of(HeightmapKind::WorldSurfaceWg, 3, 3)
        );

        let live = chunk.clone().into_chunk();
        let back = ProtoChunk::from_chunk(pos, &live);
        assert_eq!(back.highest_filled_section(), Some(section));
        assert_eq!(back.get(3, 41, 3), stone);
    }

    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn palette() -> Vec<BlockState> {
        let mut states: Vec<BlockState> = [
            Block::Stone,
            Block::Dirt,
            Block::GrassBlock,
            Block::Water,
            Block::Lava,
            Block::Kelp,
            Block::Seagrass,
            Block::BubbleColumn,
            Block::OakLeaves,
            Block::Glass,
            Block::Air,
            Block::CaveAir,
            Block::VoidAir,
            Block::Bedrock,
            Block::Deepslate,
            Block::Ice,
            Block::OakSlab,
            Block::OakFence,
        ]
        .iter()
        .map(|b| b.default_state())
        .collect();
        states.push(
            Block::OakSlab
                .default_state()
                .with("waterlogged", "true")
                .unwrap(),
        );
        states.push(
            Block::OakFence
                .default_state()
                .with("waterlogged", "true")
                .unwrap(),
        );
        states.push(Block::Water.default_state().with("level", "3").unwrap());
        states
    }

    fn reference_opaque(kind: HeightmapKind, state: BlockState) -> bool {
        kind.is_opaque(state)
    }

    fn reference_update(
        chunk: &ProtoChunk,
        heights: &mut [i32; 256],
        kind: HeightmapKind,
        (x, y, z): (i32, i32, i32),
        state: BlockState,
    ) {
        let column = (z << 4 | x) as usize;
        let first = heights[column];
        if y <= first - 2 {
            return;
        }
        if reference_opaque(kind, state) {
            if y >= first {
                heights[column] = y + 1;
            }
            return;
        }
        if first - 1 == y {
            let mut height = chunk.shape.min_y;
            for by in (chunk.shape.min_y..y).rev() {
                if reference_opaque(kind, chunk.get(x, by, z)) {
                    height = by + 1;
                    break;
                }
            }
            heights[column] = height;
        }
    }

    fn reference_prime(chunk: &ProtoChunk, kind: HeightmapKind) -> [i32; 256] {
        let mut heights = [chunk.shape.min_y; 256];
        let top = match chunk.highest_filled_section() {
            Some(section) => chunk.shape.section_base(section) + 16,
            None => chunk.shape.min_y,
        };
        for x in 0..16 {
            for z in 0..16 {
                let mut y = top - 1;
                while y >= chunk.shape.min_y {
                    let state = chunk.get(x, y, z);
                    if state.block() != Block::Air && reference_opaque(kind, state) {
                        heights[(z << 4 | x) as usize] = y + 1;
                        break;
                    }
                    y -= 1;
                }
            }
        }
        heights
    }

    fn assert_maps_match(chunk: &ProtoChunk, kinds: &[HeightmapKind], context: &str) {
        for kind in kinds {
            assert_eq!(
                chunk.heightmap(*kind).unwrap()[..],
                reference_prime(chunk, *kind)[..],
                "{kind:?} {context}"
            );
        }
    }

    fn random_y(rng: &mut XorShift, shape: ChunkShape, surface: i32) -> i32 {
        match rng.below(8) {
            0 => shape.min_y,
            1 => shape.min_y + shape.height - 1,
            2 | 3 => (surface + rng.below(5) as i32 - 2)
                .clamp(shape.min_y, shape.min_y + shape.height - 1),
            _ => shape.min_y + rng.below(shape.height as u64) as i32,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn kind_masks_agree_with_the_tag_predicates_for_every_state() {
        let tags = tuff_datapack::tags::TagSet::vanilla();
        tuff_datapack::tags::sync_scope(tags.clone(), || {
            let flags = tags.state_flags();
            let kinds = [
                HeightmapKind::WorldSurfaceWg,
                HeightmapKind::WorldSurface,
                HeightmapKind::OceanFloorWg,
                HeightmapKind::OceanFloor,
                HeightmapKind::MotionBlocking,
                HeightmapKind::MotionBlockingNoLeaves,
            ];
            for id in 0..BlockState::COUNT as u32 {
                let state = BlockState::from_id(id).unwrap();
                for kind in kinds {
                    assert_eq!(
                        flags[id as usize] & kind_mask(kind) != 0,
                        kind.is_opaque(state),
                        "{kind:?} {state:?}"
                    );
                }
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn heightmaps_follow_the_reference_through_fill_writes_and_finish() {
        heightmaps_follow_the_reference(tuff_datapack::tags::TagSet::vanilla());
    }

    #[tuff_core::watchdog]
    #[test]
    fn heightmaps_follow_the_reference_when_a_pack_counts_air_for_motion() {
        heightmaps_follow_the_reference(tuff_datapack::tags::vanilla_with(&[
            (
                "minecraft:block",
                "minecraft:blocks_motion_in_heightmap",
                "minecraft:cave_air",
            ),
            (
                "minecraft:block",
                "minecraft:blocks_motion_in_heightmap_no_leaves",
                "minecraft:oak_leaves",
            ),
        ]));
    }

    fn heightmaps_follow_the_reference(tags: Arc<tuff_datapack::tags::TagSet>) {
        let states = palette();
        let shape = ChunkShape::OVERWORLD;
        tuff_datapack::tags::sync_scope(tags.clone(), || {
            let flags = tags.state_flags();
            for seed in 1..=60u64 {
                let mut rng = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
                let mut chunk = ProtoChunk::new(ChunkPos::new(seed as i32 - 30, 3), shape);
                let mut expected = [shape.min_y; 256];
                let mut expected_floor = [shape.min_y; 256];
                for z in 0..16 {
                    for x in 0..16 {
                        let mut column = ColumnFill::default();
                        let surface = shape.min_y + rng.below(shape.height as u64) as i32;
                        let mut y = shape.min_y + shape.height - 1;
                        while y >= shape.min_y {
                            let state = if y > surface && rng.below(12) != 0 {
                                states[10]
                            } else {
                                states[rng.below(states.len() as u64) as usize]
                            };
                            if state != Block::Air.default_state() {
                                chunk.fill_block(&mut column, x, y, z, state, flags);
                                reference_update(
                                    &chunk,
                                    &mut expected,
                                    HeightmapKind::WorldSurfaceWg,
                                    (x, y, z),
                                    state,
                                );
                                reference_update(
                                    &chunk,
                                    &mut expected_floor,
                                    HeightmapKind::OceanFloorWg,
                                    (x, y, z),
                                    state,
                                );
                            }
                            y -= 1;
                        }
                    }
                }
                assert_eq!(
                    chunk.heightmap(HeightmapKind::WorldSurfaceWg).unwrap()[..],
                    expected[..]
                );
                assert_eq!(
                    chunk.heightmap(HeightmapKind::OceanFloorWg).unwrap()[..],
                    expected_floor[..]
                );
                assert_maps_match(&chunk, &HeightmapKind::WORLDGEN, "after fill");

                for step in 0..2500 {
                    let (x, z) = (rng.below(16) as i32, rng.below(16) as i32);
                    let surface = chunk.height_of(HeightmapKind::WorldSurfaceWg, x, z);
                    let y = random_y(&mut rng, shape, surface);
                    let state = states[rng.below(states.len() as u64) as usize];
                    chunk.set(x, y, z, state);
                    if step % 500 == 499 {
                        assert_maps_match(&chunk, &HeightmapKind::WORLDGEN, "terrain writes");
                    }
                }
                assert_maps_match(&chunk, &HeightmapKind::WORLDGEN, "after terrain writes");

                chunk.finish_terrain();
                assert_maps_match(&chunk, &HeightmapKind::FINAL, "derived at finish");

                for step in 0..2500 {
                    let (x, z) = (rng.below(16) as i32, rng.below(16) as i32);
                    let surface = chunk.height_of(HeightmapKind::WorldSurface, x, z);
                    let y = random_y(&mut rng, shape, surface);
                    let state = states[rng.below(states.len() as u64) as usize];
                    chunk.set(x, y, z, state);
                    if step % 500 == 499 {
                        assert_maps_match(&chunk, &HeightmapKind::FINAL, "decoration writes");
                    }
                }
                assert_maps_match(&chunk, &HeightmapKind::FINAL, "after decoration writes");
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn raw_writes_before_finish_fall_back_to_priming() {
        let states = palette();
        let shape = ChunkShape::OVERWORLD;
        let mut rng = XorShift(0xDEAD_BEEF_1234_5679);
        for _ in 0..20 {
            let mut chunk = ProtoChunk::new(ChunkPos::new(0, 0), shape);
            for _ in 0..600 {
                let (x, z) = (rng.below(16) as i32, rng.below(16) as i32);
                let y = shape.min_y + rng.below(shape.height as u64) as i32;
                chunk.set_raw(x, y, z, states[rng.below(states.len() as u64) as usize]);
            }
            chunk.finish_terrain();
            assert_maps_match(&chunk, &HeightmapKind::FINAL, "flat style raw writes");
        }
    }
}

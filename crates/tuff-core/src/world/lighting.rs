//! A region's light: a [`LevelLight`] over copies of the region's chunks and
//! its halo. The region actor owns it and hands it to rayon for each batch
//! of [`LightOp`]s, the way it hands off its region files, so lighting never
//! runs on the actor.
//!
//! Halo chunks are lit here too (their own region stays authoritative for
//! their light), so light crosses region borders: light travels at most 14
//! blocks, and a halo chunk is 16 wide, so nothing beyond the halo can reach
//! the region.

use std::collections::{HashMap, HashSet};

use tuff_vanilla_data::BlockState;

use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::light::{
    ChunkLight, ChunkSource, LevelLight, LightChunk, Pos, SkyLightSources,
    has_different_light_properties,
};
use tuff_world_model::pos::ChunkPos;

pub enum LightOp {
    /// A chunk became available (or was replaced). `owned` chunks belong to
    /// the region and get their light reported; halo chunks only contribute.
    Add {
        pos: ChunkPos,
        chunk: Chunk,
        owned: bool,
    },
    Remove(ChunkPos),
    /// Chunk-local `x`/`z`, absolute `y`.
    SetBlock {
        chunk: ChunkPos,
        x: i32,
        y: i32,
        z: i32,
        state: BlockState,
    },
}

/// New light for an owned chunk. `sky_changed`/`block_changed` mark the
/// light sections (bit 0 = the section below the world) that changed; a
/// chunk lit for the first time has every bit set.
pub struct LightChange {
    pub pos: ChunkPos,
    pub light: ChunkLight,
    pub first: bool,
    pub sky_changed: u64,
    pub block_changed: u64,
}

struct Column {
    blocks: Chunk,
    sources: SkyLightSources,
    owned: bool,
}

struct Chunks(HashMap<ChunkPos, Column>);

impl ChunkSource for Chunks {
    fn chunk(&self, pos: ChunkPos) -> Option<LightChunk<'_>> {
        self.0.get(&pos).map(|column| LightChunk {
            blocks: &column.blocks,
            sky_sources: &column.sources,
        })
    }
}

pub struct Lighting {
    engine: LevelLight,
    chunks: Chunks,
}

impl Lighting {
    pub fn new(shape: ChunkShape, has_sky_light: bool) -> Self {
        Self {
            engine: LevelLight::new(shape, has_sky_light),
            chunks: Chunks(HashMap::new()),
        }
    }

    pub fn shape(&self) -> ChunkShape {
        self.engine.shape()
    }

    /// Sky and block light at a block of a chunk this holds.
    pub fn light_at(&self, x: i32, y: i32, z: i32) -> Option<(u8, u8)> {
        let column = ChunkPos::new(x.div_euclid(16), z.div_euclid(16));
        if !self.chunks.0.contains_key(&column) {
            return None;
        }
        let pos = tuff_world_model::light::Pos { x, y, z };
        Some((
            self.engine
                .light_value(tuff_world_model::light::LightLayer::Sky, pos),
            self.engine
                .light_value(tuff_world_model::light::LightLayer::Block, pos),
        ))
    }

    /// Applies a batch in order, then lights every chunk added by it and
    /// propagates light from already lit chunks into their new neighbours.
    pub fn apply(&mut self, ops: Vec<LightOp>) -> Vec<LightChange> {
        let mut added: Vec<ChunkPos> = Vec::new();
        for op in ops {
            match op {
                LightOp::Add { pos, chunk, owned } => {
                    if self.chunks.0.contains_key(&pos) {
                        self.remove(pos);
                    }
                    let sources = SkyLightSources::from_chunk(&chunk);
                    self.chunks.0.insert(
                        pos,
                        Column {
                            blocks: chunk,
                            sources,
                            owned,
                        },
                    );
                    self.engine.initialize_light(&self.chunks, pos, false);
                    added.retain(|p| *p != pos);
                    added.push(pos);
                }
                LightOp::Remove(pos) => {
                    self.remove(pos);
                    added.retain(|p| *p != pos);
                }
                LightOp::SetBlock {
                    chunk,
                    x,
                    y,
                    z,
                    state,
                } => self.set_block(chunk, x, y, z, state),
            }
        }

        let new: HashSet<ChunkPos> = added.iter().copied().collect();
        for &pos in &added {
            self.engine.propagate_light_sources(&self.chunks, pos);
        }
        let mut relit: Vec<ChunkPos> = added
            .iter()
            .flat_map(|pos| {
                (-1..=1).flat_map(move |dz| {
                    (-1..=1).map(move |dx| ChunkPos::new(pos.x + dx, pos.z + dz))
                })
            })
            .filter(|pos| !new.contains(pos) && self.chunks.0.contains_key(pos))
            .collect();
        relit.sort_by_key(|pos| (pos.x, pos.z));
        relit.dedup();
        for pos in relit {
            self.engine.propagate_light_sources(&self.chunks, pos);
        }
        self.engine.run_light_updates(&self.chunks);

        let shape = self.shape();
        let mut masks: HashMap<ChunkPos, (u64, u64)> = self
            .engine
            .take_changed_sections()
            .into_iter()
            .map(|(column, mask)| (column, (mask, mask)))
            .collect();
        let all = (1u64 << shape.light_sections()) - 1;
        for &pos in &added {
            masks.insert(pos, (all, all));
        }
        masks
            .into_iter()
            .filter(|(pos, _)| self.chunks.0.get(pos).is_some_and(|c| c.owned))
            .map(|(pos, (sky_changed, block_changed))| LightChange {
                pos,
                light: self.engine.chunk_light(pos),
                first: new.contains(&pos),
                sky_changed,
                block_changed,
            })
            .collect()
    }

    fn remove(&mut self, pos: ChunkPos) {
        if self.chunks.0.remove(&pos).is_some() {
            self.engine.remove_chunk(&self.chunks, pos);
        }
    }

    fn set_block(&mut self, chunk: ChunkPos, x: i32, y: i32, z: i32, state: BlockState) {
        let Some(column) = self.chunks.0.get_mut(&chunk) else {
            return;
        };
        let shape = column.blocks.shape();
        if !shape.contains_y(y) {
            return;
        }
        let index = ((y - shape.min_y) >> 4) as usize;
        let only_air = |column: &Column| {
            column
                .blocks
                .sections()
                .nth(index)
                .is_some_and(|section| section.has_only_air())
        };
        let was_air = only_air(column);
        let Some(old) = column.blocks.set(x, y, z, state) else {
            return;
        };
        let now_air = only_air(column);
        let pos = Pos::new(chunk.x * 16 + x, y, chunk.z * 16 + z);
        if was_air != now_air {
            self.engine.update_section_status(pos.section(), now_air);
        }
        if has_different_light_properties(old, state) {
            column.sources.update(&column.blocks, x, y, z);
            self.engine.check_block(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Block;

    use super::*;
    use tuff_world_model::light::min_light_section;
    use tuff_world_model::light::{LightLayer, SectionLight};

    const SHAPE: ChunkShape = ChunkShape::OVERWORLD;
    const BELOW_WORLD: usize = 0;

    fn lighting() -> Lighting {
        Lighting::new(SHAPE, true)
    }

    fn section_index(y: i32) -> usize {
        ((y >> 4) - min_light_section(SHAPE)) as usize
    }

    fn ground() -> Chunk {
        let mut chunk = Chunk::empty(SHAPE);
        for y in SHAPE.min_y..64 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set(x, y, z, Block::Stone.default_state());
                }
            }
        }
        chunk
    }

    fn add(pos: ChunkPos, chunk: Chunk, owned: bool) -> LightOp {
        LightOp::Add { pos, chunk, owned }
    }

    fn block_light(lighting: &Lighting, x: i32, y: i32, z: i32) -> u8 {
        lighting
            .engine
            .light_value(LightLayer::Block, Pos::new(x, y, z))
    }

    fn glowing() -> Chunk {
        let mut chunk = ground();
        chunk.set(15, 64, 8, Block::Glowstone.default_state());
        chunk
    }

    #[tuff_core::watchdog]
    #[test]
    fn light_reaches_neighbours_that_arrive_later() {
        let (a, b) = (ChunkPos::new(0, 0), ChunkPos::new(1, 0));
        let mut together = lighting();
        together.apply(vec![add(a, glowing(), true), add(b, ground(), true)]);

        let mut apart = lighting();
        apart.apply(vec![add(a, glowing(), true)]);
        assert_eq!(block_light(&apart, 16, 64, 8), 0);
        let changes = apart.apply(vec![add(b, ground(), true)]);
        assert_eq!(block_light(&apart, 16, 64, 8), 14);
        assert!(changes.iter().any(|c| c.pos == b && c.first));

        for x in 0..32 {
            for y in 60..80 {
                assert_eq!(
                    block_light(&together, x, y, 8),
                    block_light(&apart, x, y, 8),
                    "{x} {y}"
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn halo_chunks_contribute_but_are_not_reported() {
        let (own, halo) = (ChunkPos::new(1, 0), ChunkPos::new(0, 0));
        let mut lighting = lighting();
        let changes = lighting.apply(vec![add(own, ground(), true), add(halo, glowing(), false)]);
        assert_eq!(block_light(&lighting, 16, 64, 8), 14);
        assert!(changes.iter().all(|c| c.pos == own));
        let SectionLight::Data(_) = &changes[0].light.block[section_index(64)] else {
            panic!("no block light reported for the owned chunk");
        };
    }

    #[tuff_core::watchdog]
    #[test]
    fn edits_report_the_sections_they_change() {
        let pos = ChunkPos::new(0, 0);
        let mut lighting = lighting();
        lighting.apply(vec![add(pos, ground(), true)]);
        let changes = lighting.apply(vec![LightOp::SetBlock {
            chunk: pos,
            x: 8,
            y: 100,
            z: 8,
            state: Block::Glowstone.default_state(),
        }]);
        let [change] = changes.as_slice() else {
            panic!("expected one change, got {}", changes.len());
        };
        assert!(!change.first);
        let section = 1 << section_index(100);
        assert_ne!(change.block_changed & section, 0);
        assert_eq!(change.block_changed & (1 << BELOW_WORLD), 0);
        assert_eq!(block_light(&lighting, 8, 101, 8), 14);

        lighting.apply(vec![LightOp::Remove(pos)]);
        assert!(lighting.apply(Vec::new()).is_empty());
    }
}

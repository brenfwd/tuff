//! What a bot knows of the ground: for each block column it holds, the
//! height its feet would rest at and whether liquid covers that floor.
//! Built from chunk packets and kept roughly current from block updates,
//! which is all walking and digging goals need.

use std::collections::HashMap;

use anyhow::{Result, bail, ensure};
use tuff_vanilla_data::BlockState;
use tuff_wire::PacketReader;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const SECTION_BLOCKS: usize = 4096;
const SECTION_BIOMES: usize = 64;
const MAX_INDIRECT_BLOCK_BITS: u8 = 8;
const MAX_INDIRECT_BIOME_BITS: u8 = 3;
const MAX_PALETTE: i32 = 4096;
const MAX_HEIGHTMAPS: i32 = 16;
const MAX_HEIGHTMAP_LONGS: i32 = 1024;
/// How far above a floor a removed non-solid block may sit and still be
/// taken for the floor block itself.
const FLOOR_SLACK: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    /// Where feet rest on the highest block with a collision shape;
    /// `f32::MIN` over the void.
    pub floor: f32,
    /// Liquid lies on that floor.
    pub liquid: bool,
}

impl Column {
    /// Standing here keeps feet on a full block's top, out of liquid.
    pub fn is_flat_ground(self) -> bool {
        !self.liquid && self.floor > f32::MIN && self.floor.fract() == 0.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct Terrain {
    min_y: i32,
    chunks: HashMap<ChunkPos, Box<[Column; 256]>>,
}

impl Terrain {
    pub fn new(min_y: i32) -> Self {
        Self {
            min_y,
            chunks: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn column(&self, x: i32, z: i32) -> Option<Column> {
        let chunk = self.chunks.get(&ChunkPos::new(x >> 4, z >> 4))?;
        Some(chunk[column_index(x, z)])
    }

    /// The column under a horizontal position.
    pub fn column_at(&self, x: f64, z: f64) -> Option<Column> {
        self.column(x.floor() as i32, z.floor() as i32)
    }

    pub fn forget(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    /// Reads a Chunk Data body positioned just after the chunk X and Z.
    pub fn read_chunk(&mut self, pos: ChunkPos, r: &mut PacketReader) -> Result<()> {
        let heightmaps = r.read_var_int()?;
        ensure!((0..=MAX_HEIGHTMAPS).contains(&heightmaps), "bad heightmaps");
        for _ in 0..heightmaps {
            r.read_var_int()?;
            let longs = r.read_var_int()?;
            ensure!((0..=MAX_HEIGHTMAP_LONGS).contains(&longs), "bad heightmap");
            for _ in 0..longs {
                r.read_long()?;
            }
        }
        let size = r.read_var_int()?;
        ensure!(
            size >= 0 && size as usize <= r.remaining(),
            "bad section data"
        );
        let mut data = PacketReader::new(r.read_bytes(size as usize)?);
        let mut sections = Vec::new();
        while data.remaining() > 0 {
            sections.push(read_section(&mut data)?);
        }
        let mut columns = Box::new(
            [Column {
                floor: f32::MIN,
                liquid: false,
            }; 256],
        );
        for (index, column) in columns.iter_mut().enumerate() {
            *column = scan(&sections, self.min_y, index);
        }
        self.chunks.insert(pos, columns);
        Ok(())
    }

    /// Applies one block change to the column it is in.
    pub fn set_block(&mut self, pos: BlockPos, state: u32) {
        let Some(chunk) = self.chunks.get_mut(&ChunkPos::new(pos.x >> 4, pos.z >> 4)) else {
            return;
        };
        let Some(state) = BlockState::from_id(state) else {
            return;
        };
        let column = &mut chunk[column_index(pos.x, pos.z)];
        let y = pos.y as f32;
        match top_of(state) {
            Some(top) if y + top >= column.floor => {
                column.floor = y + top;
                column.liquid = false;
            }
            Some(_) => {}
            None if y < column.floor && y >= column.floor - FLOOR_SLACK => {
                column.floor = y;
                column.liquid = is_liquid(state);
            }
            None => column.liquid |= is_liquid(state) && y >= column.floor,
        }
    }

    /// Reads a Section Blocks Update body.
    pub fn read_section_update(&mut self, r: &mut PacketReader) -> Result<()> {
        let packed = r.read_long()?;
        let x = (packed >> 42) as i32;
        let z = ((packed << 22) >> 42) as i32;
        let y = ((packed << 44) >> 44) as i32;
        let count = r.read_var_int()?;
        ensure!(
            (0..=SECTION_BLOCKS as i32).contains(&count),
            "bad block count"
        );
        for _ in 0..count {
            let entry = r.read_var_long()?;
            let local = (entry & 0xFFF) as i32;
            let pos = BlockPos::new(
                x * 16 + (local >> 8),
                y * 16 + (local & 15),
                z * 16 + ((local >> 4) & 15),
            );
            self.set_block(pos, (entry >> 12) as u32);
        }
        Ok(())
    }
}

fn column_index(x: i32, z: i32) -> usize {
    ((z & 15) * 16 + (x & 15)) as usize
}

fn is_liquid(state: BlockState) -> bool {
    !state.fluid_state().is_empty()
}

/// How far above its own base a block's collision reaches, if it has one.
fn top_of(state: BlockState) -> Option<f32> {
    state
        .collision_boxes()
        .iter()
        .map(|b| b[4])
        .reduce(f32::max)
}

enum Palette {
    Single(u32),
    Indirect(Vec<u32>),
    Direct,
}

struct Packed {
    palette: Palette,
    bits: u32,
    longs: Vec<u64>,
}

impl Packed {
    fn get(&self, index: usize) -> u32 {
        if let Palette::Single(value) = self.palette {
            return value;
        }
        let per_long = (64 / self.bits) as usize;
        let word = self.longs[index / per_long];
        let shift = (index % per_long) as u32 * self.bits;
        let value = ((word >> shift) & ((1 << self.bits) - 1)) as u32;
        match &self.palette {
            Palette::Indirect(entries) => entries.get(value as usize).copied().unwrap_or(0),
            _ => value,
        }
    }
}

fn read_section(r: &mut PacketReader) -> Result<Packed> {
    r.read_short()?;
    r.read_short()?;
    let blocks = read_packed(r, SECTION_BLOCKS, MAX_INDIRECT_BLOCK_BITS)?;
    read_packed(r, SECTION_BIOMES, MAX_INDIRECT_BIOME_BITS)?;
    Ok(blocks)
}

fn read_packed(r: &mut PacketReader, entries: usize, max_indirect: u8) -> Result<Packed> {
    let bits = r.read_unsigned_byte()?;
    if bits == 0 {
        return Ok(Packed {
            palette: Palette::Single(r.read_var_int()? as u32),
            bits: 0,
            longs: Vec::new(),
        });
    }
    if bits > 32 {
        bail!("bad palette bits {bits}");
    }
    let palette = if bits <= max_indirect {
        let len = r.read_var_int()?;
        ensure!((1..=MAX_PALETTE).contains(&len), "bad palette length");
        let mut entries = Vec::with_capacity(len as usize);
        for _ in 0..len {
            entries.push(r.read_var_int()? as u32);
        }
        Palette::Indirect(entries)
    } else {
        Palette::Direct
    };
    let per_long = 64 / u32::from(bits);
    let count = entries.div_ceil(per_long as usize);
    ensure!(count * 8 <= r.remaining(), "truncated section");
    let mut longs = Vec::with_capacity(count);
    for _ in 0..count {
        longs.push(r.read_long()? as u64);
    }
    Ok(Packed {
        palette,
        bits: u32::from(bits),
        longs,
    })
}

fn scan(sections: &[Packed], min_y: i32, column: usize) -> Column {
    let mut liquid = false;
    for (section_index, section) in sections.iter().enumerate().rev() {
        let base = min_y + section_index as i32 * 16;
        for local_y in (0..16).rev() {
            let id = section.get(local_y * 256 + column);
            let Some(state) = BlockState::from_id(id) else {
                continue;
            };
            if let Some(top) = top_of(state) {
                return Column {
                    floor: (base + local_y as i32) as f32 + top,
                    liquid,
                };
            }
            liquid |= is_liquid(state);
        }
    }
    Column {
        floor: f32::MIN,
        liquid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;
    use tuff_world_model::chunk::{Chunk, ChunkShape};

    fn parsed(chunk: &Chunk) -> Terrain {
        let body = chunk.encode(2, -1, &[]);
        let mut r = PacketReader::new(body);
        let pos = ChunkPos::new(r.read_int().unwrap(), r.read_int().unwrap());
        let mut terrain = Terrain::new(chunk.shape().min_y);
        terrain.read_chunk(pos, &mut r).unwrap();
        assert!(r.remaining() > 0, "block entities and light follow");
        terrain
    }

    #[tuff_core::watchdog]
    #[test]
    fn floors_follow_the_highest_colliding_block() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, 63, z, Block::Stone.default_state());
            }
        }
        chunk.set(1, 64, 0, Block::ShortGrass.default_state());
        chunk.set(2, 64, 0, Block::Water.default_state());
        chunk.set(3, 64, 0, Block::OakSlab.default_state());
        chunk.set(4, 100, 0, Block::OakLeaves.default_state());
        chunk.set(5, 63, 0, Block::Air.default_state());
        for y in -64..63 {
            chunk.set(5, y, 0, Block::Air.default_state());
        }
        let terrain = parsed(&chunk);
        let at = |x: i32| terrain.column(32 + x, -16).unwrap();
        assert_eq!(at(0).floor, 64.0);
        assert!(at(0).is_flat_ground());
        assert_eq!(at(1).floor, 64.0);
        assert!(at(2).liquid && !at(2).is_flat_ground());
        assert_eq!(at(3).floor, 64.5);
        assert_eq!(at(4).floor, 101.0);
        assert_eq!(at(5).floor, f32::MIN);
        assert!(terrain.column(0, 0).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn mixed_sections_decode_every_palette_width() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        let blocks: Vec<BlockState> = tuff_vanilla_data::Registry::ALL
            .iter()
            .map(|block: &Block| block.default_state())
            .filter(|state| top_of(*state) == Some(1.0))
            .take(300)
            .collect();
        for (i, state) in blocks.iter().enumerate() {
            let (x, z) = ((i % 16) as i32, (i / 16 % 16) as i32);
            chunk.set(x, 10 + (i / 256) as i32, z, *state);
        }
        let terrain = parsed(&chunk);
        assert_eq!(terrain.column(32, -16).unwrap().floor, 12.0);
        assert_eq!(terrain.column(32 + 15, -16 + 15).unwrap().floor, 11.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_updates_raise_and_lower_floors() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(0, 63, 0, Block::Dirt.default_state());
        let mut terrain = parsed(&chunk);
        let stone = Block::Stone.default_state().id();
        terrain.set_block(BlockPos::new(32, 64, -16), stone);
        assert_eq!(terrain.column(32, -16).unwrap().floor, 65.0);
        terrain.set_block(BlockPos::new(32, 64, -16), 0);
        assert_eq!(terrain.column(32, -16).unwrap().floor, 64.0);
        let water = Block::Water.default_state().id();
        terrain.set_block(BlockPos::new(32, 63, -16), water);
        assert!(terrain.column(32, -16).unwrap().liquid);
    }

    #[tuff_core::watchdog]
    #[test]
    fn section_updates_land_in_their_columns() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(3, 63, 4, Block::Dirt.default_state());
        let mut terrain = parsed(&chunk);
        let mut w = tuff_wire::PacketWriter::new();
        let (x, y, z) = (2i64, 4i64, -1i64);
        w.write_long(((x & 0x3F_FFFF) << 42) | ((z & 0x3F_FFFF) << 20) | (y & 0xF_FFFF));
        w.write_var_int(1);
        let state = i64::from(Block::Stone.default_state().id());
        w.write_var_long((state << 12) | (3 << 8) | (4 << 4));
        let mut r = PacketReader::new(w.into_bytes());
        terrain.read_section_update(&mut r).unwrap();
        assert_eq!(terrain.column(35, -12).unwrap().floor, 65.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_chunks_fail_cleanly() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(0, 0, 0, Block::Stone.default_state());
        let body = chunk.encode(0, 0, &[]);
        for cut in [9, 20, body.len() / 2] {
            let mut r = PacketReader::new(body.slice(8..cut));
            let mut terrain = Terrain::new(-64);
            assert!(terrain.read_chunk(ChunkPos::new(0, 0), &mut r).is_err());
        }
    }
}

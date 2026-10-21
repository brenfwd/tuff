//! Single hand-built pieces set on the ground: desert pyramids, jungle
//! temples and swamp huts. Each settles on the terrain the first time a
//! chunk it reaches is decorated, and keeps that height.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::json::{Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, data_value_2d};
use crate::structure::piece::{Frame, Framed, structure_loot};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::{LegacyRandom, RandomSource};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    DesertPyramid,
    JungleTemple,
    SwampHut,
}

impl Shape {
    /// Width, height and depth.
    fn size(self) -> [i32; 3] {
        match self {
            Shape::DesertPyramid => [21, 15, 21],
            Shape::JungleTemple => [12, 10, 15],
            Shape::SwampHut => [7, 7, 9],
        }
    }
}

struct Scattered {
    shape: Shape,
}

pub fn desert_pyramid(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "desert_pyramid")?;
    Ok(Arc::new(Scattered {
        shape: Shape::DesertPyramid,
    }))
}

pub fn jungle_temple(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "jungle_temple")?;
    Ok(Arc::new(Scattered {
        shape: Shape::JungleTemple,
    }))
}

pub fn swamp_hut(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "swamp_hut")?;
    Ok(Arc::new(Scattered {
        shape: Shape::SwampHut,
    }))
}

impl StructureKind for Scattered {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        let (min_x, min_z) = (chunk.x << 4, chunk.z << 4);
        let position = match self.shape {
            Shape::SwampHut => ctx.on_top_of_chunk_center(HeightmapKind::WorldSurfaceWg)?,
            _ => {
                let (x, z) = (min_x + 8, min_z + 8);
                if !ctx.could_exist_in_terrain_column(x, z) {
                    return None;
                }
                let [width, _, depth] = self.shape.size();
                if ctx.lowest_y(min_x, min_z, width, depth) < ctx.sea_level() {
                    return None;
                }
                BlockPos::new(
                    x,
                    ctx.first_occupied_height(x, z, HeightmapKind::WorldSurfaceWg),
                    z,
                )
            }
        };
        let shape = self.shape;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let direction = Facing::HORIZONTAL[ctx.random.next_int_bounded(4) as usize];
                let [width, height, depth] = shape.size();
                let b = match direction {
                    Facing::North | Facing::South => BoundingBox::new(
                        min_x,
                        64,
                        min_z,
                        min_x + width - 1,
                        64 + height - 1,
                        min_z + depth - 1,
                    ),
                    _ => BoundingBox::new(
                        min_x,
                        64,
                        min_z,
                        min_x + depth - 1,
                        64 + height - 1,
                        min_z + width - 1,
                    ),
                };
                vec![Box::new(ScatteredPiece {
                    shape,
                    orientation: direction,
                    placement: Mutex::new((-1, b)),
                    flags: Default::default(),
                }) as Box<dyn Piece>]
            }),
        })
    }
}

/// A scattered piece: where it settled (`-1` until it did) and its box.
struct ScatteredPiece {
    shape: Shape,
    orientation: Facing,
    placement: Mutex<(i32, BoundingBox)>,
    /// Chests and traps placed so far: the four desert pyramid chests; or
    /// the jungle temple's main chest, hidden chest and two traps; or the
    /// swamp hut's witch and cat.
    flags: [AtomicBool; 4],
}

/// The piece as it is placed in one chunk.
struct Placed {
    frame: Frame,
}

impl Framed for Placed {
    fn frame(&self) -> &Frame {
        &self.frame
    }
}

type Corner = (i32, i32, i32);

fn state(block: Block, properties: &[(&str, &str)]) -> BlockState {
    properties
        .iter()
        .fold(block.default_state(), |s, (k, v)| s.with(k, v).unwrap_or(s))
}

fn stairs(block: Block, facing: &str) -> BlockState {
    state(block, &[("facing", facing)])
}

impl Placed {
    fn set(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        state: BlockState,
        x: i32,
        y: i32,
        z: i32,
    ) {
        self.place_block(level, state, x, y, z, chunk);
    }

    fn fill(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        from: Corner,
        to: Corner,
        state: BlockState,
    ) {
        self.generate_box(level, chunk, from, to, state, state, false);
    }

    fn shell(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        from: Corner,
        to: Corner,
        edge: BlockState,
        fill: BlockState,
    ) {
        self.generate_box(level, chunk, from, to, edge, fill, false);
    }
}

impl ScatteredPiece {
    fn flag(&self, index: usize) -> bool {
        self.flags[index].load(Ordering::Relaxed)
    }

    fn set_flag(&self, index: usize, value: bool) {
        self.flags[index].store(value, Ordering::Relaxed);
    }

    /// Settles the piece on the average motion-blocking height of its
    /// columns inside the chunk, the first time.
    fn settle_average(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        offset: i32,
    ) -> Option<Frame> {
        let mut placement = self.placement.lock().unwrap();
        let (hpos, b) = &mut *placement;
        if *hpos < 0 {
            let (mut total, mut count) = (0, 0);
            for z in b.min_z..=b.max_z {
                for x in b.min_x..=b.max_x {
                    if chunk.contains(BlockPos::new(x, 64, z)) {
                        total += level.height(HeightmapKind::MotionBlockingNoLeaves, x, z);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                return None;
            }
            *hpos = total / count;
            *b = b.moved(0, *hpos - b.min_y + offset, 0);
        }
        Some(Frame::new(0, *b, Some(self.orientation)))
    }

    /// Settles the piece on the lowest motion-blocking height of all its
    /// columns, the first time.
    fn settle_lowest(&self, level: &mut Region, offset: i32) -> Frame {
        let mut placement = self.placement.lock().unwrap();
        let (hpos, b) = &mut *placement;
        if *hpos < 0 {
            let mut lowest = level.max_y() + 1;
            for z in b.min_z..=b.max_z {
                for x in b.min_x..=b.max_x {
                    lowest = lowest.min(level.height(HeightmapKind::MotionBlockingNoLeaves, x, z));
                }
            }
            *hpos = lowest;
            *b = b.moved(0, *hpos - b.min_y + offset, 0);
        }
        Frame::new(0, *b, Some(self.orientation))
    }

    fn desert_pyramid(&self, ctx: &mut PieceContext, chunk: &BoundingBox) {
        let offset = -ctx.random.next_int_bounded(3);
        let p = Placed {
            frame: self.settle_lowest(ctx.level, offset),
        };
        let level = &mut *ctx.level;
        let [width, _, depth] = self.shape.size();
        let sandstone = Block::Sandstone.default_state();
        let cut = Block::CutSandstone.default_state();
        let chiseled = Block::ChiseledSandstone.default_state();
        let orange = Block::OrangeTerracotta.default_state();
        let blue = Block::BlueTerracotta.default_state();
        let air = Block::Air.default_state();
        p.fill(
            level,
            chunk,
            (0, -4, 0),
            (width - 1, 0, depth - 1),
            sandstone,
        );
        for i in 1..=9 {
            p.fill(
                level,
                chunk,
                (i, i, i),
                (width - 1 - i, i, depth - 1 - i),
                sandstone,
            );
            p.fill(
                level,
                chunk,
                (i + 1, i, i + 1),
                (width - 2 - i, i, depth - 2 - i),
                air,
            );
        }
        for x in 0..width {
            for z in 0..depth {
                p.fill_column_down(level, sandstone, x, -5, z, chunk);
            }
        }
        let north = stairs(Block::SandstoneStairs, "north");
        let south = stairs(Block::SandstoneStairs, "south");
        let east = stairs(Block::SandstoneStairs, "east");
        let west = stairs(Block::SandstoneStairs, "west");
        p.shell(level, chunk, (0, 0, 0), (4, 9, 4), sandstone, air);
        p.fill(level, chunk, (1, 10, 1), (3, 10, 3), sandstone);
        p.set(level, chunk, north, 2, 10, 0);
        p.set(level, chunk, south, 2, 10, 4);
        p.set(level, chunk, east, 0, 10, 2);
        p.set(level, chunk, west, 4, 10, 2);
        p.shell(
            level,
            chunk,
            (width - 5, 0, 0),
            (width - 1, 9, 4),
            sandstone,
            air,
        );
        p.fill(
            level,
            chunk,
            (width - 4, 10, 1),
            (width - 2, 10, 3),
            sandstone,
        );
        p.set(level, chunk, north, width - 3, 10, 0);
        p.set(level, chunk, south, width - 3, 10, 4);
        p.set(level, chunk, east, width - 5, 10, 2);
        p.set(level, chunk, west, width - 1, 10, 2);
        p.shell(level, chunk, (8, 0, 0), (12, 4, 4), sandstone, air);
        p.fill(level, chunk, (9, 1, 0), (11, 3, 4), air);
        for (x, y) in [(9, 1), (9, 2), (9, 3), (10, 3), (11, 3), (11, 2), (11, 1)] {
            p.set(level, chunk, cut, x, y, 1);
        }
        p.shell(level, chunk, (4, 1, 1), (8, 3, 3), sandstone, air);
        p.fill(level, chunk, (4, 1, 2), (8, 2, 2), air);
        p.shell(level, chunk, (12, 1, 1), (16, 3, 3), sandstone, air);
        p.fill(level, chunk, (12, 1, 2), (16, 2, 2), air);
        p.fill(
            level,
            chunk,
            (5, 4, 5),
            (width - 6, 4, depth - 6),
            sandstone,
        );
        p.fill(level, chunk, (9, 4, 9), (11, 4, 11), air);
        for (x, z) in [(8, 8), (12, 8), (8, 12), (12, 12)] {
            p.fill(level, chunk, (x, 1, z), (x, 3, z), cut);
        }
        p.fill(level, chunk, (1, 1, 5), (4, 4, 11), sandstone);
        p.fill(
            level,
            chunk,
            (width - 5, 1, 5),
            (width - 2, 4, 11),
            sandstone,
        );
        p.fill(level, chunk, (6, 7, 9), (6, 7, 11), sandstone);
        p.fill(
            level,
            chunk,
            (width - 7, 7, 9),
            (width - 7, 7, 11),
            sandstone,
        );
        p.fill(level, chunk, (5, 5, 9), (5, 7, 11), cut);
        p.fill(level, chunk, (width - 6, 5, 9), (width - 6, 7, 11), cut);
        for (x, y) in [
            (5, 5),
            (5, 6),
            (6, 6),
            (width - 6, 5),
            (width - 6, 6),
            (width - 7, 6),
        ] {
            p.set(level, chunk, air, x, y, 10);
        }
        p.fill(level, chunk, (2, 4, 4), (2, 6, 4), air);
        p.fill(level, chunk, (width - 3, 4, 4), (width - 3, 6, 4), air);
        p.set(level, chunk, north, 2, 4, 5);
        p.set(level, chunk, north, 2, 3, 4);
        p.set(level, chunk, north, width - 3, 4, 5);
        p.set(level, chunk, north, width - 3, 3, 4);
        p.fill(level, chunk, (1, 1, 3), (2, 2, 3), sandstone);
        p.fill(
            level,
            chunk,
            (width - 3, 1, 3),
            (width - 2, 2, 3),
            sandstone,
        );
        p.set(level, chunk, sandstone, 1, 1, 2);
        p.set(level, chunk, sandstone, width - 2, 1, 2);
        let slab = Block::SandstoneSlab.default_state();
        p.set(level, chunk, slab, 1, 2, 2);
        p.set(level, chunk, slab, width - 2, 2, 2);
        p.set(level, chunk, west, 2, 1, 2);
        p.set(level, chunk, east, width - 3, 1, 2);
        p.fill(level, chunk, (4, 3, 5), (4, 3, 17), sandstone);
        p.fill(
            level,
            chunk,
            (width - 5, 3, 5),
            (width - 5, 3, 17),
            sandstone,
        );
        p.fill(level, chunk, (3, 1, 5), (4, 2, 16), air);
        p.fill(level, chunk, (width - 6, 1, 5), (width - 5, 2, 16), air);
        for z in (5..=17).step_by(2) {
            p.set(level, chunk, cut, 4, 1, z);
            p.set(level, chunk, chiseled, 4, 2, z);
            p.set(level, chunk, cut, width - 5, 1, z);
            p.set(level, chunk, chiseled, width - 5, 2, z);
        }
        for (x, z) in [
            (10, 7),
            (10, 8),
            (9, 9),
            (11, 9),
            (8, 10),
            (12, 10),
            (7, 10),
            (13, 10),
            (9, 11),
            (11, 11),
            (10, 12),
            (10, 13),
        ] {
            p.set(level, chunk, orange, x, 0, z);
        }
        p.set(level, chunk, blue, 10, 0, 10);
        let column = [
            [cut, orange, cut],
            [cut, orange, cut],
            [orange, chiseled, orange],
            [cut, orange, cut],
            [orange, chiseled, orange],
            [orange, orange, orange],
            [cut, cut, cut],
        ];
        let mut x = 0;
        while x < width {
            for (i, row) in column.iter().enumerate() {
                for (j, state) in row.iter().enumerate() {
                    p.set(level, chunk, *state, x, 2 + i as i32, 1 + j as i32);
                }
            }
            x += width - 1;
        }
        let mut x = 2;
        while x <= width - 3 {
            for (i, row) in column.iter().enumerate() {
                for (j, state) in row.iter().enumerate() {
                    p.set(level, chunk, *state, x - 1 + j as i32, 2 + i as i32, 0);
                }
            }
            x += width - 3 - 2;
        }
        p.fill(level, chunk, (8, 4, 0), (12, 6, 0), cut);
        p.set(level, chunk, air, 8, 6, 0);
        p.set(level, chunk, air, 12, 6, 0);
        p.set(level, chunk, orange, 9, 5, 0);
        p.set(level, chunk, chiseled, 10, 5, 0);
        p.set(level, chunk, orange, 11, 5, 0);
        p.fill(level, chunk, (8, -14, 8), (12, -11, 12), cut);
        p.fill(level, chunk, (8, -10, 8), (12, -10, 12), chiseled);
        p.fill(level, chunk, (8, -9, 8), (12, -9, 12), cut);
        p.fill(level, chunk, (8, -8, 8), (12, -1, 12), sandstone);
        p.fill(level, chunk, (9, -11, 9), (11, -1, 11), air);
        p.set(
            level,
            chunk,
            Block::StonePressurePlate.default_state(),
            10,
            -11,
            10,
        );
        p.shell(
            level,
            chunk,
            (9, -13, 9),
            (11, -13, 11),
            Block::Tnt.default_state(),
            air,
        );
        for (x, z, ox, oz) in [
            (8, 10, 7, 10),
            (12, 10, 13, 10),
            (10, 8, 10, 7),
            (10, 12, 10, 13),
        ] {
            p.set(level, chunk, air, x, -11, z);
            p.set(level, chunk, air, x, -10, z);
            p.set(level, chunk, chiseled, ox, -10, oz);
            p.set(level, chunk, cut, ox, -11, oz);
        }
        let table = structure_loot("desert_pyramid");
        for direction in Facing::HORIZONTAL {
            let index = data_value_2d(direction) as usize;
            if !self.flag(index) {
                let (dx, _, dz) = direction.step();
                let placed = p.create_chest(
                    level,
                    chunk,
                    &mut *ctx.random,
                    10 + dx * 2,
                    -11,
                    10 + dz * 2,
                    table,
                );
                self.set_flag(index, placed);
            }
        }
        self.add_cellar(&p, level, chunk);
    }

    fn add_cellar(&self, p: &Placed, level: &mut Region, chunk: &BoundingBox) {
        let (x, y, z) = (16, -4, 13);
        let stairs = stairs(Block::SandstoneStairs, "west");
        p.set(level, chunk, stairs, 13, -1, 17);
        p.set(level, chunk, stairs, 14, -2, 17);
        p.set(level, chunk, stairs, 15, -3, 17);
        let sand = Block::Sand.default_state();
        let sandstone = Block::Sandstone.default_state();
        let variant = level.random().next_bool();
        for dx in -4..=0 {
            p.set(level, chunk, sand, x + dx, y + 4, z + 4);
        }
        p.set(level, chunk, sand, x - 2, y + 3, z + 4);
        p.set(
            level,
            chunk,
            if variant { sand } else { sandstone },
            x - 1,
            y + 3,
            z + 4,
        );
        p.set(
            level,
            chunk,
            if variant { sandstone } else { sand },
            x,
            y + 3,
            z + 4,
        );
        p.set(level, chunk, sand, x - 1, y + 2, z + 4);
        p.set(level, chunk, sandstone, x, y + 2, z + 4);
        p.set(level, chunk, sand, x, y + 1, z + 4);

        let cut = Block::CutSandstone.default_state();
        let chiseled = Block::ChiseledSandstone.default_state();
        let ring = |p: &Placed, level: &mut Region, y: i32, state: BlockState| {
            for (from, to) in [
                ((x - 3, y, z - 3), (x - 3, y, z + 2)),
                ((x + 3, y, z - 3), (x + 3, y, z + 2)),
                ((x - 3, y, z - 3), (x + 3, y, z - 2)),
                ((x - 3, y, z + 3), (x + 3, y, z + 3)),
            ] {
                p.generate_box(level, chunk, from, to, state, state, true);
            }
        };
        ring(p, level, y + 1, cut);
        ring(p, level, y + 2, chiseled);
        ring(p, level, -1, cut);
        for rx in x - 2..=x + 2 {
            for rz in z - 2..=z + 2 {
                let state = if level.random().next_float() < 0.33 {
                    sandstone
                } else {
                    sand
                };
                p.set(level, chunk, state, rx, y + 4, rz);
            }
        }
        let orange = Block::OrangeTerracotta.default_state();
        p.set(level, chunk, Block::BlueTerracotta.default_state(), x, y, z);
        for (dx, dz) in [
            (1, -1),
            (1, 1),
            (-1, -1),
            (-1, 1),
            (2, 0),
            (-2, 0),
            (0, 2),
            (0, -2),
            (3, 0),
        ] {
            p.set(level, chunk, orange, x + dx, y, z + dz);
        }
        p.set(level, chunk, cut, x + 4, y + 1, z);
        p.set(level, chunk, chiseled, x + 4, y + 2, z);
        p.set(level, chunk, orange, x - 3, y, z);
        p.set(level, chunk, cut, x - 4, y + 1, z);
        p.set(level, chunk, chiseled, x - 4, y + 2, z);
        p.set(level, chunk, orange, x, y, z + 3);
        p.set(level, chunk, orange, x, y, z - 3);
        p.set(level, chunk, cut, x, y + 1, z - 4);
        p.set(level, chunk, chiseled, x, -2, z - 4);
    }

    /// Where the pyramid's cellar holds sand, some of it suspicious, and the
    /// collapsed roof block that is always suspicious.
    fn cellar_sand(&self, frame: &Frame, seed: i64) -> (Vec<BlockPos>, BlockPos) {
        let (x, y, z) = (16, -4, 13);
        let mut sand = Vec::new();
        for sy in y + 1..=y + 3 {
            for sx in x - 2..=x + 2 {
                for sz in z - 2..=z + 2 {
                    sand.push(frame.world_pos(sx, sy, sz));
                }
            }
        }
        for (sx, sz) in [(x + 3, z), (x - 3, z), (x, z + 3), (x, z - 3)] {
            sand.push(frame.world_pos(sx, y + 1, sz));
            sand.push(frame.world_pos(sx, y + 2, sz));
        }
        let (x0, y0, z0, x1, z1) = (x - 2, y + 4, z - 2, x + 2, z + 2);
        let corner = frame.world_pos(x0, y0, z0);
        let mut random = LegacyRandom::new(seed)
            .fork_positional()
            .at(corner.x, corner.y, corner.z);
        let rx = random.next_int_between_inclusive(x0, x1);
        let rz = random.next_int_between_inclusive(z0, z1);
        (sand, frame.world_pos(rx, y0, rz))
    }

    fn place_suspicious_sand(level: &mut Region, chunk: &BoundingBox, pos: BlockPos) {
        if chunk.contains(pos) {
            level.set_block(pos, Block::SuspiciousSand.default_state());
            level.set_block_entity(
                pos,
                Compound::new()
                    .with("id", "minecraft:brushable_block")
                    .with("LootTable", structure_loot("desert_pyramid_archaeology"))
                    .with("LootTableSeed", pos.pack()),
            );
        }
    }

    fn jungle_temple(&self, ctx: &mut PieceContext, chunk: &BoundingBox) {
        let Some(frame) = self.settle_average(ctx.level, chunk, 0) else {
            return;
        };
        let p = Placed { frame };
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        let [width, _, depth] = self.shape.size();
        let cobble = Block::Cobblestone.default_state();
        let mossy = Block::MossyCobblestone.default_state();
        let stone = |p: &Placed,
                     level: &mut Region,
                     random: &mut dyn RandomSource,
                     from: Corner,
                     to: Corner| {
            p.generate_box_with(level, chunk, random, from, to, false, |r, _| {
                if r.next_float() < 0.4 { cobble } else { mossy }
            });
        };
        stone(&p, level, random, (0, -4, 0), (width - 1, 0, depth - 1));
        for (from, to) in [
            ((2, 1, 2), (9, 2, 2)),
            ((2, 1, 12), (9, 2, 12)),
            ((2, 1, 3), (2, 2, 11)),
            ((9, 1, 3), (9, 2, 11)),
            ((1, 3, 1), (10, 6, 1)),
            ((1, 3, 13), (10, 6, 13)),
            ((1, 3, 2), (1, 6, 12)),
            ((10, 3, 2), (10, 6, 12)),
            ((2, 3, 2), (9, 3, 12)),
            ((2, 6, 2), (9, 6, 12)),
            ((3, 7, 3), (8, 7, 11)),
            ((4, 8, 4), (7, 8, 10)),
        ] {
            stone(&p, level, random, from, to);
        }
        for (from, to) in [
            ((3, 1, 3), (8, 2, 11)),
            ((4, 3, 6), (7, 3, 9)),
            ((2, 4, 2), (9, 5, 12)),
            ((4, 6, 5), (7, 6, 9)),
            ((5, 7, 6), (6, 7, 8)),
            ((5, 1, 2), (6, 2, 2)),
            ((5, 2, 12), (6, 2, 12)),
            ((5, 5, 1), (6, 5, 1)),
            ((5, 5, 13), (6, 5, 13)),
        ] {
            p.generate_air_box(level, chunk, from, to);
        }
        let air = Block::Air.default_state();
        for (x, z) in [(1, 5), (10, 5), (1, 9), (10, 9)] {
            p.set(level, chunk, air, x, 5, z);
        }
        for z in [0, 14] {
            for x in [2, 4, 7, 9] {
                stone(&p, level, random, (x, 4, z), (x, 5, z));
            }
        }
        stone(&p, level, random, (5, 6, 0), (6, 6, 0));
        for x in [0, 11] {
            for z in (2..=12).step_by(2) {
                stone(&p, level, random, (x, 4, z), (x, 5, z));
            }
            stone(&p, level, random, (x, 6, 5), (x, 6, 5));
            stone(&p, level, random, (x, 6, 9), (x, 6, 9));
        }
        for (from, to) in [
            ((2, 7, 2), (2, 9, 2)),
            ((9, 7, 2), (9, 9, 2)),
            ((2, 7, 12), (2, 9, 12)),
            ((9, 7, 12), (9, 9, 12)),
            ((4, 9, 4), (4, 9, 4)),
            ((7, 9, 4), (7, 9, 4)),
            ((4, 9, 10), (4, 9, 10)),
            ((7, 9, 10), (7, 9, 10)),
            ((5, 9, 7), (6, 9, 7)),
        ] {
            stone(&p, level, random, from, to);
        }
        let east = stairs(Block::CobblestoneStairs, "east");
        let west = stairs(Block::CobblestoneStairs, "west");
        let south = stairs(Block::CobblestoneStairs, "south");
        let north = stairs(Block::CobblestoneStairs, "north");
        for (state, x, y, z) in [
            (north, 5, 9, 6),
            (north, 6, 9, 6),
            (south, 5, 9, 8),
            (south, 6, 9, 8),
            (north, 4, 0, 0),
            (north, 5, 0, 0),
            (north, 6, 0, 0),
            (north, 7, 0, 0),
            (north, 4, 1, 8),
            (north, 4, 2, 9),
            (north, 4, 3, 10),
            (north, 7, 1, 8),
            (north, 7, 2, 9),
            (north, 7, 3, 10),
        ] {
            p.set(level, chunk, state, x, y, z);
        }
        stone(&p, level, random, (4, 1, 9), (4, 1, 9));
        stone(&p, level, random, (7, 1, 9), (7, 1, 9));
        stone(&p, level, random, (4, 1, 10), (7, 2, 10));
        stone(&p, level, random, (5, 4, 5), (6, 4, 5));
        p.set(level, chunk, east, 4, 4, 5);
        p.set(level, chunk, west, 7, 4, 5);
        for i in 0..4 {
            p.set(level, chunk, south, 5, -i, 6 + i);
            p.set(level, chunk, south, 6, -i, 6 + i);
            p.generate_air_box(level, chunk, (5, -i, 7 + i), (6, -i, 9 + i));
        }
        p.generate_air_box(level, chunk, (1, -3, 12), (10, -1, 13));
        p.generate_air_box(level, chunk, (1, -3, 1), (3, -1, 13));
        p.generate_air_box(level, chunk, (1, -3, 1), (9, -1, 5));
        for z in (1..=13).step_by(2) {
            stone(&p, level, random, (1, -3, z), (1, -2, z));
        }
        for z in (2..=12).step_by(2) {
            stone(&p, level, random, (1, -1, z), (3, -1, z));
        }
        stone(&p, level, random, (2, -2, 1), (5, -2, 1));
        stone(&p, level, random, (7, -2, 1), (9, -2, 1));
        stone(&p, level, random, (6, -3, 1), (6, -3, 1));
        stone(&p, level, random, (6, -1, 1), (6, -1, 1));
        let hook = |facing| {
            state(
                Block::TripwireHook,
                &[("facing", facing), ("attached", "true")],
            )
        };
        let wire = |a, b| {
            state(
                Block::Tripwire,
                &[(a, "true"), (b, "true"), ("attached", "true")],
            )
        };
        let redstone = |sides: &[(&'static str, &'static str)]| state(Block::RedstoneWire, sides);
        p.set(level, chunk, hook("east"), 1, -3, 8);
        p.set(level, chunk, hook("west"), 4, -3, 8);
        p.set(level, chunk, wire("east", "west"), 2, -3, 8);
        p.set(level, chunk, wire("east", "west"), 3, -3, 8);
        let ns = redstone(&[("north", "side"), ("south", "side")]);
        for z in (2..=7).rev() {
            p.set(level, chunk, ns, 5, -3, z);
        }
        p.set(
            level,
            chunk,
            redstone(&[("north", "side"), ("west", "side")]),
            5,
            -3,
            1,
        );
        p.set(
            level,
            chunk,
            redstone(&[("east", "side"), ("west", "side")]),
            4,
            -3,
            1,
        );
        p.set(level, chunk, mossy, 3, -3, 1);
        let dispenser_loot = structure_loot("jungle_temple_dispenser");
        if !self.flag(2) {
            let placed =
                p.create_dispenser(level, chunk, random, (3, -2, 1), "north", dispenser_loot);
            self.set_flag(2, placed);
        }
        p.set(
            level,
            chunk,
            state(Block::Vine, &[("south", "true")]),
            3,
            -2,
            2,
        );
        p.set(level, chunk, hook("north"), 7, -3, 1);
        p.set(level, chunk, hook("south"), 7, -3, 5);
        for z in 2..=4 {
            p.set(level, chunk, wire("north", "south"), 7, -3, z);
        }
        p.set(
            level,
            chunk,
            redstone(&[("east", "side"), ("west", "side")]),
            8,
            -3,
            6,
        );
        p.set(
            level,
            chunk,
            redstone(&[("west", "side"), ("south", "side")]),
            9,
            -3,
            6,
        );
        p.set(
            level,
            chunk,
            redstone(&[("north", "side"), ("south", "up")]),
            9,
            -3,
            5,
        );
        p.set(level, chunk, mossy, 9, -3, 4);
        p.set(level, chunk, ns, 9, -2, 4);
        if !self.flag(3) {
            let placed =
                p.create_dispenser(level, chunk, random, (9, -2, 3), "west", dispenser_loot);
            self.set_flag(3, placed);
        }
        let vine_east = state(Block::Vine, &[("east", "true")]);
        p.set(level, chunk, vine_east, 8, -1, 3);
        p.set(level, chunk, vine_east, 8, -2, 3);
        let table = structure_loot("jungle_temple");
        if !self.flag(0) {
            let placed = p.create_chest(level, chunk, random, 8, -3, 3, table);
            self.set_flag(0, placed);
        }
        for (x, y, z) in [
            (9, -3, 2),
            (8, -3, 1),
            (4, -3, 5),
            (5, -2, 5),
            (5, -1, 5),
            (6, -3, 5),
            (7, -2, 5),
            (7, -1, 5),
            (8, -3, 5),
        ] {
            p.set(level, chunk, mossy, x, y, z);
        }
        stone(&p, level, random, (9, -1, 1), (9, -1, 5));
        p.generate_air_box(level, chunk, (8, -3, 8), (10, -1, 10));
        let chiseled = Block::ChiseledStoneBricks.default_state();
        let lever = state(Block::Lever, &[("facing", "north"), ("face", "wall")]);
        for x in 8..=10 {
            p.set(level, chunk, chiseled, x, -2, 11);
        }
        for x in 8..=10 {
            p.set(level, chunk, lever, x, -2, 12);
        }
        stone(&p, level, random, (8, -3, 8), (8, -3, 10));
        stone(&p, level, random, (10, -3, 8), (10, -3, 10));
        p.set(level, chunk, mossy, 10, -2, 9);
        p.set(level, chunk, ns, 8, -2, 9);
        p.set(level, chunk, ns, 8, -2, 10);
        let all = redstone(&[
            ("north", "side"),
            ("south", "side"),
            ("east", "side"),
            ("west", "side"),
        ]);
        p.set(level, chunk, all, 10, -1, 9);
        let piston = |facing| state(Block::StickyPiston, &[("facing", facing)]);
        p.set(level, chunk, piston("up"), 9, -2, 8);
        p.set(level, chunk, piston("west"), 10, -2, 8);
        p.set(level, chunk, piston("west"), 10, -1, 8);
        p.set(
            level,
            chunk,
            state(Block::Repeater, &[("facing", "north")]),
            10,
            -2,
            10,
        );
        if !self.flag(1) {
            let placed = p.create_chest(level, chunk, random, 9, -3, 10, table);
            self.set_flag(1, placed);
        }
    }

    fn swamp_hut(&self, ctx: &mut PieceContext, chunk: &BoundingBox) {
        let Some(frame) = self.settle_average(ctx.level, chunk, 0) else {
            return;
        };
        let p = Placed { frame };
        let level = &mut *ctx.level;
        let planks = Block::SprucePlanks.default_state();
        let log = Block::OakLog.default_state();
        let fence = Block::OakFence.default_state();
        let air = Block::Air.default_state();
        for (from, to) in [
            ((1, 1, 1), (5, 1, 7)),
            ((1, 4, 2), (5, 4, 7)),
            ((2, 1, 0), (4, 1, 0)),
            ((2, 2, 2), (3, 3, 2)),
            ((1, 2, 3), (1, 3, 6)),
            ((5, 2, 3), (5, 3, 6)),
            ((2, 2, 7), (4, 3, 7)),
        ] {
            p.fill(level, chunk, from, to, planks);
        }
        for (x, z) in [(1, 2), (5, 2), (1, 7), (5, 7)] {
            p.fill(level, chunk, (x, 0, z), (x, 3, z), log);
        }
        p.set(level, chunk, fence, 2, 3, 2);
        p.set(level, chunk, fence, 3, 3, 7);
        p.set(level, chunk, air, 1, 3, 4);
        p.set(level, chunk, air, 5, 3, 4);
        p.set(level, chunk, air, 5, 3, 5);
        p.set(
            level,
            chunk,
            Block::PottedRedMushroom.default_state(),
            1,
            3,
            5,
        );
        p.set(level, chunk, Block::CraftingTable.default_state(), 3, 2, 6);
        p.set(level, chunk, Block::Cauldron.default_state(), 4, 2, 6);
        p.set(level, chunk, fence, 1, 2, 1);
        p.set(level, chunk, fence, 5, 2, 1);
        let north = stairs(Block::SpruceStairs, "north");
        let east = stairs(Block::SpruceStairs, "east");
        let west = stairs(Block::SpruceStairs, "west");
        let south = stairs(Block::SpruceStairs, "south");
        p.fill(level, chunk, (0, 4, 1), (6, 4, 1), north);
        p.fill(level, chunk, (0, 4, 2), (0, 4, 7), east);
        p.fill(level, chunk, (6, 4, 2), (6, 4, 7), west);
        p.fill(level, chunk, (0, 4, 8), (6, 4, 8), south);
        let shaped = |s: BlockState, shape| s.with("shape", shape).unwrap_or(s);
        p.set(level, chunk, shaped(north, "outer_right"), 0, 4, 1);
        p.set(level, chunk, shaped(north, "outer_left"), 6, 4, 1);
        p.set(level, chunk, shaped(south, "outer_left"), 0, 4, 8);
        p.set(level, chunk, shaped(south, "outer_right"), 6, 4, 8);
        for z in [2, 7] {
            for x in [1, 5] {
                p.fill_column_down(level, log, x, -1, z, chunk);
            }
        }
        let witch = p.frame.world_pos(2, 2, 5);
        if chunk.contains(witch) {
            if !self.flag(0) {
                level.add_entity(witch, hut_mob("minecraft:witch", witch));
            }
            if !self.flag(1) {
                level.add_entity(
                    witch,
                    hut_mob("minecraft:cat", witch)
                        .with("tuff:spawn_structure", "minecraft:swamp_hut"),
                );
            }
            self.set_flag(0, true);
            self.set_flag(1, true);
        }
    }
}

impl Piece for ScatteredPiece {
    fn kind(&self) -> PieceType {
        match self.shape {
            Shape::DesertPyramid => PieceType::Tedp,
            Shape::JungleTemple => PieceType::Tejp,
            Shape::SwampHut => PieceType::Tesh,
        }
    }

    fn bounding_box(&self) -> BoundingBox {
        self.placement.lock().unwrap().1
    }

    fn orientation(&self) -> Option<Facing> {
        Some(self.orientation)
    }

    fn load_progress(&self, tag: &Compound) {
        let names: &[&str] = match self.shape {
            Shape::DesertPyramid => &[
                "hasPlacedChest0",
                "hasPlacedChest1",
                "hasPlacedChest2",
                "hasPlacedChest3",
            ],
            Shape::JungleTemple => &[
                "placedMainChest",
                "placedHiddenChest",
                "placedTrap1",
                "placedTrap2",
            ],
            Shape::SwampHut => &["Witch", "Cat"],
        };
        for (i, name) in names.iter().enumerate() {
            if tag.get_bool(name) == Some(true) {
                self.set_flag(i, true);
            }
        }
        let mut placement = self.placement.lock().unwrap();
        if placement.0 < 0
            && let Some(hpos) = tag.get_int("HPos").filter(|h| *h >= 0)
            && let Some(b) = tag.get_int_array("BB").and_then(BoundingBox::from_ints)
        {
            *placement = (hpos, b);
        }
    }

    fn save_extra(&self, tag: &mut Compound) {
        let [width, height, depth] = self.shape.size();
        tag.insert("Width", width);
        tag.insert("Height", height);
        tag.insert("Depth", depth);
        tag.insert("HPos", self.placement.lock().unwrap().0);
        let names: &[&str] = match self.shape {
            Shape::DesertPyramid => &[
                "hasPlacedChest0",
                "hasPlacedChest1",
                "hasPlacedChest2",
                "hasPlacedChest3",
            ],
            Shape::JungleTemple => &[
                "placedMainChest",
                "placedHiddenChest",
                "placedTrap1",
                "placedTrap2",
            ],
            Shape::SwampHut => &["Witch", "Cat"],
        };
        for (i, name) in names.iter().enumerate() {
            tag.insert(*name, self.flag(i));
        }
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        _reference: BlockPos,
    ) {
        let chunk = *chunk_box;
        match self.shape {
            Shape::DesertPyramid => self.desert_pyramid(ctx, &chunk),
            Shape::JungleTemple => self.jungle_temple(ctx, &chunk),
            Shape::SwampHut => self.swamp_hut(ctx, &chunk),
        }
    }

    fn after_place(&self, ctx: &mut PieceContext, chunk_box: &BoundingBox, _chunk: ChunkPos) {
        if self.shape != Shape::DesertPyramid {
            return;
        }
        let (hpos, b) = *self.placement.lock().unwrap();
        if hpos < 0 {
            return;
        }
        let frame = Frame::new(0, b, Some(self.orientation));
        let seed = ctx.level.seed();
        let (mut sand, roof) = self.cellar_sand(&frame, seed);
        Self::place_suspicious_sand(ctx.level, chunk_box, roof);
        sand.sort_by_key(|p| (p.y, p.z, p.x));
        sand.dedup();
        let center = b.center();
        let mut random = LegacyRandom::new(seed)
            .fork_positional()
            .at(center.x, center.y, center.z);
        for i in (2..=sand.len()).rev() {
            let j = random.next_int_bounded(i as i32) as usize;
            sand.swap(i - 1, j);
        }
        let mut suspicious = (sand.len() as i32).min(5 + random.next_int_bounded(3));
        for pos in sand {
            if suspicious > 0 {
                suspicious -= 1;
                Self::place_suspicious_sand(ctx.level, chunk_box, pos);
            } else if chunk_box.contains(pos) {
                ctx.level.set_block(pos, Block::Sand.default_state());
            }
        }
    }
}

/// A swamp hut's witch or cat: kept for good and set up as a fresh
/// structure spawn when the chunk goes live.
fn hut_mob(id: &str, pos: BlockPos) -> Compound {
    let at = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
    Compound::new()
        .with("id", id)
        .with("Pos", Tag::List(at.map(Tag::Double).to_vec()))
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(0.0), Tag::Float(0.0)]),
        )
        .with("PersistenceRequired", true)
        .with(super::template::FINALIZE_SPAWN, true)
}

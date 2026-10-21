use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_vanilla_data::{Block, BlockState};

use super::*;
use crate::chunk::{Chunk, ChunkShape};

const SHAPE: ChunkShape = ChunkShape::OVERWORLD;
const MIN_Y: i32 = SHAPE.min_y;
const HEIGHT: i32 = SHAPE.height;

#[derive(Default)]
struct World {
    chunks: HashMap<ChunkPos, (Chunk, SkyLightSources)>,
}

impl ChunkSource for World {
    fn chunk(&self, pos: ChunkPos) -> Option<LightChunk<'_>> {
        self.chunks
            .get(&pos)
            .map(|(blocks, sky_sources)| LightChunk {
                blocks,
                sky_sources,
            })
    }
}

impl World {
    /// Adds every chunk, then lights them all, as the game does once a
    /// chunk's neighbours are initialized.
    fn lit(chunks: impl IntoIterator<Item = (ChunkPos, Chunk)>) -> (Self, LevelLight) {
        let mut world = Self::default();
        let mut positions = Vec::new();
        for (pos, chunk) in chunks {
            let sources = SkyLightSources::from_chunk(&chunk);
            world.chunks.insert(pos, (chunk, sources));
            positions.push(pos);
        }
        let mut light = LevelLight::new(SHAPE, true);
        for &pos in &positions {
            light.initialize_light(&world, pos, false);
        }
        for &pos in &positions {
            light.light_chunk(&world, pos, false);
        }
        (world, light)
    }

    fn set(&mut self, light: &mut LevelLight, pos: Pos, state: BlockState) {
        let (chunk, sources) = self.chunks.get_mut(&pos.column()).unwrap();
        let index = ((pos.y - MIN_Y) >> 4) as usize;
        let was_air = chunk.sections().nth(index).unwrap().has_only_air();
        let old = chunk.set(pos.x, pos.y, pos.z, state).unwrap();
        let now_air = chunk.sections().nth(index).unwrap().has_only_air();
        if was_air != now_air {
            light.update_section_status(pos.section(), now_air);
        }
        if has_different_light_properties(old, state) {
            sources.update(chunk, pos.x & 15, pos.y, pos.z & 15);
            light.check_block(pos);
        }
        light.run_light_updates(self);
    }
}

fn stone() -> BlockState {
    Block::Stone.default_state()
}

/// Stone from the bottom of the world up to and including `top`.
fn ground(top: i32) -> Chunk {
    let mut chunk = Chunk::empty(SHAPE);
    for y in MIN_Y..=top {
        for z in 0..16 {
            for x in 0..16 {
                chunk.set(x, y, z, stone());
            }
        }
    }
    chunk
}

fn layer(chunk: &mut Chunk, y: i32, state: BlockState, xs: std::ops::RangeInclusive<i32>) {
    for z in 0..16 {
        for x in xs.clone() {
            chunk.set(x, y, z, state);
        }
    }
}

fn state(block: Block, properties: &[(&str, &str)]) -> BlockState {
    properties
        .iter()
        .fold(block.default_state(), |state, (name, value)| {
            state.with(name, value).unwrap()
        })
}

fn sky(light: &LevelLight, x: i32, y: i32, z: i32) -> u8 {
    light.light_value(LightLayer::Sky, Pos::new(x, y, z))
}

fn block(light: &LevelLight, x: i32, y: i32, z: i32) -> u8 {
    light.light_value(LightLayer::Block, Pos::new(x, y, z))
}

const ORIGIN: ChunkPos = ChunkPos::new(0, 0);

#[tuff_core::watchdog]
#[test]
fn torch_light_falls_off_with_distance() {
    let mut chunk = ground(63);
    chunk.set(8, 64, 8, Block::Torch.default_state());
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    assert_eq!(block(&light, 8, 64, 8), 14);
    assert_eq!(block(&light, 8, 70, 8), 8);
    assert_eq!(block(&light, 3, 64, 8), 9);
    assert_eq!(block(&light, 9, 65, 9), 11);
    assert_eq!(block(&light, 0, 64, 8), 6);
    assert_eq!(block(&light, 8, 63, 8), 0);
    assert_eq!(block(&light, 8, 64, 23), 0);
}

#[tuff_core::watchdog]
#[test]
fn block_light_crosses_into_initialized_neighbours() {
    let mut chunk = ground(63);
    chunk.set(15, 64, 8, Block::Glowstone.default_state());
    let (_, light) = World::lit([(ORIGIN, chunk), (ChunkPos::new(1, 0), ground(63))]);
    assert_eq!(block(&light, 15, 64, 8), 15);
    assert_eq!(block(&light, 16, 64, 8), 14);
    assert_eq!(block(&light, 20, 64, 8), 10);
}

#[tuff_core::watchdog]
#[test]
fn missing_neighbours_are_opaque() {
    let mut chunk = ground(63);
    chunk.set(15, 64, 8, Block::Glowstone.default_state());
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    assert_eq!(block(&light, 16, 64, 8), 0);
    assert_eq!(
        light.section_light(LightLayer::Block, SectionPos::new(1, 4, 0)),
        SectionLight::Empty
    );
}

#[tuff_core::watchdog]
#[test]
fn open_sky_is_full_down_to_the_ground() {
    let (_, light) = World::lit([(ORIGIN, ground(63))]);
    assert_eq!(sky(&light, 8, 64, 8), 15);
    assert_eq!(sky(&light, 8, 63, 8), 0);
    assert_eq!(sky(&light, 8, 300, 8), 15);

    let sections = light.chunk_light(ORIGIN);
    let full = SectionLight::Data(Box::new([0xFF; LAYER_BYTES]));
    for (index, (sky, block)) in sections.sky.iter().zip(&sections.block).enumerate() {
        let (expected_sky, expected_block) = match index {
            0..9 => (SectionLight::Empty, SectionLight::Empty),
            9 => (full.clone(), SectionLight::Empty),
            _ => (SectionLight::Missing, SectionLight::Missing),
        };
        assert_eq!(*sky, expected_sky, "sky section {index}");
        assert_eq!(*block, expected_block, "block section {index}");
    }
}

#[tuff_core::watchdog]
#[test]
fn sky_light_spreads_under_an_overhang() {
    let mut chunk = ground(63);
    layer(&mut chunk, 70, stone(), 0..=10);
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    for x in 0..=10 {
        assert_eq!(sky(&light, x, 65, 5), (x + 4) as u8, "x = {x}");
        assert_eq!(sky(&light, x, 69, 5), (x + 4) as u8, "x = {x}");
    }
    assert_eq!(sky(&light, 11, 65, 5), 15);
    assert_eq!(sky(&light, 5, 70, 5), 0);
    assert_eq!(sky(&light, 5, 71, 5), 15);
}

#[tuff_core::watchdog]
#[test]
fn sky_light_dims_through_water() {
    let mut chunk = ground(63);
    for y in 64..=66 {
        layer(&mut chunk, y, Block::Water.default_state(), 0..=15);
    }
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    assert_eq!(sky(&light, 4, 67, 4), 15);
    assert_eq!(sky(&light, 4, 66, 4), 14);
    assert_eq!(sky(&light, 4, 65, 4), 13);
    assert_eq!(sky(&light, 4, 64, 4), 12);
}

#[tuff_core::watchdog]
#[test]
fn slabs_block_light_through_their_full_face_only() {
    let top = state(Block::OakSlab, &[("type", "top")]);
    let bottom = state(Block::OakSlab, &[("type", "bottom")]);

    let mut chunk = ground(63);
    layer(&mut chunk, 70, top, 0..=15);
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    assert_eq!(sky(&light, 8, 71, 8), 15);
    assert_eq!(sky(&light, 8, 70, 8), 0);
    assert_eq!(sky(&light, 8, 69, 8), 0);

    let mut chunk = ground(63);
    layer(&mut chunk, 70, bottom, 0..=15);
    let (_, light) = World::lit([(ORIGIN, chunk)]);
    assert_eq!(sky(&light, 8, 70, 8), 15);
    assert_eq!(sky(&light, 8, 69, 8), 0);
}

#[tuff_core::watchdog]
#[test]
fn removing_a_light_source_darkens_its_surroundings() {
    let (mut world, mut light) = World::lit([(ORIGIN, ground(63))]);
    let torch = Pos::new(8, 64, 8);
    world.set(&mut light, torch, Block::Torch.default_state());
    assert_eq!(block(&light, 8, 66, 8), 12);
    world.set(&mut light, torch, Block::Air.default_state());
    for y in 64..80 {
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(block(&light, x, y, z), 0);
            }
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn covering_a_column_moves_its_sky_source() {
    let (mut world, mut light) = World::lit([(ORIGIN, ground(63))]);
    world.set(&mut light, Pos::new(8, 80, 8), stone());
    assert_eq!(sky(&light, 8, 81, 8), 15);
    assert_eq!(sky(&light, 8, 79, 8), 14);
    world.set(&mut light, Pos::new(8, 80, 8), Block::Air.default_state());
    assert_eq!(sky(&light, 8, 79, 8), 15);
}

/// Edits applied one at a time must converge to the same light as lighting
/// the final blocks from scratch.
#[tuff_core::watchdog]
#[test]
fn incremental_updates_match_a_full_relight() {
    let palette = [
        Block::Air.default_state(),
        Block::Air.default_state(),
        stone(),
        Block::Glass.default_state(),
        Block::Torch.default_state(),
        Block::Glowstone.default_state(),
        Block::Water.default_state(),
        Block::OakLeaves.default_state(),
        state(Block::OakSlab, &[("type", "top")]),
        state(Block::OakSlab, &[("type", "bottom")]),
        state(Block::OakStairs, &[("facing", "east")]),
    ];
    let columns = [
        ChunkPos::new(0, 0),
        ChunkPos::new(1, 0),
        ChunkPos::new(0, 1),
        ChunkPos::new(1, 1),
    ];
    let (mut world, mut light) = World::lit(columns.map(|pos| (pos, ground(63))));
    let mut rng = StdRng::seed_from_u64(7);
    for _ in 0..400 {
        let pos = Pos::new(
            rng.gen_range(0..32),
            rng.gen_range(60..96),
            rng.gen_range(0..32),
        );
        let state = palette[rng.gen_range(0..palette.len())];
        world.set(&mut light, pos, state);
    }

    let (_, fresh) = World::lit(
        world
            .chunks
            .iter()
            .map(|(pos, (chunk, _))| (*pos, chunk.clone())),
    );
    for layer in [LightLayer::Sky, LightLayer::Block] {
        for y in MIN_Y..MIN_Y + HEIGHT {
            for z in 0..32 {
                for x in 0..32 {
                    let pos = Pos::new(x, y, z);
                    assert_eq!(
                        light.light_value(layer, pos),
                        fresh.light_value(layer, pos),
                        "{layer:?} at {pos:?}"
                    );
                }
            }
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn dimensions_without_sky_light_only_track_block_light() {
    let shape = ChunkShape::new(0, 256);
    let pos = ChunkPos::new(0, 0);
    let mut chunk = Chunk::empty(shape);
    for y in 0..40 {
        layer(&mut chunk, y, stone(), 0..=15);
    }
    chunk.set(8, 40, 8, Block::Glowstone.default_state());
    let mut world = World::default();
    let sources = SkyLightSources::from_chunk(&chunk);
    world.chunks.insert(pos, (chunk, sources));
    let mut light = LevelLight::new(shape, false);
    assert!(!light.has_sky_light());
    light.initialize_light(&world, pos, false);
    light.light_chunk(&world, pos, false);
    assert_eq!(light.light_value(LightLayer::Block, Pos::new(8, 41, 8)), 14);
    assert_eq!(light.light_value(LightLayer::Block, Pos::new(8, 45, 8)), 10);
    for y in [0, 41, 100, 255] {
        assert_eq!(light.light_value(LightLayer::Sky, Pos::new(8, y, 8)), 0);
    }
    let sections = light.chunk_light(pos);
    assert_eq!(sections.sky.len(), shape.light_sections());
    assert!(sections.sky.iter().all(|s| *s == SectionLight::Missing));
    assert!(matches!(sections.block[3], SectionLight::Data(_)));
    assert!(light.light_on_in_column(pos));
    world.set(&mut light, Pos::new(8, 40, 8), stone());
    assert_eq!(light.light_value(LightLayer::Block, Pos::new(8, 41, 8)), 0);
}

const REF_LOW: i32 = 56;
const REF_HIGH: i32 = 112;

fn reference_sky(blocks: &HashMap<(i32, i32, i32), BlockState>) -> HashMap<(i32, i32, i32), u8> {
    let damp = |pos: (i32, i32, i32)| blocks.get(&pos).map_or(0, |s| s.light_dampening());
    let mut level: HashMap<(i32, i32, i32), u8> = HashMap::new();
    for x in 0..32 {
        for z in 0..32 {
            let mut open = true;
            for y in (REF_LOW..=REF_HIGH).rev() {
                open &= damp((x, y, z)) == 0;
                level.insert((x, y, z), if open { 15 } else { 0 });
            }
        }
    }
    loop {
        let mut changed = false;
        for x in 0..32 {
            for z in 0..32 {
                for y in REF_LOW..=REF_HIGH {
                    let cost = damp((x, y, z)).max(1);
                    let mut best = level[&(x, y, z)];
                    for (dx, dy, dz) in [
                        (1, 0, 0),
                        (-1, 0, 0),
                        (0, 1, 0),
                        (0, -1, 0),
                        (0, 0, 1),
                        (0, 0, -1),
                    ] {
                        if let Some(&n) = level.get(&(x + dx, y + dy, z + dz)) {
                            best = best.max(n.saturating_sub(cost));
                        }
                    }
                    if best != level[&(x, y, z)] {
                        level.insert((x, y, z), best);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return level;
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn sky_light_matches_a_reference_over_varied_terrain() {
    let features = [
        Block::Air.default_state(),
        Block::Air.default_state(),
        Block::Air.default_state(),
        stone(),
        stone(),
        Block::Glass.default_state(),
        Block::Water.default_state(),
        Block::OakLeaves.default_state(),
    ];
    for seed in 0..4u64 {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut blocks = HashMap::new();
        let mut chunks = Vec::new();
        for (cx, cz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let mut chunk = ground(63);
            for z in 0..16 {
                for x in 0..16 {
                    let height = rng.gen_range(64..76);
                    for y in 64..=height {
                        let s = if rng.gen_bool(0.25) {
                            Block::Air.default_state()
                        } else {
                            stone()
                        };
                        chunk.set(x, y, z, s);
                        blocks.insert((cx * 16 + x, y, cz * 16 + z), s);
                    }
                    if rng.gen_bool(0.3) {
                        let y = rng.gen_range(height + 2..height + 10);
                        let s = features[rng.gen_range(0..features.len())];
                        chunk.set(x, y, z, s);
                        blocks.insert((cx * 16 + x, y, cz * 16 + z), s);
                    }
                }
            }
            chunks.push((ChunkPos::new(cx, cz), chunk));
        }
        for z in 0..32 {
            for x in 0..32 {
                for y in REF_LOW..64 {
                    blocks.insert((x, y, z), stone());
                }
            }
        }
        let (_, light) = World::lit(chunks);
        let expected = reference_sky(&blocks);
        for (&(x, y, z), &want) in &expected {
            assert_eq!(sky(&light, x, y, z), want, "seed {seed} at {x},{y},{z}");
        }
    }
}

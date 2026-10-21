use tuff_core::storage::level::{DimensionSettings, OVERWORLD};
use tuff_core::world::generator::{ChunkGenerator, DisabledStages, GeneratorData};
use tuff_vanilla_data::Block;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

const SHAPE: ChunkShape = ChunkShape::OVERWORLD;

#[tuff_core::watchdog]
#[test]
fn retrogen_replaces_the_flat_fill_below_zero_with_noise_terrain_and_keeps_the_rest() {
    let dimensions = DimensionSettings::vanilla();
    let overworld = dimensions.iter().find(|d| d.key == OVERWORLD).unwrap();
    let registries = tuff_item::registries::Registries::vanilla();
    let generator = crate::chunk_generator::vanilla(overworld, 1).unwrap();
    generator.bind(&GeneratorData {
        registries: &registries,
        compile_loaded: true,
        shape: SHAPE,
        disabled: DisabledStages::default(),
    });
    let pos = ChunkPos::new(3, -2);
    let mut chunk = Chunk::empty(SHAPE);
    let stone = Block::Stone.default_state();
    for y in SHAPE.min_y..0 {
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, y, z, Block::Deepslate.default_state());
            }
        }
    }
    chunk.set(4, 10, 4, stone);
    assert!(generator.retrogen_below_zero(pos, SHAPE, &mut chunk));
    assert_eq!(chunk.get(4, 10, 4).block(), Block::Stone);
    let mut non_deepslate = 0;
    for y in SHAPE.min_y..0 {
        for x in 0..16 {
            for z in 0..16 {
                let block = chunk.get(x, y, z).block();
                non_deepslate += usize::from(block != Block::Deepslate);
            }
        }
    }
    assert!(non_deepslate > 0);
    assert_eq!(chunk.get(0, SHAPE.min_y, 0).block(), Block::Bedrock);
}

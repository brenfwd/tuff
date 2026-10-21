use std::sync::Arc;

use tuff_core::world::generator::{ChunkGenerator, DisabledStages, GeneratorData};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

use crate::biome_source::MultiNoise;
use crate::generator::{NoiseGenerator, Stages};
use crate::proto::ProtoChunk;

const SHAPE: ChunkShape = ChunkShape::OVERWORLD;

fn generator(disabled: DisabledStages) -> NoiseGenerator {
    NoiseGenerator::new(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
        1,
    )
    .with_disabled(disabled)
}

fn count(chunk: &ProtoChunk, matches: impl Fn(Block) -> bool) -> usize {
    let mut n = 0;
    for y in SHAPE.min_y..SHAPE.min_y + SHAPE.height {
        for x in 0..16 {
            for z in 0..16 {
                n += usize::from(matches(chunk.get(x, y, z).block()));
            }
        }
    }
    n
}

fn chunks() -> impl Iterator<Item = ChunkPos> {
    (0..3).flat_map(|x| (0..3).map(move |z| ChunkPos::new(x * 7 - 4, z * 5 - 3)))
}

fn total(generator: &NoiseGenerator, matches: impl Fn(Block) -> bool + Copy) -> usize {
    chunks()
        .map(|pos| count(&generator.generate(pos, SHAPE, Stages::ALL), matches))
        .sum()
}

#[tuff_core::watchdog]
#[test]
fn surface_and_carver_switches_match_the_stage_cut() {
    let surface = generator(DisabledStages {
        surface: true,
        ..Default::default()
    });
    let carvers = generator(DisabledStages {
        carvers: true,
        ..Default::default()
    });
    let reference = generator(DisabledStages::default());
    for pos in chunks() {
        let cut = |stages| reference.generate(pos, SHAPE, stages).into_chunk();
        assert!(
            surface.generate(pos, SHAPE, Stages::ALL).into_chunk()
                == cut(Stages {
                    surface: false,
                    carvers: true
                }),
            "{pos:?} without surface"
        );
        assert!(
            carvers.generate(pos, SHAPE, Stages::ALL).into_chunk()
                == cut(Stages {
                    surface: true,
                    carvers: false
                }),
            "{pos:?} without carvers"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn fluid_switch_leaves_no_water_or_lava() {
    let fluid = |b: Block| matches!(b, Block::Water | Block::Lava);
    assert!(total(&generator(DisabledStages::default()), fluid) > 0);
    let dry = generator(DisabledStages {
        fluid_generation: true,
        ..Default::default()
    });
    assert_eq!(total(&dry, fluid), 0);
}

#[tuff_core::watchdog]
#[test]
fn ore_vein_switch_leaves_no_vein_ores() {
    let vein = |b: Block| {
        matches!(
            b,
            Block::CopperOre
                | Block::DeepslateIronOre
                | Block::RawCopperBlock
                | Block::RawIronBlock
        )
    };
    assert!(total(&generator(DisabledStages::default()), vein) > 0);
    let plain = generator(DisabledStages {
        ore_veins: true,
        ..Default::default()
    });
    assert_eq!(total(&plain, vein), 0);
}

#[tuff_core::watchdog]
#[test]
fn structure_switch_starts_nothing() {
    let around = || (-8..8).flat_map(|x| (-8..8).map(move |z| ChunkPos::new(x, z)));
    let enabled = generator(DisabledStages::default());
    assert!(around().any(|pos| !enabled.structure_starts(pos, SHAPE).is_empty()));
    let disabled = generator(DisabledStages {
        structures: true,
        ..Default::default()
    });
    assert!(around().all(|pos| disabled.structure_starts(pos, SHAPE).is_empty()));
}

#[tuff_core::watchdog]
#[test]
fn bound_switches_reach_the_generator_and_stop_locate() {
    let dimensions = tuff_core::storage::level::DimensionSettings::vanilla();
    let overworld = dimensions
        .iter()
        .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
        .unwrap();
    let registries = tuff_item::registries::Registries::vanilla();
    let bind = |disabled| {
        let generator = crate::chunk_generator::vanilla(overworld, 1).unwrap();
        generator.bind(&GeneratorData {
            registries: &registries,
            compile_loaded: true,
            shape: SHAPE,
            disabled,
        });
        generator
    };
    let origin = BlockPos::new(0, 64, 0);
    let village = [
        "minecraft:village_plains",
        "minecraft:village_desert",
        "minecraft:village_savanna",
        "minecraft:village_taiga",
        "minecraft:village_snowy",
    ];
    assert!(
        bind(DisabledStages::default())
            .locate_structure(&village, origin, SHAPE)
            .is_some()
    );
    let featureless = bind(DisabledStages {
        features: true,
        ..Default::default()
    });
    assert!(
        featureless
            .locate_structure(&village, origin, SHAPE)
            .is_none()
    );
}

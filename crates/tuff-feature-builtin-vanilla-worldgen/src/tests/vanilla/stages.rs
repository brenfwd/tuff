use super::fixtures::*;
use crate::generator::Stages;
use crate::proto::ProtoChunk;
use tuff_world_model::pos::ChunkPos;

/// A chunk generated up to `stages`, then post-processed as vanilla does
/// when it promotes the chunk: the stage fixtures are live chunks too, so
/// aquifer water marked next to open caves has flowed one step.
fn staged_live(dimension: &str, pos: ChunkPos, stages: Stages) -> ProtoChunk {
    let generator = generator(dimension);
    let center = generator.generate(pos, shape(dimension), stages);
    if center.post_processing.is_empty() {
        return center;
    }
    let mut chunks = std::collections::HashMap::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let at = ChunkPos::new(pos.x + dx, pos.z + dz);
            if at != pos {
                chunks.insert(at, generator.generate(at, shape(dimension), stages));
            }
        }
    }
    chunks.insert(pos, center);
    go_live(dimension, pos, &mut chunks);
    chunks.remove(&pos).unwrap()
}

fn compare_variant(variant: &str, stages: Stages) {
    let _watchdog = tuff_core::watchdog::arm(tuff_core::watchdog::DEFAULT);
    let failures: Vec<String> = map_on_threads(&kept(variant), |&(dimension, pos)| {
        let (theirs, _) = load(variant, dimension, pos);
        let ours = staged_live(dimension, pos, stages);
        let (blocks, biomes, lines) = diff(&ours, &theirs, 8);
        (blocks + biomes > 0).then(|| {
            format!(
                "{dimension} {pos:?}: {blocks} blocks and {biomes} biomes differ\n  {}",
                lines.join("\n  ")
            )
        })
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{variant}:\n{}", failures.join("\n"));
}

#[tuff_core::watchdog]
#[test]
fn noise_stage_matches_vanilla() {
    compare_variant(
        "noise",
        Stages {
            surface: false,
            carvers: false,
        },
    );
}

#[tuff_core::watchdog]
#[test]
fn surface_stage_matches_vanilla() {
    compare_variant(
        "surface",
        Stages {
            surface: true,
            carvers: false,
        },
    );
}

#[tuff_core::watchdog]
#[test]
fn carver_stage_matches_vanilla() {
    compare_variant("carvers", Stages::ALL);
}

/// The generator memoizes biome columns and carver biomes across chunks;
/// a chunk generated with those caches warm must equal one generated cold.
#[tuff_core::watchdog]
#[test]
fn warm_generator_caches_reproduce_cold_terrain() {
    use crate::biome_source::MultiNoise;
    use crate::generator::NoiseGenerator;
    use std::sync::Arc;
    use tuff_vanilla_data::worldgen::NoiseSettings;

    let fresh = || {
        NoiseGenerator::new(
            NoiseSettings::get("minecraft:overworld").unwrap(),
            Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
            1,
        )
    };
    let shape = shape("overworld");
    let pos = ChunkPos::new(7, -5);
    let cold = fresh().generate(pos, shape, Stages::ALL).into_chunk();
    let generator = fresh();
    for dz in -1..=1 {
        for dx in -1..=1 {
            generator.generate(ChunkPos::new(pos.x + dx, pos.z + dz), shape, Stages::ALL);
        }
    }
    let warm = generator.generate(pos, shape, Stages::ALL);
    let (blocks, biomes, lines) = diff(&warm, &cold, 8);
    assert_eq!((blocks, biomes), (0, 0), "{}", lines.join("\n"));
}

//! Block-for-block hashes of generated chunks (surface rules and carvers
//! included) across seeds, dimensions and biomes, recorded before the
//! surface rules were compiled to an op list.

use std::sync::Arc;

use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::chunk::{biome_id, biome_name};
use tuff_world_model::pos::ChunkPos;

use super::fixtures::{map_on_threads, shape};
use crate::biome_source::{BiomeSource, MultiNoise, TheEnd};
use crate::generator::{NoiseGenerator, Stages};

const WANTED: &[&str] = &[
    "minecraft:badlands",
    "minecraft:eroded_badlands",
    "minecraft:wooded_badlands",
    "minecraft:frozen_peaks",
    "minecraft:jagged_peaks",
    "minecraft:mangrove_swamp",
    "minecraft:swamp",
    "minecraft:desert",
    "minecraft:deep_dark",
    "minecraft:ocean",
    "minecraft:frozen_ocean",
    "minecraft:deep_frozen_ocean",
    "minecraft:ice_spikes",
    "minecraft:stony_shore",
    "minecraft:beach",
    "minecraft:snowy_plains",
    "minecraft:plains",
];

const SEEDS: [i64; 6] = [1, 777, 42, 2024, -99, 31337];

const EXPECTED: &str = include_str!("surface_exact.txt");

fn hash(chunk: &crate::proto::ProtoChunk, dimension: &str) -> u64 {
    let shape = shape(dimension);
    let mut h = 0xcbf29ce484222325u64;
    for y in shape.min_y..shape.min_y + shape.height {
        for z in 0..16 {
            for x in 0..16 {
                h ^= chunk.get(x, y, z).id() as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
    }
    h
}

fn make(settings: &str, source: Arc<dyn BiomeSource>, seed: i64) -> NoiseGenerator {
    NoiseGenerator::new(NoiseSettings::get(settings).unwrap(), source, seed)
}

fn jobs() -> Vec<(String, i64, &'static str, ChunkPos)> {
    let mut jobs: Vec<(String, i64, &'static str, ChunkPos)> = Vec::new();
    for seed in SEEDS {
        let generator = make(
            "minecraft:overworld",
            Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
            seed,
        );
        let scan: Vec<ChunkPos> = (-24..=24)
            .flat_map(|x| (-24..=24).map(move |z| ChunkPos::new(x * 8, z * 8)))
            .collect();
        let columns = map_on_threads(&scan, |pos| {
            generator.chunk_biomes(*pos, shape("overworld"))
        });
        for name in WANTED {
            let id = biome_id(name).unwrap();
            if let Some(i) = columns.iter().position(|c| c.contains(&id)) {
                let pos = scan[i];
                if !jobs
                    .iter()
                    .any(|j| j.1 == seed && j.2 == "overworld" && j.3 == pos)
                {
                    jobs.push((
                        format!("ow/{seed}/{}/{}/{}", pos.x, pos.z, biome_name(id)),
                        seed,
                        "overworld",
                        pos,
                    ));
                }
            }
        }
    }
    for (x, z) in [(0, 0), (40, -25), (-90, 60), (130, 130)] {
        jobs.push((
            format!("nether/1/{x}/{z}"),
            1,
            "the_nether",
            ChunkPos::new(x, z),
        ));
    }
    for (x, z) in [(0, 0), (-3, 5), (40, 40), (-80, 20)] {
        jobs.push((format!("end/1/{x}/{z}"), 1, "the_end", ChunkPos::new(x, z)));
    }
    jobs
}

fn results() -> Vec<String> {
    let nether = make(
        "minecraft:nether",
        Arc::new(MultiNoise::preset("minecraft:nether").unwrap()),
        1,
    )
    .without_sky_light();
    let end = make("minecraft:end", Arc::new(TheEnd::new()), 1);
    let overworld = |seed| {
        make(
            "minecraft:overworld",
            Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
            seed,
        )
    };
    let generators: Vec<NoiseGenerator> = SEEDS.iter().map(|s| overworld(*s)).collect();
    let jobs = jobs();
    map_on_threads(&jobs, |(key, seed, dimension, pos)| {
        let generator = match *dimension {
            "overworld" => &generators[SEEDS.iter().position(|s| s == seed).unwrap()],
            "the_nether" => &nether,
            _ => &end,
        };
        let chunk = generator.generate(*pos, shape(dimension), Stages::ALL);
        format!("{key}={:016x}", hash(&chunk, dimension))
    })
}

#[tuff_core::watchdog]
#[test]
fn generated_chunks_match_recorded_hashes() {
    let mut got = results();
    got.sort();
    if std::env::var_os("TUFF_RECORD_SURFACE").is_some() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/tests/vanilla/surface_exact.txt"
            ),
            got.join("\n") + "\n",
        )
        .unwrap();
        return;
    }
    let mut want: Vec<&str> = EXPECTED.lines().collect();
    want.sort();
    assert!(want.len() >= 20, "too few recorded chunks: {}", want.len());
    assert_eq!(got, want);
}

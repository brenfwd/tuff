use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tuff_feature_builtin_vanilla_worldgen::biome_source::MultiNoise;
use tuff_feature_builtin_vanilla_worldgen::generator::{NoiseGenerator, Stages};
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

const SEED: i64 = 903;
const RADIUS: i32 = 3;
const ROUNDS: usize = 2;

fn checksum(generator: &NoiseGenerator, pos: ChunkPos, shape: ChunkShape) -> u64 {
    let chunk = generator.generate(pos, shape, Stages::ALL);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for x in (0..16).step_by(3) {
        for z in (0..16).step_by(3) {
            for y in (shape.min_y..shape.min_y + shape.height).step_by(5) {
                chunk.get(x, y, z).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn main() {
    let shape = ChunkShape::OVERWORLD;
    let generator = NoiseGenerator::new(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
        SEED,
    );
    let positions: Vec<ChunkPos> = (-RADIUS..=RADIUS)
        .flat_map(|x| (-RADIUS..=RADIUS).map(move |z| ChunkPos::new(x * 7, z * 7)))
        .collect();
    let mut digest = 0u64;
    for &pos in positions.iter().take(2) {
        digest ^= checksum(&generator, pos, shape);
    }
    let mut best = Duration::MAX;
    for round in 0..ROUNDS {
        let start = Instant::now();
        for &pos in &positions {
            std::hint::black_box(generator.generate(pos, shape, Stages::ALL));
        }
        let elapsed = start.elapsed();
        best = best.min(elapsed);
        println!(
            "round {round}: {:.2} chunks/s",
            positions.len() as f64 / elapsed.as_secs_f64()
        );
    }
    println!(
        "worldgen-terrain chunks={} best={:.2} chunks/s digest={digest:016x}",
        positions.len(),
        positions.len() as f64 / best.as_secs_f64()
    );
}

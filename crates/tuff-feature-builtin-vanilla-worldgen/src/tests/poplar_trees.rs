//! Poplar trees grown from fixed seeds keep their exact block output.

use std::time::Instant;

use serde_json::Value;
use tuff_item::random::WorldgenRandom;
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{ChunkShape, is_air};
use tuff_world_model::pos::ChunkPos;

use crate::biome_source::MultiNoise;
use crate::feature::level::Region;
use crate::feature::{PlaceContext, live_registry};
use crate::generator::{NoiseGenerator, Stages};

const TREES: [&str; 3] = [
    "minecraft:yellow_poplar_leaf_litter",
    "minecraft:orange_poplar_leaf_litter",
    "minecraft:red_poplar_leaf_litter",
];

fn grow_all(rounds: u64) -> (u64, u32) {
    let mut placed = 0;
    let generator = NoiseGenerator::new(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        std::sync::Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
        42,
    );
    let center = ChunkPos::new(0, 0);
    let mut window = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            window.push(generator.generate(pos, ChunkShape::OVERWORLD, Stages::ALL));
        }
    }
    let features: Vec<_> = TREES
        .iter()
        .map(|t| {
            live_registry()
                .lock()
                .unwrap()
                .feature(&Value::String((*t).to_string()))
                .unwrap()
        })
        .collect();
    let mut h = 0xcbf29ce484222325u64;
    for round in 0..rounds {
        let mut region = Region::new(&generator, center, window.clone());
        for (i, feature) in features.iter().enumerate() {
            for spot in 0..4i32 {
                let (x, z) = (spot * 4 + 1, (i as i32) * 5 + 1);
                let mut y = 200;
                while y > -60 && is_air(region.block(BlockPos::new(x, y, z))) {
                    y -= 1;
                }
                let seed = (round as i64) * 1000 + (i as i64) * 10 + spot as i64;
                let mut random = WorldgenRandom::legacy(seed);
                let mut ctx = PlaceContext {
                    level: &mut region,
                    random: &mut random,
                };
                let grew = feature.place(&mut ctx, BlockPos::new(x, y + 1, z));
                placed += u32::from(grew);
                h ^= u64::from(grew);
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        for chunk in region.into_chunks() {
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        h ^= chunk.get(x, y, z).id() as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
    }
    (h, placed)
}

#[tuff_core::watchdog]
#[test]
fn poplar_trees_place_the_pinned_blocks() {
    let (hash, placed) = grow_all(2);
    assert!(placed >= 8, "only {placed} trees grew");
    assert_eq!(hash, 13817940720344705714);
}

#[tuff_core::watchdog]
#[test]
#[ignore = "release timing run"]
fn poplar_tree_timing() {
    let start = Instant::now();
    let (h, _) = grow_all(40);
    eprintln!("poplar timing {:?} hash {h:x}", start.elapsed());
}

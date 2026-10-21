//! Pins the blocks a few features place over real terrain for fixed seeds,
//! so rewrites of their placement code stay bit-identical.

use super::fixtures::*;
use crate::feature::PlaceContext;
use crate::feature::level::Region;
use tuff_item::random::{Random, WorldgenRandom, XoroshiroRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const CENTERS: [(i32, i32); 3] = [(-18, 48), (40, -30), (200, 200)];

fn placed_hash(feature: &str, tries: u32) -> u64 {
    placed_hash_timed(feature, tries).0
}

fn placed_hash_timed(feature: &str, tries: u32) -> (u64, std::time::Duration) {
    let mut spent = std::time::Duration::ZERO;
    let generator = generator("overworld");
    let configured = generator
        .decorator()
        .registry
        .lock()
        .unwrap()
        .feature(&serde_json::Value::String(feature.into()))
        .unwrap();
    let mut h = 0xcbf29ce484222325u64;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x100000001b3);
    };
    for (cx, cz) in CENTERS {
        let pos = ChunkPos::new(cx, cz);
        let mut chunks = terrain("overworld", pos);
        let window = ROW_ORDER
            .iter()
            .map(|(dx, dz)| {
                chunks
                    .remove(&ChunkPos::new(pos.x + dx, pos.z + dz))
                    .unwrap()
            })
            .collect();
        let mut region = Region::new(generator, pos, window);
        let (min_y, max_y) = (region.min_y(), region.max_y());
        let started = std::time::Instant::now();
        for i in 0..tries {
            let seed = (i as i64) * 0x9E37_79B9 + cx as i64 * 31 + cz as i64;
            let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(seed)));
            let span = (max_y - min_y) as u32;
            let origin = BlockPos::new(
                (cx << 4) + (i * 7 % 16) as i32,
                min_y + 8 + (i * 37 % (span - 16)) as i32,
                (cz << 4) + (i * 11 % 16) as i32,
            );
            let mut ctx = PlaceContext {
                level: &mut region,
                random: &mut random,
            };
            mix(configured.place(&mut ctx, origin) as u64);
        }
        spent += started.elapsed();
        for chunk in region.into_chunks() {
            for y in min_y..max_y {
                for z in 0..16 {
                    for x in 0..16 {
                        mix(chunk.get(x, y, z).id() as u64);
                    }
                }
            }
        }
    }
    (h, spent)
}

#[tuff_core::watchdog]
#[test]
fn rewritten_features_place_pinned_blocks() {
    let got: Vec<(&str, u64)> = [
        ("minecraft:amethyst_geode", 40),
        ("minecraft:monster_room", 1500),
        ("minecraft:ore_diamond_large", 200),
        ("minecraft:ore_diamond_buried", 200),
        ("minecraft:ore_gravel", 200),
        ("minecraft:large_dripstone", 1500),
    ]
    .into_iter()
    .map(|(name, tries)| (name, placed_hash(name, tries)))
    .collect();
    let expected: [(&str, u64); 6] = [
        ("minecraft:amethyst_geode", 6706393717198707634),
        ("minecraft:monster_room", 17321468568908193023),
        ("minecraft:ore_diamond_large", 11731807351934148794),
        ("minecraft:ore_diamond_buried", 13247057743065645000),
        ("minecraft:ore_gravel", 9394004129658481266),
        ("minecraft:large_dripstone", 1248854682027205229),
    ];
    assert_eq!(got, expected);
}

/// Release timing of the pinned placements, for comparing rewrites.
#[tuff_core::watchdog]
#[test]
#[ignore = "timing run, release only"]
fn pinned_feature_placement_timing() {
    for (name, tries) in [
        ("minecraft:amethyst_geode", 400),
        ("minecraft:monster_room", 6000),
        ("minecraft:ore_diamond_large", 3000),
        ("minecraft:ore_gravel", 3000),
        ("minecraft:large_dripstone", 6000),
    ] {
        let best = (0..5)
            .map(|_| placed_hash_timed(name, tries).1)
            .min()
            .unwrap();
        eprintln!("TIMING {name} {:.2} ms", best.as_secs_f64() * 1e3);
    }
}

fn mineshaft_hash(rounds: i64) -> (u64, std::time::Duration) {
    use crate::structure::PieceContext;
    use crate::structure::geometry::BoundingBox;
    let generator = generator("overworld");
    let mut h = 0xcbf29ce484222325u64;
    let mut spent = std::time::Duration::ZERO;
    for (cx, cz) in CENTERS {
        let pos = ChunkPos::new(cx, cz);
        let mut chunks = terrain("overworld", pos);
        let window = ROW_ORDER
            .iter()
            .map(|(dx, dz)| {
                chunks
                    .remove(&ChunkPos::new(pos.x + dx, pos.z + dz))
                    .unwrap()
            })
            .collect();
        let mut region = Region::new(generator, pos, window);
        let (min_y, max_y) = (region.min_y(), region.max_y());
        let started = std::time::Instant::now();
        for round in 0..rounds {
            let wood = if round % 2 == 0 { "normal" } else { "mesa" };
            let pieces = crate::structure::mineshaft::grown_for_test(
                ChunkPos::new(cx - 1, cz - 1),
                round * 0x5DEECE66D + cx as i64,
                wood,
            );
            let mut random =
                WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(round ^ 0x1234)));
            for (dx, dz) in ROW_ORDER {
                let (bx, bz) = ((cx + dx) << 4, (cz + dz) << 4);
                let chunk_box = BoundingBox::new(bx, min_y, bz, bx + 15, max_y - 1, bz + 15);
                for piece in &pieces {
                    let mut b = chunk_box;
                    let mut ctx = PieceContext {
                        level: &mut region,
                        random: &mut random,
                    };
                    piece.place(&mut ctx, &mut b, pos, BlockPos::new(bx, 0, bz));
                }
            }
        }
        spent += started.elapsed();
        for chunk in region.into_chunks() {
            for y in min_y..max_y {
                for z in 0..16 {
                    for x in 0..16 {
                        h ^= chunk.get(x, y, z).id() as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
    }
    (h, spent)
}

#[tuff_core::watchdog]
#[test]
fn mineshaft_pieces_place_pinned_blocks() {
    assert_eq!(mineshaft_hash(6).0, 17183243967145645627);
}

#[tuff_core::watchdog]
#[test]
#[ignore = "timing run: cargo test --release -p tuff-feature-builtin-vanilla-worldgen mineshaft_place_timing -- --ignored --nocapture"]
fn mineshaft_place_timing() {
    let (h, spent) = mineshaft_hash(400);
    eprintln!("mineshaft place: {spent:?} ({h})");
}

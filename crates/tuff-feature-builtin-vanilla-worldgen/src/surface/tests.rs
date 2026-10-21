use super::*;
use crate::biome_source::MultiNoise;
use crate::generator::{NoiseGenerator, Stages};
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

fn band_digest(random: Random) -> u64 {
    let mut random = random;
    let bands = generate_bands(&mut random);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for state in bands {
        h = (h ^ u64::from(state.id())).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (random.next_long() as u64)
}

#[tuff_core::watchdog]
#[test]
fn clay_bands_are_pinned_for_fixed_seeds() {
    let digests: Vec<u64> = (0..6i64)
        .flat_map(|seed| {
            [
                band_digest(Random::Legacy(tuff_item::random::LegacyRandom::new(
                    seed * 7919,
                ))),
                band_digest(Random::Xoroshiro(tuff_item::random::XoroshiroRandom::new(
                    seed * 7919,
                ))),
            ]
        })
        .collect();
    assert_eq!(
        digests,
        [
            17624245102692654744,
            1948248711150823374,
            12121502704287501377,
            5933440108774371408,
            12336441237243687548,
            777626981334359073,
            14824374004279453355,
            4471073748303978719,
            9507645579119765333,
            10005839594690313420,
            18325258686814226965,
            15512648903473347105,
        ]
    );
}

#[tuff_core::watchdog]
#[test]
#[ignore = "timing"]
fn clay_band_generation_timing() {
    let start = std::time::Instant::now();
    let mut sink = 0u64;
    for seed in 0..200_000i64 {
        sink ^= band_digest(Random::Xoroshiro(tuff_item::random::XoroshiroRandom::new(
            seed,
        )));
    }
    eprintln!("clay bands: {:?} ({sink})", start.elapsed());
}

fn extension_digest(rounds: i32) -> u64 {
    let generator = NoiseGenerator::new(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        std::sync::Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
        1,
    );
    let base = generator.generate(ChunkPos::new(0, 0), ChunkShape::OVERWORLD, Stages::ALL);
    let system = &generator.surface;
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut ice = 0;
    for round in 0..rounds {
        let mut chunk = base.clone();
        for x in 0..16 {
            for z in 0..16 {
                let bx = x + 16 * (round % 97) - 700;
                let bz = z + 16 * (round / 97) - 300;
                let height = chunk.height_of(HeightmapKind::WorldSurfaceWg, x, z) + 1;
                let biome = [system.frozen_ocean, system.deep_frozen_ocean][(round & 1) as usize];
                system.eroded_badlands_extension(&mut chunk, x, z, bx, bz, height);
                system.frozen_ocean_extension(
                    &mut chunk,
                    x,
                    z,
                    bx,
                    bz,
                    height,
                    system.sea_level - 12,
                    biome,
                );
            }
        }
        for x in 0..16 {
            for z in 0..16 {
                for y in 30..140 {
                    let state = chunk.get(x, y, z);
                    ice += matches!(state.block(), Block::PackedIce | Block::SnowBlock) as u32;
                    h = (h ^ u64::from(state.id())).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
    }
    assert!(ice > 0);
    h
}

#[tuff_core::watchdog]
#[test]
fn surface_extensions_are_pinned() {
    assert_eq!(extension_digest(24), 11258818610980338657);
}

#[tuff_core::watchdog]
#[test]
#[ignore = "timing"]
fn surface_extension_timing() {
    let start = std::time::Instant::now();
    let digest = extension_digest(2000);
    eprintln!("surface extensions: {:?} ({digest})", start.elapsed());
}

struct Scatter(u64);

impl BiomeLookup for Scatter {
    fn biome(&mut self, x: i32, y: i32, z: i32) -> u16 {
        let mut h = self.0
            ^ (x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ (z as u64).wrapping_mul(0x1656_67b1_9e37_79f9);
        h ^= h >> 29;
        h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h ^= h >> 32;
        (h % tuff_vanilla_data::data_registry::WORLDGEN_BIOME
            .entries
            .len() as u64) as u16
    }
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[tuff_core::watchdog]
#[test]
fn biome_sets_match_vec_membership() {
    let mut state = 0x1234_5678_9abc_def1u64;
    for _ in 0..200 {
        let count = next(&mut state) % 12;
        let ids: Vec<u16> = (0..count)
            .map(|_| (next(&mut state) % 150) as u16)
            .collect();
        let set = BiomeSet::new(ids.iter().copied());
        for id in 0..300u16 {
            assert_eq!(set.contains(id), ids.contains(&id), "{ids:?} {id}");
        }
    }
    assert!(!BiomeSet::new(std::iter::empty()).contains(0));
}

#[tuff_core::watchdog]
#[test]
fn reused_lazy_point_rules_match_fresh_eager_ones() {
    for seed in [1i64, -4_172_144_997_902_289_642] {
        let generator = NoiseGenerator::new(
            NoiseSettings::get("minecraft:overworld").unwrap(),
            std::sync::Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
            seed,
        );
        let shape = ChunkShape::OVERWORLD;
        let span = GenContext {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: generator.span.sea_level,
        };
        assert!(generator.surface.template(span).lazy_ore);
        let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ seed as u64;
        for pos in [ChunkPos::new(0, 0), ChunkPos::new(-13, 7)] {
            let chunk = generator.generate(pos, shape, Stages::ALL);
            let mut ctx = Context::builder().caches().build();
            let mut reused = generator.surface.point_rules(span);
            let mut ore_hits = 0;
            for n in 0..6000u32 {
                let spread = if n.is_multiple_of(2) { 16 } else { 4000 };
                let x = chunk.min_x() + (next(&mut state) % spread) as i32;
                let z = chunk.min_z() + (next(&mut state) % spread) as i32;
                let y = match n % 3 {
                    0 => -64 + (next(&mut state) % 120) as i32,
                    1 => 40 + (next(&mut state) % 120) as i32,
                    _ => -64 + (next(&mut state) % 384) as i32,
                };
                let fluid = next(&mut state).is_multiple_of(3);
                let mut biomes = Scatter(state);
                let got = reused.top_material(&mut ctx, &mut biomes, &chunk, x, y, z, fluid);
                let mut fresh_ctx = Context::builder().caches().build();
                let mut fresh = generator.surface.point_rules(span);
                fresh.lazy = false;
                let mut biomes = Scatter(state);
                let want = fresh.top_material(&mut fresh_ctx, &mut biomes, &chunk, x, y, z, fluid);
                fresh.release(&mut fresh_ctx);
                assert_eq!(got, want, "seed {seed} at {x} {y} {z}");
                ore_hits += got.is_some_and(|s| {
                    matches!(
                        s.block(),
                        Block::Granite
                            | Block::Tuff
                            | Block::RawIronBlock
                            | Block::RawCopperBlock
                            | Block::CopperOre
                            | Block::DeepslateIronOre
                            | Block::IronOre
                    )
                }) as i32;
            }
            assert!(ore_hits > 0, "no vein was hit");
            reused.release(&mut ctx);
        }
    }
}

fn surface_digest(
    settings: &str,
    source: std::sync::Arc<dyn crate::biome_source::BiomeSource>,
    seed: i64,
    interpret: bool,
) -> u64 {
    let mut generator = NoiseGenerator::new(NoiseSettings::get(settings).unwrap(), source, seed);
    generator.surface.interpret = interpret;
    let shape = if settings == "minecraft:overworld" {
        ChunkShape::OVERWORLD
    } else {
        ChunkShape::new(0, 256)
    };
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (cx, cz) in [(0, 0), (-3, 7), (11, -4)] {
        let chunk = generator.generate(ChunkPos::new(cx, cz), shape, Stages::ALL);
        for y in shape.min_y..shape.min_y + shape.height {
            for z in 0..16 {
                for x in 0..16 {
                    h = (h ^ u64::from(chunk.get(x, y, z).id())).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
    }
    h
}

#[tuff_core::watchdog]
#[test]
fn the_surface_rule_interpreter_matches_the_compiled_rules() {
    use crate::biome_source::TheEnd;
    for seed in [1, -77] {
        let overworld = || -> std::sync::Arc<dyn crate::biome_source::BiomeSource> {
            std::sync::Arc::new(MultiNoise::preset("minecraft:overworld").unwrap())
        };
        assert_eq!(
            surface_digest("minecraft:overworld", overworld(), seed, false),
            surface_digest("minecraft:overworld", overworld(), seed, true),
            "overworld {seed}"
        );
        let nether = || -> std::sync::Arc<dyn crate::biome_source::BiomeSource> {
            std::sync::Arc::new(MultiNoise::preset("minecraft:nether").unwrap())
        };
        assert_eq!(
            surface_digest("minecraft:nether", nether(), seed, false),
            surface_digest("minecraft:nether", nether(), seed, true),
            "nether {seed}"
        );
        let end = || -> std::sync::Arc<dyn crate::biome_source::BiomeSource> {
            std::sync::Arc::new(TheEnd::new())
        };
        assert_eq!(
            surface_digest("minecraft:end", end(), seed, false),
            surface_digest("minecraft:end", end(), seed, true),
            "end {seed}"
        );
    }
}

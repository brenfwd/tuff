//! Bit-exactness tests against values the game's own code produced
//! (`scripts/worldgen-reference.sh` writes the fixture).

use std::collections::HashMap;
use std::sync::OnceLock;

use tuff_vanilla_data::worldgen::{Noise, NoiseSettings};

use crate::data::Loader;
use crate::density::{Context, DensityVolume, Sampler};
use crate::noise::{Octave, Simplex};
use crate::state::RandomState;
use tuff_item::random::{
    LegacyRandom, PositionalRandom, Random, RandomSource, WorldgenRandom, XoroshiroRandom,
};

fn reference() -> &'static HashMap<String, Vec<String>> {
    static REFERENCE: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        let text = include_str!("../../../tests/fixtures/worldgen/reference.txt");
        text.lines()
            .map(|line| {
                let mut parts = line.split(' ');
                let key = parts.next().unwrap().to_string();
                (key, parts.map(str::to_string).collect())
            })
            .collect()
    })
}

fn expect(key: &str) -> &'static [String] {
    reference()
        .get(key)
        .unwrap_or_else(|| panic!("no reference value {key}"))
}

fn f(v: f32) -> String {
    format!("{:x}", v.to_bits())
}

fn d(v: f64) -> String {
    format!("{:x}", v.to_bits())
}

fn crc(values: &[f32]) -> String {
    let mut crc = flate2::Crc::new();
    for v in values {
        crc.update(&v.to_bits().to_le_bytes());
    }
    format!("{:x}", crc.sum())
}

fn sequence(random: &mut impl RandomSource) -> Vec<String> {
    vec![
        random.next_int().to_string(),
        random.next_int_bounded(10).to_string(),
        random.next_int_bounded(16).to_string(),
        random.next_int_bounded(1000000007).to_string(),
        random.next_long().to_string(),
        random.next_bool().to_string(),
        f(random.next_float()),
        d(random.next_double()),
        d(random.next_gaussian()),
        d(random.next_gaussian()),
        random.next_int().to_string(),
    ]
}

#[tuff_core::watchdog]
#[test]
fn random_sources_match_the_game() {
    assert_eq!(sequence(&mut LegacyRandom::new(12345)), expect("legacy"));
    assert_eq!(
        sequence(&mut XoroshiroRandom::new(12345)),
        expect("xoroshiro")
    );
    assert_eq!(
        sequence(&mut XoroshiroRandom::new(-99).fork()),
        expect("xoroshiro_fork")
    );
    let positional = XoroshiroRandom::new(7).fork_positional();
    assert_eq!(
        sequence(&mut positional.at(1, -2, 300)),
        expect("xoroshiro_at")
    );
    assert_eq!(
        sequence(&mut positional.from_hash_of("minecraft:test")),
        expect("xoroshiro_hash")
    );
    assert_eq!(
        sequence(&mut positional.from_seed(424242)),
        expect("xoroshiro_seed")
    );
    let legacy = LegacyRandom::new(7).fork_positional();
    assert!(matches!(legacy, PositionalRandom::Legacy(_)));
    assert_eq!(sequence(&mut legacy.at(1, -2, 300)), expect("legacy_at"));
    assert_eq!(
        sequence(&mut legacy.from_hash_of("minecraft:test")),
        expect("legacy_hash")
    );

    let mut decoration = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(0)));
    let seed = decoration.set_decoration_seed(1, -160, 32);
    assert_eq!(vec![seed.to_string()], expect("decoration_seed"));
    decoration.set_feature_seed(seed, 3, 6);
    assert_eq!(sequence(&mut decoration), expect("feature_seed"));
    let mut large = WorldgenRandom::legacy(0);
    large.set_large_feature_seed(1, 5, -9);
    assert_eq!(sequence(&mut large), expect("large_feature_seed"));
    large.set_large_feature_with_salt(1, 5, -9, 10387312);
    assert_eq!(sequence(&mut large), expect("large_feature_salt"));
}

const POINTS: [[f64; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.5, 1.25, -7.75],
    [1234.567, -64.0, 98765.4321],
    [-3e7, 17.0, 2e7],
];

#[tuff_core::watchdog]
#[test]
fn perlin_and_simplex_noise_match_the_game() {
    let perlin = Octave::perlin(&mut Random::Xoroshiro(XoroshiroRandom::new(42)));
    let simplex = Simplex::new(&mut Random::Xoroshiro(XoroshiroRandom::new(42)), false);
    for (i, p) in POINTS.iter().enumerate() {
        assert_eq!(
            vec![
                f(perlin.get(p[0], p[1], p[2])),
                f(perlin.get_2d(p[0], p[2]))
            ],
            expect(&format!("perlin_{i}")),
            "perlin {i}"
        );
        assert_eq!(
            vec![f(simplex.get_2d(p[0], p[2]))],
            expect(&format!("simplex_{i}")),
            "simplex {i}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn normal_noise_matches_the_game() {
    let state = RandomState::new(NoiseSettings::get("minecraft:overworld").unwrap(), 1);
    let mut loader = Loader::default();
    let create = |name: &str, loader: &mut Loader| {
        let holder = loader.noise(Noise::get(name).unwrap());
        holder.noise.create(&mut state.random.from_hash_of(name))
    };
    let continents = create("minecraft:continentalness", &mut loader);
    let surface = create("minecraft:surface", &mut loader);
    for (i, p) in POINTS.iter().enumerate() {
        assert_eq!(
            vec![
                f(continents.get(p[0], p[1], p[2])),
                f(surface.get(p[0], p[1], p[2]))
            ],
            expect(&format!("continentalness_{i}")),
            "point {i}"
        );
    }
    let volume = DensityVolume::stepped([5, 7, 3], [-37, -64, 1022], [4, 8, 4]);
    let mut buffer = vec![0.0; volume.size()];
    continents.add_to_volume(&mut buffer, &volume, 0.25, 0.0, 1.0);
    assert_eq!(
        vec![crc(&buffer), f(buffer[0]), f(buffer[volume.size() - 1])],
        expect("continentalness_volume")
    );
}

fn router_samplers(state: &RandomState) -> [&Sampler; 8] {
    let r = &state.router;
    [
        &r.temperature,
        &r.vegetation,
        &r.continents,
        &r.erosion,
        &r.depth,
        &r.ridges,
        &r.chunk_surface_level,
        &r.final_density,
    ]
}

fn check_router(dimension: &str) {
    let settings = NoiseSettings::get(&format!("minecraft:{dimension}")).unwrap();
    let state = RandomState::new(settings, 1);
    let points = [
        [0, 0, 0],
        [5, 70, -3],
        [-1000, 40, 2003],
        [123456, -60, -98765],
        [8, 100, 8],
        [1, 2, 3],
    ];
    for (i, p) in points.iter().enumerate() {
        let values: Vec<String> = router_samplers(&state)
            .iter()
            .map(|s| f(s.sample_value(&mut Context::uncached(), p[0], p[1], p[2])))
            .collect();
        assert_eq!(
            values,
            expect(&format!("point_{dimension}_{i}")),
            "point {i}"
        );
    }
    for [cx, cz] in [[0, 0], [10, 10], [-7, 13]] {
        let volume = DensityVolume::blocks(
            [16, settings.height, 16],
            [cx * 16, settings.min_y, cz * 16],
        );
        let mut ctx = Context::builder().caches().build();
        let mut buffer = vec![0.0; volume.size()];
        let values: Vec<String> = router_samplers(&state)
            .iter()
            .map(|s| {
                s.sample_volume(&mut ctx, &mut buffer, &volume);
                crc(&buffer)
            })
            .collect();
        assert_eq!(
            values,
            expect(&format!("volume_{dimension}_{cx}_{cz}")),
            "chunk {cx} {cz}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn overworld_router_matches_the_game() {
    check_router("overworld");
}

#[tuff_core::watchdog]
#[test]
fn nether_router_matches_the_game() {
    check_router("nether");
}

#[tuff_core::watchdog]
#[test]
fn end_router_matches_the_game() {
    check_router("end");
}

mod disabled_stages;
mod loaded;
mod loaded_features;
mod loaded_structures;
mod pipeline_tags;
mod poplar_trees;
mod retrogen;
mod vanilla;

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|v| v.to_bits()).collect()
}

fn assert_programs_match_trees(dimension: &str, seed: i64, chunks: &[[i32; 2]]) {
    let settings = NoiseSettings::get(&format!("minecraft:{dimension}")).unwrap();
    let state = RandomState::new(settings, seed);
    let height = settings.height;
    let min_y = settings.min_y;
    let mut volumes = Vec::new();
    for &[cx, cz] in chunks {
        let (x, z) = (cx * 16, cz * 16);
        volumes.push(DensityVolume::blocks([16, height, 16], [x, min_y, z]));
        volumes.push(DensityVolume::stepped(
            [5, height / 8 + 1, 5],
            [x, min_y, z],
            [4, 8, 4],
        ));
        volumes.push(DensityVolume::stepped(
            [4, 24, 4],
            [x + 3, min_y + 5, z - 2],
            [4, 4, 4],
        ));
        volumes.push(DensityVolume::blocks([1, height, 1], [x + 7, min_y, z + 9]));
        volumes.push(DensityVolume::blocks(
            [16, 1, 16],
            [x, min_y + height / 2, z],
        ));
        volumes.push(DensityVolume::blocks([7, 33, 9], [x - 5, min_y + 3, z + 1]));
    }
    for (index, sampler) in router_samplers(&state).into_iter().enumerate() {
        let mut tree_ctx = Context::builder().caches().build();
        let mut program_ctx = Context::builder().caches().build();
        for volume in &volumes {
            let mut expected = vec![0.0; volume.size()];
            let mut actual = vec![0.0; volume.size()];
            sampler.sample_volume_tree(&mut tree_ctx, &mut expected, volume);
            sampler.sample_volume(&mut program_ctx, &mut actual, volume);
            assert_eq!(
                bits(&actual),
                bits(&expected),
                "{dimension} seed {seed} sampler {index} volume {volume:?}"
            );
        }
        let mut tree_ctx = Context::uncached();
        let mut program_ctx = Context::uncached();
        for volume in volumes.iter().step_by(3) {
            let mut expected = vec![0.0; volume.size()];
            let mut actual = vec![0.0; volume.size()];
            sampler.sample_volume_tree(&mut tree_ctx, &mut expected, volume);
            sampler.sample_volume(&mut program_ctx, &mut actual, volume);
            assert_eq!(
                bits(&actual),
                bits(&expected),
                "uncached {dimension} {seed} {index}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn overworld_interval_selects_run_only_used_variants() {
    let settings = NoiseSettings::get("minecraft:overworld").unwrap();
    let state = RandomState::new(settings, 1);
    let (mut lazy, mut eager) = (0, 0);
    for sampler in router_samplers(&state) {
        if let Sampler::Compiled(c) = sampler {
            let (l, e) = c.lazy_selects();
            lazy += l;
            eager += e;
        }
    }
    assert_eq!((lazy > 0, eager), (true, 0), "lazy {lazy} eager {eager}");
}

const SPREAD: [[i32; 2]; 5] = [[0, 0], [-7, 13], [40, -25], [120, 77], [-300, -310]];

#[tuff_core::watchdog]
#[test]
fn compiled_programs_match_trees_overworld() {
    for seed in [1, -987654321987, 42] {
        assert_programs_match_trees("overworld", seed, &SPREAD);
    }
}

#[tuff_core::watchdog]
#[test]
fn compiled_programs_match_trees_nether_and_end() {
    for seed in [1, 31337] {
        assert_programs_match_trees("nether", seed, &SPREAD[..3]);
        assert_programs_match_trees("end", seed, &[[0, 0], [20, 20], [-70, 35], [3, -90]]);
    }
}

#[tuff_core::watchdog]
#[test]
fn stronghold_rings_match_vanilla() {
    let generator = vanilla::generator("overworld");
    let rings = generator
        .structures()
        .ring_starts(generator, "minecraft:strongholds")
        .expect("strongholds use concentric rings");
    let ours: Vec<String> = rings.iter().map(|p| format!("{},{}", p.x, p.z)).collect();
    assert_eq!(ours, expect("stronghold_rings"));
}

#[tuff_core::watchdog]
#[test]
fn nearest_stronghold_comes_from_the_rings() {
    use tuff_world_model::BlockPos;

    let generator = vanilla::generator("overworld");
    let structures = generator.structures();
    let near = |x, z| {
        structures.nearest_ring_start(generator, "minecraft:strongholds", BlockPos::new(x, 64, z))
    };
    assert_eq!(near(1757, 697), Some(BlockPos::new(1728, 0, 704)));
    assert_eq!(near(-150, -1808), Some(BlockPos::new(-144, 0, -1808)));
    assert_eq!(
        structures.nearest_ring_start(generator, "minecraft:villages", BlockPos::new(0, 0, 0)),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn eyes_of_ender_locate_the_nearest_stronghold() {
    use tuff_vanilla_data::tag::worldgen::structure::EYE_OF_ENDER_LOCATED;
    use tuff_world_model::BlockPos;

    let generator = vanilla::generator("overworld");
    let structures = generator.structures();
    let locate = |x, z| {
        structures.nearest_ring_structure(generator, EYE_OF_ENDER_LOCATED, BlockPos::new(x, 64, z))
    };
    assert_eq!(locate(1757, 697), Some(BlockPos::new(1728, 0, 704)));
    assert_eq!(locate(-150, -1808), Some(BlockPos::new(-144, 0, -1808)));
    assert_eq!(
        structures.nearest_ring_structure(
            generator,
            &["minecraft:village_plains"],
            BlockPos::new(0, 64, 0)
        ),
        None
    );
}
mod loaded_biomes;

#[tuff_core::watchdog]
#[test]
fn jigsaw_assemblies_are_pinned() {
    use tuff_world_model::pos::ChunkPos;

    let generator = vanilla::generator("overworld");
    let structures = generator.structures();
    let shape = vanilla::shape("overworld");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut pieces = 0;
    let started = std::time::Instant::now();
    for name in [
        "minecraft:village_plains",
        "minecraft:village_desert",
        "minecraft:village_snowy",
        "minecraft:pillager_outpost",
        "minecraft:ancient_city",
        "minecraft:trail_ruins",
        "minecraft:trial_chambers",
    ] {
        let index = structures.index_of(name).expect(name);
        for (x, z) in [(0, 0), (7, -3), (-20, 11), (33, 40), (-5, -60)] {
            let Some(start) = structures.forced_start(generator, index, ChunkPos::new(x, z), shape)
            else {
                continue;
            };
            pieces += start.pieces.len();
            for byte in format!("{:?}", start.to_nbt()).bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    eprintln!(
        "jigsaw assembly: {pieces} pieces in {:?}",
        started.elapsed()
    );
    assert_eq!(hash, 13_127_203_242_175_456_260);
}

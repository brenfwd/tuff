//! Noise settings decoded from the built-in pack's JSON must generate
//! exactly what the generated tables do, compiled and interpreted; a pack
//! that changes them changes terrain deterministically.

use std::sync::Arc;

use serde_json::{Value, json};
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

use super::{bits, router_samplers};
use crate::biome_source::{BiomeSource, MultiNoise, TheEnd};
use crate::density::{Context, DensityVolume};
use crate::generator::{NoiseGenerator, Stages};
use crate::loaded::{
    DENSITY_FUNCTION, Linker, MATERIAL_CONDITION, MATERIAL_RULE, NOISE, NOISE_SETTINGS, Options,
    Sources,
};
use crate::state::RandomState;

fn pack() -> Vec<(&'static str, String, Arc<Value>)> {
    let text = std::fs::read_to_string(tuff_vanilla_data::worldgen::PACK_JSON_PATH)
        .expect("the build writes the worldgen pack json");
    let files: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
    let mut out = Vec::new();
    for (path, json) in files {
        let (namespace, rest) = path.split_once('/').unwrap();
        let rest = rest.strip_suffix(".json").unwrap();
        for registry in [
            NOISE,
            DENSITY_FUNCTION,
            MATERIAL_RULE,
            MATERIAL_CONDITION,
            NOISE_SETTINGS,
        ] {
            let dir = registry.strip_prefix("minecraft:").unwrap();
            if let Some(name) = rest.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) {
                out.push((
                    registry,
                    format!("{namespace}:{name}"),
                    Arc::new(json.clone()),
                ));
            }
        }
    }
    assert!(out.len() > 100, "{} worldgen files", out.len());
    out
}

/// A linker that decodes every entry from the built-in pack's JSON.
fn decoded() -> Linker {
    let mut sources = Sources::default();
    for (registry, id, json) in pack() {
        sources.insert(registry, &id, json);
    }
    Linker::new(sources)
}

fn volumes(settings: &NoiseSettings) -> Vec<DensityVolume> {
    let (height, min_y) = (settings.height, settings.min_y);
    let mut volumes = Vec::new();
    for (cx, cz) in [(0, 0), (-7, 12)] {
        let (x, z) = (cx * 16, cz * 16);
        volumes.push(DensityVolume::blocks([16, height, 16], [x, min_y, z]));
        volumes.push(DensityVolume::stepped(
            [5, height / 8 + 1, 5],
            [x, min_y, z],
            [4, 8, 4],
        ));
        volumes.push(DensityVolume::blocks([7, 33, 9], [x - 5, min_y + 3, z + 1]));
    }
    volumes
}

fn samples(state: &RandomState, volumes: &[DensityVolume]) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    for sampler in router_samplers(state) {
        let mut ctx = Context::builder().caches().build();
        for volume in volumes {
            let mut values = vec![0.0; volume.size()];
            sampler.sample_volume(&mut ctx, &mut values, volume);
            out.push(bits(&values));
        }
        let mut ctx = Context::uncached();
        let points: Vec<u32> = [(0, 0, 0), (13, -40, 77), (-301, 64, 5)]
            .iter()
            .map(|&(x, y, z)| sampler.sample_value(&mut ctx, x, y, z).to_bits())
            .collect();
        out.push(points);
    }
    out
}

#[tuff_core::watchdog]
#[test]
fn decoded_density_matches_generated_compiled_and_interpreted() {
    let mut linker = decoded();
    for dimension in ["overworld", "nether", "end"] {
        let id = format!("minecraft:{dimension}");
        let builtin = NoiseSettings::get(&id).unwrap();
        let loaded = linker.settings(&id).unwrap();
        assert!(!std::ptr::eq(builtin, loaded), "{id} was decoded");
        let volumes = volumes(builtin);
        for seed in [1, -4242, 99_000_017] {
            let expected = samples(&RandomState::new(builtin, seed), &volumes);
            for interpret in [false, true] {
                let options = linker.options(interpret).unwrap();
                let state = RandomState::with_options(loaded, seed, options);
                assert!(
                    samples(&state, &volumes) == expected,
                    "{id} seed {seed} interpret {interpret}"
                );
            }
        }
    }
}

fn source(dimension: &str) -> Arc<dyn BiomeSource> {
    match dimension {
        "the_end" => Arc::new(TheEnd::new()),
        "nether" => Arc::new(MultiNoise::preset("minecraft:nether").unwrap()),
        _ => Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
    }
}

fn chunk_hash(generator: &NoiseGenerator, pos: ChunkPos, shape: ChunkShape) -> u64 {
    let chunk = generator.generate(pos, shape, Stages::ALL);
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

const CHUNKS: [(i32, i32); 3] = [(0, 0), (-37, 52), (211, -9)];

fn hashes(generator: &NoiseGenerator, shape: ChunkShape) -> Vec<u64> {
    CHUNKS
        .iter()
        .map(|&(x, z)| chunk_hash(generator, ChunkPos::new(x, z), shape))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn decoded_surface_rules_generate_the_same_chunks() {
    let mut linker = decoded();
    for (dimension, settings, shape, seed) in [
        ("overworld", "minecraft:overworld", ChunkShape::OVERWORLD, 7),
        (
            "overworld",
            "minecraft:overworld",
            ChunkShape::OVERWORLD,
            -123,
        ),
        ("nether", "minecraft:nether", ChunkShape::new(0, 256), 7),
        ("the_end", "minecraft:end", ChunkShape::new(0, 256), 7),
    ] {
        let builtin = NoiseSettings::get(settings).unwrap();
        let expected = hashes(
            &NoiseGenerator::new(builtin, source(dimension), seed),
            shape,
        );
        let loaded = linker.settings(settings).unwrap();
        for interpret in [false, true] {
            let options = linker.options(interpret).unwrap();
            let generator = NoiseGenerator::with_options(loaded, source(dimension), seed, options);
            assert_eq!(
                hashes(&generator, shape),
                expected,
                "{settings} seed {seed} interpret {interpret}"
            );
        }
    }
}

fn overworld_json() -> Value {
    pack()
        .into_iter()
        .find(|(registry, id, _)| *registry == NOISE_SETTINGS && id == "minecraft:overworld")
        .map(|(_, _, json)| (*json).clone())
        .unwrap()
}

fn with(sources: &[(&'static str, &str, Value)]) -> Linker {
    let mut out = Sources::default();
    for (registry, id, json) in sources {
        out.insert(registry, id, Arc::new(json.clone()));
    }
    Linker::new(out)
}

fn overworld_hashes(settings: &'static NoiseSettings, options: Options) -> Vec<u64> {
    let generator = NoiseGenerator::with_options(settings, source("overworld"), 5, options);
    hashes(&generator, ChunkShape::OVERWORLD)
}

#[tuff_core::watchdog]
#[test]
fn a_pack_noise_settings_change_terrain_deterministically() {
    let mut json = overworld_json();
    json["sea_level"] = json!(100);
    json["default_block"] = json!({"Name": "minecraft:deepslate"});
    let builtin = overworld_hashes(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        Options::default(),
    );
    let run = || {
        let mut linker = with(&[(NOISE_SETTINGS, "minecraft:overworld", json.clone())]);
        let settings = linker.settings("minecraft:overworld").unwrap();
        assert_eq!(settings.sea_level, 100);
        overworld_hashes(settings, linker.options(false).unwrap())
    };
    let first = run();
    assert_ne!(first, builtin);
    assert_eq!(first, run());
}

#[tuff_core::watchdog]
#[test]
fn a_pack_density_function_reaches_built_in_settings_that_use_it() {
    let mut linker = with(&[(
        DENSITY_FUNCTION,
        "minecraft:overworld/continents",
        json!(0.8),
    )]);
    let builtin = NoiseSettings::get("minecraft:overworld").unwrap();
    let linked = linker.settings("minecraft:overworld").unwrap();
    assert!(!std::ptr::eq(builtin, linked));
    let state = RandomState::with_options(linked, 3, linker.options(false).unwrap());
    let mut ctx = Context::uncached();
    assert_eq!(
        state.router.continents.sample_value(&mut ctx, 17, 0, -900),
        0.8
    );
    let untouched = NoiseSettings::get("minecraft:end").unwrap();
    assert!(std::ptr::eq(
        linker.settings("minecraft:end").unwrap(),
        untouched
    ));
}

#[tuff_core::watchdog]
#[test]
fn bad_pack_worldgen_entries_are_refused() {
    let mut cyclic = with(&[
        (DENSITY_FUNCTION, "example:a", json!("example:b")),
        (
            DENSITY_FUNCTION,
            "example:b",
            json!({"type": "minecraft:abs", "input": "example:a"}),
        ),
    ]);
    assert!(
        cyclic
            .function("example:a")
            .unwrap_err()
            .contains("references itself")
    );
    let mut unknown = with(&[(
        DENSITY_FUNCTION,
        "example:c",
        json!({"type": "minecraft:abs", "argument": 1.0}),
    )]);
    assert!(unknown.function("example:c").is_err());
    let mut missing = with(&[(DENSITY_FUNCTION, "example:d", json!("example:nope"))]);
    assert!(missing.function("example:d").is_err());
}

#[tuff_core::watchdog]
#[test]
fn unmodified_registries_keep_the_generated_tables() {
    let registries = tuff_item::registries::Registries::vanilla();
    assert!(crate::loaded::resolve(&registries, "minecraft:overworld", true).is_none());
}

#[tuff_core::watchdog]
#[test]
fn binding_pack_registries_switches_a_generator_to_the_pack_settings() {
    use tuff_core::world::generator::{ChunkGenerator, GeneratorData};
    let mut json = overworld_json();
    json["sea_level"] = json!(90);
    let registry = tuff_vanilla_data::data_registry::ALL
        .iter()
        .find(|r| r.key == NOISE_SETTINGS)
        .unwrap();
    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        registry,
        "minecraft:overworld".to_owned(),
        "file/sea",
        serde_json::to_vec(&json).unwrap(),
    );
    let registries = loader.freeze().unwrap();
    let dimensions = tuff_core::storage::level::DimensionSettings::vanilla();
    let overworld = dimensions
        .iter()
        .find(|d| d.key == tuff_core::storage::level::OVERWORLD)
        .unwrap();
    let generator = crate::chunk_generator::vanilla(overworld, 1).unwrap();
    generator.bind(&GeneratorData {
        registries: &registries,
        compile_loaded: true,
        shape: tuff_world_model::chunk::ChunkShape::OVERWORLD,
        disabled: Default::default(),
    });
    assert_eq!(generator.sea_level(), 90);
    let untouched = crate::chunk_generator::vanilla(overworld, 1).unwrap();
    untouched.bind(&GeneratorData {
        registries: &tuff_item::registries::Registries::vanilla(),
        compile_loaded: true,
        shape: tuff_world_model::chunk::ChunkShape::OVERWORLD,
        disabled: Default::default(),
    });
    assert_eq!(untouched.sea_level(), 63);
}

#[tuff_core::watchdog]
#[test]
fn a_bad_pack_worldgen_entry_rejects_the_freeze() {
    tuff_item::registries::add_validator("vanilla-worldgen", crate::loaded::validate);
    let registry = tuff_vanilla_data::data_registry::ALL
        .iter()
        .find(|r| r.key == DENSITY_FUNCTION)
        .unwrap();
    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        registry,
        "example:broken".to_owned(),
        "file/broken",
        serde_json::to_vec(&json!({"type": "minecraft:abs", "argument": 1.0})).unwrap(),
    );
    let err = loader
        .freeze()
        .err()
        .expect("a bad density function rejects the load");
    assert_eq!(err.registry, DENSITY_FUNCTION);
    assert_eq!(err.id, "example:broken");
    assert_eq!(err.pack, "file/broken");
}

/// Chunks per second for the generated path and the loaded JSON path,
/// compiled and interpreted. Run in release:
/// `cargo test --release -p vanilla-worldgen --lib loaded_worldgen_speed -- --ignored --nocapture`
#[tuff_core::watchdog]
#[test]
#[ignore]
fn loaded_worldgen_speed() {
    const N: i32 = 24;
    let time = |generator: &NoiseGenerator| {
        let start = std::time::Instant::now();
        for i in 0..N {
            let chunk =
                generator.generate(ChunkPos::new(i * 3, -i), ChunkShape::OVERWORLD, Stages::ALL);
            std::hint::black_box(chunk);
        }
        f64::from(N) / start.elapsed().as_secs_f64()
    };
    let mut linker = decoded();
    let builtin = NoiseSettings::get("minecraft:overworld").unwrap();
    let loaded = linker.settings("minecraft:overworld").unwrap();
    for round in 0..3 {
        let generated = time(&NoiseGenerator::new(builtin, source("overworld"), 7));
        let compiled = time(&NoiseGenerator::with_options(
            loaded,
            source("overworld"),
            7,
            linker.options(false).unwrap(),
        ));
        let interpreted = time(&NoiseGenerator::with_options(
            loaded,
            source("overworld"),
            7,
            linker.options(true).unwrap(),
        ));
        println!(
            "round {round}: generated {generated:.1} loaded-compiled {compiled:.1} loaded-interpreted {interpreted:.1} chunks/s"
        );
    }
}

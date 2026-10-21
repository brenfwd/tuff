//! Configured and placed features and biome feature lists from pack JSON:
//! the built-in JSON fed as pack entries decorates exactly like the
//! generated tables, and a pack can add a placed feature to biomes.

use std::sync::Arc;

use serde_json::{Value, json};
use tuff_vanilla_data::worldgen::{CONFIGURED_FEATURES, NoiseSettings, PLACED_FEATURES};
use tuff_world_model::chunk::{ChunkShape, biome_name};
use tuff_world_model::pos::ChunkPos;

use crate::biome_source::{BiomeSource, MultiNoise};
use crate::feature::Registry;
use crate::feature::level::Region;
use crate::generator::{NoiseGenerator, Stages};
use crate::loaded::Options;
use crate::loaded::features::FeatureSources;

fn builtin_as_pack() -> FeatureSources {
    let mut sources = FeatureSources::default();
    for (id, text) in CONFIGURED_FEATURES {
        let json: Value = serde_json::from_str(text).unwrap();
        sources.configured.insert(id.to_string(), Arc::new(json));
    }
    for (id, text) in PLACED_FEATURES {
        let json: Value = serde_json::from_str(text).unwrap();
        sources.placed.insert(id.to_string(), Arc::new(json));
    }
    for biome in source().possible_biomes() {
        let data = tuff_vanilla_data::biome::Biome::get(biome_name(biome)).unwrap();
        let json = json!({ "features": data.features });
        sources.insert_biome(biome_name(biome), &json).unwrap();
    }
    sources
}

fn source() -> Arc<dyn BiomeSource> {
    Arc::new(MultiNoise::preset("minecraft:overworld").unwrap())
}

fn generator(features: Option<FeatureSources>, seed: i64) -> NoiseGenerator {
    let mut options = Options::default();
    options.features = features.map(Arc::new);
    NoiseGenerator::with_options(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        source(),
        seed,
        options,
    )
}

fn decorated(generator: &NoiseGenerator, center: ChunkPos) -> crate::proto::ProtoChunk {
    let mut window = Vec::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            window.push(generator.generate(pos, ChunkShape::OVERWORLD, Stages::ALL));
        }
    }
    let mut region = Region::new(generator, center, window);
    generator.decorate(&mut region);
    region
        .into_chunks()
        .into_iter()
        .find(|c| c.pos == center)
        .unwrap()
}

fn hash(chunk: &crate::proto::ProtoChunk) -> u64 {
    let shape = ChunkShape::OVERWORLD;
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

#[tuff_core::watchdog]
#[test]
fn every_builtin_feature_decodes_as_a_pack_entry() {
    let sources = Arc::new(builtin_as_pack());
    let mut registry = Registry::with_sources(Some(sources.clone()));
    for id in sources.configured.keys() {
        registry
            .feature(&Value::String(id.clone()))
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    for id in sources.placed.keys() {
        registry
            .placed(&Value::String(id.clone()))
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    assert!(registry.errors.is_empty(), "{:?}", registry.errors);
    let builtin = Registry::builtin();
    let mut reference = builtin;
    for id in sources.configured.keys() {
        reference.feature(&Value::String(id.clone())).unwrap();
    }
    assert_eq!(registry.missing, reference.missing);
}

#[tuff_core::watchdog]
#[test]
fn pack_json_of_builtin_features_decorates_the_same_chunks() {
    for seed in [3, -77_001] {
        let builtin = generator(None, seed);
        let loaded = generator(Some(builtin_as_pack()), seed);
        for (x, z) in [(0, 0), (-21, 40)] {
            let pos = ChunkPos::new(x, z);
            assert_eq!(
                hash(&decorated(&loaded, pos)),
                hash(&decorated(&builtin, pos)),
                "seed {seed} chunk {x} {z}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn a_pack_placed_feature_added_to_biomes_is_placed() {
    let gold = crate::json::block_state(&json!({ "Name": "minecraft:gold_block" })).unwrap();
    let mut sources = FeatureSources::default();
    sources.configured.insert(
        "test:gold".into(),
        Arc::new(json!({
            "type": "minecraft:simple_block",
            "to_place": {
                "type": "minecraft:simple",
                "state": { "Name": "minecraft:gold_block" }
            }
        })),
    );
    sources.placed.insert(
        "test:gold".into(),
        Arc::new(json!({
            "feature": "test:gold",
            "placement": [
                { "type": "minecraft:in_square" },
                { "type": "minecraft:heightmap", "heightmap": "WORLD_SURFACE_WG" },
                { "type": "minecraft:biome" }
            ]
        })),
    );
    for biome in source().possible_biomes() {
        let data = tuff_vanilla_data::biome::Biome::get(biome_name(biome)).unwrap();
        let mut steps: Vec<Vec<String>> = data
            .features
            .iter()
            .map(|s| s.iter().map(|n| crate::json::qualify(n)).collect())
            .collect();
        steps.resize(10, Vec::new());
        steps[9].push("test:gold".into());
        let json = json!({ "features": steps });
        sources.insert_biome(biome_name(biome), &json).unwrap();
    }
    let generator = generator(Some(sources), 11);
    let chunk = decorated(&generator, ChunkPos::new(2, -3));
    let shape = ChunkShape::OVERWORLD;
    let count = (shape.min_y..shape.min_y + shape.height)
        .flat_map(|y| (0..16).flat_map(move |z| (0..16).map(move |x| (x, y, z))))
        .filter(|&(x, y, z)| chunk.get(x, y, z) == gold)
        .count();
    assert_eq!(count, 1);
    let plain = decorated(&self::generator(None, 11), ChunkPos::new(2, -3));
    assert!(
        (shape.min_y..shape.min_y + shape.height)
            .flat_map(|y| (0..16).flat_map(move |z| (0..16).map(move |x| (x, y, z))))
            .all(|(x, y, z)| plain.get(x, y, z) != gold)
    );
}

#[tuff_core::watchdog]
#[test]
fn biome_features_expand_tags_and_take_inline_placed_features() {
    let plains = tuff_world_model::chunk::plains();
    let mut sources = FeatureSources::default();
    sources
        .insert_biome(
            "minecraft:plains",
            &json!({ "features": [
                [],
                "#test:lakes",
                [{ "feature": "minecraft:ore_gold", "placement": [] }]
            ] }),
        )
        .unwrap();
    let mut builders = tuff_datapack::tags::vanilla_builders();
    builders
        .entry("minecraft:worldgen/placed_feature")
        .or_default()
        .insert(
            "test:lakes".to_owned(),
            vec![tuff_datapack::tags::TagEntry {
                id: "minecraft:lake_lava_surface".to_owned(),
                tag: false,
                required: true,
            }],
        );
    let tags = Arc::new(tuff_datapack::tags::TagSet::resolve_all(&builders).0);
    let decorator = tuff_datapack::tags::sync_scope(tags, || {
        crate::feature::Decorator::new(&[plains], Some(Arc::new(sources)))
    });
    assert!(decorator.has_feature(plains, "minecraft:lake_lava_surface"));
    assert!(decorator.has_feature(plains, &format!("tuff:inline/biome_{plains}/2/0")));
    assert!(!decorator.has_feature(plains, "minecraft:patch_grass_plain"));
}

#[tuff_core::watchdog]
#[test]
fn self_referencing_features_are_rejected() {
    let mut sources = FeatureSources::default();
    sources.configured.insert(
        "test:loop".into(),
        Arc::new(json!({
            "type": "minecraft:random_boolean_selector",
            "feature_true": "test:loop",
            "feature_false": "test:loop"
        })),
    );
    sources.placed.insert(
        "test:loop".into(),
        Arc::new(json!({ "feature": "test:loop", "placement": [] })),
    );
    let mut registry = Registry::with_sources(Some(Arc::new(sources)));
    let err = registry.feature(&json!("test:loop")).err().unwrap();
    assert!(err.contains("references itself"), "{err}");
}

fn data_registry(key: &str) -> &'static tuff_vanilla_data::data_registry::DataRegistry {
    tuff_vanilla_data::data_registry::ALL
        .iter()
        .find(|r| r.key == key)
        .unwrap()
}

#[tuff_core::watchdog]
#[test]
fn pack_features_are_validated_at_freeze_and_bound() {
    use crate::loaded::features::{BIOME, PLACED_FEATURE};
    tuff_item::registries::add_validator("vanilla-worldgen", crate::loaded::validate);

    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        data_registry(PLACED_FEATURE),
        "example:broken".to_owned(),
        "file/broken",
        serde_json::to_vec(&json!({ "feature": "example:nothing", "placement": [] })).unwrap(),
    );
    let err = loader
        .freeze()
        .err()
        .expect("an unknown feature rejects the load");
    assert_eq!(err.registry, PLACED_FEATURE);
    assert_eq!(err.id, "example:broken");

    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        data_registry(BIOME),
        "minecraft:plains".to_owned(),
        "file/plains",
        serde_json::to_vec(&json!({
            "has_precipitation": true, "temperature": 0.8, "downfall": 0.4,
            "effects": { "water_color": 4159204 },
            "features": [[], ["example:missing"]]
        }))
        .unwrap(),
    );
    let err = loader
        .freeze()
        .err()
        .expect("a missing biome feature rejects the load");
    assert_eq!(err.registry, BIOME);
    assert_eq!(err.id, "minecraft:plains");

    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        data_registry(PLACED_FEATURE),
        "example:ok".to_owned(),
        "file/ok",
        serde_json::to_vec(&json!({ "feature": "minecraft:oak", "placement": [] })).unwrap(),
    );
    let registries = loader.freeze().unwrap();
    let (_, options) = crate::loaded::resolve(&registries, "minecraft:overworld", true)
        .expect("pack features bind")
        .unwrap();
    assert!(options.features.unwrap().placed.contains_key("example:ok"));
}

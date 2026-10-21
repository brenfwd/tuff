//! Structures, structure sets, template pools and processor lists from
//! data pack JSON: the built-in pack's JSON decodes to the generated
//! tables, and pack entries replace or join them in a generator.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tuff_vanilla_data::structure::{Structure, StructureSet};

use crate::loaded::structures::{
    PROCESSOR_LIST, STRUCTURE, STRUCTURE_SET, StructureSources, TEMPLATE_POOL, decode_set,
    decode_structure,
};
use crate::structure::Structures;

fn pack(dir: &str) -> Vec<(String, Value)> {
    let text = std::fs::read_to_string(tuff_vanilla_data::worldgen::PACK_JSON_PATH)
        .expect("the build writes the worldgen pack json");
    let files: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
    files
        .into_iter()
        .filter_map(|(path, json)| {
            let (namespace, rest) = path.split_once('/')?;
            let name = rest
                .strip_suffix(".json")?
                .strip_prefix(dir)?
                .strip_prefix('/')?;
            Some((format!("{namespace}:{name}"), json))
        })
        .collect()
}

fn same_json(a: &str, b: &str) -> bool {
    let a: Value = serde_json::from_str(a).unwrap();
    let b: Value = serde_json::from_str(b).unwrap();
    a == b
}

#[tuff_core::watchdog]
#[test]
fn built_in_structure_json_decodes_to_the_generated_structures() {
    let structures = pack("worldgen/structure");
    assert_eq!(
        structures.len(),
        tuff_vanilla_data::structure::STRUCTURES.len()
    );
    for (name, json) in structures {
        let decoded = decode_structure(&name, &json).unwrap_or_else(|e| panic!("{name}: {e}"));
        let generated = Structure::get(&name).unwrap();
        assert_eq!(decoded.kind, generated.kind, "{name}");
        assert_eq!(decoded.step, generated.step, "{name}");
        assert_eq!(
            decoded.terrain_adaptation, generated.terrain_adaptation,
            "{name}"
        );
        assert_eq!(decoded.biome_tag, generated.biome_tag, "{name}");
        let a: BTreeSet<&str> = decoded.biomes.iter().copied().collect();
        let b: BTreeSet<&str> = generated.biomes.iter().copied().collect();
        assert_eq!(a, b, "{name}");
        assert!(same_json(decoded.config, generated.config), "{name}");
        assert!(
            same_json(decoded.spawn_overrides, generated.spawn_overrides),
            "{name}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn built_in_structure_set_json_decodes_to_the_generated_sets() {
    let sets = pack("worldgen/structure_set");
    assert_eq!(
        sets.len(),
        tuff_vanilla_data::structure::STRUCTURE_SETS.len()
    );
    for (name, json) in sets {
        let decoded = decode_set(&name, &json, &|s| Structure::get(s).is_some(), &|s| {
            StructureSet::get(s).is_some()
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let generated = StructureSet::get(&name).unwrap();
        assert_eq!(decoded.structures, generated.structures, "{name}");
        assert_eq!(
            format!("{:?}", decoded.placement),
            format!("{:?}", generated.placement),
            "{name}"
        );
    }
}

fn sources(entries: &[(&'static str, &str, Value)]) -> Arc<StructureSources> {
    let mut loader = tuff_item::registries::Loader::default();
    for (registry, id, json) in entries {
        loader.insert(
            tuff_vanilla_data::data_registry::DataRegistry::get(registry).unwrap(),
            (*id).to_owned(),
            "test",
            serde_json::to_vec(json).unwrap(),
        );
    }
    let registries = loader.freeze().unwrap_or_else(|e| panic!("{e}"));
    Arc::new(StructureSources::from_registries(&registries).unwrap())
}

#[tuff_core::watchdog]
#[test]
fn pack_structures_and_sets_replace_and_join_the_built_in_ones() {
    let igloo = Structure::get("minecraft:igloo").unwrap();
    let mut copy: Value = serde_json::from_str(igloo.config).unwrap();
    let map = copy.as_object_mut().unwrap();
    map.insert("type".into(), json!("minecraft:igloo"));
    map.insert("biomes".into(), json!("minecraft:plains"));
    map.insert("step".into(), json!("surface_structures"));
    map.insert("spawn_overrides".into(), json!({}));
    let sources = sources(&[
        (STRUCTURE, "test:plains_igloo", copy),
        (
            STRUCTURE_SET,
            "test:igloos",
            json!({
                "structures": [{"structure": "test:plains_igloo", "weight": 1}],
                "placement": {"type": "minecraft:random_spread", "spacing": 4, "separation": 2, "salt": 7}
            }),
        ),
        (
            STRUCTURE_SET,
            "minecraft:igloos",
            json!({
                "structures": [{"structure": "minecraft:igloo", "weight": 1}],
                "placement": {"type": "minecraft:random_spread", "spacing": 8, "separation": 3, "salt": 14357618}
            }),
        ),
    ]);
    let plains = tuff_world_model::chunk::plains();
    let structures = Structures::with_sources(&[plains], 1, Some(sources), None);
    let index = structures.index_of("test:plains_igloo").expect("added");
    assert!(structures.entries[index].allows(plains));
    assert!(
        structures
            .reachable_sets()
            .any(|set| set.data.name == "test:igloos")
    );
    let replaced = structures
        .sets
        .iter()
        .find(|set| set.data.name == "minecraft:igloos")
        .unwrap();
    assert!(matches!(
        replaced.data.placement,
        tuff_vanilla_data::structure::StructurePlacement::RandomSpread { spacing: 8, .. }
    ));
}

#[tuff_core::watchdog]
#[test]
fn bad_pack_structures_reject_the_load() {
    let mut loader = tuff_item::registries::Loader::default();
    tuff_item::registries::add_validator("vanilla-worldgen", crate::loaded::validate);
    loader.insert(
        tuff_vanilla_data::data_registry::DataRegistry::get(STRUCTURE_SET).unwrap(),
        "test:broken".to_owned(),
        "test",
        serde_json::to_vec(&json!({
            "structures": [{"structure": "test:nothing", "weight": 1}],
            "placement": {"type": "minecraft:random_spread", "spacing": 4, "separation": 2, "salt": 7}
        }))
        .unwrap(),
    );
    let err = loader.freeze().err().expect("rejected");
    assert_eq!(
        (err.registry, err.id.as_str()),
        (STRUCTURE_SET, "test:broken")
    );

    let mut loader = tuff_item::registries::Loader::default();
    loader.insert(
        tuff_vanilla_data::data_registry::DataRegistry::get(TEMPLATE_POOL).unwrap(),
        "test:pool".to_owned(),
        "test",
        serde_json::to_vec(&json!({"fallback": "minecraft:empty", "elements": [{"weight": 1}]}))
            .unwrap(),
    );
    let err = loader.freeze().err().expect("rejected");
    assert_eq!(err.registry, TEMPLATE_POOL);
}

#[tuff_core::watchdog]
#[test]
fn pack_pools_and_processor_lists_shadow_the_built_in_ones_in_scope() {
    let sources = sources(&[
        (
            TEMPLATE_POOL,
            "minecraft:village/plains/town_centers",
            json!({"fallback": "minecraft:empty", "elements": [{"weight": 1, "element": {
                "element_type": "minecraft:single_pool_element",
                "location": "minecraft:igloo/top",
                "processors": "test:nothing_changes",
                "projection": "rigid"
            }}]}),
        ),
        (
            PROCESSOR_LIST,
            "test:nothing_changes",
            json!({"processors": []}),
        ),
    ]);
    let outside = crate::structure::pool::get("minecraft:village/plains/town_centers").unwrap();
    assert!(outside.templates.len() > 1);
    crate::loaded::structures::scope(Some(&sources), || {
        let inside = crate::structure::pool::get("minecraft:village/plains/town_centers").unwrap();
        assert_eq!(inside.templates.len(), 1);
        assert!(crate::structure::processor::named("test:nothing_changes").is_ok());
    });
    assert!(crate::structure::processor::named("test:nothing_changes").is_err());
}

#[tuff_core::watchdog]
#[test]
fn a_pack_structure_may_start_in_the_biomes_of_a_pack_only_tag() {
    let igloo = Structure::get("minecraft:igloo").unwrap();
    let mut copy: Value = serde_json::from_str(igloo.config).unwrap();
    let map = copy.as_object_mut().unwrap();
    map.insert("type".into(), json!("minecraft:igloo"));
    map.insert("biomes".into(), json!("#test:cold_enough"));
    map.insert("step".into(), json!("surface_structures"));
    let sources = sources(&[(STRUCTURE, "test:tagged_igloo", copy)]);
    let mut builders = tuff_datapack::tags::vanilla_builders();
    builders
        .entry("minecraft:worldgen/biome")
        .or_default()
        .insert(
            "test:cold_enough".to_owned(),
            vec![tuff_datapack::tags::TagEntry {
                id: "minecraft:plains".to_owned(),
                tag: false,
                required: true,
            }],
        );
    let tags = Arc::new(tuff_datapack::tags::TagSet::resolve_all(&builders).0);
    let plains = tuff_world_model::chunk::plains();
    let desert = tuff_world_model::chunk::biome_id("minecraft:desert").unwrap();
    let structures = Structures::with_sources(&[plains], 1, Some(sources), None);
    let index = structures.index_of("test:tagged_igloo").unwrap();
    let entry = &structures.entries[index];
    assert!(!entry.allows(plains), "the built-in tags lack it");
    tuff_datapack::tags::sync_scope(tags, || {
        assert!(entry.allows(plains));
        assert!(!entry.allows(desert));
    });
}

//! `worldgen/structure_set`, `worldgen/structure`, `worldgen/template_pool`
//! and `worldgen/processor_list`, plus the structure templates
//! (`structure/**.nbt`) embedded as one blob.
//!
//! Structure sets and the settings every structure shares are typed. The
//! kind-specific structure fields, template pools and processor lists stay
//! JSON, parsed by the runtime's strict parsers, but every kind and field is
//! checked here and every reference (structures, pools, processor lists,
//! templates, placed features, biomes) must resolve, so new data fails the
//! build.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use super::worldgen::{f32_lit, field, int, object, string};
use crate::input::Input;
use crate::names::str_literal;

fn collect<'a>(input: &'a Input, dir: &str) -> BTreeMap<String, &'a Value> {
    let mut out = BTreeMap::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        if let Some(name) = rest
            .strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .and_then(|r| r.strip_suffix(".json"))
        {
            out.insert(format!("{namespace}:{name}"), json);
        }
    }
    out
}

/// `Some("tag")` when a biome set names a tag, else `None`.
fn biome_tag(value: &Value) -> String {
    match value {
        Value::String(name) if name.starts_with('#') => {
            format!("Some({})", str_literal(&qualify(&name[1..])))
        }
        _ => "None".to_owned(),
    }
}

fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn kind_of<'v>(m: &'v Map<String, Value>, key: &str) -> Result<&'v str> {
    let kind = string(m, key)?;
    kind.strip_prefix("minecraft:")
        .or((!kind.contains(':')).then_some(kind))
        .with_context(|| format!("unmodeled kind {kind}"))
}

fn check_fields(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<()> {
    for key in m.keys() {
        ensure!(
            allowed.contains(&key.as_str()),
            "unknown field {key} on {what}"
        );
    }
    Ok(())
}

fn table_fields<'t>(table: &'t [(&str, &[&str])], kind: &str, what: &str) -> Result<&'t [&'t str]> {
    table
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, fields)| *fields)
        .with_context(|| format!("unmodeled {what} kind {kind}"))
}

/// Kind-specific structure fields, beside the shared settings.
const STRUCTURE_FIELDS: &[(&str, &[&str])] = &[
    ("buried_treasure", &[]),
    ("desert_pyramid", &[]),
    ("end_city", &[]),
    ("fortress", &[]),
    ("igloo", &[]),
    (
        "jigsaw",
        &[
            "dimension_padding",
            "liquid_settings",
            "max_distance_from_center",
            "pool_aliases",
            "project_start_to_heightmap",
            "size",
            "start_height",
            "start_jigsaw_name",
            "start_pool",
            "use_expansion_hack",
        ],
    ),
    ("jungle_temple", &[]),
    ("mineshaft", &["mineshaft_type"]),
    ("nether_fossil", &["height"]),
    ("ocean_monument", &[]),
    (
        "ocean_ruin",
        &["biome_temp", "cluster_probability", "large_probability"],
    ),
    ("ruined_portal", &["setups"]),
    ("shipwreck", &["is_beached"]),
    ("stronghold", &[]),
    ("swamp_hut", &[]),
    ("woodland_mansion", &[]),
];
const SHARED_FIELDS: [&str; 5] = [
    "type",
    "biomes",
    "spawn_overrides",
    "step",
    "terrain_adaptation",
];
const ELEMENT_FIELDS: &[(&str, &[&str])] = &[
    ("empty_pool_element", &[]),
    ("feature_pool_element", &["feature", "projection"]),
    (
        "legacy_single_pool_element",
        &["location", "processors", "projection"],
    ),
    ("list_pool_element", &["elements", "projection"]),
    (
        "single_pool_element",
        &["location", "processors", "projection"],
    ),
];
const PROCESSOR_FIELDS: &[(&str, &[&str])] = &[
    ("blackstone_replace", &[]),
    ("block_age", &["mossiness"]),
    ("block_ignore", &["blocks"]),
    ("block_rot", &["integrity", "rottable_blocks"]),
    ("capped", &["delegate", "limit"]),
    ("gravity", &["heightmap", "offset"]),
    ("jigsaw_replacement", &[]),
    ("lava_submerged_block", &[]),
    ("nop", &[]),
    ("protected_blocks", &["value"]),
    ("rule", &["rules"]),
];
const RULE_FIELDS: [&str; 5] = [
    "block_entity_modifier",
    "input_predicate",
    "location_predicate",
    "output_state",
    "position_predicate",
];
const RULE_TEST_FIELDS: &[(&str, &[&str])] = &[
    ("all_of", &["rules"]),
    ("always_true", &[]),
    ("any_of", &["rules"]),
    ("height_match", &["max_inclusive", "min_inclusive"]),
    ("not", &["rule"]),
    ("block_match", &["block"]),
    ("blockstate_match", &["block_state"]),
    ("random_block_match", &["block", "probability"]),
    ("random_blockstate_match", &["block_state", "probability"]),
    ("tag_match", &["tag"]),
];
const POS_RULE_TEST_FIELDS: &[(&str, &[&str])] = &[
    ("always_true", &[]),
    (
        "axis_aligned_linear_pos",
        &["axis", "max_chance", "max_dist", "min_chance", "min_dist"],
    ),
    (
        "linear_pos",
        &["max_chance", "max_dist", "min_chance", "min_dist"],
    ),
];
const BLOCK_ENTITY_MODIFIER_FIELDS: &[(&str, &[&str])] = &[
    ("append_loot", &["loot_table"]),
    ("clear", &[]),
    ("passthrough", &[]),
];
const SETUP_FIELDS: [&str; 8] = [
    "air_pocket_probability",
    "can_be_cold",
    "mossiness",
    "overgrown",
    "placement",
    "replace_with_blackstone",
    "vines",
    "weight",
];

struct Gen<'a> {
    input: &'a Input,
    structures: BTreeMap<String, &'a Value>,
    pools: BTreeMap<String, &'a Value>,
    processor_lists: BTreeMap<String, &'a Value>,
    placed: BTreeMap<String, &'a Value>,
    biome_tags: BTreeMap<String, Vec<String>>,
    block_tags: BTreeSet<String>,
}

impl Gen<'_> {
    fn template_exists(&self, name: &str) -> bool {
        let name = qualify(name);
        let (namespace, path) = name.split_once(':').unwrap();
        self.input
            .structure_templates
            .contains_key(&format!("{namespace}/structure/{path}.nbt"))
    }

    fn biomes(&self, value: &Value) -> Result<Vec<String>> {
        let names: Vec<String> = match value {
            Value::String(name) if name.starts_with('#') => self
                .biome_tags
                .get(&qualify(&name[1..]))
                .with_context(|| format!("unknown biome tag {name}"))?
                .clone(),
            Value::String(name) => vec![qualify(name)],
            Value::Array(list) => list
                .iter()
                .map(|v| v.as_str().map(qualify).context("biome names"))
                .collect::<Result<_>>()?,
            _ => bail!("bad biome set"),
        };
        for name in &names {
            let (namespace, path) = name.split_once(':').unwrap();
            ensure!(
                self.input
                    .data_json
                    .contains_key(&format!("{namespace}/worldgen/biome/{path}.json")),
                "unknown biome {name}"
            );
        }
        Ok(names)
    }

    fn block(&self, name: &str) -> Result<()> {
        ensure!(
            self.input.blocks.contains_key(&qualify(name)),
            "unknown block {name}"
        );
        Ok(())
    }

    fn block_set(&self, value: &Value) -> Result<()> {
        match value {
            Value::String(name) if name.starts_with('#') => ensure!(
                self.block_tags.contains(&qualify(&name[1..])),
                "unknown block tag {name}"
            ),
            Value::String(name) => self.block(name)?,
            Value::Array(list) => {
                for name in list {
                    self.block(name.as_str().context("block names")?)?;
                }
            }
            _ => bail!("bad block set"),
        }
        Ok(())
    }

    fn block_state(&self, value: &Value) -> Result<()> {
        let m = match value {
            Value::String(name) => return self.block(name),
            other => object(other, &["id", "properties", "Name", "Properties"])?,
        };
        self.block(string(m, "id").or_else(|_| string(m, "Name"))?)
    }

    fn rule_test(&self, value: &Value) -> Result<()> {
        let m = value.as_object().context("rule test is an object")?;
        let kind = kind_of(m, "predicate_type")?;
        let fields = table_fields(RULE_TEST_FIELDS, kind, "rule test")?;
        for key in m.keys() {
            ensure!(
                key == "predicate_type" || fields.contains(&key.as_str()),
                "unknown field {key} on rule test {kind}"
            );
        }
        match kind {
            "block_match" | "random_block_match" => self.block(string(m, "block")?)?,
            "blockstate_match" | "random_blockstate_match" => {
                self.block_state(field(m, "block_state")?)?
            }
            "all_of" | "any_of" => {
                for rule in field(m, "rules")?.as_array().context("rules is a list")? {
                    self.rule_test(rule)?;
                }
            }
            "not" => self.rule_test(field(m, "rule")?)?,
            "tag_match" => {
                let tag = qualify(string(m, "tag")?);
                ensure!(self.block_tags.contains(&tag), "unknown block tag {tag}");
            }
            _ => {}
        }
        Ok(())
    }

    fn typed(&self, value: &Value, key: &str, table: &[(&str, &[&str])], what: &str) -> Result<()> {
        let m = value
            .as_object()
            .with_context(|| format!("{what} is an object"))?;
        let kind = kind_of(m, key)?;
        let fields = table_fields(table, kind, what)?;
        for name in m.keys() {
            ensure!(
                name == key || fields.contains(&name.as_str()),
                "unknown field {name} on {what} {kind}"
            );
        }
        Ok(())
    }

    fn processor(&self, value: &Value) -> Result<()> {
        self.typed(value, "processor_type", PROCESSOR_FIELDS, "processor")?;
        let m = value.as_object().unwrap();
        match kind_of(m, "processor_type")? {
            "rule" => {
                for rule in field(m, "rules")?.as_array().context("rules is a list")? {
                    let r = object(rule, &RULE_FIELDS)?;
                    self.rule_test(field(r, "input_predicate")?)?;
                    self.rule_test(field(r, "location_predicate")?)?;
                    if let Some(pos) = r.get("position_predicate") {
                        self.typed(
                            pos,
                            "predicate_type",
                            POS_RULE_TEST_FIELDS,
                            "position rule test",
                        )?;
                    }
                    if let Some(modifier) = r.get("block_entity_modifier") {
                        self.typed(
                            modifier,
                            "type",
                            BLOCK_ENTITY_MODIFIER_FIELDS,
                            "block entity modifier",
                        )?;
                    }
                    self.block_state(field(r, "output_state")?)?;
                }
            }
            "capped" => self.processor(field(m, "delegate")?)?,
            "block_rot" => {
                if let Some(blocks) = m.get("rottable_blocks") {
                    self.block_set(blocks)?;
                }
            }
            "protected_blocks" => self.block_set(field(m, "value")?)?,
            "block_ignore" => {
                for state in field(m, "blocks")?.as_array().context("blocks is a list")? {
                    self.block_state(state)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn processor_list(&self, value: &Value) -> Result<()> {
        match value {
            Value::String(name) => ensure!(
                self.processor_lists.contains_key(&qualify(name)),
                "unknown processor list {name}"
            ),
            Value::Array(list) => {
                for processor in list {
                    self.processor(processor)?;
                }
            }
            Value::Object(m) => {
                check_fields(m, &["processors"], "processor list")?;
                for processor in field(m, "processors")?
                    .as_array()
                    .context("processors is a list")?
                {
                    self.processor(processor)?;
                }
            }
            _ => bail!("bad processor list"),
        }
        Ok(())
    }

    fn element(&self, value: &Value) -> Result<()> {
        self.typed(value, "element_type", ELEMENT_FIELDS, "pool element")?;
        let m = value.as_object().unwrap();
        if let Some(projection) = m.get("projection") {
            let projection = projection.as_str().context("projection is a string")?;
            ensure!(
                ["rigid", "terrain_matching"].contains(&projection),
                "unknown projection {projection}"
            );
        }
        match kind_of(m, "element_type")? {
            "single_pool_element" | "legacy_single_pool_element" => {
                // Vanilla's own pools name one template the jar lacks
                // (ancient_city/walls/intact_horizontal_wall_stairs_5); the
                // game places a missing template as an empty one.
                string(m, "location")?;
                self.processor_list(field(m, "processors")?)?;
            }
            "feature_pool_element" => {
                let feature = field(m, "feature")?;
                if let Value::String(name) = feature {
                    ensure!(
                        self.placed.contains_key(&qualify(name)),
                        "unknown placed feature {name}"
                    );
                }
            }
            "list_pool_element" => {
                for element in field(m, "elements")?
                    .as_array()
                    .context("elements is a list")?
                {
                    self.element(element)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn pool(&self, value: &Value) -> Result<()> {
        let m = object(value, &["elements", "fallback"])?;
        let fallback = qualify(string(m, "fallback")?);
        ensure!(
            self.pools.contains_key(&fallback) || fallback == "minecraft:empty",
            "unknown fallback pool {fallback}"
        );
        for entry in field(m, "elements")?
            .as_array()
            .context("elements is a list")?
        {
            let e = object(entry, &["element", "weight"])?;
            int(e, "weight")?;
            self.element(field(e, "element")?)?;
        }
        Ok(())
    }

    fn pool_ref(&self, name: &str) -> Result<()> {
        ensure!(
            self.pools.contains_key(&qualify(name)),
            "unknown template pool {name}"
        );
        Ok(())
    }

    fn structure_config(&self, kind: &str, m: &Map<String, Value>) -> Result<()> {
        match kind {
            "jigsaw" => {
                self.pool_ref(string(m, "start_pool")?)?;
                for alias in m
                    .get("pool_aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    self.pool_alias(alias)?;
                }
            }
            "ruined_portal" => {
                for setup in field(m, "setups")?.as_array().context("setups is a list")? {
                    let s = object(setup, &SETUP_FIELDS)?;
                    let placement = string(s, "placement")?;
                    ensure!(
                        [
                            "on_land_surface",
                            "partly_buried",
                            "on_ocean_floor",
                            "in_mountain",
                            "underground",
                            "in_nether"
                        ]
                        .contains(&placement),
                        "unknown ruined portal placement {placement}"
                    );
                }
            }
            "mineshaft" => {
                let kind = string(m, "mineshaft_type")?;
                ensure!(
                    ["normal", "mesa"].contains(&kind),
                    "unknown mineshaft type {kind}"
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn pool_alias(&self, value: &Value) -> Result<()> {
        let m = value.as_object().context("pool alias is an object")?;
        match kind_of(m, "type")? {
            "direct" => {
                check_fields(m, &["type", "alias", "target"], "pool alias")?;
                self.pool_ref(string(m, "target")?)?;
            }
            "random" => {
                check_fields(m, &["type", "alias", "targets"], "pool alias")?;
                for target in field(m, "targets")?.as_array().context("targets")? {
                    let t = object(target, &["data", "weight"])?;
                    self.pool_ref(string(t, "data")?)?;
                }
            }
            "random_group" => {
                check_fields(m, &["type", "groups"], "pool alias")?;
                for group in field(m, "groups")?.as_array().context("groups")? {
                    let g = object(group, &["data", "weight"])?;
                    for alias in field(g, "data")?.as_array().context("group data")? {
                        self.pool_alias(alias)?;
                    }
                }
            }
            other => bail!("unmodeled pool alias kind {other}"),
        }
        Ok(())
    }

    fn spawn_overrides(&self, value: &Value) -> Result<()> {
        let m = value.as_object().context("spawn overrides are an object")?;
        for (category, override_) in m {
            let o = object(override_, &["bounding_box", "spawns"])
                .with_context(|| format!("spawn override {category}"))?;
            let bounds = string(o, "bounding_box")?;
            ensure!(
                ["full", "piece"].contains(&bounds),
                "unknown spawn bounding box {bounds}"
            );
            for spawn in field(o, "spawns")?.as_array().context("spawns")? {
                object(spawn, &["type", "count", "weight"])?;
            }
        }
        Ok(())
    }

    fn structure(&self, name: &str, value: &Value, steps: &[String]) -> Result<String> {
        let m = value.as_object().context("structure is an object")?;
        let kind = kind_of(m, "type")?;
        let fields = table_fields(STRUCTURE_FIELDS, kind, "structure")?;
        let mut config = Map::new();
        for (key, v) in m {
            if SHARED_FIELDS.contains(&key.as_str()) {
                continue;
            }
            ensure!(
                fields.contains(&key.as_str()),
                "unknown field {key} on structure {kind}"
            );
            config.insert(key.clone(), v.clone());
        }
        self.structure_config(kind, m)?;
        self.spawn_overrides(field(m, "spawn_overrides")?)?;
        let step = string(m, "step")?;
        let step = steps
            .iter()
            .position(|s| s == step)
            .with_context(|| format!("unknown decoration step {step}"))?;
        let adaptation = match m.get("terrain_adaptation").map(Value::as_str) {
            None | Some(Some("none")) => "None",
            Some(Some("beard_thin")) => "BeardThin",
            Some(Some("beard_box")) => "BeardBox",
            Some(Some("bury")) => "Bury",
            Some(Some("encapsulate")) => "Encapsulate",
            Some(other) => bail!("unknown terrain adaptation {other:?}"),
        };
        let biomes = self.biomes(field(m, "biomes")?)?;
        let biome_tag = biome_tag(field(m, "biomes")?);
        Ok(format!(
            "Structure {{ name: {}, kind: {}, biomes: &[{}], biome_tag: {biome_tag}, step: {step}, \
             terrain_adaptation: TerrainAdaptation::{adaptation}, spawn_overrides: {}, config: {} }}",
            str_literal(name),
            str_literal(&qualify(string(m, "type")?)),
            biomes
                .iter()
                .map(|b| str_literal(b))
                .collect::<Vec<_>>()
                .join(", "),
            super::json::literal(field(m, "spawn_overrides")?),
            super::json::literal(&Value::Object(config)),
        ))
    }

    fn spread(&self, m: &Map<String, Value>, sets: &BTreeMap<String, &Value>) -> Result<String> {
        let offset = match m.get("locate_offset") {
            None => [0, 0, 0],
            Some(v) => {
                let list = v.as_array().context("locate_offset is a list")?;
                ensure!(list.len() == 3, "locate_offset has three values");
                let mut out = [0; 3];
                for (o, v) in out.iter_mut().zip(list) {
                    *o = v.as_i64().context("locate_offset values")? as i32;
                }
                out
            }
        };
        let reduction = match m.get("frequency_reduction_method").map(Value::as_str) {
            None | Some(Some("default")) => "Default",
            Some(Some("legacy_type_1")) => "LegacyType1",
            Some(Some("legacy_type_2")) => "LegacyType2",
            Some(Some("legacy_type_3")) => "LegacyType3",
            Some(other) => bail!("unknown frequency reduction {other:?}"),
        };
        let frequency = match m.get("frequency") {
            None => 1.0,
            Some(v) => v.as_f64().context("frequency is a number")? as f32,
        };
        let exclusion = match m.get("exclusion_zone") {
            None => "None".to_string(),
            Some(v) => {
                let e = object(v, &["chunk_count", "other_set"])?;
                let other = qualify(string(e, "other_set")?);
                ensure!(sets.contains_key(&other), "unknown structure set {other}");
                format!(
                    "Some(ExclusionZone {{ other_set: {}, chunk_count: {} }})",
                    str_literal(&other),
                    int(e, "chunk_count")?
                )
            }
        };
        Ok(format!(
            "Spread {{ locate_offset: {offset:?}, frequency_reduction: FrequencyReduction::{reduction}, \
             frequency: {}, salt: {}, exclusion_zone: {exclusion} }}",
            f32_lit(frequency),
            int(m, "salt")?
        ))
    }

    fn structure_set(&self, value: &Value, sets: &BTreeMap<String, &Value>) -> Result<String> {
        let m = object(value, &["placement", "structures"])?;
        let mut entries = Vec::new();
        for entry in field(m, "structures")?
            .as_array()
            .context("structures is a list")?
        {
            let e = object(entry, &["structure", "weight"])?;
            let structure = qualify(string(e, "structure")?);
            ensure!(
                self.structures.contains_key(&structure),
                "unknown structure {structure}"
            );
            let weight = int(e, "weight")?;
            ensure!(weight > 0, "structure weights are positive");
            entries.push(format!("({}, {weight})", str_literal(&structure)));
        }
        ensure!(!entries.is_empty(), "structure sets are not empty");
        let p = field(m, "placement")?
            .as_object()
            .context("placement is an object")?;
        let common = [
            "type",
            "locate_offset",
            "frequency_reduction_method",
            "frequency",
            "salt",
            "exclusion_zone",
        ];
        let placement = match kind_of(p, "type")? {
            "random_spread" => {
                let mut allowed = common.to_vec();
                allowed.extend(["spacing", "separation", "spread_type"]);
                check_fields(p, &allowed, "random spread placement")?;
                let spread_type = match p.get("spread_type").map(Value::as_str) {
                    None | Some(Some("linear")) => "Linear",
                    Some(Some("triangular")) => "Triangular",
                    Some(other) => bail!("unknown spread type {other:?}"),
                };
                let (spacing, separation) = (int(p, "spacing")?, int(p, "separation")?);
                ensure!(spacing > separation, "spacing exceeds separation");
                format!(
                    "StructurePlacement::RandomSpread {{ spread: {}, spacing: {spacing}, \
                     separation: {separation}, spread_type: SpreadType::{spread_type} }}",
                    self.spread(p, sets)?
                )
            }
            "concentric_rings" => {
                let mut allowed = common.to_vec();
                allowed.extend(["distance", "spread", "count", "preferred_biomes"]);
                check_fields(p, &allowed, "concentric rings placement")?;
                format!(
                    "StructurePlacement::ConcentricRings {{ spread: {}, distance: {}, \
                     ring_spread: {}, count: {}, preferred_biomes: &[{}], preferred_tag: {} }}",
                    self.spread(p, sets)?,
                    int(p, "distance")?,
                    int(p, "spread")?,
                    int(p, "count")?,
                    self.biomes(field(p, "preferred_biomes")?)?
                        .iter()
                        .map(|b| str_literal(b))
                        .collect::<Vec<_>>()
                        .join(", "),
                    biome_tag(field(p, "preferred_biomes")?),
                )
            }
            "dimension_origin" => {
                check_fields(p, &common, "dimension origin placement")?;
                format!(
                    "StructurePlacement::DimensionOrigin {{ spread: {} }}",
                    self.spread(p, sets)?
                )
            }
            other => bail!("unmodeled structure placement {other}"),
        };
        Ok(format!(
            "structures: &[{}], placement: {placement}",
            entries.join(", ")
        ))
    }
}

/// The templates, in name order, as `(name, gzip bytes)`.
pub fn templates(input: &Input) -> Vec<(String, &[u8])> {
    let mut out: Vec<(String, &[u8])> = input
        .structure_templates
        .iter()
        .filter_map(|(path, bytes)| {
            let (namespace, rest) = path.split_once('/')?;
            let name = rest.strip_prefix("structure/")?.strip_suffix(".nbt")?;
            Some((format!("{namespace}:{name}"), bytes.as_slice()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn generate(input: &Input) -> Result<String> {
    let steps: Vec<String> = input
        .world_properties
        .lines()
        .filter_map(|line| line.strip_prefix("decoration_step "))
        .map(str::to_string)
        .collect();
    ensure!(!steps.is_empty(), "world properties: no decoration steps");
    let steps = steps.as_slice();
    let biome_tags = super::tags::resolved_registry_tags(input, "minecraft:worldgen/biome")?
        .into_iter()
        .collect();
    let block_tags = super::tags::resolved_registry_tags(input, "minecraft:block")?
        .into_keys()
        .collect();
    let g = Gen {
        input,
        structures: collect(input, "worldgen/structure"),
        pools: collect(input, "worldgen/template_pool"),
        processor_lists: collect(input, "worldgen/processor_list"),
        placed: collect(input, "worldgen/placed_feature"),
        biome_tags,
        block_tags,
    };
    let mut out = String::new();

    let mut table = Vec::new();
    for (index, (name, value)) in g.structures.iter().enumerate() {
        let body = g
            .structure(name, value, steps)
            .with_context(|| format!("structure {name}"))?;
        writeln!(out, "static STRUCTURE_{index}: Structure = {body};")?;
        table.push(format!("({}, &STRUCTURE_{index})", str_literal(name)));
    }
    writeln!(
        out,
        "/// Every structure, sorted by name (the registry's order).\n\
         pub static STRUCTURES: &[(&str, &Structure)] = &[{}];",
        table.join(", ")
    )?;

    let sets = collect(input, "worldgen/structure_set");
    let mut table = Vec::new();
    for (index, (name, value)) in sets.iter().enumerate() {
        let body = g
            .structure_set(value, &sets)
            .with_context(|| format!("structure set {name}"))?;
        writeln!(
            out,
            "static STRUCTURE_SET_{index}: StructureSet = StructureSet {{ name: {}, {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &STRUCTURE_SET_{index})", str_literal(name)));
    }
    writeln!(
        out,
        "/// Every structure set, sorted by name.\n\
         pub static STRUCTURE_SETS: &[(&str, &StructureSet)] = &[{}];",
        table.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &g.pools {
        g.pool(value)
            .with_context(|| format!("template pool {name}"))?;
        table.push(format!(
            "({}, {})",
            str_literal(name),
            super::json::literal(value)
        ));
    }
    writeln!(
        out,
        "pub static TEMPLATE_POOLS: &[(&str, &str)] = &[{}];",
        table.join(",\n    ")
    )?;
    let mut table = Vec::new();
    for (name, value) in &g.processor_lists {
        g.processor_list(value)
            .with_context(|| format!("processor list {name}"))?;
        table.push(format!(
            "({}, {})",
            str_literal(name),
            super::json::literal(value)
        ));
    }
    writeln!(
        out,
        "pub static PROCESSOR_LISTS: &[(&str, &str)] = &[{}];",
        table.join(",\n    ")
    )?;

    for (path, json) in &input.data_json {
        if path.contains("/worldgen/configured_feature/") || path.contains("/worldgen/feature/") {
            check_feature_templates(&g, json).with_context(|| format!("data/{path}"))?;
        }
    }

    let mut offset = 0usize;
    let mut table = Vec::new();
    for (name, bytes) in templates(input) {
        table.push(format!(
            "({}, {offset}, {})",
            str_literal(&name),
            bytes.len()
        ));
        offset += bytes.len();
    }
    writeln!(
        out,
        "static TEMPLATE_BLOB: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/structure_templates.bin\"));\n\
         /// `(name, offset, length)` of each gzip template in the blob, sorted by name.\n\
         static TEMPLATES: &[(&str, usize, usize)] = &[{}];",
        table.join(",\n    ")
    )?;
    Ok(out)
}

/// Template and fossil features name templates and processor lists; they
/// must exist.
fn check_feature_templates(g: &Gen, value: &Value) -> Result<()> {
    match value {
        Value::Object(m) => {
            match m
                .get("type")
                .and_then(Value::as_str)
                .map(qualify)
                .as_deref()
            {
                Some("minecraft:template") => {
                    for key in m.keys() {
                        ensure!(
                            ["type", "templates", "processors"].contains(&key.as_str()),
                            "unknown field {key} on template feature"
                        );
                    }
                    for entry in field(m, "templates")?.as_array().context("templates")? {
                        let e = object(entry, &["data", "weight"])?;
                        let data = object(field(e, "data")?, &["id"])?;
                        let id = string(data, "id")?;
                        ensure!(g.template_exists(id), "unknown structure template {id}");
                    }
                    if let Some(processors) = m.get("processors") {
                        g.processor_list(processors)?;
                    }
                }
                Some("minecraft:fossil") => {
                    for key in ["fossil_structures", "overlay_structures"] {
                        for id in field(m, key)?.as_array().context("fossil templates")? {
                            let id = id.as_str().context("template names")?;
                            ensure!(g.template_exists(id), "unknown structure template {id}");
                        }
                    }
                    for key in ["fossil_processors", "overlay_processors"] {
                        g.processor_list(field(m, key)?)?;
                    }
                }
                _ => {
                    for v in m.values() {
                        check_feature_templates(g, v)?;
                    }
                }
            }
        }
        Value::Array(list) => {
            for v in list {
                check_feature_templates(g, v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

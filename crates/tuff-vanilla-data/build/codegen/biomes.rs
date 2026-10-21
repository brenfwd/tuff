//! The built-in `minecraft:worldgen/biome` entries. Every field is typed
//! except `attributes`, an open-ended map (mods and future versions can
//! register new attribute kinds) kept as `(key, raw JSON value)` pairs.

use std::fmt::Write;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::enums::{emit_value_enum, unique_sorted};
use super::json;
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Biome {
    has_precipitation: bool,
    temperature: f32,
    #[serde(default)]
    temperature_modifier: Option<String>,
    downfall: f32,
    effects: Effects,
    /// A single carver is written as a bare string rather than a one-element
    /// list.
    #[serde(default, deserialize_with = "string_or_list")]
    carvers: Vec<String>,
    #[serde(default)]
    features: Vec<Vec<String>>,
    #[serde(default)]
    attributes: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Effects {
    water_color: String,
    #[serde(default)]
    foliage_color: Option<String>,
    #[serde(default)]
    grass_color: Option<String>,
    #[serde(default)]
    dry_foliage_color: Option<String>,
    #[serde(default)]
    grass_color_modifier: Option<String>,
}

fn string_or_list<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrList {
        One(String),
        Many(Vec<String>),
    }
    Ok(match StringOrList::deserialize(deserializer)? {
        StringOrList::One(value) => vec![value],
        StringOrList::Many(values) => values,
    })
}

const NATURAL_SPAWNS: &str = "minecraft:gameplay/natural_mob_spawns";
const CREATURE_PROBABILITY: &str = "minecraft:gameplay/creature_world_gen_spawn_probability";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAttribute {
    argument: SpawnSettings,
    modifier: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnSettings {
    spawn_costs: std::collections::BTreeMap<String, SpawnCost>,
    spawns_by_category: std::collections::BTreeMap<String, Vec<Spawner>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnCost {
    charge: f64,
    energy_budget: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spawner {
    #[serde(rename = "type")]
    kind: String,
    count: SpawnCount,
    weight: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SpawnCount {
    Constant(u32),
    Provider(CountProvider),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CountProvider {
    #[serde(rename = "type")]
    kind: String,
    min_inclusive: u32,
    max_inclusive: u32,
}

/// The typed `natural_spawns` field of a biome, taken out of its attributes.
fn natural_spawns(
    attributes: &mut Map<String, Value>,
    entity_types: &std::collections::BTreeMap<&str, String>,
    name: &str,
) -> Result<String> {
    let probability = match attributes.remove(CREATURE_PROBABILITY) {
        Some(value) => {
            let value = value
                .as_f64()
                .with_context(|| format!("{name}: creature spawn probability is not a number"))?;
            format!("Some({:?})", value as f32)
        }
        None => "None".into(),
    };
    let Some(raw) = attributes.remove(NATURAL_SPAWNS) else {
        return Ok(format!(
            "NaturalSpawns {{ categories: &[], costs: &[], creature_probability: {probability} }}"
        ));
    };
    let parsed: SpawnAttribute =
        serde_json::from_value(raw).with_context(|| format!("{name}: natural mob spawns"))?;
    anyhow::ensure!(
        parsed.modifier == "overlay",
        "{name}: natural mob spawns use modifier {:?}, only overlay is modeled",
        parsed.modifier
    );
    let entity = |kind: &str| -> Result<String> {
        entity_types
            .get(kind)
            .map(|variant| format!("crate::registry::EntityType::{variant}"))
            .with_context(|| format!("{name}: unknown entity type {kind}"))
    };
    let mut categories = Vec::new();
    for (category, spawners) in &parsed.argument.spawns_by_category {
        let mut entries = Vec::new();
        for spawner in spawners {
            let (min, max) = match &spawner.count {
                SpawnCount::Constant(count) => (*count, *count),
                SpawnCount::Provider(provider) => {
                    anyhow::ensure!(
                        provider.kind == "minecraft:uniform",
                        "{name}: unknown spawn count provider {}",
                        provider.kind
                    );
                    (provider.min_inclusive, provider.max_inclusive)
                }
            };
            anyhow::ensure!(min <= max, "{name}: spawn count {min} above {max}");
            entries.push(format!(
                "Spawner {{ kind: {}, weight: {}, min_count: {min}, max_count: {max} }}",
                entity(&spawner.kind)?,
                spawner.weight
            ));
        }
        categories.push(format!(
            "(crate::entity::MobCategory::{}, &[{}])",
            names::pascal(category),
            entries.join(", ")
        ));
    }
    let mut costs = Vec::new();
    for (kind, cost) in &parsed.argument.spawn_costs {
        costs.push(format!(
            "({}, SpawnCost {{ energy_budget: {:?}, charge: {:?} }})",
            entity(kind)?,
            cost.energy_budget,
            cost.charge
        ));
    }
    Ok(format!(
        "NaturalSpawns {{ categories: &[{}], costs: &[{}], creature_probability: {probability} }}",
        categories.join(", "),
        costs.join(", ")
    ))
}

pub fn generate(input: &Input) -> Result<String> {
    let entity_types = super::registries::variants(&input.registries["minecraft:entity_type"])?;
    let mut biomes = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("worldgen/biome/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let mut parsed: Biome =
            serde_json::from_value(json.clone()).with_context(|| format!("data/{path}"))?;
        let spawns = natural_spawns(&mut parsed.attributes, &entity_types, name)?;
        biomes.push((format!("{namespace}:{name}"), (parsed, spawns)));
    }
    biomes.sort_by(|(a, _), (b, _)| a.cmp(b));

    let modifiers: Vec<String> = biomes
        .iter()
        .filter_map(|(_, (b, _))| b.temperature_modifier.clone())
        .collect();
    let grass_modifiers: Vec<String> = biomes
        .iter()
        .filter_map(|(_, (b, _))| b.effects.grass_color_modifier.clone())
        .collect();

    let mut out = String::new();
    emit_value_enum(&mut out, "TemperatureModifier", &unique_sorted(modifiers))?;
    emit_value_enum(
        &mut out,
        "GrassColorModifier",
        &unique_sorted(grass_modifiers),
    )?;

    let consts = names::unique(biomes.iter().map(|(name, _)| name.as_str()), shouty)?;
    for ((name, (biome, spawns)), ident) in biomes.iter().zip(&consts) {
        write!(
            out,
            "pub static {ident}: Biome = Biome {{
    name: {},
    has_precipitation: {},
    temperature: {:?},
    temperature_modifier: {},
    downfall: {:?},
    effects: BiomeEffects {{
        water_color: {},
        foliage_color: {},
        grass_color: {},
        dry_foliage_color: {},
        grass_color_modifier: {},
    }},
    carvers: &[{}],
    features: &[{}],
    attributes: &[{}],
    natural_spawns: {spawns},
}};\n",
            str_literal(name),
            biome.has_precipitation,
            biome.temperature,
            optional_variant("TemperatureModifier", biome.temperature_modifier.as_deref()),
            biome.downfall,
            color(&biome.effects.water_color)?,
            optional_color(biome.effects.foliage_color.as_deref())?,
            optional_color(biome.effects.grass_color.as_deref())?,
            optional_color(biome.effects.dry_foliage_color.as_deref())?,
            optional_variant(
                "GrassColorModifier",
                biome.effects.grass_color_modifier.as_deref()
            ),
            biome
                .carvers
                .iter()
                .map(|c| str_literal(c))
                .collect::<Vec<_>>()
                .join(", "),
            biome
                .features
                .iter()
                .map(|step| format!(
                    "&[{}]",
                    step.iter()
                        .map(|f| str_literal(f))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .collect::<Vec<_>>()
                .join(", "),
            biome
                .attributes
                .iter()
                .map(|(key, value)| format!("({}, {})", str_literal(key), json::literal(value)))
                .collect::<Vec<_>>()
                .join(", "),
        )?;
    }
    writeln!(out, "pub const ALL: &[(&str, &Biome)] = &[")?;
    for (name, ident) in biomes.iter().map(|(name, _)| name).zip(&consts) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

/// `#rrggbb` to a packed `0xRRGGBB` literal.
fn color(hex: &str) -> Result<String> {
    let digits = hex
        .strip_prefix('#')
        .filter(|d| d.len() == 6)
        .with_context(|| format!("bad color {hex}"))?;
    let value = u32::from_str_radix(digits, 16).with_context(|| format!("bad color {hex}"))?;
    Ok(format!("0x{value:06X}"))
}

fn optional_color(hex: Option<&str>) -> Result<String> {
    Ok(match hex {
        Some(hex) => format!("Some({})", color(hex)?),
        None => "None".to_owned(),
    })
}

fn optional_variant(type_name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!("Some({type_name}::{})", names::pascal(value)),
        None => "None".to_owned(),
    }
}

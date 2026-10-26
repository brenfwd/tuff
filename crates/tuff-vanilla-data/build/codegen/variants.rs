//! Animal-family variant registries (`cow_variant`, `pig_variant`,
//! `chicken_variant`, `wolf_variant`, `cat_variant`, `frog_variant`, ...)
//! with their biome spawn conditions resolved to biome lists, and the
//! voiced ones' sound variants. Entries are sorted by name, the order
//! their network ids follow. Unknown fields or condition kinds fail the
//! build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use super::tags;
use crate::input::Input;
use crate::names::str_literal;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "helpers used by only some targets")]
struct Variant {
    /// A cow/pig/chicken/cat/frog-shaped variant names one texture per age.
    #[serde(default)]
    asset_id: Option<String>,
    #[serde(default)]
    baby_asset_id: Option<String>,
    /// A wolf-shaped variant instead names one per (angry, tame, wild) mood;
    /// their content is unused server-side, so it is read and dropped.
    #[serde(default)]
    assets: Option<Value>,
    #[serde(default)]
    baby_assets: Option<Value>,
    #[serde(default)]
    model: Option<String>,
    spawn_conditions: Vec<Condition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Condition {
    priority: i32,
    #[serde(default)]
    condition: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sounds {
    ambient_sound: String,
    death_sound: String,
    hurt_sound: String,
    #[serde(default)]
    step_sound: Option<String>,
    #[serde(default)]
    eat_sound: Option<String>,
    /// Wolf-only fields.
    #[serde(default)]
    growl_sound: Option<String>,
    #[serde(default)]
    pant_sound: Option<String>,
    #[serde(default)]
    whine_sound: Option<String>,
    /// Cat-only fields.
    #[serde(default)]
    beg_for_food_sound: Option<String>,
    #[serde(default)]
    hiss_sound: Option<String>,
    #[serde(default)]
    purr_sound: Option<String>,
    #[serde(default)]
    purreow_sound: Option<String>,
    #[serde(default)]
    stray_ambient_sound: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgedSounds {
    adult_sounds: Sounds,
    baby_sounds: Sounds,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SoundVariant {
    Aged(Box<AgedSounds>),
    Single(Box<Sounds>),
}

fn entries<'a>(input: &'a Input, registry: &str) -> Result<Vec<(String, &'a Value)>> {
    let mut found = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix(registry)
            .and_then(|r| r.strip_prefix('/'))
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        found.push((format!("{namespace}:{name}"), json));
    }
    found.sort_by(|(a, _), (b, _)| a.cmp(b));
    anyhow::ensure!(!found.is_empty(), "variants: no {registry} entries");
    Ok(found)
}

fn sound_set(sounds: &Sounds, events: &BTreeMap<&str, String>, what: &str) -> Result<String> {
    let event = |name: &str| -> Result<String> {
        events
            .get(name)
            .map(|variant| format!("crate::registry::SoundEvent::{variant}"))
            .with_context(|| format!("{what}: unknown sound {name}"))
    };
    let optional = |name: &Option<String>| -> Result<String> {
        Ok(match name {
            Some(name) => format!("Some({})", event(name)?),
            None => "None".into(),
        })
    };
    Ok(format!(
        "SoundSet {{ ambient: {}, hurt: {}, death: {}, step: {}, eat: {}, growl: {}, pant: {}, whine: {}, beg_for_food: {}, hiss: {}, purr: {}, purreow: {}, stray_ambient: {} }}",
        event(&sounds.ambient_sound)?,
        event(&sounds.hurt_sound)?,
        event(&sounds.death_sound)?,
        optional(&sounds.step_sound)?,
        optional(&sounds.eat_sound)?,
        optional(&sounds.growl_sound)?,
        optional(&sounds.pant_sound)?,
        optional(&sounds.whine_sound)?,
        optional(&sounds.beg_for_food_sound)?,
        optional(&sounds.hiss_sound)?,
        optional(&sounds.purr_sound)?,
        optional(&sounds.purreow_sound)?,
        optional(&sounds.stray_ambient_sound)?,
    ))
}

fn spawn_conditions(
    variant: &Variant,
    biome_tags: &BTreeMap<String, Vec<String>>,
    structure_tags: &BTreeMap<String, Vec<String>>,
    what: &str,
) -> Result<Vec<String>> {
    let mut conditions = Vec::new();
    for condition in &variant.spawn_conditions {
        let biomes = match &condition.condition {
            None => "None".to_string(),
            Some(value)
                if value.get("type").and_then(Value::as_str) == Some("minecraft:structure") =>
            {
                let list = match value.get("structures") {
                    Some(Value::String(id)) => match id.strip_prefix('#') {
                        Some(tag) => structure_tags
                            .get(tag)
                            .with_context(|| format!("{what}: unknown structure tag {tag}"))?
                            .clone(),
                        None => vec![id.clone()],
                    },
                    Some(Value::Array(ids)) => ids
                        .iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect::<Option<_>>()
                        .with_context(|| format!("{what}: bad structure list"))?,
                    other => bail!("{what}: bad structures {other:?}"),
                };
                let list: Vec<String> = list.iter().map(|s| str_literal(s)).collect();
                conditions.push(format!(
                    "SpawnPriority {{ priority: {}, biomes: None, structures: Some(&[{}]), min_moon_brightness: None }}",
                    condition.priority,
                    list.join(", ")
                ));
                continue;
            }
            Some(value)
                if value.get("type").and_then(Value::as_str)
                    == Some("minecraft:moon_brightness") =>
            {
                let range = value
                    .get("range")
                    .with_context(|| format!("{what}: moon brightness has no range"))?;
                if range.get("max").is_some() {
                    bail!("{what}: moon brightness max is unsupported");
                }
                let min = range
                    .get("min")
                    .and_then(Value::as_f64)
                    .with_context(|| format!("{what}: moon brightness has no min"))?;
                conditions.push(format!(
                    "SpawnPriority {{ priority: {}, biomes: None, structures: None, min_moon_brightness: Some({min:?}) }}",
                    condition.priority
                ));
                continue;
            }
            Some(value) => {
                let object = value
                    .as_object()
                    .with_context(|| format!("{what}: condition is not an object"))?;
                if object.get("type").and_then(Value::as_str) != Some("minecraft:biome")
                    || object.len() != 2
                {
                    bail!("{what}: unknown spawn condition {value}");
                }
                let list = match &object["biomes"] {
                    Value::String(id) => match id.strip_prefix('#') {
                        Some(tag) => biome_tags
                            .get(tag)
                            .with_context(|| format!("{what}: unknown tag {tag}"))?
                            .clone(),
                        None => vec![id.clone()],
                    },
                    Value::Array(ids) => ids
                        .iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect::<Option<_>>()
                        .with_context(|| format!("{what}: bad biome list"))?,
                    other => bail!("{what}: bad biomes {other}"),
                };
                let list: Vec<String> = list.iter().map(|b| str_literal(b)).collect();
                format!("Some(&[{}])", list.join(", "))
            }
        };
        conditions.push(format!(
            "SpawnPriority {{ priority: {}, biomes: {biomes}, structures: None, min_moon_brightness: None }}",
            condition.priority
        ));
    }
    Ok(conditions)
}

fn variant_list(
    out: &mut String,
    input: &Input,
    biome_tags: &BTreeMap<String, Vec<String>>,
    structure_tags: &BTreeMap<String, Vec<String>>,
    constant: &str,
    registry: &str,
) -> Result<()> {
    writeln!(out, "pub static {constant}: &[FarmVariant] = &[")?;
    for (name, json) in entries(input, registry)? {
        let what = format!("{registry} {name}");
        let variant: Variant =
            serde_json::from_value(json.clone()).with_context(|| what.clone())?;
        let conditions = spawn_conditions(&variant, biome_tags, structure_tags, &what)?;
        writeln!(
            out,
            "    FarmVariant {{ name: {}, spawn_conditions: &[{}] }},",
            str_literal(&name),
            conditions.join(", ")
        )?;
    }
    writeln!(out, "];")?;
    Ok(())
}

fn sound_variant_list(
    out: &mut String,
    input: &Input,
    events: &BTreeMap<&str, String>,
    constant: &str,
    registry: &str,
) -> Result<()> {
    writeln!(out, "pub static {constant}_SOUNDS: &[SoundVariant] = &[")?;
    for (name, json) in entries(input, registry)? {
        let what = format!("{registry} {name}");
        let parsed: SoundVariant =
            serde_json::from_value(json.clone()).with_context(|| what.clone())?;
        let (adult, baby) = match &parsed {
            SoundVariant::Aged(aged) => (
                sound_set(&aged.adult_sounds, events, &what)?,
                format!("Some({})", sound_set(&aged.baby_sounds, events, &what)?),
            ),
            SoundVariant::Single(sounds) => (sound_set(sounds, events, &what)?, "None".into()),
        };
        writeln!(
            out,
            "    SoundVariant {{ name: {}, adult: {adult}, baby: {baby} }},",
            str_literal(&name)
        )?;
    }
    writeln!(out, "];")?;
    Ok(())
}

/// Registries with both a variant (spawn conditions) and a matching sound
/// variant registry.
const VOICED_ANIMALS: [(&str, &str); 5] = [
    ("COW", "cow"),
    ("PIG", "pig"),
    ("CHICKEN", "chicken"),
    ("WOLF", "wolf"),
    ("CAT", "cat"),
];

/// Registries with only a variant (no per-variant sounds).
const SILENT_ANIMALS: [(&str, &str); 1] = [("FROG", "frog")];

pub fn generate(input: &Input) -> Result<String> {
    let biome_tags = tags::resolved_registry_tags(input, "minecraft:worldgen/biome")?;
    let structure_tags = tags::resolved_registry_tags(input, "minecraft:worldgen/structure")?;
    let events = super::registries::variants(&input.registries["minecraft:sound_event"])?;
    let mut out = String::new();
    for (constant, animal) in VOICED_ANIMALS {
        variant_list(
            &mut out,
            input,
            &biome_tags,
            &structure_tags,
            constant,
            &format!("{animal}_variant"),
        )?;
        sound_variant_list(
            &mut out,
            input,
            &events,
            constant,
            &format!("{animal}_sound_variant"),
        )?;
    }
    for (constant, animal) in SILENT_ANIMALS {
        variant_list(
            &mut out,
            input,
            &biome_tags,
            &structure_tags,
            constant,
            &format!("{animal}_variant"),
        )?;
    }
    Ok(out)
}

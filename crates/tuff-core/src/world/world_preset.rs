//! World presets and dimension stems from data packs: a new world takes
//! the `dimensions` of the preset its `level-type` names when a pack
//! provides that preset, and pack dimension stems join every world at
//! startup over the level's own dimensions.

use serde_json::Value;
use tuff_nbt::{Compound, Tag};

use crate::storage::level::{DimensionSettings, LevelData, WORLD_BORDER, inner, parse_dimensions};
use tuff_item::registries::{Entry, Registries};
use tuff_storage::backend::Documents;
use tuff_storage::document::Document;

const WORLD_PRESET: &str = "minecraft:worldgen/world_preset";
const DIMENSION: &str = "minecraft:dimension";

/// The built-in preset name of a `level-type`, without its namespace and
/// with the legacy names mapped.
pub fn builtin_name(level_type: &str) -> &str {
    match level_type.strip_prefix("minecraft:").unwrap_or(level_type) {
        "default" => "normal",
        "largebiomes" => "large_biomes",
        name => name,
    }
}

fn preset_id(level_type: &str) -> String {
    if level_type.contains(':') {
        level_type.to_owned()
    } else {
        format!("minecraft:{}", builtin_name(level_type))
    }
}

/// The `dimensions` of the data pack preset `level_type` names; `None`
/// when no pack provides it (the built-in presets apply) or it is
/// malformed.
pub fn dimensions(registries: &Registries, level_type: &str) -> Option<Compound> {
    let id = preset_id(level_type.trim());
    let json = pack_json(registries, WORLD_PRESET, &id)?;
    match stem_map(json.get("dimensions")) {
        Ok(dimensions) => Some(dimensions),
        Err(err) => {
            log::warn!("world preset {id} is ignored: {err}");
            None
        }
    }
}

/// Every dimension stem the data packs provide, by key.
pub fn stems(registries: &Registries) -> Compound {
    let mut out = Compound::new();
    let Some(registry) = registries.get(DIMENSION) else {
        return out;
    };
    for (key, entry) in registry.iter() {
        let Entry::Loaded(raw) = entry else {
            continue;
        };
        let Some(json) = raw.json.as_deref() else {
            continue;
        };
        match stem(json) {
            Ok(stem) => {
                out.insert(key, stem);
            }
            Err(err) => log::warn!("dimension {key} is ignored: {err}"),
        }
    }
    out
}

/// The level's dimensions with the data pack dimension `stems` over
/// them: a stem replaces the dimension of its key or adds one. Stems are
/// not saved with the level, as the game re-reads them every start.
pub fn with_stems(
    level: &LevelData,
    stems: &Compound,
    docs: &dyn Documents,
) -> Vec<DimensionSettings> {
    let generate = level
        .dimensions
        .first()
        .is_none_or(|d| d.generate_structures);
    let border = |key: &str| {
        docs.read(Document::DimensionData {
            dimension: key,
            id: WORLD_BORDER,
        })
        .map(inner)
        .unwrap_or_else(|err| {
            log::warn!("cannot read the border of {key}: {err}");
            Compound::new()
        })
    };
    let mut out: Vec<DimensionSettings> = level
        .dimensions
        .iter()
        .filter(|d| stems.get_compound(&d.key).is_none())
        .cloned()
        .chain(parse_dimensions(stems, border).into_iter().map(|mut d| {
            d.generate_structures = generate;
            d
        }))
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Gives a new world the dimensions of the `level_type` preset: a data
/// pack's when one provides it, else the built-in one.
pub fn apply(level: &mut LevelData, registries: &Registries, level_type: &str, settings: &str) {
    match dimensions(registries, level_type) {
        Some(preset) => level.set_new_world_dimensions(preset),
        None => {
            if !level.set_new_world_type(builtin_name(level_type), settings) {
                log::warn!("level-type {level_type} cannot be generated; creating a normal world");
            }
        }
    }
}

/// A new world's spawn in the overworld (none without one), searched on
/// the generator bound to the data packs.
pub async fn spawn(
    dimensions: &crate::world::dimension::Dimensions,
    tags: &std::sync::Arc<tuff_datapack::tags::TagSet>,
) -> std::io::Result<Option<tuff_world_model::BlockPos>> {
    let Some(overworld) = dimensions.by_key(crate::storage::level::OVERWORLD) else {
        return Ok(None);
    };
    let (generator, tags) = (overworld.generator.clone(), tags.clone());
    tokio::task::spawn_blocking(move || {
        Some(tuff_datapack::tags::sync_scope(tags, || generator.spawn()))
    })
    .await
    .map_err(std::io::Error::other)
}

fn pack_json<'a>(registries: &'a Registries, key: &str, id: &str) -> Option<&'a Value> {
    match registries.get(key)?.entry(id)? {
        Entry::Loaded(raw) => raw.json.as_deref(),
        Entry::Builtin(_) => None,
    }
}

fn stem_map(dimensions: Option<&Value>) -> Result<Compound, String> {
    let map = dimensions
        .and_then(Value::as_object)
        .ok_or("dimensions is not an object")?;
    let mut out = Compound::new();
    for (key, value) in map {
        out.insert(
            key.as_str(),
            stem(value).map_err(|err| format!("{key}: {err}"))?,
        );
    }
    Ok(out)
}

fn stem(json: &Value) -> Result<Compound, String> {
    if !json.get("type").is_some_and(Value::is_string) {
        return Err("type is not a dimension type id".into());
    }
    if !json
        .get("generator")
        .and_then(|g| g.get("type"))
        .is_some_and(Value::is_string)
    {
        return Err("generator has no type".into());
    }
    match tuff_item::registries::codecs::json_to_nbt(json) {
        Some(Tag::Compound(compound)) => Ok(compound),
        _ => Err("cannot be stored as NBT".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn level_types_name_presets() {
        assert_eq!(builtin_name("minecraft:flat"), "flat");
        assert_eq!(builtin_name("largebiomes"), "large_biomes");
        assert_eq!(builtin_name("default"), "normal");
        assert_eq!(preset_id("flat"), "minecraft:flat");
        assert_eq!(preset_id("ex:sky"), "ex:sky");
    }

    #[tuff_core::watchdog]
    #[test]
    fn stems_need_a_type_and_a_generator() {
        let good = serde_json::json!({
            "type": "minecraft:overworld",
            "generator": {"type": "minecraft:noise", "settings": "minecraft:overworld"}
        });
        assert!(stem(&good).is_ok());
        assert!(stem(&serde_json::json!({"type": "minecraft:overworld"})).is_err());
        assert!(stem_map(Some(&serde_json::json!({"ex:a": good}))).is_ok());
        assert!(stem_map(Some(&serde_json::json!([]))).is_err());
    }
}

//! Codecs for synchronized registries that clients can be sent.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::Value;
use tuff_nbt::{Compound, Tag};

use tuff_vanilla_data::data_registry::DataRegistry;
use tuff_vanilla_data::dimension_type::DimensionType;

use super::{DecodeCtx, NetworkCodec, RegistryCodec};

const SCALINGS: &[&str] = &["never", "when_caused_by_living_non_player", "always"];
const EFFECTS: &[&str] = &[
    "hurt", "thorns", "drowning", "burning", "poking", "freezing",
];
const DEATH_MESSAGES: &[&str] = &["default", "fall_variants", "intentional_game_design"];

#[derive(Clone, Debug, PartialEq)]
pub struct DamageType {
    pub message_id: String,
    pub scaling: &'static str,
    pub exhaustion: f32,
    pub effects: &'static str,
    pub death_message_type: &'static str,
}

pub struct DamageTypes;

impl RegistryCodec for DamageTypes {
    const KEY: &'static str = "minecraft:damage_type";
    type Value = DamageType;

    fn builtin(id: &str) -> Option<&'static DamageType> {
        static BUILTIN: OnceLock<HashMap<String, DamageType>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                tuff_vanilla_data::damage_type::ALL
                    .iter()
                    .map(|t| {
                        let value = DamageType {
                            message_id: t.message_id.to_owned(),
                            scaling: t.scaling.name(),
                            exhaustion: t.exhaustion,
                            effects: t.effects.map_or("hurt", |e| e.name()),
                            death_message_type: t
                                .death_message_type
                                .map_or("default", |d| d.name()),
                        };
                        (
                            format!("minecraft:{}", t.name.trim_start_matches("minecraft:")),
                            value,
                        )
                    })
                    .collect()
            })
            .get(id)
    }

    fn decode(json: &Value, _: &DecodeCtx<'_>) -> Result<DamageType, String> {
        let message_id = json["message_id"]
            .as_str()
            .ok_or("message_id: expected a string")?
            .to_owned();
        let exhaustion = json["exhaustion"]
            .as_f64()
            .ok_or("exhaustion: expected a number")? as f32;
        Ok(DamageType {
            message_id,
            scaling: one_of(json, "scaling", SCALINGS, None)?,
            exhaustion,
            effects: one_of(json, "effects", EFFECTS, Some("hurt"))?,
            death_message_type: one_of(
                json,
                "death_message_type",
                DEATH_MESSAGES,
                Some("default"),
            )?,
        })
    }
}

impl NetworkCodec for DamageTypes {
    fn to_network_nbt(value: &DamageType) -> Result<Vec<u8>, String> {
        let mut nbt = Compound::new()
            .with("message_id", value.message_id.clone())
            .with("scaling", value.scaling)
            .with("exhaustion", value.exhaustion);
        if value.effects != "hurt" {
            nbt.insert("effects", value.effects);
        }
        if value.death_message_type != "default" {
            nbt.insert("death_message_type", value.death_message_type);
        }
        network_bytes(nbt)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaintingVariant {
    pub width: i32,
    pub height: i32,
    pub asset_id: String,
    pub title: Option<Tag>,
    pub author: Option<Tag>,
}

/// Built-in painting variants are not generated as values, so only pack
/// entries decode.
pub struct PaintingVariants;

impl RegistryCodec for PaintingVariants {
    const KEY: &'static str = "minecraft:painting_variant";
    type Value = PaintingVariant;

    fn builtin(_: &str) -> Option<&'static PaintingVariant> {
        None
    }

    fn decode(json: &Value, _: &DecodeCtx<'_>) -> Result<PaintingVariant, String> {
        let size = |field: &str| {
            json[field]
                .as_i64()
                .filter(|n| (1..=16).contains(n))
                .map(|n| n as i32)
                .ok_or_else(|| format!("{field}: expected an integer from 1 to 16"))
        };
        let asset_id = json["asset_id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or("asset_id: expected a resource location")?;
        let component = |field: &str| match json.get(field) {
            None => Ok(None),
            Some(value) => json_to_nbt(value)
                .map(Some)
                .ok_or_else(|| format!("{field}: expected a text component")),
        };
        Ok(PaintingVariant {
            width: size("width")?,
            height: size("height")?,
            asset_id: qualify(asset_id),
            title: component("title")?,
            author: component("author")?,
        })
    }
}

impl NetworkCodec for PaintingVariants {
    fn to_network_nbt(value: &PaintingVariant) -> Result<Vec<u8>, String> {
        let mut nbt = Compound::new()
            .with("width", value.width)
            .with("height", value.height)
            .with("asset_id", value.asset_id.clone());
        if let Some(title) = &value.title {
            nbt.insert("title", title.clone());
        }
        if let Some(author) = &value.author {
            nbt.insert("author", author.clone());
        }
        network_bytes(nbt)
    }
}

fn one_of(
    json: &Value,
    field: &str,
    allowed: &[&'static str],
    default: Option<&'static str>,
) -> Result<&'static str, String> {
    match (json.get(field), default) {
        (None, Some(default)) => Ok(default),
        (value, _) => value
            .and_then(Value::as_str)
            .and_then(|name| allowed.iter().find(|a| **a == name).copied())
            .ok_or_else(|| format!("{field}: expected one of {}", allowed.join(", "))),
    }
}

fn valid_id(id: &str) -> bool {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let ok = |c: char, slash: bool| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '_' | '-' | '.')
            || (slash && c == '/')
    };
    !path.is_empty() && namespace.chars().all(|c| ok(c, false)) && path.chars().all(|c| ok(c, true))
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn network_bytes(nbt: Compound) -> Result<Vec<u8>, String> {
    Tag::Compound(nbt)
        .to_network_bytes()
        .map_err(|err| err.to_string())
}

type FieldCheck = (&'static str, fn(&Value) -> bool);

fn require(json: &Value, fields: &[FieldCheck]) -> Result<(), String> {
    if !json.is_object() {
        return Err("expected an object".to_owned());
    }
    for (field, check) in fields {
        match json.get(field) {
            Some(value) if check(value) => {}
            Some(_) => return Err(format!("{field}: wrong type")),
            None => return Err(format!("{field}: missing")),
        }
    }
    Ok(())
}

fn json_network_bytes(json: &Value) -> Result<Vec<u8>, String> {
    crate::enchantment::json_tag(json)
        .ok_or_else(|| "null value".to_owned())?
        .to_network_bytes()
        .map_err(|err| err.to_string())
}

fn is_color(value: &Value) -> bool {
    value.is_i64() || value.as_str().is_some_and(|s| s.starts_with('#'))
}

/// A pack entry of a synchronized registry without a dedicated codec: its
/// JSON as NBT, once it has every field the built-in entries share.
pub fn generic_network_nbt(key: &str, json: &Value) -> Result<Vec<u8>, String> {
    if json.is_array() && DataRegistry::get(key).is_some_and(DataRegistry::list_entries) {
        return json_network_bytes(json);
    }
    if !json.is_object() {
        return Err("expected an object".to_owned());
    }
    for (field, kind) in DataRegistry::required_fields(key) {
        match json.get(field) {
            Some(value) if kind.matches(value) => {}
            Some(_) => return Err(format!("{field}: wrong type")),
            None => return Err(format!("{field}: missing")),
        }
    }
    json_network_bytes(json)
}

/// A pack test instance; its environment must be a known one.
pub fn test_instance_network_nbt(json: &Value, ctx: &DecodeCtx<'_>) -> Result<Vec<u8>, String> {
    let nbt = generic_network_nbt("minecraft:test_instance", json)?;
    if let Some(environment) = json.get("environment").and_then(Value::as_str)
        && ctx
            .registries
            .exists("minecraft:test_environment", &qualify(environment))
            == Some(false)
    {
        return Err(format!("unknown test environment {environment}"));
    }
    Ok(nbt)
}

/// A pack biome as clients receive it: climate, attributes and effects,
/// without the generation settings.
pub fn biome_network_nbt(json: &Value, _: &DecodeCtx<'_>) -> Result<Vec<u8>, String> {
    require(
        json,
        &[
            ("has_precipitation", Value::is_boolean),
            ("temperature", Value::is_number),
            ("downfall", Value::is_number),
            ("effects", Value::is_object),
            ("carvers", |v| v.is_array() || v.is_string()),
            ("features", Value::is_array),
        ],
    )?;
    require(&json["effects"], &[("water_color", is_color)]).map_err(|e| format!("effects.{e}"))?;
    one_of(
        json,
        "temperature_modifier",
        &["none", "frozen"],
        Some("none"),
    )?;
    let mut network = json.clone();
    if let Some(map) = network.as_object_mut() {
        map.remove("carvers");
        map.remove("features");
    }
    json_network_bytes(&network)
}

/// A pack dimension type as clients receive it.
/// A pack timeline, checked as the game checks it (its clock must be a
/// `minecraft:world_clock` entry); clients drop the tracks they cannot sync.
pub fn timeline_network_nbt(json: &Value, ctx: &DecodeCtx<'_>) -> Result<Vec<u8>, String> {
    let timeline = tuff_vanilla_data::timeline::Timeline::from_json(ctx.id, json)?;
    if ctx
        .registries
        .exists("minecraft:world_clock", timeline.clock)
        == Some(false)
    {
        return Err(format!("unknown world clock {}", timeline.clock));
    }
    json_network_bytes(json)
}

pub fn dimension_type_network_nbt(json: &Value, ctx: &DecodeCtx<'_>) -> Result<Vec<u8>, String> {
    require(
        json,
        &[
            ("has_skylight", Value::is_boolean),
            ("has_ceiling", Value::is_boolean),
            ("has_ender_dragon_fight", Value::is_boolean),
            ("coordinate_scale", Value::is_number),
            ("min_y", Value::is_i64),
            ("height", Value::is_i64),
            ("logical_height", Value::is_i64),
            ("infiniburn", Value::is_string),
            ("ambient_light", Value::is_number),
            ("monster_spawn_light_level", |v| v.is_i64() || v.is_object()),
            ("monster_spawn_block_light_limit", Value::is_i64),
        ],
    )?;
    let int = |field: &str| json[field].as_i64().unwrap_or_default();
    let (min_y, height, logical) = (int("min_y"), int("height"), int("logical_height"));
    if height < 16 || height % 16 != 0 || min_y % 16 != 0 {
        return Err("height and min_y must be multiples of 16, height at least 16".to_owned());
    }
    if !(-2032..=2031).contains(&min_y) || min_y + height > 2032 {
        return Err("min_y + height cannot be higher than 2032".to_owned());
    }
    if logical > height {
        return Err(format!(
            "logical_height cannot be higher than height: {logical} > {height}"
        ));
    }
    dimension_type(ctx.id, json)?;
    json_network_bytes(json)
}

pub fn json_to_nbt(value: &Value) -> Option<Tag> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i32::try_from(i).map_or(Tag::Long(i), Tag::Int),
            (None, Some(f)) => Tag::Double(f),
            _ => return None,
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(values) => Tag::List(values.iter().map(json_to_nbt).collect::<Option<_>>()?),
        Value::Object(map) => Tag::Compound(
            map.iter()
                .map(|(k, v)| Some((k.clone(), json_to_nbt(v)?)))
                .collect::<Option<Compound>>()?,
        ),
    })
}

const MAX_Y: i64 = 2031;
const MIN_Y: i64 = -2032;
const Y_SIZE: i64 = 4064;

/// A data pack or plugin dimension type, checked the way the world decodes it.
pub fn dimension_type(name: &str, json: &Value) -> Result<DimensionType, String> {
    let field = |key: &str| {
        json.get(key)
            .ok_or_else(|| format!("dimension type {name}: no {key}"))
    };
    let flag = |key: &str| {
        field(key)?
            .as_bool()
            .ok_or_else(|| format!("dimension type {name}: {key} is not a boolean"))
    };
    let int = |key: &str| {
        field(key)?
            .as_i64()
            .ok_or_else(|| format!("dimension type {name}: {key} is not an integer"))
    };
    let number = |key: &str| {
        field(key)?
            .as_f64()
            .ok_or_else(|| format!("dimension type {name}: {key} is not a number"))
    };
    let invalid = |what: &str| Err(format!("dimension type {name}: {what}"));
    let min_y = int("min_y")?;
    let height = int("height")?;
    let logical_height = int("logical_height")?;
    if !(MIN_Y..=MAX_Y).contains(&min_y) || min_y % 16 != 0 {
        return invalid("min_y must be a multiple of 16 within -2032..=2031");
    }
    if !(16..=Y_SIZE).contains(&height) || height % 16 != 0 {
        return invalid("height must be a multiple of 16 within 16..=4064");
    }
    if min_y + height > MAX_Y + 1 {
        return invalid("min_y + height cannot be higher than 2032");
    }
    if !(0..=height).contains(&logical_height) {
        return invalid("logical_height cannot be higher than height");
    }
    let coordinate_scale = number("coordinate_scale")?;
    if !(1e-5..=3e7).contains(&coordinate_scale) {
        return invalid("coordinate_scale must be within 0.00001..=30000000");
    }
    let infiniburn = field("infiniburn")?
        .as_str()
        .ok_or_else(|| format!("dimension type {name}: infiniburn is not a string"))?;
    let default_clock = match json.get("default_clock") {
        None => None,
        Some(clock) => Some(leak(clock.as_str().ok_or_else(|| {
            format!("dimension type {name}: default_clock is not a string")
        })?)),
    };
    let block_light_limit = int("monster_spawn_block_light_limit")?;
    if !(0..=15).contains(&block_light_limit) {
        return invalid("monster_spawn_block_light_limit must be within 0..=15");
    }
    let light_level = match field("monster_spawn_light_level")? {
        Value::Object(provider) => {
            let bound = |key: &str| provider.get(key).and_then(Value::as_i64);
            match (
                provider.get("type").and_then(Value::as_str),
                bound("min_inclusive"),
                bound("max_inclusive"),
            ) {
                (Some("minecraft:uniform"), Some(min), Some(max)) if min <= max => (min, max),
                _ => return invalid("monster_spawn_light_level must be an int or uniform"),
            }
        }
        level => {
            let level = level.as_i64().ok_or_else(|| {
                format!("dimension type {name}: monster_spawn_light_level is not an int")
            })?;
            (level, level)
        }
    };
    if !(0..=15).contains(&light_level.0) || !(0..=15).contains(&light_level.1) {
        return invalid("monster_spawn_light_level must be within 0..=15");
    }
    Ok(DimensionType {
        name: leak(name),
        min_y: min_y as i32,
        height: height as u32,
        logical_height: logical_height as u32,
        has_skylight: flag("has_skylight")?,
        has_ceiling: flag("has_ceiling")?,
        has_ender_dragon_fight: flag("has_ender_dragon_fight")?,
        coordinate_scale,
        ambient_light: number("ambient_light")? as f32,
        infiniburn: leak(infiniburn),
        default_clock,
        monster_spawn_block_light_limit: block_light_limit as u8,
        monster_spawn_light_level: (light_level.0 as u8, light_level.1 as u8),
    })
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

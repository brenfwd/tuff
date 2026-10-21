//! The data pack's `minecraft:sulfur_cube_archetype` entries: what a block
//! swallowed by a sulfur cube does to it. Every field is checked, so a new
//! field in a future jar fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use crate::input::Input;
use crate::names::{pascal, snake, str_literal, strip_namespace};

const FIELDS: [&str; 7] = [
    "items",
    "attribute_modifiers",
    "buoyant",
    "explosion",
    "contact_damage",
    "knockback_modifiers",
    "sound_settings",
];

pub fn generate(input: &Input) -> Result<String> {
    let attributes = registry_names(input, "minecraft:attribute")?;
    let sounds = registry_names(input, "minecraft:sound_event")?;
    let items = registry_names(input, "minecraft:item")?;
    let mut archetypes = BTreeMap::new();
    for (path, value) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("sulfur_cube_archetype/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let name = format!("{namespace}:{name}");
        let code = archetype(&name, value, &attributes, &sounds, &items)
            .with_context(|| format!("data/{path}"))?;
        archetypes.insert(name, code);
    }
    ensure!(
        !archetypes.is_empty(),
        "no sulfur cube archetypes in the data pack"
    );
    let mut out = String::new();
    writeln!(
        out,
        "/// Every sulfur cube archetype, sorted by name.\npub static ALL: &[Archetype] = &["
    )?;
    for code in archetypes.values() {
        writeln!(out, "    {code},")?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

fn registry_names(input: &Input, registry: &str) -> Result<Vec<String>> {
    Ok(input
        .registries
        .get(registry)
        .with_context(|| format!("no {registry} registry"))?
        .entries
        .keys()
        .cloned()
        .collect())
}

fn known<'a>(names: &[String], name: &'a str, what: &str) -> Result<&'a str> {
    ensure!(names.iter().any(|n| n == name), "unknown {what} {name}");
    Ok(name)
}

fn checked<'a>(value: &'a Value, what: &str, fields: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .with_context(|| format!("{what} is not an object"))?;
    for key in object.keys() {
        ensure!(fields.contains(&key.as_str()), "unknown {what} field {key}");
    }
    Ok(object)
}

fn float(object: &Map<String, Value>, key: &str) -> Result<String> {
    let value = object
        .get(key)
        .with_context(|| format!("no {key}"))?
        .as_f64()
        .with_context(|| format!("{key} is not a number"))?;
    Ok(format!("{:?}f32", value as f32))
}

fn sound(value: Option<&Value>, sounds: &[String], key: &str) -> Result<String> {
    let id = value
        .with_context(|| format!("no {key}"))?
        .as_str()
        .with_context(|| format!("{key} is not a sound id"))?;
    let id = known(sounds, id, "sound event")?;
    Ok(format!(
        "crate::registry::SoundEvent::{}",
        pascal(strip_namespace(id))
    ))
}

fn archetype(
    name: &str,
    value: &Value,
    attributes: &[String],
    sounds: &[String],
    items: &[String],
) -> Result<String> {
    let object = checked(value, "archetype", &FIELDS)?;
    let item_set = match object.get("items").context("no items")? {
        Value::String(tag) if tag.starts_with('#') => {
            format!("Items::Tag({})", str_literal(&tag[1..]))
        }
        Value::String(id) => format!(
            "Items::List(&[crate::Item::{}])",
            pascal(known(items, id, "item")?)
        ),
        Value::Array(list) => {
            let mut out = String::from("Items::List(&[");
            for id in list {
                let id = id.as_str().context("item is not an id")?;
                write!(out, "crate::Item::{}, ", pascal(known(items, id, "item")?))?;
            }
            out.push_str("])");
            out
        }
        other => bail!("items is {other}"),
    };
    let mut modifiers = String::new();
    for entry in object
        .get("attribute_modifiers")
        .context("no attribute_modifiers")?
        .as_array()
        .context("attribute_modifiers is not a list")?
    {
        let entry = object_of(entry)?;
        let attribute = entry
            .get("attribute")
            .and_then(Value::as_str)
            .context("modifier without attribute")?;
        let attribute = known(attributes, attribute, "attribute")?;
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .context("modifier without id")?;
        let amount = entry
            .get("amount")
            .and_then(Value::as_f64)
            .context("modifier without amount")?;
        let operation = entry
            .get("operation")
            .and_then(Value::as_str)
            .context("modifier without operation")?;
        ensure!(
            ["add_value", "add_multiplied_base", "add_multiplied_total"].contains(&operation),
            "unknown modifier operation {operation}"
        );
        write!(
            modifiers,
            "ArchetypeModifier {{ attribute: crate::registry::Attribute::{}, id: {}, amount: {amount:?}, operation: crate::item::ModifierOperation::{} }}, ",
            pascal(strip_namespace(attribute)),
            str_literal(id),
            pascal(operation),
        )?;
    }
    let buoyant = match object.get("buoyant") {
        None => false,
        Some(v) => v.as_bool().context("buoyant is not a boolean")?,
    };
    let explosion = match object.get("explosion") {
        None => "None".to_owned(),
        Some(v) => {
            let e = checked(v, "explosion", &["power", "causes_fire", "fuse"])?;
            let int = |key: &str| -> Result<i64> {
                e.get(key)
                    .and_then(Value::as_i64)
                    .with_context(|| format!("explosion {key} is not an integer"))
            };
            let fire = e
                .get("causes_fire")
                .and_then(Value::as_bool)
                .context("causes_fire is not a boolean")?;
            format!(
                "Some(ExplosionData {{ power: {}, causes_fire: {fire}, fuse: {} }})",
                int("power")?,
                int("fuse")?
            )
        }
    };
    let contact = match object.get("contact_damage") {
        None => "None".to_owned(),
        Some(v) => {
            let c = checked(
                v,
                "contact_damage",
                &["damage_type", "amount", "attribute_to_source"],
            )?;
            let kind = c
                .get("damage_type")
                .and_then(Value::as_str)
                .context("damage_type is not an id")?;
            let (min, max) = match c.get("amount").context("no amount")? {
                Value::Number(n) => {
                    let v = n.as_f64().context("amount")? as f32;
                    (v, v)
                }
                Value::Object(provider) => match provider.get("type").and_then(Value::as_str) {
                    Some("minecraft:uniform") => {
                        let bound = |k: &str| -> Result<f32> {
                            Ok(provider
                                .get(k)
                                .and_then(Value::as_f64)
                                .with_context(|| format!("uniform {k}"))?
                                as f32)
                        };
                        (bound("min_inclusive")?, bound("max_exclusive")?)
                    }
                    Some("minecraft:constant") => {
                        let v = provider
                            .get("value")
                            .and_then(Value::as_f64)
                            .context("constant value")? as f32;
                        (v, v)
                    }
                    other => bail!("unsupported contact damage amount {other:?}"),
                },
                other => bail!("contact damage amount is {other}"),
            };
            let source = c
                .get("attribute_to_source")
                .and_then(Value::as_bool)
                .context("attribute_to_source is not a boolean")?;
            format!(
                "Some(ContactDamage {{ damage_type: &crate::damage_type::{}, min: {min:?}f32, max: {max:?}f32, attribute_to_source: {source} }})",
                snake(strip_namespace(kind)).to_ascii_uppercase()
            )
        }
    };
    let knockback = checked(
        object
            .get("knockback_modifiers")
            .context("no knockback_modifiers")?,
        "knockback_modifiers",
        &["horizontal_power", "vertical_power"],
    )?;
    let sound_settings = checked(
        object.get("sound_settings").context("no sound_settings")?,
        "sound_settings",
        &[
            "hit_sound",
            "push_sound",
            "push_sound_impulse_threshold",
            "push_sound_cooldown",
        ],
    )?;
    Ok(format!(
        "Archetype {{ name: {}, items: {item_set}, attribute_modifiers: &[{modifiers}], buoyant: {buoyant}, \
         explosion: {explosion}, contact_damage: {contact}, \
         knockback: KnockbackModifiers {{ horizontal_power: {}, vertical_power: {} }}, \
         sounds: SoundSettings {{ hit: {}, push: {}, push_threshold: {}, push_cooldown: {} }} }}",
        str_literal(name),
        float(knockback, "horizontal_power")?,
        float(knockback, "vertical_power")?,
        sound(sound_settings.get("hit_sound"), sounds, "hit_sound")?,
        sound(sound_settings.get("push_sound"), sounds, "push_sound")?,
        float(sound_settings, "push_sound_impulse_threshold")?,
        float(sound_settings, "push_sound_cooldown")?,
    ))
}

fn object_of(value: &Value) -> Result<&Map<String, Value>> {
    checked(
        value,
        "attribute modifier",
        &["attribute", "id", "amount", "operation"],
    )
}

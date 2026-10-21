//! The data pack's `minecraft:block_transformer` entries (what axes, hoes
//! and shovels do to the blocks they are used on) and the items'
//! `minecraft:block_transformer` components. Every field is checked, so a
//! new field or kind in a future jar fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use super::array_body;
use super::registries::{self, sorted_entries};
use super::tags::{Tags, resolved_registry_tags};
use crate::input::Input;
use crate::names::{pascal, str_literal};

const TRANSFORM_FIELDS: [&str; 10] = [
    "block_state_provider",
    "sound",
    "particle",
    "disallowed_faces",
    "loot",
    "drop_strategy",
    "update_from_neighbors",
    "transform_type",
    "consume_on_use",
    "item_damage_per_use",
];
const PARTICLES: [&str; 4] = ["none", "scrape", "wax_on", "wax_off"];
const DROP_STRATEGIES: [&str; 2] = ["clicked_face", "from_middle"];
const TRANSFORM_TYPES: [&str; 2] = ["single_block", "copper_chest"];
const FACES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
/// The sound a transform without one plays: none at all.
const EMPTY_SOUND: &str = "minecraft:intentionally_empty";

struct Data<'a> {
    input: &'a Input,
    blocks: BTreeMap<&'a str, String>,
    block_tags: Tags,
    sounds: BTreeMap<&'a str, String>,
}

pub fn generate(input: &Input) -> Result<String> {
    let context = Data {
        input,
        blocks: registries::variants(&input.registries["minecraft:block"])?,
        block_tags: resolved_registry_tags(input, "minecraft:block")?,
        sounds: registries::variants(&input.registries["minecraft:sound_event"])?,
    };

    let mut transformers = BTreeMap::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("block_transformer/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let name = format!("{namespace}:{name}");
        let code = transformer(&context, json).with_context(|| format!("data/{path}"))?;
        transformers.insert(name, code);
    }
    ensure!(
        !transformers.is_empty(),
        "no block transformers in the data pack"
    );

    let names: Vec<&String> = transformers.keys().collect();
    let items = sorted_entries(&input.registries["minecraft:item"])?;
    let mut item_index = Vec::with_capacity(items.len());
    for item in &items {
        let components = input
            .item_components
            .get(*item)
            .with_context(|| format!("no component report for {item}"))?;
        item_index.push(match components.get("minecraft:block_transformer") {
            None => u8::MAX,
            Some(Value::String(name)) => {
                let index = names
                    .iter()
                    .position(|n| *n == name)
                    .with_context(|| format!("{item} names unknown block transformer {name}"))?;
                u8::try_from(index)?
            }
            Some(other) => bail!("{item}: inline block transformer {other} is not modeled"),
        });
    }

    let mut out = String::new();
    write!(
        out,
        "pub static ALL: [(&str, BlockTransformer); {}] = [",
        transformers.len()
    )?;
    for (name, code) in &transformers {
        write!(
            out,
            "\n    ({}, BlockTransformer {{ name: {}, transforms: &[{code}] }}),",
            str_literal(name),
            str_literal(name)
        )?;
    }
    writeln!(out, "\n];")?;
    write!(
        out,
        "static ITEM_BLOCK_TRANSFORMER: [u8; {}] = [",
        item_index.len()
    )?;
    array_body(&mut out, &item_index);
    writeln!(out, "];")?;
    Ok(out)
}

fn fields<'v>(value: &'v Value, known: &[&str]) -> Result<&'v Map<String, Value>> {
    let map = value.as_object().context("expected an object")?;
    for key in map.keys() {
        ensure!(known.contains(&key.as_str()), "unknown field {key}");
    }
    Ok(map)
}

fn one_of(value: Option<&Value>, allowed: &[&str], default: &str) -> Result<String> {
    let value = match value {
        None => default,
        Some(value) => value.as_str().context("expected a string")?,
    };
    ensure!(allowed.contains(&value), "unknown value {value}");
    Ok(pascal(value))
}

fn boolean(value: Option<&Value>, default: bool) -> Result<bool> {
    match value {
        None => Ok(default),
        Some(value) => value.as_bool().context("expected a boolean"),
    }
}

fn transformer(context: &Data, json: &Value) -> Result<String> {
    let list = json.as_array().context("expected a list of transforms")?;
    ensure!(!list.is_empty(), "no transforms");
    let mut code = Vec::new();
    for transform in list {
        let f = fields(transform, &TRANSFORM_FIELDS)?;
        let provider = rule_provider(
            context,
            f.get("block_state_provider")
                .context("missing block_state_provider")?,
        )?;
        let sound = match f.get("sound") {
            None => "None".to_string(),
            Some(value) => {
                let name = value.as_str().context("bad sound")?;
                if name == EMPTY_SOUND {
                    "None".to_string()
                } else {
                    let variant = context
                        .sounds
                        .get(name)
                        .with_context(|| format!("unknown sound {name}"))?;
                    format!("Some(SoundEvent::{variant})")
                }
            }
        };
        let faces = match f.get("disallowed_faces") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .context("bad disallowed_faces")?
                .iter()
                .map(|face| {
                    let face = face.as_str().context("bad face")?;
                    ensure!(FACES.contains(&face), "unknown face {face}");
                    Ok(format!("Facing::{}", pascal(face)))
                })
                .collect::<Result<_>>()?,
        };
        let loot = match f.get("loot") {
            None => "None".to_string(),
            Some(value) => {
                let name = value.as_str().context("bad loot")?;
                let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
                ensure!(
                    context
                        .input
                        .data_json
                        .contains_key(&format!("{namespace}/loot_table/{path}.json")),
                    "unknown loot table {name}"
                );
                format!("Some({})", str_literal(name))
            }
        };
        let damage = match f.get("item_damage_per_use") {
            None => 0,
            Some(value) => value.as_u64().context("bad item_damage_per_use")?,
        };
        code.push(format!(
            "Transform {{ provider: {provider}, sound: {sound}, particle: TransformParticle::{}, \
             disallowed_faces: &[{}], loot: {loot}, drop_strategy: DropStrategy::{}, \
             update_from_neighbors: {}, transform_type: TransformType::{}, consume_on_use: {}, \
             item_damage_per_use: {damage} }}",
            one_of(f.get("particle"), &PARTICLES, "none")?,
            faces.join(", "),
            one_of(f.get("drop_strategy"), &DROP_STRATEGIES, "from_middle")?,
            boolean(f.get("update_from_neighbors"), true)?,
            one_of(f.get("transform_type"), &TRANSFORM_TYPES, "single_block")?,
            boolean(f.get("consume_on_use"), true)?,
        ));
    }
    Ok(code.join(", "))
}

fn rule_provider(context: &Data, value: &Value) -> Result<String> {
    let f = fields(value, &["type", "rules", "fallback"])?;
    ensure!(
        f.get("type").and_then(Value::as_str) == Some("minecraft:rule_based"),
        "only rule_based state providers are modeled"
    );
    ensure!(
        f.get("fallback").is_none(),
        "rule_based fallbacks are not modeled"
    );
    let mut rules = Vec::new();
    for rule in f
        .get("rules")
        .and_then(Value::as_array)
        .context("missing rules")?
    {
        let r = fields(rule, &["if_true", "then"])?;
        rules.push(format!(
            "Rule {{ if_true: {}, then: {} }}",
            predicate(context, r.get("if_true").context("missing if_true")?)?,
            source(context, r.get("then").context("missing then")?)?,
        ));
    }
    Ok(format!("&[{}]", rules.join(", ")))
}

fn predicate(context: &Data, value: &Value) -> Result<String> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .context("predicate without type")?;
    let offset = |f: &Map<String, Value>| -> Result<String> {
        Ok(match f.get("offset") {
            None => "[0, 0, 0]".to_string(),
            Some(value) => {
                let values = value.as_array().context("bad offset")?;
                ensure!(values.len() == 3, "bad offset");
                let values: Vec<i64> = values
                    .iter()
                    .map(|v| v.as_i64().context("bad offset"))
                    .collect::<Result<_>>()?;
                format!("[{}, {}, {}]", values[0], values[1], values[2])
            }
        })
    };
    Ok(match kind {
        "minecraft:matching_blocks" => {
            let f = fields(value, &["type", "offset", "blocks"])?;
            let names: Vec<&str> = match f.get("blocks").context("missing blocks")? {
                Value::String(name) => vec![name.as_str()],
                Value::Array(names) => names
                    .iter()
                    .map(|n| n.as_str().context("bad block"))
                    .collect::<Result<_>>()?,
                _ => bail!("bad blocks"),
            };
            format!(
                "Predicate::MatchingBlocks {{ offset: {}, blocks: {} }}",
                offset(f)?,
                block_list(context, names.iter().copied())?
            )
        }
        "minecraft:matching_block_tag" => {
            let f = fields(value, &["type", "offset", "tag"])?;
            let tag = f.get("tag").and_then(Value::as_str).context("bad tag")?;
            let members = context
                .block_tags
                .get(tag)
                .with_context(|| format!("unknown block tag {tag}"))?;
            format!(
                "Predicate::MatchingBlocks {{ offset: {}, blocks: {} }}",
                offset(f)?,
                block_list(context, members.iter().map(String::as_str))?
            )
        }
        "minecraft:all_of" | "minecraft:any_of" => {
            let f = fields(value, &["type", "predicates"])?;
            let inner: Vec<String> = f
                .get("predicates")
                .and_then(Value::as_array)
                .context("missing predicates")?
                .iter()
                .map(|p| predicate(context, p))
                .collect::<Result<_>>()?;
            let variant = if kind == "minecraft:all_of" {
                "AllOf"
            } else {
                "AnyOf"
            };
            format!("Predicate::{variant}(&[{}])", inner.join(", "))
        }
        "minecraft:not" => {
            let f = fields(value, &["type", "predicate"])?;
            format!(
                "Predicate::Not(&{})",
                predicate(context, f.get("predicate").context("missing predicate")?)?
            )
        }
        other => bail!("unmodeled block predicate {other}"),
    })
}

/// A sorted `&[Block]` literal.
fn block_list<'a>(context: &Data, names: impl Iterator<Item = &'a str>) -> Result<String> {
    let order = sorted_entries(&context.input.registries["minecraft:block"])?;
    let mut indices: Vec<usize> = names
        .map(|name| {
            order
                .iter()
                .position(|n| *n == name)
                .with_context(|| format!("unknown block {name}"))
        })
        .collect::<Result<_>>()?;
    indices.sort_unstable();
    indices.dedup();
    let variants: Vec<String> = indices
        .iter()
        .map(|i| format!("Block::{}", context.blocks[order[*i]]))
        .collect();
    Ok(format!("&[{}]", variants.join(", ")))
}

fn source(context: &Data, value: &Value) -> Result<String> {
    match value.get("type").and_then(Value::as_str) {
        None => Ok(format!("StateSource::State({})", state(context, value)?)),
        Some("minecraft:simple_state_provider") => {
            let f = fields(value, &["type", "state"])?;
            Ok(format!(
                "StateSource::State({})",
                state(context, f.get("state").context("missing state")?)?
            ))
        }
        Some("minecraft:copy_properties") => {
            let f = fields(value, &["type", "source"])?;
            let inner = f.get("source").context("missing source")?;
            ensure!(
                inner.get("type").is_none(),
                "copy_properties from a provider is not modeled"
            );
            Ok(format!(
                "StateSource::CopyProperties({})",
                state(context, inner)?
            ))
        }
        Some(other) => bail!("unmodeled block state provider {other}"),
    }
}

/// `{id, properties}` as a `BlockState` literal.
fn state(context: &Data, value: &Value) -> Result<String> {
    let f = fields(value, &["id", "properties"])?;
    let name = f.get("id").and_then(Value::as_str).context("missing id")?;
    let block = context
        .input
        .blocks
        .get(name)
        .with_context(|| format!("unknown block {name}"))?;
    let default = block
        .states
        .iter()
        .find(|s| s.default)
        .with_context(|| format!("{name} has no default state"))?;
    let mut wanted = default.properties.clone();
    if let Some(properties) = f.get("properties") {
        for (key, value) in properties.as_object().context("bad properties")? {
            let value = value.as_str().context("bad property value")?;
            ensure!(
                block
                    .properties
                    .get(key)
                    .is_some_and(|values| values.iter().any(|v| v == value)),
                "{name} has no {key}={value}"
            );
            wanted.insert(key.clone(), value.to_string());
        }
    }
    let state = block
        .states
        .iter()
        .find(|s| s.properties == wanted)
        .with_context(|| format!("no state of {name} matches"))?;
    Ok(format!("BlockState::raw({})", state.id))
}

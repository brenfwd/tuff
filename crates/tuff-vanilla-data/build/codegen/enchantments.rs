//! Enchantment definitions from the data pack (`enchantment/`): the levels,
//! costs, the items each one goes on and the enchantments it excludes, all
//! typed. Item sets (`#tag`, one item, or a list) are expanded to items at
//! build time. The open-ended `effects` map and the `description` component
//! are kept as raw JSON.

use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{json, registries, tags};
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enchantment {
    description: Value,
    #[serde(default)]
    exclusive_set: Option<Value>,
    supported_items: Value,
    #[serde(default)]
    primary_items: Option<Value>,
    weight: u32,
    max_level: u32,
    min_cost: Cost,
    max_cost: Cost,
    anvil_cost: u32,
    slots: Vec<String>,
    #[serde(default)]
    effects: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cost {
    base: i32,
    per_level_above_first: i32,
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// A `#tag`, single id or list of ids as the ids it names.
fn resolve_set(value: &Value, tags: &tags::Tags, what: &str) -> Result<Vec<String>> {
    let ids: Vec<&str> = match value {
        Value::String(id) => vec![id.as_str()],
        Value::Array(ids) => ids
            .iter()
            .map(|id| {
                id.as_str()
                    .with_context(|| format!("{what}: non-string entry"))
            })
            .collect::<Result<_>>()?,
        _ => bail!("{what}: expected a tag, id or list"),
    };
    let mut out = Vec::new();
    for id in ids {
        match id.strip_prefix('#') {
            Some(tag) => {
                let tag = qualified(tag);
                let entries = tags
                    .get(&tag)
                    .with_context(|| format!("{what}: unknown tag {tag}"))?;
                out.extend(entries.iter().cloned());
            }
            None => out.push(qualified(id)),
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn generate(input: &Input) -> Result<String> {
    let items = registries::variants(
        input
            .registries
            .get("minecraft:item")
            .context("registries.json has no minecraft:item")?,
    )?;
    let item_tags = tags::resolved_registry_tags(input, "minecraft:item")?;
    let enchantment_tags = tags::resolved_registry_tags(input, "minecraft:enchantment")?;

    let mut enchantments = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("enchantment/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let parsed: Enchantment =
            serde_json::from_value(json.clone()).with_context(|| format!("data/{path}"))?;
        ensure!(
            parsed.max_level >= 1,
            "data/{path}: max_level must be positive"
        );
        enchantments.push((format!("{namespace}:{name}"), parsed, path));
    }
    enchantments.sort_by(|(a, ..), (b, ..)| a.cmp(b));
    ensure!(
        !enchantments.is_empty(),
        "no enchantments found in the data pack"
    );

    let consts = names::unique(enchantments.iter().map(|(name, ..)| name.as_str()), shouty)?;
    let item_list = |ids: &[String], what: &str| -> Result<String> {
        let variants = ids
            .iter()
            .map(|id| {
                items
                    .get(id.as_str())
                    .map(|variant| format!("Item::{variant}"))
                    .with_context(|| format!("{what}: unknown item {id}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(format!("&[{}]", variants.join(", ")))
    };
    let mut out = String::new();
    for ((name, enchantment, path), ident) in enchantments.iter().zip(&consts) {
        let what = format!("data/{path}");
        let supported = item_list(
            &resolve_set(&enchantment.supported_items, &item_tags, &what)?,
            &what,
        )?;
        let primary = match &enchantment.primary_items {
            Some(set) => format!(
                "Some({})",
                item_list(&resolve_set(set, &item_tags, &what)?, &what)?
            ),
            None => "None".to_string(),
        };
        let exclusive = match &enchantment.exclusive_set {
            Some(set) => resolve_set(set, &enchantment_tags, &what)?,
            None => Vec::new(),
        };
        let exclusive: Vec<String> = exclusive.iter().map(|id| str_literal(id)).collect();
        let slots: Vec<String> = enchantment.slots.iter().map(|s| str_literal(s)).collect();
        let effects: Vec<String> = enchantment
            .effects
            .iter()
            .map(|(key, value)| {
                format!(
                    "({}, {})",
                    str_literal(&qualified(key)),
                    json::literal(value)
                )
            })
            .collect();
        writeln!(
            out,
            "pub const {ident}: Enchantment = Enchantment {{ name: {}, description: {}, max_level: {}, weight: {}, anvil_cost: {}, min_cost: EnchantmentCost {{ base: {}, per_level_above_first: {} }}, max_cost: EnchantmentCost {{ base: {}, per_level_above_first: {} }}, slots: &[{}], supported_items: {supported}, primary_items: {primary}, exclusive_set: &[{}], effects: &[{}] }};",
            str_literal(name),
            json::literal(&enchantment.description),
            enchantment.max_level,
            enchantment.weight,
            enchantment.anvil_cost,
            enchantment.min_cost.base,
            enchantment.min_cost.per_level_above_first,
            enchantment.max_cost.base,
            enchantment.max_cost.per_level_above_first,
            slots.join(", "),
            exclusive.join(", "),
            effects.join(", "),
        )?;
    }
    writeln!(
        out,
        "/// Every enchantment as `(name, definition)`, sorted by name.\npub const ALL: &[(&str, &Enchantment)] = &[{}];",
        enchantments
            .iter()
            .zip(&consts)
            .map(|((name, ..), ident)| format!("({}, &{ident})", str_literal(name)))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    Ok(out)
}

//! Every loot table in `data/minecraft/loot_table/`.
//!
//! A pool's or entry's condition (`condition`) and functions (`modifier`) can
//! reference the separate item/block/entity "predicate" mini-language, which
//! this crate does not model further: their parameters are kept as raw JSON
//! text. A bare condition name that is not itself a registered condition kind
//! is a reference to a stored predicate file (`data/minecraft/predicate/`),
//! which is resolved and inlined here so nothing is lost.

use std::fmt::Write;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{json, registries, tags};
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

pub fn generate(input: &Input) -> Result<String> {
    let condition_kinds =
        registries::sorted_entries(&input.registries["minecraft:loot_condition_type"])?;
    let item_variants = registries::variants(&input.registries["minecraft:item"])?;
    let item_tags = tags::resolved_registry_tags(input, "minecraft:item")?;
    let ctx = Ctx {
        input,
        condition_kinds,
        item_variants,
        item_tags,
    };

    let mut tables = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("loot_table/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        tables.push((format!("{namespace}:{name}"), json));
    }
    tables.sort_by(|(a, _), (b, _)| a.cmp(b));

    let kinds: Vec<String> = tables
        .iter()
        .filter_map(|(_, json)| json.get("type").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut out = String::new();
    super::enums::emit_value_enum(
        &mut out,
        "LootTableKind",
        &super::enums::unique_sorted(kinds),
    )?;

    let consts = names::unique(tables.iter().map(|(name, _)| name.as_str()), shouty)?;
    for ((name, json), ident) in tables.iter().zip(&consts) {
        let table = ctx
            .table(json)
            .with_context(|| format!("loot table {name}"))?;
        writeln!(out, "pub static {ident}: LootTable = {table};")?;
    }
    writeln!(out, "pub const ALL: &[(&str, &LootTable)] = &[")?;
    for (name, ident) in tables.iter().map(|(name, _)| name).zip(&consts) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;

    writeln!(
        out,
        "/// Every stored predicate file, by resource location, as compact JSON."
    )?;
    writeln!(out, "pub const PREDICATES: &[(&str, &str)] = &[")?;
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("predicate/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        writeln!(
            out,
            "    ({}, {}),",
            str_literal(&format!("{namespace}:{name}")),
            json::literal(json)
        )?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

/// Every JSON field the generator reads, per object kind. A field outside
/// these fails the build, so a new field in a future version is noticed and
/// modeled instead of silently dropped.
const TABLE_FIELDS: &[&str] = &["type", "pools", "random_sequence", "modifier"];
const POOL_FIELDS: &[&str] = &["rolls", "bonus_rolls", "condition", "modifier", "entries"];
/// Fields every entry kind accepts, in addition to its own below.
const ENTRY_FIELDS: &[&str] = &["type", "weight", "quality", "condition", "modifier"];
const ENTRY_KIND_FIELDS: &[(&str, &[&str])] = &[
    ("minecraft:item", &["name"]),
    ("minecraft:tag", &["items", "expand"]),
    ("minecraft:loot_table", &["value"]),
    ("minecraft:dynamic", &["name"]),
    ("minecraft:empty", &[]),
    ("minecraft:group", &["children"]),
    ("minecraft:alternatives", &["children"]),
    ("minecraft:sequence", &["children"]),
];

fn check_fields(what: &str, value: &Value, known: &[&[&str]]) -> Result<()> {
    for field in value.as_object().into_iter().flat_map(|o| o.keys()) {
        anyhow::ensure!(
            known.iter().any(|list| list.contains(&field.as_str())),
            "{what} has an unhandled field `{field}`"
        );
    }
    Ok(())
}

struct Ctx<'a> {
    input: &'a Input,
    /// Every `minecraft:loot_condition_type` entry, used to tell a
    /// parameterless condition name apart from a stored predicate reference.
    condition_kinds: Vec<&'a str>,
    item_variants: std::collections::BTreeMap<&'a str, String>,
    item_tags: tags::Tags,
}

impl Ctx<'_> {
    fn table(&self, value: &Value) -> Result<String> {
        check_fields("loot table", value, &[TABLE_FIELDS])?;
        let kind = match value.get("type") {
            Some(kind) => format!(
                "Some(LootTableKind::{})",
                names::pascal(kind.as_str().context("type is not a string")?)
            ),
            None => "None".to_owned(),
        };
        Ok(format!(
            "LootTable {{ kind: {kind}, random_sequence: {}, functions: Static(&[{}]), pools: Static(&[{}]) }}",
            match value.get("random_sequence") {
                Some(sequence) => format!(
                    "Some(Static({}))",
                    str_literal(
                        sequence
                            .as_str()
                            .context("random_sequence is not a string")?
                    )
                ),
                None => "None".to_owned(),
            },
            self.functions(value, "modifier")?.join(", "),
            self.pools(value)?.join(", "),
        ))
    }

    fn pools(&self, table: &Value) -> Result<Vec<String>> {
        let Some(pools) = table.get("pools").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        pools.iter().map(|pool| self.pool(pool)).collect()
    }

    fn pool(&self, value: &Value) -> Result<String> {
        check_fields("loot pool", value, &[POOL_FIELDS])?;
        Ok(format!(
            "LootPool {{ rolls: {}, bonus_rolls: {}, condition: {}, functions: Static(&[{}]), entries: Static(&[{}]) }}",
            self.number(value.get("rolls").context("missing rolls")?)?,
            match value.get("bonus_rolls") {
                Some(v) => self.number(v)?,
                None => "NumberProvider::Constant(0.0)".to_owned(),
            },
            self.condition_opt(value)?,
            self.functions(value, "modifier")?.join(", "),
            self.entries(value)?.join(", "),
        ))
    }

    fn entries(&self, value: &Value) -> Result<Vec<String>> {
        let Some(entries) = value.get("entries").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        entries.iter().map(|entry| self.entry(entry)).collect()
    }

    fn entry(&self, value: &Value) -> Result<String> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .context("entry has no type")?;
        if let Some((_, own)) = ENTRY_KIND_FIELDS.iter().find(|(k, _)| *k == kind) {
            check_fields(kind, value, &[ENTRY_FIELDS, own])?;
        }
        let weight = value.get("weight").and_then(Value::as_u64).unwrap_or(1);
        let quality = value.get("quality").and_then(Value::as_i64).unwrap_or(0);
        let condition = self.condition_opt(value)?;
        let functions = self.functions(value, "modifier")?.join(", ");
        let entry_kind = match kind {
            "minecraft:item" => {
                format!(
                    "LootEntryKind::Item({})",
                    self.item(value.get("name").context("missing name")?)?
                )
            }
            "minecraft:tag" => {
                let tag = value
                    .get("items")
                    .and_then(Value::as_str)
                    .context("missing items")?;
                let tag = tag
                    .strip_prefix('#')
                    .with_context(|| format!("expected a #tag, got {tag}"))?;
                let items = self
                    .item_tags
                    .get(normalize(tag).as_str())
                    .with_context(|| format!("unknown item tag #{tag}"))?;
                let items = items
                    .iter()
                    .map(|i| self.item(&Value::String(i.clone())))
                    .collect::<Result<Vec<_>>>()?;
                let expand = value
                    .get("expand")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                format!(
                    "LootEntryKind::Tag {{ tag: Static({}), items: Static(&[{}]), expand: {expand} }}",
                    crate::names::str_literal(&normalize(tag)),
                    items.join(", ")
                )
            }
            "minecraft:loot_table" => match value.get("value").context("missing value")? {
                Value::String(name) => {
                    format!("LootEntryKind::LootTable(Static({}))", str_literal(name))
                }
                inline => format!(
                    "LootEntryKind::Inline(Static(&{}))",
                    self.table(inline).context("inline loot table")?
                ),
            },
            "minecraft:dynamic" => format!(
                "LootEntryKind::Dynamic(Static({}))",
                str_literal(
                    value
                        .get("name")
                        .and_then(Value::as_str)
                        .context("missing name")?
                )
            ),
            "minecraft:empty" => "LootEntryKind::Empty".to_owned(),
            "minecraft:group" => format!(
                "LootEntryKind::Group(Static(&[{}]))",
                self.children(value)?.join(", ")
            ),
            "minecraft:alternatives" => {
                format!(
                    "LootEntryKind::Alternatives(Static(&[{}]))",
                    self.children(value)?.join(", ")
                )
            }
            "minecraft:sequence" => format!(
                "LootEntryKind::Sequence(Static(&[{}]))",
                self.children(value)?.join(", ")
            ),
            other => format!(
                "LootEntryKind::Unknown {{ kind: Static({}), raw: Static({}) }}",
                str_literal(other),
                json::literal(value)
            ),
        };
        Ok(format!(
            "LootEntry {{ kind: {entry_kind}, weight: {weight}, quality: {quality}, condition: {condition}, functions: Static(&[{functions}]) }}",
        ))
    }

    fn children(&self, value: &Value) -> Result<Vec<String>> {
        value
            .get("children")
            .and_then(Value::as_array)
            .context("missing children")?
            .iter()
            .map(|child| self.entry(child))
            .collect()
    }

    fn item(&self, value: &Value) -> Result<String> {
        let name = value.as_str().context("expected an item name")?;
        let name = normalize(name);
        let variant = self
            .item_variants
            .get(name.as_str())
            .with_context(|| format!("unknown item {name}"))?;
        Ok(format!("Item::{variant}"))
    }

    /// A number provider. Bounds can themselves be nested providers; any
    /// shape besides a plain constant, `uniform`, or `binomial` over plain
    /// numbers is kept as [`NumberProvider::Other`].
    fn number(&self, value: &Value) -> Result<String> {
        if let Some(n) = value.as_f64() {
            return Ok(format!("NumberProvider::Constant({n:?})"));
        }
        let (kind, params) = json::kind_and_rest(value)?;
        let params = params.unwrap_or_else(|| Value::Object(Default::default()));
        let plain = |key: &str| params.get(key).and_then(Value::as_f64);
        let typed = match kind.as_str() {
            "minecraft:constant" => {
                plain("value").map(|v| format!("NumberProvider::Constant({v:?})"))
            }
            "minecraft:uniform" => plain("min").zip(plain("max")).map(|(min, max)| {
                format!("NumberProvider::Uniform {{ min: {min:?}, max: {max:?} }}")
            }),
            "minecraft:binomial" => plain("n")
                .zip(plain("p"))
                .map(|(n, p)| format!("NumberProvider::Binomial {{ n: {n:?}, p: {p:?} }}")),
            _ => None,
        };
        Ok(typed.unwrap_or_else(|| {
            format!(
                "NumberProvider::Other {{ kind: Static({}), params: Static({}) }}",
                str_literal(&kind),
                json::literal(&params)
            )
        }))
    }

    /// The `condition` field, resolving a bare predicate-reference string.
    fn condition_opt(&self, value: &Value) -> Result<String> {
        Ok(match value.get("condition") {
            Some(condition) => format!("Some({})", self.condition(condition)?),
            None => "None".to_owned(),
        })
    }

    fn condition(&self, value: &Value) -> Result<String> {
        let value = self.inline_predicates(value)?;
        let (kind, params) = json::kind_and_rest(&value)?;
        Ok(format!(
            "LootCondition {{ kind: Static({}), params: {} }}",
            str_literal(&kind),
            static_literal(params.as_ref())
        ))
    }

    /// `value` with every stored predicate reference replaced by the
    /// predicate itself, including those nested in `all_of`, `any_of` and
    /// `inverted` terms.
    fn inline_predicates(&self, value: &Value) -> Result<Value> {
        if let Some(name) = value.as_str() {
            let full = normalize(name);
            return if self.condition_kinds.contains(&full.as_str()) {
                Ok(serde_json::json!({ "type": full }))
            } else {
                self.inline_predicates(self.input.predicate(name)?)
            };
        }
        if let Value::Array(terms) = value {
            return self.inline_predicates(
                &serde_json::json!({ "type": "minecraft:all_of", "terms": terms }),
            );
        }
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            if let Some(term) = object.get_mut("term") {
                *term = self.inline_predicates(term)?;
            }
            if let Some(Value::Array(terms)) = object.get_mut("terms") {
                for term in terms {
                    *term = self.inline_predicates(term)?;
                }
            }
        }
        Ok(value)
    }

    /// Functions at `key`, which is `None`, a single object, or an array.
    fn functions(&self, value: &Value, key: &str) -> Result<Vec<String>> {
        let function = |v: &Value| -> Result<String> {
            let (kind, params) = json::kind_and_rest(v)?;
            Ok(format!(
                "LootFunction {{ kind: Static({}), params: {} }}",
                str_literal(&kind),
                static_literal(params.as_ref())
            ))
        };
        Ok(match value.get(key) {
            None => Vec::new(),
            Some(Value::Array(items)) => items.iter().map(function).collect::<Result<_>>()?,
            Some(other) => vec![function(other)?],
        })
    }
}

fn static_literal(value: Option<&Value>) -> String {
    match value {
        Some(value) => format!("Some(Static({}))", json::literal(value)),
        None => "None".to_owned(),
    }
}

fn normalize(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

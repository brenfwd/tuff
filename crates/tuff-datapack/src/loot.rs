//! Loot table files (`loot_table/<name>.json`) as the compiled-in loot
//! table shapes. Stored predicate references (`predicate/<name>.json`, the
//! packs' over the built-in ones) are inlined and item tags resolve against
//! the loaded [`TagSet`], as the built-in tables were at build time. A
//! parsed table owns its data, so it is freed with the last [`LootTables`]
//! using it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value};
use tuff_vanilla_data::Frozen;
use tuff_vanilla_data::loot_table::{
    LootCondition, LootEntry, LootEntryKind, LootFunction, LootPool, LootTable, LootTableKind,
    NumberProvider,
};
use tuff_vanilla_data::registry::{Item, LootConditionType};

use super::tags::TagSet;

/// How deeply stored predicates may refer to one another.
const MAX_PREDICATE_DEPTH: usize = 32;

/// The loot tables in effect: the built-in ones, with those packs define
/// replacing them by name.
#[derive(Debug, Default)]
pub struct LootTables {
    /// `None` for a table whose file did not parse: it rolls nothing.
    overrides: HashMap<String, Option<LootTable>>,
    slot_sources: Arc<crate::slot_source::SlotSources>,
}

impl LootTables {
    fn shared_vanilla() -> &'static Arc<LootTables> {
        static TABLES: OnceLock<Arc<LootTables>> = OnceLock::new();
        TABLES.get_or_init(Default::default)
    }

    /// The built-in tables alone.
    pub fn vanilla() -> Arc<LootTables> {
        Self::shared_vanilla().clone()
    }

    /// [`Self::vanilla`], borrowed for the life of the process.
    pub fn builtin() -> &'static LootTables {
        Self::shared_vanilla()
    }

    pub fn new(overrides: HashMap<String, Option<LootTable>>) -> Self {
        Self {
            overrides,
            slot_sources: Arc::default(),
        }
    }

    /// These tables with the stored slot sources `minecraft:slot` entries
    /// refer to.
    pub fn with_slot_sources(mut self, slot_sources: Arc<crate::slot_source::SlotSources>) -> Self {
        self.slot_sources = slot_sources;
        self
    }

    pub fn slot_source(&self, id: &str) -> Option<Arc<crate::slot_source::SlotSource>> {
        self.slot_sources.get(id)
    }

    pub fn get(&self, name: &str) -> Option<&LootTable> {
        match self.overrides.get(name) {
            Some(table) => table.as_ref(),
            None => LootTable::get(name),
        }
    }

    /// Whether no pack changed any table.
    pub fn is_vanilla(&self) -> bool {
        self.overrides.is_empty() && self.slot_sources.ids().next().is_none()
    }
}

/// Stored predicates by id: the packs', over the built-in ones. Each is
/// kept with every reference it makes inlined, so it is read once per
/// reload and freed with the last contents holding it.
#[derive(Debug)]
pub struct Predicates {
    raw: BTreeMap<String, Value>,
    resolved: BTreeMap<String, Arc<Value>>,
}

impl Default for Predicates {
    fn default() -> Self {
        Self::new(BTreeMap::new())
    }
}

impl Predicates {
    fn shared_vanilla() -> &'static Arc<Predicates> {
        static PREDICATES: OnceLock<Arc<Predicates>> = OnceLock::new();
        PREDICATES.get_or_init(|| Arc::new(Self::new(BTreeMap::new())))
    }

    /// The built-in predicates alone.
    pub fn vanilla() -> Arc<Predicates> {
        Self::shared_vanilla().clone()
    }

    /// Reads pack predicate files over the built-in ones; one that is not
    /// JSON, names an unknown condition or refers to a predicate that
    /// cannot be resolved is logged and left out.
    pub fn new(files: BTreeMap<String, (String, Vec<u8>)>) -> Self {
        let mut raw: BTreeMap<String, Value> = tuff_vanilla_data::loot_table::PREDICATES
            .iter()
            .filter_map(|(name, json)| Some(((*name).to_owned(), serde_json::from_str(json).ok()?)))
            .collect();
        let mut sources = BTreeMap::new();
        for (id, (source, bytes)) in files {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => {
                    raw.insert(id.clone(), value);
                    sources.insert(id, source);
                }
                Err(err) => log::warn!("Couldn't parse data file {id} from {source}: {err}"),
            }
        }
        let mut predicates = Self {
            raw,
            resolved: BTreeMap::new(),
        };
        let resolved = predicates
            .raw
            .iter()
            .filter_map(|(id, value)| match predicates.inline_at(value, 0) {
                Ok(condition) => Some((id.clone(), Arc::new(condition))),
                Err(err) => {
                    let source = sources.get(id).map_or("the built-in pack", String::as_str);
                    log::warn!("Couldn't parse data file {id} from {source}: {err}");
                    None
                }
            })
            .collect();
        predicates.resolved = resolved;
        predicates
    }

    /// The predicate `id` with its references inlined, if it loaded.
    pub fn get(&self, id: &str) -> Option<Arc<Value>> {
        self.resolved.get(id).cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.resolved.contains_key(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.resolved.keys().map(String::as_str)
    }

    /// `condition` (an inline predicate) with every stored predicate it
    /// refers to inlined, a list of terms as `all_of`, and every `type`
    /// namespaced.
    pub fn inline(&self, condition: &Value) -> Result<Value, String> {
        self.inline_at(condition, 0)
    }

    fn inline_at(&self, value: &Value, depth: usize) -> Result<Value, String> {
        if depth > MAX_PREDICATE_DEPTH {
            return Err("Predicates refer to each other too deeply".to_owned());
        }
        if let Some(name) = value.as_str() {
            let full = normalize(name);
            if LootConditionType::from_name(&full).is_some() {
                return Ok(serde_json::json!({ "type": full }));
            }
            let predicate = self
                .raw
                .get(&full)
                .ok_or_else(|| format!("Unknown predicate {full}"))?;
            return self.inline_at(predicate, depth + 1);
        }
        if let Value::Array(terms) = value {
            return self.inline_at(
                &serde_json::json!({ "type": "minecraft:all_of", "terms": terms }),
                depth,
            );
        }
        let mut value = value.clone();
        let Some(condition) = value.as_object_mut() else {
            return Err(format!("Not a condition: {value}"));
        };
        let kind = kind_of(condition)?;
        if LootConditionType::from_name(&kind).is_none() {
            return Err(format!("Unknown condition type {kind}"));
        }
        condition.insert("type".to_owned(), Value::String(kind));
        if let Some(term) = condition.get_mut("term") {
            *term = self.inline_at(term, depth + 1)?;
        }
        if let Some(Value::Array(terms)) = condition.get_mut("terms") {
            for term in terms {
                *term = self.inline_at(term, depth + 1)?;
            }
        }
        Ok(value)
    }
}

/// The loot table a pack file holds.
pub fn parse(bytes: &[u8], tags: &TagSet, predicates: &Predicates) -> Result<LootTable, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    Parser { tags, predicates }.table(&value)
}

fn text(text: &str) -> Frozen<str> {
    Frozen::from(text.to_owned())
}

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

type Object = Map<String, Value>;

fn object(value: &Value) -> Result<&Object, String> {
    value
        .as_object()
        .ok_or_else(|| format!("Not a JSON object: {value}"))
}

fn kind_of(object: &Object) -> Result<String, String> {
    object
        .get("type")
        .and_then(Value::as_str)
        .map(normalize)
        .ok_or_else(|| format!("No key type in {}", Value::Object(object.clone())))
}

/// The fields besides `type` as compact JSON, `None` when there are none.
fn rest(object: &Object) -> Option<Frozen<str>> {
    let mut rest = object.clone();
    rest.remove("type");
    (!rest.is_empty()).then(|| text(&Value::Object(rest).to_string()))
}

struct Parser<'a> {
    tags: &'a TagSet,
    predicates: &'a Predicates,
}

impl Parser<'_> {
    fn table(&self, value: &Value) -> Result<LootTable, String> {
        let table = object(value)?;
        let kind = match table.get("type") {
            None => None,
            Some(kind) => {
                let kind = kind.as_str().ok_or("type is not a string")?;
                LootTableKind::from_name(&normalize(kind))
            }
        };
        let random_sequence = match table.get("random_sequence") {
            None => None,
            Some(Value::String(sequence)) => Some(text(&normalize(sequence))),
            Some(_) => return Err("random_sequence is not a string".to_owned()),
        };
        let pools = match table.get("pools") {
            None => Vec::new(),
            Some(Value::Array(pools)) => pools
                .iter()
                .map(|pool| self.pool(pool))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("pools is not a list".to_owned()),
        };
        Ok(LootTable {
            kind,
            random_sequence,
            functions: self.functions(table)?,
            pools: pools.into(),
        })
    }

    fn pool(&self, value: &Value) -> Result<LootPool, String> {
        let pool = object(value)?;
        let entries = pool
            .get("entries")
            .ok_or("No key entries")?
            .as_array()
            .ok_or("entries is not a list")?
            .iter()
            .map(|entry| self.entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LootPool {
            rolls: self.number(pool.get("rolls").ok_or("No key rolls")?)?,
            bonus_rolls: match pool.get("bonus_rolls") {
                Some(value) => self.number(value)?,
                None => NumberProvider::Constant(0.0),
            },
            condition: self.condition_field(pool)?,
            functions: self.functions(pool)?,
            entries: entries.into(),
        })
    }

    fn entry(&self, value: &Value) -> Result<LootEntry, String> {
        let entry = object(value)?;
        let kind = kind_of(entry)?;
        let name = || {
            entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "No key name".to_owned())
        };
        let entry_kind = match kind.as_str() {
            "minecraft:item" => LootEntryKind::Item(item(name()?)?),
            "minecraft:tag" => {
                let tag = entry
                    .get("items")
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
                    .ok_or("No key items")?;
                let tag = normalize(tag.strip_prefix('#').unwrap_or(tag));
                let items = self
                    .tags
                    .typed::<Item>(&tag)
                    .ok_or_else(|| format!("Missing tag #{tag}"))?;
                LootEntryKind::Tag {
                    tag: text(&tag),
                    items: items.into(),
                    expand: match entry.get("expand") {
                        None => false,
                        Some(Value::Bool(expand)) => *expand,
                        Some(_) => return Err("expand is not a boolean".to_owned()),
                    },
                }
            }
            "minecraft:loot_table" => match entry.get("value").ok_or("No key value")? {
                Value::String(name) => LootEntryKind::LootTable(text(&normalize(name))),
                inline => LootEntryKind::Inline(Frozen::new(self.table(inline)?)),
            },
            "minecraft:dynamic" => LootEntryKind::Dynamic(text(&normalize(name()?))),
            "minecraft:empty" => LootEntryKind::Empty,
            "minecraft:group" => LootEntryKind::Group(self.children(entry)?),
            "minecraft:alternatives" => LootEntryKind::Alternatives(self.children(entry)?),
            "minecraft:sequence" => LootEntryKind::Sequence(self.children(entry)?),
            _ => LootEntryKind::Unknown {
                kind: text(&kind),
                raw: text(&value.to_string()),
            },
        };
        let weight = match entry.get("weight") {
            None => 1,
            Some(weight) => weight
                .as_u64()
                .filter(|weight| *weight <= i32::MAX as u64)
                .ok_or_else(|| format!("weight {weight} is not a non-negative int"))?,
        };
        let quality = match entry.get("quality") {
            None => 0,
            Some(quality) => quality
                .as_i64()
                .filter(|quality| i32::try_from(*quality).is_ok())
                .ok_or_else(|| format!("quality {quality} is not an int"))?,
        };
        Ok(LootEntry {
            kind: entry_kind,
            weight,
            quality,
            condition: self.condition_field(entry)?,
            functions: self.functions(entry)?,
        })
    }

    fn children(&self, entry: &Object) -> Result<Frozen<[LootEntry]>, String> {
        let children = entry
            .get("children")
            .ok_or("No key children")?
            .as_array()
            .ok_or("children is not a list")?
            .iter()
            .map(|child| self.entry(child))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(children.into())
    }

    /// A number provider; any shape besides a plain constant, `uniform` or
    /// `binomial` over plain numbers is kept as raw JSON.
    fn number(&self, value: &Value) -> Result<NumberProvider, String> {
        if let Some(n) = value.as_f64() {
            return Ok(NumberProvider::Constant(n));
        }
        let provider = object(value)?;
        let kind = provider
            .get("type")
            .and_then(Value::as_str)
            .map_or_else(|| "minecraft:uniform".to_owned(), normalize);
        let plain = |key: &str| provider.get(key).and_then(Value::as_f64);
        let typed = match kind.as_str() {
            "minecraft:constant" => plain("value").map(NumberProvider::Constant),
            "minecraft:uniform" => plain("min")
                .zip(plain("max"))
                .map(|(min, max)| NumberProvider::Uniform { min, max }),
            "minecraft:binomial" => plain("n")
                .zip(plain("p"))
                .map(|(n, p)| NumberProvider::Binomial { n, p }),
            _ => None,
        };
        Ok(typed.unwrap_or_else(|| {
            let mut params = provider.clone();
            params.remove("type");
            NumberProvider::Other {
                kind: text(&kind),
                params: text(&Value::Object(params).to_string()),
            }
        }))
    }

    fn condition_field(&self, object: &Object) -> Result<Option<LootCondition>, String> {
        let Some(value) = object.get("condition") else {
            return Ok(None);
        };
        let value = self.inline_predicates(value, 0)?;
        let condition = self::object(&value)?;
        Ok(Some(LootCondition {
            kind: text(&kind_of(condition)?),
            params: rest(condition),
        }))
    }

    fn inline_predicates(&self, value: &Value, depth: usize) -> Result<Value, String> {
        self.predicates.inline_at(value, depth)
    }

    /// The `modifier` functions: none, one object or a list.
    fn functions(&self, object: &Object) -> Result<Frozen<[LootFunction]>, String> {
        let function = |value: &Value| function_of(value, self.predicates);
        let functions = match object.get("modifier") {
            None => Vec::new(),
            Some(Value::Array(list)) => list.iter().map(function).collect::<Result<_, _>>()?,
            Some(one) => vec![function(one)?],
        };
        Ok(functions.into())
    }
}

const FUNCTION_TYPES: &[&str] = &[
    "minecraft:set_count",
    "minecraft:set_item",
    "minecraft:enchant_with_levels",
    "minecraft:enchant_randomly",
    "minecraft:set_enchantments",
    "minecraft:set_custom_data",
    "minecraft:set_components",
    "minecraft:furnace_smelt",
    "minecraft:enchanted_count_increase",
    "minecraft:set_damage",
    "minecraft:set_attributes",
    "minecraft:set_name",
    "minecraft:exploration_map",
    "minecraft:set_stew_effect",
    "minecraft:copy_name",
    "minecraft:set_contents",
    "minecraft:modify_contents",
    "minecraft:filtered",
    "minecraft:limit_count",
    "minecraft:apply_bonus",
    "minecraft:set_loot_table",
    "minecraft:explosion_decay",
    "minecraft:set_lore",
    "minecraft:fill_player_head",
    "minecraft:copy_custom_data",
    "minecraft:copy_state",
    "minecraft:set_banner_pattern",
    "minecraft:set_potion",
    "minecraft:set_random_dyes",
    "minecraft:set_random_potion",
    "minecraft:set_instrument",
    "minecraft:sequence",
    "minecraft:copy_components",
    "minecraft:set_fireworks",
    "minecraft:set_firework_explosion",
    "minecraft:set_book_cover",
    "minecraft:set_written_book_pages",
    "minecraft:set_writable_book_pages",
    "minecraft:toggle_tooltips",
    "minecraft:set_ominous_bottle_amplifier",
    "minecraft:set_custom_model_data",
    "minecraft:discard",
];

fn function_of(value: &Value, predicates: &Predicates) -> Result<LootFunction, String> {
    let mut function = object(value)?.clone();
    let kind = kind_of(&function)?;
    if !FUNCTION_TYPES.contains(&kind.as_str()) {
        return Err(format!("Unknown function type {kind}"));
    }
    if let Some(condition) = function.get("condition") {
        let inlined = predicates.inline_at(condition, 0)?;
        function.insert("condition".to_owned(), inlined);
    }
    Ok(LootFunction {
        kind: text(&kind_of(&function)?),
        params: rest(&function),
    })
}

fn flatten_functions(
    value: &Value,
    predicates: &Predicates,
    depth: usize,
    out: &mut Vec<LootFunction>,
) -> Result<(), String> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err("Item modifiers nest too deeply".to_owned());
    }
    match value {
        Value::Array(list) => list
            .iter()
            .try_for_each(|one| flatten_functions(one, predicates, depth + 1, out)),
        Value::Object(map)
            if map
                .get("type")
                .and_then(Value::as_str)
                .map(normalize)
                .as_deref()
                == Some("minecraft:sequence") =>
        {
            match map.get("functions") {
                Some(list) => flatten_functions(list, predicates, depth + 1, out),
                None => Ok(()),
            }
        }
        one => {
            out.push(function_of(one, predicates)?);
            Ok(())
        }
    }
}

/// An item modifier (one function, a list or a `sequence`) as its
/// functions in order, with stored predicates inlined.
pub fn parse_item_modifier(
    value: &Value,
    predicates: &Predicates,
) -> Result<Vec<LootFunction>, String> {
    let mut functions = Vec::new();
    flatten_functions(value, predicates, 0, &mut functions)?;
    Ok(functions)
}

/// Stored item modifiers (`item_modifier/<name>.json`) by id.
#[derive(Debug, Default)]
pub struct ItemModifiers {
    resolved: BTreeMap<String, Arc<Vec<LootFunction>>>,
}

impl ItemModifiers {
    /// Reads pack item modifier files; one that is not JSON or does not
    /// parse is logged and left out.
    pub fn new(files: BTreeMap<String, (String, Vec<u8>)>, predicates: &Predicates) -> Self {
        let resolved = files
            .into_iter()
            .filter_map(|(id, (source, bytes))| {
                let parsed = serde_json::from_slice::<Value>(&bytes)
                    .map_err(|err| err.to_string())
                    .and_then(|value| parse_item_modifier(&value, predicates));
                match parsed {
                    Ok(functions) => Some((id, Arc::new(functions))),
                    Err(err) => {
                        log::warn!("Couldn't parse data file {id} from {source}: {err}");
                        None
                    }
                }
            })
            .collect();
        Self { resolved }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Vec<LootFunction>>> {
        self.resolved.get(id).cloned()
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.resolved.keys().map(String::as_str)
    }
}

fn item(name: &str) -> Result<Item, String> {
    Item::from_name(&normalize(name)).ok_or_else(|| format!("Unknown item {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(json: &str, predicates: &Predicates) -> Result<LootTable, String> {
        let value: Value = serde_json::from_str(json).unwrap();
        Parser {
            tags: &TagSet::vanilla(),
            predicates,
        }
        .table(&value)
    }

    #[tuff_core::watchdog]
    #[test]
    fn item_modifiers_read_one_function_a_list_or_a_sequence() {
        let predicates = Predicates::vanilla();
        let one = serde_json::json!({"type": "set_count", "count": 3});
        let parsed = parse_item_modifier(&one, &predicates).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(&*parsed[0].kind, "minecraft:set_count");
        let nested = serde_json::json!([
            {"type": "set_count", "count": 3},
            {"type": "minecraft:sequence", "functions": [{"type": "limit_count", "limit": 2}]},
        ]);
        let parsed = parse_item_modifier(&nested, &predicates).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(&*parsed[1].kind, "minecraft:limit_count");
    }

    #[tuff_core::watchdog]
    #[test]
    fn item_modifiers_reject_unknown_types_and_bad_files() {
        let predicates = Predicates::vanilla();
        assert!(parse_item_modifier(&serde_json::json!({"type": "nope"}), &predicates).is_err());
        assert!(parse_item_modifier(&serde_json::json!(5), &predicates).is_err());
        let files = BTreeMap::from([
            (
                "test:good".to_owned(),
                (
                    "pack".to_owned(),
                    br#"{"type":"set_count","count":1}"#.to_vec(),
                ),
            ),
            ("test:bad".to_owned(), ("pack".to_owned(), b"{".to_vec())),
        ]);
        let stored = ItemModifiers::new(files, &predicates);
        assert!(stored.get("test:good").is_some());
        assert!(stored.get("test:bad").is_none());
        assert_eq!(stored.ids().collect::<Vec<_>>(), ["test:good"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_table_reads_pools_entries_conditions_and_functions() {
        let table = table(
            r#"{"type":"block","pools":[{"rolls":{"type":"uniform","min":1,"max":3},
                "condition":"minecraft:tool/can_shear",
                "entries":[{"type":"item","name":"diamond","weight":5,
                    "modifier":{"type":"set_count","count":2}},
                  {"type":"tag","name":"minecraft:logs","expand":true},
                  {"type":"alternatives","children":[{"type":"empty"}]}]}]}"#,
            &Predicates::default(),
        )
        .unwrap();
        assert!(matches!(table.kind, Some(LootTableKind::Block)));
        let pool = &table.pools[0];
        assert_eq!(pool.rolls, NumberProvider::Uniform { min: 1.0, max: 3.0 });
        let condition = pool.condition.as_ref().unwrap();
        assert_eq!(condition.kind, "minecraft:match_tool");
        let entries = &pool.entries;
        assert!(matches!(
            entries[0].kind,
            LootEntryKind::Item(Item::Diamond)
        ));
        assert_eq!(entries[0].weight, 5);
        assert_eq!(entries[0].functions[0].kind, "minecraft:set_count");
        assert!(matches!(
            &entries[1].kind,
            LootEntryKind::Tag { items, expand: true, .. } if items.contains(&Item::OakLog)
        ));
        assert!(
            matches!(&entries[2].kind, LootEntryKind::Alternatives(children) if children.len() == 1)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn pack_predicates_are_inlined_and_cycles_fail() {
        let file = |json: &str| (String::from("file/p"), json.as_bytes().to_vec());
        let predicates = Predicates::new(BTreeMap::from([
            (
                "a:lucky".to_owned(),
                file(r#"{"condition":"random_chance","chance":0.5}"#),
            ),
            ("a:loop".to_owned(), file(r#""a:loop""#)),
        ]));
        let lucky = table(
            r#"{"pools":[{"rolls":1,"condition":["a:lucky"],"entries":[{"type":"empty"}]}]}"#,
            &predicates,
        );
        assert!(
            lucky.is_err(),
            "a predicate needs a type, not a condition key"
        );
        let predicates = Predicates::new(BTreeMap::from([
            (
                "a:lucky".to_owned(),
                file(r#"{"type":"random_chance","chance":0.5}"#),
            ),
            ("a:loop".to_owned(), file(r#""a:loop""#)),
        ]));
        let lucky = table(
            r#"{"pools":[{"rolls":1,"condition":["a:lucky"],"entries":[{"type":"empty"}]}]}"#,
            &predicates,
        )
        .unwrap();
        let condition = lucky.pools[0].condition.as_ref().unwrap();
        assert_eq!(condition.kind, "minecraft:all_of");
        assert!(
            condition
                .params
                .as_ref()
                .unwrap()
                .contains("minecraft:random_chance")
        );
        let looping = table(
            r#"{"pools":[{"rolls":1,"condition":"a:loop","entries":[]}]}"#,
            &predicates,
        );
        assert!(looping.unwrap_err().contains("too deeply"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn stored_predicates_resolve_once_and_bad_ones_are_left_out() {
        let file = |json: &str| (String::from("file/p"), json.as_bytes().to_vec());
        let predicates = Predicates::new(BTreeMap::from([
            (
                "a:lucky".to_owned(),
                file(r#"{"type":"random_chance","chance":0.5}"#),
            ),
            (
                "a:both".to_owned(),
                file(r#"["a:lucky","minecraft:tool/can_shear"]"#),
            ),
            ("a:loop".to_owned(), file(r#""a:loop""#)),
            (
                "a:dangling".to_owned(),
                file(r#"{"type":"inverted","term":"a:nope"}"#),
            ),
            ("a:unknown".to_owned(), file(r#"{"type":"minecraft:nope"}"#)),
            ("a:text".to_owned(), file("{not json")),
        ]));
        let both = predicates.get("a:both").unwrap();
        assert_eq!(both["type"], "minecraft:all_of");
        assert_eq!(both["terms"][0]["type"], "minecraft:random_chance");
        assert_eq!(both["terms"][1]["type"], "minecraft:match_tool");
        assert!(Arc::ptr_eq(&both, &predicates.get("a:both").unwrap()));
        for bad in ["a:loop", "a:dangling", "a:unknown", "a:text"] {
            assert!(!predicates.contains(bad), "{bad}");
        }
        assert!(predicates.contains("minecraft:block/fast_cooking"));
        let inline = predicates
            .inline(&serde_json::json!({"type": "inverted", "term": "a:lucky"}))
            .unwrap();
        assert_eq!(inline["term"]["type"], "minecraft:random_chance");
        assert!(
            predicates
                .inline(&serde_json::json!({"type": "inverted", "term": "a:loop"}))
                .is_err()
        );
        assert!(Predicates::vanilla().contains("minecraft:tool/can_silk_touch"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_tables_are_rejected() {
        let bad = [
            r#"[]"#,
            r#"{"pools":{}}"#,
            r#"{"pools":[{"entries":[]}]}"#,
            r#"{"pools":[{"rolls":1}]}"#,
            r#"{"pools":[{"rolls":1,"entries":[{"name":"stone"}]}]}"#,
            r#"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"nope"}]}]}"#,
            r#"{"pools":[{"rolls":1,"entries":[{"type":"tag","name":"minecraft:nope"}]}]}"#,
            r#"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"stone","weight":-1}]}]}"#,
            r#"{"pools":[{"rolls":1,"condition":"minecraft:nope","entries":[]}]}"#,
            r#"{"pools":[{"rolls":1,"entries":[{"type":"group"}]}]}"#,
        ];
        for json in bad {
            assert!(table(json, &Predicates::default()).is_err(), "{json}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn overrides_replace_and_remove_built_in_tables() {
        let stone = "minecraft:blocks/stone";
        let dirt = "minecraft:blocks/dirt";
        let parsed = parse(
            br#"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"diamond"}]}]}"#,
            &TagSet::vanilla(),
            &Predicates::default(),
        )
        .unwrap();
        let pools = parsed.pools.clone();
        let tables = LootTables::new(HashMap::from([
            (stone.to_owned(), Some(parsed)),
            (dirt.to_owned(), None),
        ]));
        assert!(Frozen::ptr_eq(&tables.get(stone).unwrap().pools, &pools));
        assert!(tables.get(dirt).is_none());
        assert!(std::ptr::eq(
            tables.get("minecraft:blocks/oak_log").unwrap(),
            LootTable::get("minecraft:blocks/oak_log").unwrap()
        ));
        assert!(LootTables::vanilla().is_vanilla());
    }
}

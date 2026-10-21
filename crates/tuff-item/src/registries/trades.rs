//! The `villager_trade` and `trade_set` registries: built-in entries come
//! from the generated tables, pack entries are decoded into the same shape.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Map, Value};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::VillagerType;
use tuff_vanilla_data::villager as generated;

use super::{DecodeCtx, RegistryCodec, RuntimeRegistry};

/// Item modifier function kinds a trade can apply to its result.
const MODIFIER_KINDS: &[&str] = &[
    "minecraft:enchant_randomly",
    "minecraft:enchant_with_levels",
    "minecraft:exploration_map",
    "minecraft:filtered",
    "minecraft:set_enchantments",
    "minecraft:set_potion",
    "minecraft:set_random_dyes",
    "minecraft:set_random_potion",
    "minecraft:set_stew_effect",
];

const TRADE_FIELDS: &[&str] = &[
    "wants",
    "additional_wants",
    "gives",
    "max_uses",
    "xp",
    "reputation_discount",
    "merchant_predicate",
    "given_item_modifier",
    "double_trade_price_enchantments",
];

/// What a trade asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    pub item: Item,
    pub count: i64,
    /// An int provider rolled for the count instead.
    pub count_provider: Option<Value>,
    pub potion: Option<String>,
}

/// Registry entries named by a tag or listed by id.
#[derive(Clone, Debug, PartialEq)]
pub enum IdSet {
    /// A tag name without the `#`.
    Tag(String),
    Ids(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trade {
    pub name: String,
    pub wants: Cost,
    pub additional_wants: Option<Cost>,
    pub gives: Item,
    pub gives_count: i64,
    pub max_uses: i64,
    pub xp: i64,
    pub reputation_discount: f32,
    /// The villager types that offer it; `None` for every type.
    pub merchant_types: Option<Vec<VillagerType>>,
    /// Loot item functions applied to the result (one or a list).
    pub modifier: Option<Value>,
    /// Stored enchantments that double the extra cost the modifier adds.
    pub double_price_enchantments: Option<IdSet>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradeSet {
    pub name: String,
    /// The trades it draws from, in draw order once resolved.
    pub trades: IdSet,
    pub amount: i64,
    pub allow_duplicates: bool,
    pub random_sequence: Option<String>,
}

/// Both trade registries, frozen.
pub struct Trades {
    pub trades: RuntimeRegistry<Trade>,
    pub sets: RuntimeRegistry<TradeSet>,
}

impl Trades {
    /// The trades of `set` in draw order: a tag as the tags in effect have
    /// it, else its list. Feature pack trades outside the registry come
    /// from the generated table; unknown ids are skipped.
    pub fn of_set(&self, set: &TradeSet) -> Vec<&Trade> {
        let resolve = |names: &[String]| -> Vec<&Trade> {
            names
                .iter()
                .filter_map(|name| self.trades.get(name).or_else(|| Trade::builtin(name)))
                .collect()
        };
        match &set.trades {
            IdSet::Ids(ids) => resolve(ids),
            IdSet::Tag(tag) => tuff_datapack::tags::with(|tags| {
                tags.get("minecraft:villager_trade", tag)
                    .map(resolve)
                    .unwrap_or_default()
            }),
        }
    }
}

pub struct TradeCodec;

impl Trade {
    pub fn builtin(name: &str) -> Option<&'static Trade> {
        TradeCodec::builtin(name)
    }

    /// Every built-in trade.
    pub fn builtins() -> impl Iterator<Item = &'static Trade> {
        generated::TRADES
            .iter()
            .filter_map(|(name, _)| TradeCodec::builtin(name))
    }
}

impl TradeSet {
    pub fn builtin(name: &str) -> Option<&'static TradeSet> {
        TradeSetCodec::builtin(name)
    }
}

fn convert_cost(cost: &generated::TradeCost) -> Cost {
    Cost {
        item: cost.item,
        count: cost.count,
        count_provider: cost
            .count_provider
            .map(|raw| serde_json::from_str(raw).expect("generated provider is JSON")),
        potion: cost.potion.map(str::to_owned),
    }
}

fn convert_trade(trade: &generated::Trade) -> Trade {
    Trade {
        name: trade.name.to_owned(),
        wants: convert_cost(&trade.wants),
        additional_wants: trade.additional_wants.as_ref().map(convert_cost),
        gives: trade.gives,
        gives_count: trade.gives_count,
        max_uses: trade.max_uses,
        xp: trade.xp,
        reputation_discount: trade.reputation_discount,
        merchant_types: trade.merchant_types.map(<[_]>::to_vec),
        modifier: trade
            .modifier
            .map(|raw| serde_json::from_str(raw).expect("generated modifier is JSON")),
        double_price_enchantments: trade.double_price_tag.map(|tag| IdSet::Tag(tag.to_owned())),
    }
}

impl RegistryCodec for TradeCodec {
    const KEY: &'static str = "minecraft:villager_trade";
    type Value = Trade;

    fn builtin(id: &str) -> Option<&'static Trade> {
        static BUILTIN: OnceLock<HashMap<&'static str, Trade>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                generated::TRADES
                    .iter()
                    .map(|(name, trade)| (*name, convert_trade(trade)))
                    .collect()
            })
            .get(id)
    }

    fn decode(json: &Value, ctx: &DecodeCtx<'_>) -> Result<Trade, String> {
        let object = json.as_object().ok_or("not an object")?;
        known_fields(object, TRADE_FIELDS, "")?;
        let gives = object
            .get("gives")
            .and_then(Value::as_object)
            .ok_or("gives: expected an object")?;
        known_fields(gives, &["id", "count"], "gives.")?;
        Ok(Trade {
            name: ctx.id.to_owned(),
            wants: cost(object.get("wants").ok_or("wants: missing")?, "wants")?,
            additional_wants: object
                .get("additional_wants")
                .map(|v| cost(v, "additional_wants"))
                .transpose()?,
            gives: item(gives.get("id"), "gives.id")?,
            gives_count: int_or(gives.get("count"), 1, "gives.count")?,
            max_uses: int_or(object.get("max_uses"), 4, "max_uses")?,
            xp: int_or(object.get("xp"), 1, "xp")?,
            reputation_discount: match object.get("reputation_discount") {
                None => 0.0,
                Some(v) => v.as_f64().ok_or("reputation_discount: expected a number")? as f32,
            },
            merchant_types: object
                .get("merchant_predicate")
                .map(merchant_types)
                .transpose()?,
            modifier: object
                .get("given_item_modifier")
                .map(|v| check_modifier(v).map(|()| v.clone()))
                .transpose()?,
            double_price_enchantments: object
                .get("double_trade_price_enchantments")
                .map(|v| id_set(v, "double_trade_price_enchantments"))
                .transpose()?,
        })
    }
}

pub struct TradeSetCodec;

impl RegistryCodec for TradeSetCodec {
    const KEY: &'static str = "minecraft:trade_set";
    type Value = TradeSet;

    fn builtin(id: &str) -> Option<&'static TradeSet> {
        static BUILTIN: OnceLock<HashMap<&'static str, TradeSet>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                generated::TRADE_SETS
                    .iter()
                    .map(|(name, set)| {
                        let trades = match set.tag {
                            Some(tag) => IdSet::Tag(tag.to_owned()),
                            None => {
                                IdSet::Ids(set.trades.iter().map(|t| t.name.to_owned()).collect())
                            }
                        };
                        let value = TradeSet {
                            name: set.name.to_owned(),
                            trades,
                            amount: set.amount,
                            allow_duplicates: set.allow_duplicates,
                            random_sequence: set.random_sequence.map(str::to_owned),
                        };
                        (*name, value)
                    })
                    .collect()
            })
            .get(id)
    }

    fn decode(json: &Value, ctx: &DecodeCtx<'_>) -> Result<TradeSet, String> {
        let object = json.as_object().ok_or("not an object")?;
        known_fields(
            object,
            &["trades", "amount", "allow_duplicates", "random_sequence"],
            "",
        )?;
        let trades = id_set(object.get("trades").ok_or("trades: missing")?, "trades")?;
        if let IdSet::Ids(ids) = &trades
            && let Some(registry) = ctx.registries.get(TradeCodec::KEY)
            && let Some(missing) = ids.iter().find(|id| !registry.contains(id))
        {
            return Err(format!("trades: unknown villager trade {missing}"));
        }
        Ok(TradeSet {
            name: ctx.id.to_owned(),
            trades,
            amount: int(object.get("amount").ok_or("amount: missing")?, "amount")?,
            allow_duplicates: match object.get("allow_duplicates") {
                None => false,
                Some(v) => v.as_bool().ok_or("allow_duplicates: expected a boolean")?,
            },
            random_sequence: object
                .get("random_sequence")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or("random_sequence: expected a string")
                })
                .transpose()?,
        })
    }
}

fn known_fields(object: &Map<String, Value>, known: &[&str], at: &str) -> Result<(), String> {
    match object.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(format!("{at}{key}: unsupported field")),
        None => Ok(()),
    }
}

fn int(value: &Value, what: &str) -> Result<i64, String> {
    value
        .as_i64()
        .ok_or_else(|| format!("{what}: expected an integer"))
}

fn int_or(value: Option<&Value>, default: i64, what: &str) -> Result<i64, String> {
    value.map_or(Ok(default), |v| int(v, what))
}

fn item(value: Option<&Value>, what: &str) -> Result<Item, String> {
    let name = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what}: expected an item id"))?;
    Item::from_name(name)
        .or_else(|| Item::from_name(&format!("minecraft:{name}")))
        .ok_or_else(|| format!("{what}: unknown item {name}"))
}

fn id_set(value: &Value, what: &str) -> Result<IdSet, String> {
    match value {
        Value::String(s) => Ok(match s.strip_prefix('#') {
            Some(tag) => IdSet::Tag(tag.to_owned()),
            None => IdSet::Ids(vec![s.clone()]),
        }),
        Value::Array(list) => list
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{what}: expected ids"))
            })
            .collect::<Result<_, _>>()
            .map(IdSet::Ids),
        _ => Err(format!("{what}: expected a tag or a list of ids")),
    }
}

fn cost(value: &Value, what: &str) -> Result<Cost, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what}: expected an object"))?;
    known_fields(object, &["id", "count", "components"], &format!("{what}."))?;
    let (count, count_provider) = match object.get("count") {
        None => (1, None),
        Some(provider @ Value::Object(_)) => (1, Some(provider.clone())),
        Some(count) => (int(count, what)?, None),
    };
    let potion = match object.get("components") {
        None => None,
        Some(components) => {
            let components = components
                .as_object()
                .filter(|c| c.len() == 1)
                .and_then(|c| c.get("minecraft:potion_contents"))
                .and_then(Value::as_object)
                .filter(|c| c.len() == 1)
                .ok_or_else(|| format!("{what}: only a potion_contents potion is supported"))?;
            let potion = components
                .get("potion")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{what}: potion: expected an id"))?;
            Some(potion.to_owned())
        }
    };
    Ok(Cost {
        item: item(object.get("id"), what)?,
        count,
        count_provider,
        potion,
    })
}

fn check_modifier(value: &Value) -> Result<(), String> {
    let functions: Vec<&Value> = match value {
        Value::Array(list) => list.iter().collect(),
        other => vec![other],
    };
    for function in functions {
        let kind = function["type"]
            .as_str()
            .ok_or("given_item_modifier: function without a type")?;
        if !MODIFIER_KINDS.contains(&kind) {
            return Err(format!("given_item_modifier: unsupported function {kind}"));
        }
        if function.get("conditions").is_some() {
            return Err("given_item_modifier: conditions are not supported".to_owned());
        }
        if kind == "minecraft:filtered"
            && (function["on_fail"]["type"].as_str() != Some("minecraft:discard")
                || function.get("on_pass").is_some())
        {
            return Err(
                "given_item_modifier: only filters that discard on failure are supported"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn merchant_types(value: &Value) -> Result<Vec<VillagerType>, String> {
    const WHAT: &str = "merchant_predicate: only villager variants of this are supported";
    if value["type"] != "minecraft:entity_properties" || value["entity"] != "this" {
        return Err(WHAT.to_owned());
    }
    let components = value["predicate"]
        .as_object()
        .filter(|p| p.len() == 1)
        .and_then(|p| p.get("minecraft:predicates"))
        .and_then(Value::as_object)
        .filter(|c| c.len() == 1)
        .and_then(|c| c.get("minecraft:villager/variant"))
        .ok_or(WHAT)?;
    let variants: Vec<&Value> = match components {
        Value::Array(list) => list.iter().collect(),
        single => vec![single],
    };
    variants
        .into_iter()
        .map(|v| {
            let name = v.as_str().ok_or(WHAT)?;
            VillagerType::from_name(name)
                .or_else(|| VillagerType::from_name(&format!("minecraft:{name}")))
                .ok_or_else(|| format!("merchant_predicate: unknown villager type {name}"))
        })
        .collect()
}

#[cfg(test)]
mod tests;

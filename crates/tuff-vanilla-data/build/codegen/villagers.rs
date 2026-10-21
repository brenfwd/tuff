//! Villager data: professions, points of interest, types by biome, levels
//! and gossip from the villager dumper, and trades from the data pack
//! (`villager_trade/` entries, `trade_set/` entries and the trade tags).
//! Trades are typed; a trade's item modifier stays raw JSON (loot item
//! functions), but its function kinds are checked against the ones the
//! server evaluates, so a new kind fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::{json, registries, tags};
use crate::input::Input;
use crate::names::str_literal;

/// Item modifier function kinds the server knows how to apply to a trade.
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

struct Lookups<'a> {
    items: BTreeMap<&'a str, String>,
    blocks: BTreeMap<&'a str, String>,
    sounds: BTreeMap<&'a str, String>,
    pois: BTreeMap<&'a str, String>,
    professions: BTreeMap<&'a str, String>,
    types: BTreeMap<&'a str, String>,
}

impl Lookups<'_> {
    fn get(table: &BTreeMap<&str, String>, name: &str, what: &str, ty: &str) -> Result<String> {
        table
            .get(name)
            .map(|v| format!("{ty}::{v}"))
            .with_context(|| format!("unknown {what} {name}"))
    }

    fn item(&self, name: &str) -> Result<String> {
        Self::get(&self.items, name, "item", "Item")
    }

    fn poi(&self, name: &str) -> Result<String> {
        Self::get(
            &self.pois,
            name,
            "point of interest type",
            "PointOfInterestType",
        )
    }
}

fn int(value: &Value, what: &str) -> Result<i64> {
    value
        .as_i64()
        .with_context(|| format!("{what}: only constant integers are supported, got {value}"))
}

fn trade_cost(value: &Value, lookups: &Lookups, what: &str) -> Result<String> {
    let object = value
        .as_object()
        .with_context(|| format!("{what}: not an object"))?;
    for key in object.keys() {
        ensure!(
            ["id", "count", "components"].contains(&key.as_str()),
            "{what}: unknown field {key}"
        );
    }
    let item = lookups.item(object["id"].as_str().context("id")?)?;
    let (count, provider) = match object.get("count") {
        Some(count) if count.is_object() => (
            1,
            format!("Some({})", str_literal(&serde_json::to_string(count)?)),
        ),
        Some(count) => (int(count, what)?, "None".to_owned()),
        None => (1, "None".to_owned()),
    };
    let potion = match object.get("components") {
        None => "None".to_owned(),
        Some(components) => {
            let components = components.as_object().context("components")?;
            ensure!(
                components.len() == 1,
                "{what}: only a potion_contents component is supported"
            );
            let contents = components
                .get("minecraft:potion_contents")
                .and_then(Value::as_object)
                .with_context(|| format!("{what}: only potion_contents is supported"))?;
            ensure!(
                contents.len() == 1,
                "{what}: only a potion is supported in potion_contents"
            );
            let potion = contents["potion"].as_str().context("potion")?;
            format!("Some({})", str_literal(potion))
        }
    };
    Ok(format!(
        "TradeCost {{ item: {item}, count: {count}, count_provider: {provider}, potion: {potion} }}"
    ))
}

fn check_modifier(value: &Value, what: &str) -> Result<()> {
    let functions: Vec<&Value> = match value {
        Value::Array(list) => list.iter().collect(),
        other => vec![other],
    };
    for function in functions {
        let kind = function["type"]
            .as_str()
            .with_context(|| format!("{what}: function without a type"))?;
        ensure!(
            MODIFIER_KINDS.contains(&kind),
            "{what}: unsupported item modifier {kind}"
        );
        ensure!(
            function.get("conditions").is_none(),
            "{what}: conditional item modifiers are not supported"
        );
        if kind == "minecraft:filtered" {
            let on_fail = function["on_fail"]["type"].as_str();
            ensure!(
                on_fail == Some("minecraft:discard") && function.get("on_pass").is_none(),
                "{what}: only filters that discard on failure are supported"
            );
        }
    }
    Ok(())
}

fn merchant_types(value: &Value, lookups: &Lookups, what: &str) -> Result<String> {
    ensure!(
        value["type"] == "minecraft:entity_properties" && value["entity"] == "this",
        "{what}: only entity_properties of this are supported"
    );
    let predicate = value["predicate"].as_object().context("predicate")?;
    ensure!(predicate.len() == 1, "{what}: unsupported predicate fields");
    let components = predicate["minecraft:predicates"]
        .as_object()
        .with_context(|| format!("{what}: only component predicates are supported"))?;
    ensure!(
        components.len() == 1,
        "{what}: unsupported component predicates"
    );
    let variants = match &components["minecraft:villager/variant"] {
        Value::Array(list) => list.iter().collect::<Vec<_>>(),
        single @ Value::String(_) => vec![single],
        _ => bail!("{what}: only villager variants are supported"),
    };
    let types = variants
        .into_iter()
        .map(|v| {
            let name = v.as_str().context("variant")?;
            Lookups::get(&lookups.types, name, "villager type", "VillagerType")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("Some(&[{}])", types.join(", ")))
}

fn trade(name: &str, json: &Value, lookups: &Lookups, input: &Input) -> Result<String> {
    let what = format!("villager trade {name}");
    let object = json.as_object().context("not an object")?;
    for key in object.keys() {
        ensure!(
            TRADE_FIELDS.contains(&key.as_str()),
            "{what}: unknown field {key}"
        );
    }
    let wants = trade_cost(&object["wants"], lookups, &what)?;
    let additional = match object.get("additional_wants") {
        Some(value) => format!("Some({})", trade_cost(value, lookups, &what)?),
        None => "None".to_owned(),
    };
    let gives = object["gives"].as_object().context("gives")?;
    for key in gives.keys() {
        ensure!(
            ["id", "count"].contains(&key.as_str()),
            "{what}: gives has unsupported field {key}"
        );
    }
    let gives_item = lookups.item(gives["id"].as_str().context("gives id")?)?;
    let gives_count = match gives.get("count") {
        Some(count) => int(count, &what)?,
        None => 1,
    };
    let max_uses = object.get("max_uses").map_or(Ok(4), |v| int(v, &what))?;
    let xp = object.get("xp").map_or(Ok(1), |v| int(v, &what))?;
    let discount = match object.get("reputation_discount") {
        Some(value) => value
            .as_f64()
            .with_context(|| format!("{what}: only constant discounts are supported"))?,
        None => 0.0,
    };
    let merchant = match object.get("merchant_predicate") {
        Some(value) => merchant_types(value, lookups, &what)?,
        None => "None".to_owned(),
    };
    let modifier = match object.get("given_item_modifier") {
        Some(value) => {
            check_modifier(value, &what)?;
            json::opt_literal(Some(value))
        }
        None => "None".to_owned(),
    };
    let double_tag = match object
        .get("double_trade_price_enchantments")
        .and_then(Value::as_str)
        .and_then(|t| t.strip_prefix('#'))
    {
        Some(tag) => format!("Some({})", str_literal(tag)),
        None => "None".to_owned(),
    };
    let double = match object.get("double_trade_price_enchantments") {
        Some(value) => {
            let tag = value
                .as_str()
                .and_then(|t| t.strip_prefix('#'))
                .with_context(|| format!("{what}: expected an enchantment tag"))?;
            let tags = tags::resolved_registry_tags(input, "minecraft:enchantment")?;
            let entries = tags
                .get(tag)
                .with_context(|| format!("{what}: unknown enchantment tag {tag}"))?;
            let names: Vec<String> = entries.iter().map(|e| str_literal(e)).collect();
            format!("Some(&[{}])", names.join(", "))
        }
        None => "None".to_owned(),
    };
    Ok(format!(
        "Trade {{ name: {}, wants: {wants}, additional_wants: {additional}, gives: {gives_item}, gives_count: {gives_count}, max_uses: {max_uses}, xp: {xp}, reputation_discount: {discount:?}_f32, merchant_types: {merchant}, modifier: {modifier}, double_price_enchantments: {double}, double_price_tag: {double_tag} }}",
        str_literal(name)
    ))
}

pub fn generate(input: &Input) -> Result<String> {
    let reg = |key: &str| {
        input
            .registries
            .get(key)
            .with_context(|| format!("registries.json has no {key}"))
    };
    let lookups = Lookups {
        items: registries::variants(reg("minecraft:item")?)?,
        blocks: registries::variants(reg("minecraft:block")?)?,
        sounds: registries::variants(reg("minecraft:sound_event")?)?,
        pois: registries::variants(reg("minecraft:point_of_interest_type")?)?,
        professions: registries::variants(reg("minecraft:villager_profession")?)?,
        types: registries::variants(reg("minecraft:villager_type")?)?,
    };

    let mut out = String::new();

    let mut trades: BTreeMap<String, &Value> = BTreeMap::new();
    let mut sets: BTreeMap<String, &Value> = BTreeMap::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(rest) = rest.strip_suffix(".json") else {
            continue;
        };
        if let Some(name) = rest.strip_prefix("villager_trade/") {
            trades.insert(format!("{namespace}:{name}"), json);
        } else if let Some(name) = rest.strip_prefix("trade_set/") {
            sets.insert(format!("{namespace}:{name}"), json);
        }
    }
    for (path, json) in &input.feature_pack_json {
        let Some((_pack, rest)) = path
            .strip_prefix(crate::input::FEATURE_PACKS)
            .and_then(|rest| rest.split_once("/data/"))
        else {
            continue;
        };
        let Some((namespace, rest)) = rest.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_suffix(".json")
            .and_then(|rest| rest.strip_prefix("villager_trade/"))
        else {
            continue;
        };
        let name = format!("{namespace}:{name}");
        match trades.get(&name) {
            Some(base) => ensure!(
                *base == json,
                "feature pack trade {name} differs from the base game's"
            ),
            None => {
                trades.insert(name, json);
            }
        }
    }
    ensure!(!trades.is_empty(), "no villager trades in the data pack");
    ensure!(!sets.is_empty(), "no trade sets in the data pack");

    let mut trade_consts = BTreeMap::new();
    for (index, (name, json)) in trades.iter().enumerate() {
        let ident = format!("TRADE_{index}");
        writeln!(
            out,
            "static {ident}: Trade = {};",
            trade(name, json, &lookups, input).with_context(|| format!("villager trade {name}"))?
        )?;
        trade_consts.insert(name.as_str(), ident);
    }
    writeln!(out, "/// Every villager trade, by name.")?;
    writeln!(out, "pub static TRADES: &[(&str, &Trade)] = &[")?;
    for (name, ident) in &trade_consts {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;

    let trade_tags = tags::resolved_registry_tags(input, "minecraft:villager_trade")?;
    let mut set_consts = BTreeMap::new();
    for (index, (name, json)) in sets.iter().enumerate() {
        let what = format!("trade set {name}");
        let object = json.as_object().context("not an object")?;
        for key in object.keys() {
            ensure!(
                ["trades", "amount", "allow_duplicates", "random_sequence"].contains(&key.as_str()),
                "{what}: unknown field {key}"
            );
        }
        let tag = match object["trades"].as_str().and_then(|s| s.strip_prefix('#')) {
            Some(tag) => format!("Some({})", str_literal(tag)),
            None => "None".to_owned(),
        };
        let names: Vec<String> = match &object["trades"] {
            Value::String(s) => match s.strip_prefix('#') {
                Some(tag) => trade_tags
                    .get(tag)
                    .with_context(|| format!("{what}: unknown tag {tag}"))?
                    .clone(),
                None => vec![s.clone()],
            },
            Value::Array(list) => list
                .iter()
                .map(|v| v.as_str().map(str::to_owned).context("trade name"))
                .collect::<Result<_>>()?,
            other => bail!("{what}: unexpected trades {other}"),
        };
        let refs = names
            .iter()
            .map(|n| {
                trade_consts
                    .get(n.as_str())
                    .map(|ident| format!("&{ident}"))
                    .with_context(|| format!("{what}: unknown trade {n}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let amount = int(&object["amount"], &what)?;
        let duplicates = object
            .get("allow_duplicates")
            .map_or(Some(false), Value::as_bool)
            .context("allow_duplicates")?;
        let sequence = match object.get("random_sequence") {
            Some(value) => format!(
                "Some({})",
                str_literal(value.as_str().context("random_sequence")?)
            ),
            None => "None".to_owned(),
        };
        let ident = format!("TRADE_SET_{index}");
        writeln!(
            out,
            "static {ident}: TradeSet = TradeSet {{ name: {}, tag: {tag}, trades: &[{}], amount: {amount}, allow_duplicates: {duplicates}, random_sequence: {sequence} }};",
            str_literal(name),
            refs.join(", ")
        )?;
        set_consts.insert(name.as_str(), ident);
    }
    writeln!(out, "/// Every trade set, by name.")?;
    writeln!(out, "pub static TRADE_SETS: &[(&str, &TradeSet)] = &[")?;
    for (name, ident) in &set_consts {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    for (table, entries) in [("TRADE_JSON", &trades), ("TRADE_SET_JSON", &sets)] {
        writeln!(out, "/// The data pack file of each entry, minified.")?;
        writeln!(out, "pub static {table}: &[(&str, &str)] = &[")?;
        for (name, json) in entries.iter() {
            writeln!(
                out,
                "    ({}, {}),",
                str_literal(name),
                str_literal(&serde_json::to_string(json)?)
            )?;
        }
        writeln!(out, "];")?;
    }

    let mut pois = Vec::new();
    let mut state_pois: Vec<(u32, String)> = Vec::new();
    let mut professions = BTreeMap::new();
    let mut requested: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut secondary: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut biome_types = Vec::new();
    let mut default_type = None;
    let mut thresholds = Vec::new();
    let mut gossip = Vec::new();
    let mut hostiles = Vec::new();
    let entity_types = registries::variants(reg("minecraft:entity_type")?)?;
    for line in input.villager_properties.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let what = || format!("villager dumper line {line:?}");
        match fields.as_slice() {
            ["poi", name, tickets, range, states @ ..] => {
                let variant = lookups.poi(name)?;
                pois.push(format!(
                    "({variant}, PoiInfo {{ max_tickets: {tickets}, valid_range: {range} }})"
                ));
                for state in states {
                    state_pois.push((state.parse().with_context(what)?, variant.clone()));
                }
            }
            ["profession", name, held, acquirable, sound, sets @ ..] => {
                ensure!(sets.len() == 5, "{}: expected five trade sets", what());
                let held = match *held {
                    "-" => "None".to_owned(),
                    poi => format!("Some({})", lookups.poi(poi)?),
                };
                let acquirable = match *acquirable {
                    "-" => Vec::new(),
                    list => list
                        .split(',')
                        .map(|p| lookups.poi(p))
                        .collect::<Result<_>>()?,
                };
                let sound = match *sound {
                    "-" => "None".to_owned(),
                    s => format!(
                        "Some({})",
                        Lookups::get(&lookups.sounds, s, "sound event", "SoundEvent")?
                    ),
                };
                let sets = sets
                    .iter()
                    .map(|s| match *s {
                        "-" => Ok("None".to_owned()),
                        s => set_consts
                            .get(s)
                            .map(|ident| format!("Some(&{ident})"))
                            .with_context(|| format!("unknown trade set {s}")),
                    })
                    .collect::<Result<Vec<_>>>()?;
                professions.insert(
                    name.to_string(),
                    (held, acquirable.join(", "), sound, sets.join(", ")),
                );
            }
            ["requested", name, items @ ..] => {
                let items = items
                    .iter()
                    .map(|i| lookups.item(i))
                    .collect::<Result<_>>()?;
                requested.insert(name.to_string(), items);
            }
            ["secondary", name, blocks @ ..] => {
                let blocks = blocks
                    .iter()
                    .map(|b| Lookups::get(&lookups.blocks, b, "block", "Block"))
                    .collect::<Result<_>>()?;
                secondary.insert(name.to_string(), blocks);
            }
            ["biome_type", biome, kind] => {
                let kind = Lookups::get(&lookups.types, kind, "villager type", "VillagerType")?;
                biome_types.push(format!("({}, {kind})", str_literal(biome)));
            }
            ["default_type", kind] => {
                default_type = Some(Lookups::get(
                    &lookups.types,
                    kind,
                    "villager type",
                    "VillagerType",
                )?);
            }
            ["xp_threshold", level, xp] => {
                ensure!(
                    level.parse::<usize>().with_context(what)? == thresholds.len() + 1,
                    "{}: levels out of order",
                    what()
                );
                thresholds.push(xp.parse::<i32>().with_context(what)?);
            }
            ["gossip", name, weight, max, day, transfer] => {
                gossip.push(format!(
                    "GossipKind {{ name: {}, weight: {weight}, max: {max}, decay_per_day: {day}, decay_per_transfer: {transfer} }}",
                    str_literal(name)
                ));
            }
            ["hostile", kind, distance] => {
                let kind = Lookups::get(&entity_types, kind, "entity type", "EntityType")?;
                let distance: f32 = distance.parse().with_context(what)?;
                hostiles.push(format!("({kind}, {distance:?}_f32)"));
            }
            _ => bail!("{}: unknown line", what()),
        }
    }
    ensure!(!pois.is_empty(), "the villager dumper listed no pois");
    ensure!(thresholds.len() == 5, "expected five villager levels");
    ensure!(
        professions.len() == lookups.professions.len(),
        "the villager dumper listed {} professions, the registry has {}",
        professions.len(),
        lookups.professions.len()
    );
    state_pois.sort();
    ensure!(
        state_pois.windows(2).all(|w| w[0].0 != w[1].0),
        "a block state makes two point of interest types"
    );

    writeln!(
        out,
        "static POI_INFO: &[(PointOfInterestType, PoiInfo)] = &[{}];",
        pois.join(", ")
    )?;
    writeln!(out, "static STATE_POIS: &[(u32, PointOfInterestType)] = &[")?;
    for (state, poi) in &state_pois {
        writeln!(out, "    ({state}, {poi}),")?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "static PROFESSIONS: &[(VillagerProfession, Profession)] = &["
    )?;
    for (name, (held, acquirable, sound, sets)) in &professions {
        let variant = Lookups::get(
            &lookups.professions,
            name,
            "villager profession",
            "VillagerProfession",
        )?;
        let requested = requested
            .get(name)
            .map(|v| v.join(", "))
            .unwrap_or_default();
        let secondary = secondary
            .get(name)
            .map(|v| v.join(", "))
            .unwrap_or_default();
        writeln!(
            out,
            "    ({variant}, Profession {{ job_site: {held}, acquirable_job_sites: &[{acquirable}], work_sound: {sound}, trade_sets: [{sets}], requested_items: &[{requested}], secondary_blocks: &[{secondary}] }}),"
        )?;
    }
    writeln!(out, "];")?;
    biome_types.sort();
    writeln!(
        out,
        "static BIOME_TYPES: &[(&str, VillagerType)] = &[{}];",
        biome_types.join(", ")
    )?;
    writeln!(
        out,
        "/// The villager type of biomes the game does not map.\npub const DEFAULT_TYPE: VillagerType = {};",
        default_type.context("the villager dumper gave no default type")?
    )?;
    writeln!(
        out,
        "/// Experience at which each villager level (1 to 5) starts.\npub const LEVEL_XP: [i32; 5] = {thresholds:?};"
    )?;
    writeln!(
        out,
        "/// Gossip types, in the game's order.\npub static GOSSIP_KINDS: &[GossipKind] = &[{}];",
        gossip.join(", ")
    )?;
    ensure!(
        !hostiles.is_empty(),
        "the villager dumper listed no hostiles"
    );
    writeln!(
        out,
        "/// Mobs villagers panic at, and how close they must be.\npub static HOSTILE_DISTANCES: &[(EntityType, f32)] = &[{}];",
        hostiles.join(", ")
    )?;
    Ok(out)
}

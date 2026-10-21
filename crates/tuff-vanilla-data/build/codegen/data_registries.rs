//! Entry names of the data-driven registries defined by the built-in data
//! pack. Their network IDs are assigned by the order the server sends them in,
//! so only names are generated. Which ones are synchronized to clients, and
//! in what order, comes from `build/NetworkRegistries.java`.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Result, bail, ensure};

use crate::input::Input;
use crate::names::{self, shouty, str_literal, strip_namespace};

fn synchronized(input: &Input) -> Result<Vec<&str>> {
    let mut keys: Vec<&str> = Vec::new();
    for line in input.network_registries.lines() {
        let Some(key) = line.strip_prefix("synchronized ") else {
            bail!("unexpected NetworkRegistries.java line: {line:?}");
        };
        let Some(registry) = input.datapack.registries.get(key) else {
            bail!("synchronized registry {key} is not in datapack.json");
        };
        ensure!(
            registry.elements && !registry.stable,
            "synchronized registry {key} is not a data-driven registry \
             (elements: {}, stable: {})",
            registry.elements,
            registry.stable,
        );
        ensure!(
            !keys.contains(&key),
            "synchronized registry {key} is listed twice"
        );
        keys.push(key);
    }
    ensure!(
        !keys.is_empty(),
        "NetworkRegistries.java listed no synchronized registries"
    );
    Ok(keys)
}

const NON_EMPTY: &[&str] = &[
    "minecraft:wolf_variant",
    "minecraft:wolf_sound_variant",
    "minecraft:pig_variant",
    "minecraft:pig_sound_variant",
    "minecraft:frog_variant",
    "minecraft:cat_variant",
    "minecraft:cat_sound_variant",
    "minecraft:cow_variant",
    "minecraft:cow_sound_variant",
    "minecraft:chicken_variant",
    "minecraft:chicken_sound_variant",
    "minecraft:zombie_nautilus_variant",
    "minecraft:painting_variant",
];

pub fn generate(input: &Input) -> Result<String> {
    let synchronized = synchronized(input)?;
    let mut registries: BTreeMap<&str, Vec<(String, u64)>> = input
        .datapack
        .registries
        .iter()
        .filter(|(_, r)| r.elements)
        .map(|(key, _)| (key.as_str(), Vec::new()))
        .collect();
    let mut paths: Vec<_> = registries
        .keys()
        .map(|key| (strip_namespace(key), *key))
        .collect();
    paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));

    let mut shapes: BTreeMap<&str, Vec<&serde_json::Value>> = BTreeMap::new();
    for file in &input.data_files {
        let Some((namespace, rest)) = file.split_once('/') else {
            continue;
        };
        let Some(rest) = rest.strip_suffix(".json") else {
            continue;
        };
        let registry = paths.iter().find_map(|(path, key)| {
            let entry = rest.strip_prefix(path)?.strip_prefix('/')?;
            Some((*key, entry))
        });
        if let Some((key, entry)) = registry {
            if let Some(json) = input.data_json.get(file) {
                shapes.entry(key).or_default().push(json);
            }
            registries
                .get_mut(key)
                .expect("key from map")
                .push((format!("{namespace}:{entry}"), input.data_hashes[file]));
        }
    }

    for entries in registries.values_mut() {
        entries.sort();
    }
    for key in NON_EMPTY {
        if let Some(entries) = registries.get(key) {
            anyhow::ensure!(!entries.is_empty(), "Registry {key} must be non-empty");
        }
    }
    let consts = names::unique(registries.keys().copied(), shouty)?;
    let mut out = String::new();
    for ((key, entries), name) in registries.iter().zip(&consts) {
        let quoted: Vec<_> = entries.iter().map(|(e, _)| str_literal(e)).collect();
        let hashes: Vec<_> = entries.iter().map(|(_, h)| format!("{h:#x}")).collect();
        writeln!(
            out,
            "pub const {name}: DataRegistry = DataRegistry {{ key: {}, synchronized: {}, entries: &[{}], hashes: &[{}] }};",
            str_literal(key),
            synchronized.contains(key),
            quoted.join(", "),
            hashes.join(", "),
        )?;
    }
    writeln!(
        out,
        "pub const ALL: &[DataRegistry] = &[{}];",
        consts.join(", ")
    )?;
    let by_key: BTreeMap<&str, &String> = registries.keys().copied().zip(&consts).collect();
    let sent: Vec<&str> = synchronized
        .iter()
        .map(|key| by_key[key].as_str())
        .collect();
    writeln!(
        out,
        "pub const SYNCHRONIZED: &[DataRegistry] = &[{}];",
        sent.join(", ")
    )?;
    writeln!(
        out,
        "pub const REQUIRED_FIELDS: &[(&str, &[(&str, FieldKind)])] = &["
    )?;
    for key in &synchronized {
        let fields = shapes
            .get(key)
            .map(|e| required_fields(e))
            .unwrap_or_default();
        let fields: Vec<String> = fields
            .iter()
            .map(|(name, kind)| format!("({}, FieldKind::{kind})", str_literal(name)))
            .collect();
        writeln!(out, "    ({}, &[{}]),", str_literal(key), fields.join(", "))?;
    }
    writeln!(out, "];")?;
    let lists: Vec<String> = shapes
        .iter()
        .filter(|(_, entries)| entries.iter().all(|e| e.is_array()))
        .map(|(key, _)| str_literal(key))
        .collect();
    writeln!(
        out,
        "pub const LIST_ENTRIES: &[&str] = &[{}];",
        lists.join(", ")
    )?;
    Ok(out)
}

/// The top-level fields every built-in entry has (a dispatched registry
/// needs only its `type`), with the JSON kind they share.
fn required_fields(entries: &[&serde_json::Value]) -> Vec<(String, &'static str)> {
    use serde_json::Value;
    let kind = |value: &Value| match value {
        Value::Bool(_) => "Bool",
        Value::Number(_) => "Number",
        Value::String(_) => "String",
        Value::Array(_) | Value::Object(_) => "Structure",
        Value::Null => "Any",
    };
    let Some(first) = entries.first().and_then(|e| e.as_object()) else {
        return Vec::new();
    };
    let dispatched = entries
        .iter()
        .all(|e| e.get("type").is_some_and(Value::is_string));
    first
        .iter()
        .filter(|(name, _)| !dispatched || name.as_str() == "type")
        .filter(|(name, _)| entries.iter().all(|e| e.get(name.as_str()).is_some()))
        .map(|(name, value)| {
            let k = kind(value);
            let same = entries.iter().all(|e| kind(&e[name.as_str()]) == k);
            (name.clone(), if same { k } else { "Any" })
        })
        .collect()
}

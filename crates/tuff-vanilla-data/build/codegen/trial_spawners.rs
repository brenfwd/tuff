//! The data pack's `minecraft:trial_spawner` entries: how many mobs a trial
//! spawner sends, which ones, and what it ejects afterwards. Every field is
//! checked, so a new field in a future jar fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::json;
use crate::input::Input;
use crate::names::str_literal;

const FIELDS: [&str; 9] = [
    "spawn_range",
    "total_mobs",
    "simultaneous_mobs",
    "total_mobs_added_per_player",
    "simultaneous_mobs_added_per_player",
    "ticks_between_spawn",
    "spawn_potentials",
    "loot_tables_to_eject",
    "items_to_drop_when_ominous",
];

pub fn generate(input: &Input) -> Result<String> {
    let mut configs = BTreeMap::new();
    for (path, value) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("trial_spawner/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let name = format!("{namespace}:{name}");
        let code = config(&name, value).with_context(|| format!("data/{path}"))?;
        configs.insert(name, code);
    }
    ensure!(
        !configs.is_empty(),
        "no trial spawner entries in the data pack"
    );
    let mut out = String::new();
    writeln!(
        out,
        "/// Every trial spawner config, sorted by name.\npub static ALL: &[(&str, &TrialSpawnerConfig)] = &["
    )?;
    for (name, code) in &configs {
        writeln!(out, "    ({}, &{code}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

fn config(name: &str, value: &Value) -> Result<String> {
    let object = value.as_object().context("expected an object")?;
    for key in object.keys() {
        ensure!(FIELDS.contains(&key.as_str()), "unknown field {key}");
    }
    let int = |key: &str, default: i64| -> Result<i64> {
        match object.get(key) {
            None => Ok(default),
            Some(v) => v
                .as_i64()
                .with_context(|| format!("{key} is not an integer")),
        }
    };
    let float = |key: &str, default: f64| -> Result<String> {
        let v = match object.get(key) {
            None => default,
            Some(v) => v
                .as_f64()
                .with_context(|| format!("{key} is not a number"))?,
        };
        Ok(format!("{:?}f32", v as f32))
    };
    let mut potentials = String::new();
    for entry in object
        .get("spawn_potentials")
        .map(|v| v.as_array().context("spawn_potentials is not a list"))
        .transpose()?
        .into_iter()
        .flatten()
    {
        let (weight, data) = weighted(entry)?;
        let data = data.as_object().context("spawn data is not an object")?;
        for key in data.keys() {
            ensure!(
                ["entity", "equipment", "custom_spawn_rules"].contains(&key.as_str()),
                "unknown spawn data field {key}"
            );
        }
        let entity = data.get("entity").context("spawn data without entity")?;
        ensure!(entity.is_object(), "spawn entity is not an object");
        write!(
            potentials,
            "SpawnPotential {{ weight: {weight}, entity: {}, equipment: {}, custom_spawn_rules: {} }}, ",
            json::literal(entity),
            json::opt_literal(data.get("equipment")),
            json::opt_literal(data.get("custom_spawn_rules")),
        )?;
    }
    let ejected = match object.get("loot_tables_to_eject") {
        None => "DEFAULT_LOOT_TABLES_TO_EJECT".to_owned(),
        Some(list) => {
            let mut out = String::from("&[");
            for entry in list
                .as_array()
                .context("loot_tables_to_eject is not a list")?
            {
                let (weight, table) = weighted(entry)?;
                let table = table.as_str().context("loot table is not an id")?;
                write!(out, "({weight}, {}), ", str_literal(table))?;
            }
            out.push(']');
            out
        }
    };
    let ominous = match object.get("items_to_drop_when_ominous") {
        None => "DEFAULT_ITEMS_TO_DROP_WHEN_OMINOUS".to_owned(),
        Some(v) => str_literal(v.as_str().context("items_to_drop_when_ominous")?),
    };
    Ok(format!(
        "TrialSpawnerConfig {{ name: {}, spawn_range: {}, total_mobs: {}, simultaneous_mobs: {}, \
         total_mobs_added_per_player: {}, simultaneous_mobs_added_per_player: {}, \
         ticks_between_spawn: {}, spawn_potentials: &[{potentials}], loot_tables_to_eject: {ejected}, \
         items_to_drop_when_ominous: {ominous} }}",
        str_literal(name),
        int("spawn_range", 4)?,
        float("total_mobs", 6.0)?,
        float("simultaneous_mobs", 2.0)?,
        float("total_mobs_added_per_player", 2.0)?,
        float("simultaneous_mobs_added_per_player", 1.0)?,
        int("ticks_between_spawn", 40)?,
    ))
}

fn weighted(entry: &Value) -> Result<(u64, &Value)> {
    let object = entry
        .as_object()
        .context("weighted entry is not an object")?;
    for key in object.keys() {
        if key != "data" && key != "weight" {
            bail!("unknown weighted entry field {key}");
        }
    }
    let weight = object
        .get("weight")
        .context("weighted entry without weight")?
        .as_u64()
        .context("weight is not a positive integer")?;
    Ok((
        weight,
        object.get("data").context("weighted entry without data")?,
    ))
}

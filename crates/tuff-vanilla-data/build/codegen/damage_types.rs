use std::fmt::Write;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::enums::{emit_value_enum, unique_sorted};
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DamageType {
    message_id: String,
    exhaustion: f32,
    scaling: String,
    #[serde(default)]
    death_message_type: Option<String>,
    #[serde(default)]
    effects: Option<String>,
}

pub fn generate(input: &Input) -> Result<String> {
    let mut damage_types = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("damage_type/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let parsed: DamageType =
            serde_json::from_value(json.clone()).with_context(|| format!("data/{path}"))?;
        damage_types.push((format!("{namespace}:{name}"), parsed));
    }

    let scaling: Vec<String> = damage_types
        .iter()
        .map(|(_, d)| d.scaling.clone())
        .collect();
    let death_message_type: Vec<String> = damage_types
        .iter()
        .filter_map(|(_, d)| d.death_message_type.clone())
        .collect();
    let effects: Vec<String> = damage_types
        .iter()
        .filter_map(|(_, d)| d.effects.clone())
        .collect();

    let mut out = String::new();
    emit_value_enum(&mut out, "Scaling", &unique_sorted(scaling))?;
    emit_value_enum(
        &mut out,
        "DeathMessageType",
        &unique_sorted(death_message_type),
    )?;
    emit_value_enum(&mut out, "DamageEffects", &unique_sorted(effects))?;

    let consts = names::unique(damage_types.iter().map(|(name, _)| name.as_str()), shouty)?;
    for ((name, d), ident) in damage_types.iter().zip(&consts) {
        writeln!(
            out,
            "pub const {ident}: DamageType = DamageType {{
    name: {},
    message_id: {},
    exhaustion: {:?},
    scaling: Scaling::{},
    death_message_type: {},
    effects: {},
}};",
            str_literal(name),
            str_literal(&d.message_id),
            d.exhaustion,
            names::pascal(&d.scaling),
            optional_variant("DeathMessageType", d.death_message_type.as_deref()),
            optional_variant("DamageEffects", d.effects.as_deref()),
        )?;
    }
    writeln!(
        out,
        "pub const ALL: &[DamageType] = &[{}];",
        consts.join(", ")
    )?;
    Ok(out)
}

fn optional_variant(type_name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!("Some({type_name}::{})", names::pascal(value)),
        None => "None".to_owned(),
    }
}

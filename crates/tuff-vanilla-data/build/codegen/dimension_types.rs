use std::fmt::Write;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::input::Input;
use crate::names::{self, shouty, str_literal};

#[derive(Deserialize)]
struct DimensionType {
    min_y: i32,
    height: u32,
    logical_height: u32,
    has_skylight: bool,
    has_ceiling: bool,
    #[serde(default)]
    has_ender_dragon_fight: bool,
    coordinate_scale: f64,
    ambient_light: f32,
    infiniburn: String,
    #[serde(default)]
    default_clock: Option<String>,
    monster_spawn_block_light_limit: u8,
    monster_spawn_light_level: LightLevel,
}

/// A constant or `minecraft:uniform` light level.
#[derive(Deserialize)]
#[serde(untagged)]
enum LightLevel {
    Constant(u8),
    Uniform(UniformLevel),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UniformLevel {
    #[serde(rename = "type")]
    kind: String,
    min_inclusive: u8,
    max_inclusive: u8,
}

pub fn generate(input: &Input) -> Result<String> {
    let mut dimensions = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("dimension_type/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let parsed: DimensionType =
            serde_json::from_value(json.clone()).with_context(|| format!("data/{path}"))?;
        dimensions.push((format!("{namespace}:{name}"), parsed));
    }

    let consts = names::unique(dimensions.iter().map(|(name, _)| name.as_str()), shouty)?;
    let mut out = String::new();
    for ((name, d), ident) in dimensions.iter().zip(&consts) {
        let light = match &d.monster_spawn_light_level {
            LightLevel::Constant(level) => (*level, *level),
            LightLevel::Uniform(uniform) => {
                anyhow::ensure!(
                    uniform.kind == "minecraft:uniform",
                    "{name}: unknown monster spawn light provider {}",
                    uniform.kind
                );
                (uniform.min_inclusive, uniform.max_inclusive)
            }
        };
        writeln!(
            out,
            "pub const {ident}: DimensionType = DimensionType {{
    name: {},
    min_y: {},
    height: {},
    logical_height: {},
    has_skylight: {},
    has_ceiling: {},
    has_ender_dragon_fight: {},
    coordinate_scale: {:?},
    ambient_light: {:?},
    infiniburn: {},
    default_clock: {},
    monster_spawn_block_light_limit: {},
    monster_spawn_light_level: ({}, {}),
}};",
            str_literal(name),
            d.min_y,
            d.height,
            d.logical_height,
            d.has_skylight,
            d.has_ceiling,
            d.has_ender_dragon_fight,
            d.coordinate_scale,
            d.ambient_light,
            str_literal(&d.infiniburn),
            match &d.default_clock {
                Some(clock) => format!("Some({})", str_literal(clock)),
                None => "None".to_owned(),
            },
            d.monster_spawn_block_light_limit,
            light.0,
            light.1,
        )?;
    }
    writeln!(
        out,
        "pub const ALL: &[DimensionType] = &[{}];",
        consts.join(", ")
    )?;
    Ok(out)
}

//! Map colours and decoration types from `build/MapProperties.java`: the
//! base colour table, every block state's map colour and each decoration
//! type's flags. Every state and every registered decoration type must be
//! covered exactly once.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::array_body;
use super::registries::sorted_entries;
use crate::input::Input;
use crate::names::str_literal;

pub fn generate(input: &Input) -> Result<String> {
    let decoration_names = sorted_entries(&input.registries["minecraft:map_decoration_type"])?;
    let state_count = input
        .block_properties
        .lines()
        .filter(|line| line.starts_with("state "))
        .count();

    let mut colors = [0u32; 64];
    let mut seen_colors = [false; 64];
    let mut states: Vec<Option<u8>> = vec![None; state_count];
    let mut decorations: BTreeMap<&str, (String, bool, bool)> = BTreeMap::new();
    for line in input.map_properties.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["color", id, rgb] => {
                let id: usize = id.parse()?;
                ensure!(
                    id < 64 && !seen_colors[id],
                    "map properties: bad color {id}"
                );
                seen_colors[id] = true;
                colors[id] = rgb.parse()?;
            }
            ["states", first, last, color] => {
                let (first, last): (usize, usize) = (first.parse()?, last.parse()?);
                let color: u8 = color.parse()?;
                ensure!(
                    first <= last && last < state_count && seen_colors[usize::from(color)],
                    "map properties: bad state run {line:?}"
                );
                for slot in &mut states[first..=last] {
                    ensure!(slot.is_none(), "map properties: state covered twice");
                    *slot = Some(color);
                }
            }
            ["decoration", name, asset, frame, track] => {
                ensure!(
                    decoration_names.contains(name),
                    "map properties: unknown decoration {name}"
                );
                let flag = |value: &str| match value {
                    "0" => Ok(false),
                    "1" => Ok(true),
                    _ => bail!("map properties: bad flag {value:?}"),
                };
                let previous =
                    decorations.insert(name, (asset.to_string(), flag(frame)?, flag(track)?));
                ensure!(previous.is_none(), "map properties: {name} twice");
            }
            _ => bail!("map properties: unrecognized line {line:?}"),
        }
    }

    let mut out = String::new();
    writeln!(out, "static BASE_COLORS: [u32; 64] = [")?;
    array_body(&mut out, colors);
    writeln!(out, "];")?;
    writeln!(out, "static STATE_COLORS: [u8; {state_count}] = [")?;
    let mut expanded = Vec::with_capacity(state_count);
    for (id, color) in states.iter().enumerate() {
        expanded.push(color.with_context(|| format!("map properties: state {id} missing"))?);
    }
    array_body(&mut out, expanded);
    writeln!(out, "];")?;
    writeln!(
        out,
        "static DECORATIONS: [DecorationInfo; {}] = [",
        decoration_names.len()
    )?;
    for name in &decoration_names {
        let (asset, frame, track) = decorations
            .get(name)
            .with_context(|| format!("map properties: decoration {name} missing"))?;
        writeln!(
            out,
            "    DecorationInfo {{ asset: {}, show_on_item_frame: {frame}, track_count: {track} }},",
            str_literal(asset)
        )?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

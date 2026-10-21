//! Game rules from `build/GameRuleProperties.java`: each rule's value type,
//! default and integer bounds, keyed by the `minecraft:game_rule` registry.

use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::registries;
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

struct Rule {
    name: String,
    literal: String,
    features: Vec<String>,
}

pub fn generate(input: &Input) -> Result<String> {
    let registry = input
        .registries
        .get("minecraft:game_rule")
        .context("registries.json has no minecraft:game_rule")?;
    let names = registries::sorted_entries(registry)?;
    let mut rules: Vec<Rule> = Vec::new();
    for line in input.game_rules.lines() {
        let values: Vec<&str> = line.split(' ').collect();
        let (name, literal, features) = match values.as_slice() {
            ["rule", name, "bool", default, features] => {
                let default: bool = default.parse()?;
                (name, format!("GameRuleValue::Bool({default})"), features)
            }
            ["rule", name, "int", default, min, max, features] => {
                let (default, min, max): (i32, i32, i32) =
                    (default.parse()?, min.parse()?, max.parse()?);
                ensure!(
                    min <= default && default <= max,
                    "game rules: default out of bounds in {line:?}"
                );
                (
                    name,
                    format!("GameRuleValue::Int {{ default: {default}, min: {min}, max: {max} }}"),
                    features,
                )
            }
            _ => bail!("game rules: bad line {line:?}"),
        };
        ensure!(
            names.contains(name),
            "game rules: {name} is not in the registry"
        );
        let features: Vec<String> = match *features {
            "-" => Vec::new(),
            "" => bail!("game rules: no feature list in {line:?}"),
            list => list.split(',').map(str_literal).collect(),
        };
        rules.push(Rule {
            name: name.to_string(),
            literal,
            features,
        });
    }
    ensure!(
        rules.len() == names.len(),
        "game rules: {} dumped, but the registry has {}",
        rules.len(),
        names.len()
    );
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    let consts = names::unique(rules.iter().map(|r| r.name.as_str()), shouty)?;
    let mut out = String::new();
    for (rule, ident) in rules.iter().zip(&consts) {
        writeln!(
            out,
            "pub const {ident}: GameRule = GameRule {{ name: {}, value: {}, features: &[{}] }};",
            str_literal(&rule.name),
            rule.literal,
            rule.features.join(", ")
        )?;
    }
    writeln!(
        out,
        "/// Every game rule, sorted by name.\npub const ALL: &[GameRule] = &[{}];",
        consts.join(", ")
    )?;
    Ok(out)
}

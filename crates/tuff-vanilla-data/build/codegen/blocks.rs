//! Block state tables and typed property accessors.
//!
//! Each block's states form the cartesian product of its property values, with
//! properties ordered by name and the last one varying fastest. That layout is
//! verified here, which lets the runtime locate any property value arithmetically
//! instead of storing a table per state.
//!
//! Property values are typed by name: `true`/`false` properties become `bool`,
//! numeric ones `u8`, and all others an enum holding the union of every value
//! that property name takes on any block (`facing` is one `Facing` enum even
//! though stairs only accept the horizontal directions).

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::array_body;
use super::registries::sorted_entries;
use crate::input::{BlockReport, Input};
use crate::names::{pascal, snake, str_literal};

/// Names for enums shared by several property names with the same values.
const SHARED_ENUM_NAMES: &[(&[&str], &str)] = &[(&["east", "north", "south", "west"], "Side")];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Bool,
    Int,
    Enum,
}

fn kind(values: &[String]) -> Kind {
    if values == ["true", "false"] || values == ["false", "true"] {
        Kind::Bool
    } else if values.iter().all(|v| v.parse::<u8>().is_ok()) {
        Kind::Int
    } else {
        Kind::Enum
    }
}

struct Accessor {
    /// Method name without any `r#` prefix.
    base: String,
    variant: String,
    value_type: String,
}

pub fn generate(input: &Input) -> Result<String> {
    let registry = &input.registries["minecraft:block"];
    let blocks = sorted_entries(registry)?
        .into_iter()
        .map(|name| {
            let report = input
                .blocks
                .get(name)
                .with_context(|| format!("no report for {name}"))?;
            Ok((name, report))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        blocks.len() == input.blocks.len(),
        "blocks.json and the block registry disagree"
    );

    let mut state_count = 0u32;
    for (name, report) in &blocks {
        validate_states(name, report, state_count)?;
        state_count += report.states.len() as u32;
    }
    ensure!(
        state_count <= u16::MAX as u32 + 1,
        "{state_count} block states do not fit in u16"
    );

    let (enums, accessors) = property_types(&blocks)?;

    let mut out = String::new();
    for (type_name, values) in &enums {
        super::enums::emit_value_enum(&mut out, type_name, values)?;
    }

    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\nenum Property {{"
    )?;
    for accessor in accessors.values() {
        writeln!(out, "    {},", accessor.variant)?;
    }
    writeln!(out, "}}\n")?;

    writeln!(out, "impl BlockState {{")?;
    for ((name, kind), accessor) in &accessors {
        emit_accessor(&mut out, name, *kind, accessor)?;
    }
    writeln!(out, "}}\n")?;

    let mut defs: BTreeMap<(&str, &[String]), usize> = BTreeMap::new();
    for (_, report) in &blocks {
        for (name, values) in &report.properties {
            let next = defs.len();
            defs.entry((name, values)).or_insert(next);
        }
    }
    let mut ordered_defs: Vec<_> = defs.iter().collect();
    ordered_defs.sort_by_key(|(_, index)| **index);
    for ((name, values), index) in ordered_defs {
        let kind = kind(values);
        let accessor = &accessors[&(name.to_string(), kind)];
        let raw: Vec<String> = values
            .iter()
            .map(|value| match kind {
                Kind::Bool => Ok(u8::from(value == "true").to_string()),
                Kind::Int => Ok(value.clone()),
                Kind::Enum => enums[&accessor.value_type]
                    .iter()
                    .position(|v| v == value)
                    .map(|i| i.to_string())
                    .context("enum value missing from union"),
            })
            .collect::<Result<_>>()?;
        let quoted: Vec<_> = values.iter().map(|v| str_literal(v)).collect();
        writeln!(
            out,
            "static P{index}: PropertyDef = PropertyDef {{ property: Property::{}, name: {}, values: &[{}], raw: &[{}] }};",
            accessor.variant,
            str_literal(name),
            quoted.join(", "),
            raw.join(", "),
        )?;
    }

    writeln!(out, "\nconst STATE_COUNT: usize = {state_count};")?;
    write!(
        out,
        "static BLOCK_PROPERTIES: [&[&PropertyDef]; {}] = [",
        blocks.len()
    )?;
    for (_, report) in &blocks {
        let refs: Vec<_> = report
            .properties
            .iter()
            .map(|(name, values)| format!("&P{}", defs[&(name.as_str(), values.as_slice())]))
            .collect();
        write!(out, "\n    &[{}],", refs.join(", "))?;
    }
    writeln!(out, "\n];")?;

    write!(
        out,
        "static BLOCK_FIRST_STATE: [u16; {}] = [",
        blocks.len() + 1
    )?;
    let firsts = blocks
        .iter()
        .map(|(_, r)| r.states[0].id)
        .chain([state_count]);
    array_body(&mut out, firsts);
    writeln!(out, "];")?;

    write!(
        out,
        "static BLOCK_DEFAULT_STATE: [u16; {}] = [",
        blocks.len()
    )?;
    let defaults = blocks.iter().map(|(_, r)| {
        r.states
            .iter()
            .find(|s| s.default)
            .map(|s| s.id)
            .unwrap_or_default()
    });
    array_body(&mut out, defaults);
    writeln!(out, "];")?;

    write!(out, "static STATE_BLOCK: [Block; STATE_COUNT] = [")?;
    let variants = super::registries::variants(registry)?;
    let owners = blocks.iter().flat_map(|(name, r)| {
        std::iter::repeat_n(format!("Block::{}", variants[name]), r.states.len())
    });
    array_body(&mut out, owners);
    writeln!(out, "];")?;
    Ok(out)
}

fn validate_states(name: &str, report: &BlockReport, first: u32) -> Result<()> {
    ensure!(
        report.states.iter().filter(|s| s.default).count() == 1,
        "{name} does not have exactly one default state"
    );
    let expected_len: usize = report.properties.values().map(Vec::len).product();
    ensure!(
        report.states.len() == expected_len,
        "{name} has an unexpected number of states"
    );
    for (offset, state) in report.states.iter().enumerate() {
        ensure!(
            state.id == first + offset as u32,
            "{name} states are not contiguous"
        );
        let mut remainder = offset;
        for (property, values) in report.properties.iter().rev() {
            let expected = &values[remainder % values.len()];
            remainder /= values.len();
            ensure!(
                state.properties.get(property) == Some(expected),
                "{name} state {} does not follow the name-ordered cartesian layout",
                state.id
            );
        }
        ensure!(
            state.properties.len() == report.properties.len(),
            "{name} has stray properties"
        );
    }
    Ok(())
}

type Enums = BTreeMap<String, Vec<String>>;
type Accessors = BTreeMap<(String, Kind), Accessor>;

fn property_types(blocks: &[(&str, &BlockReport)]) -> Result<(Enums, Accessors)> {
    let mut kinds: BTreeMap<&str, Vec<Kind>> = BTreeMap::new();
    let mut unions: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (_, report) in blocks {
        for (name, values) in &report.properties {
            let kind = kind(values);
            let kinds = kinds.entry(name).or_default();
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
            if kind == Kind::Enum {
                let union = unions.entry(name).or_default();
                for value in values {
                    if !union.contains(value) {
                        union.push(value.clone());
                    }
                }
            }
        }
    }

    let mut sharing: BTreeMap<Vec<String>, Vec<&str>> = BTreeMap::new();
    for (name, union) in &unions {
        let mut key = union.clone();
        key.sort();
        sharing.entry(key).or_default().push(name);
    }
    let mut enums = Enums::new();
    let mut enum_of: BTreeMap<&str, String> = BTreeMap::new();
    for names in sharing.values() {
        let type_name = match names.as_slice() {
            [name] => pascal(name),
            _ => match SHARED_ENUM_NAMES.iter().find(|(shared, _)| shared == names) {
                Some((_, type_name)) => type_name.to_string(),
                None => bail!("properties {names:?} share values; add a name to SHARED_ENUM_NAMES"),
            },
        };
        let prior = enums.insert(type_name.clone(), unions[names[0]].clone());
        ensure!(prior.is_none(), "two property enums are named {type_name}");
        for name in names {
            enum_of.insert(name, type_name.clone());
        }
    }

    let mut accessors = Accessors::new();
    for (name, kinds) in &kinds {
        for &kind in kinds {
            let value_type = match kind {
                Kind::Bool => "bool".to_owned(),
                Kind::Int => "u8".to_owned(),
                Kind::Enum => enum_of[name].clone(),
            };
            let base = match kind {
                Kind::Bool => name.to_string(),
                _ if kinds.len() == 1 => name.to_string(),
                Kind::Int => format!("{name}_value"),
                Kind::Enum => format!("{name}_{}", snake(&value_type)),
            };
            let variant = pascal(&base);
            accessors.insert(
                (name.to_string(), kind),
                Accessor {
                    base,
                    variant,
                    value_type,
                },
            );
        }
    }
    let variants = accessors.values().map(|a| a.variant.as_str());
    crate::names::unique(variants, str::to_owned)?;
    Ok((enums, accessors))
}

fn emit_accessor(out: &mut String, name: &str, kind: Kind, accessor: &Accessor) -> Result<()> {
    let Accessor {
        base,
        variant,
        value_type,
    } = accessor;
    let getter = snake(base);
    let (from_raw, to_raw) = match kind {
        Kind::Bool => (".map(|raw| raw != 0)".to_owned(), "u8::from(value)"),
        Kind::Int => (String::new(), "value"),
        Kind::Enum => (
            format!(".map(|raw| {value_type}::ALL[raw as usize])"),
            "value as u8",
        ),
    };
    writeln!(
        out,
        r#"    /// The `{name}` property, or `None` if this block does not have it.
    pub fn {getter}(self) -> Option<{value_type}> {{
        self.get_raw(Property::{variant}){from_raw}
    }}

    /// Sets the `{name}` property, or returns `None` if this block does not
    /// have it or does not accept `value`.
    pub fn with_{base}(self, value: {value_type}) -> Option<Self> {{
        self.with_raw(Property::{variant}, {to_raw})
    }}
"#
    )?;
    Ok(())
}

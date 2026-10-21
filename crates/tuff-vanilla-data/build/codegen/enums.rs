//! Emits fieldless enums whose discriminants are dense protocol IDs.

use std::fmt::Write;

use anyhow::Result;

use super::array_body;
use crate::names::{self, str_literal};

pub struct IdEnum<'a> {
    pub doc: String,
    pub name: &'a str,
    /// Integer type of `id()`/`from_id()`, matching how the protocol encodes it.
    pub id_type: &'a str,
    /// Resource locations, indexed by ID.
    pub entries: Vec<&'a str>,
    /// Entry that `Default` returns, if the registry has one.
    pub default: Option<&'a str>,
}

impl IdEnum<'_> {
    pub fn variants(&self) -> Result<Vec<String>> {
        names::unique(self.entries.iter().copied(), names::pascal)
    }

    pub fn emit(&self, out: &mut String) -> Result<()> {
        let IdEnum {
            doc,
            name,
            id_type,
            entries,
            default,
        } = self;
        anyhow::ensure!(
            default.is_none_or(|d| entries.contains(&d)),
            "default {default:?} of {name} is not an entry"
        );
        let variants = self.variants()?;
        let count = entries.len();
        let lookup = if entries.iter().all(|e| e.contains(':')) {
            "crate::lookup(&Self::BY_NAME, name)"
        } else {
            "Self::BY_NAME\n            .binary_search_by(|(key, _)| (*key).cmp(name))\n            .ok()\n            .map(|index| Self::BY_NAME[index].1)"
        };
        let repr = if count <= 1 << 8 { "u8" } else { "u16" };

        writeln!(out, "#[doc = {}]", str_literal(doc))?;
        let derive_default = if default.is_some() { ", Default" } else { "" };
        writeln!(
            out,
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash{derive_default})]"
        )?;
        writeln!(out, "#[repr({repr})]")?;
        writeln!(out, "pub enum {name} {{")?;
        for (entry, variant) in entries.iter().zip(&variants) {
            writeln!(out, "    #[doc = \"`{entry}`\"]")?;
            if *default == Some(*entry) {
                writeln!(out, "    #[default]")?;
            }
            writeln!(out, "    {variant},")?;
        }
        writeln!(out, "}}\n")?;

        let mut by_name: Vec<_> = entries.iter().zip(&variants).collect();
        by_name.sort();
        writeln!(out, "impl {name} {{")?;
        writeln!(out, "    pub const COUNT: usize = {count};")?;
        write!(out, "    pub const ALL: [Self; {count}] = [")?;
        array_body(out, variants.iter().map(|v| format!("Self::{v}")));
        writeln!(out, "    ];")?;
        write!(out, "    const NAMES: [&'static str; {count}] = [")?;
        array_body(out, entries.iter().map(|e| str_literal(e)));
        writeln!(out, "    ];")?;
        write!(
            out,
            "    const BY_NAME: [(&'static str, Self); {count}] = ["
        )?;
        array_body(
            out,
            by_name
                .iter()
                .map(|(e, v)| format!("({}, Self::{v})", str_literal(e))),
        );
        writeln!(out, "    ];")?;
        writeln!(
            out,
            r#"
    pub const fn id(self) -> {id_type} {{
        self as {id_type}
    }}

    pub const fn from_id(id: {id_type}) -> Option<Self> {{
        if (id as u64) < Self::COUNT as u64 {{
            Some(Self::ALL[id as usize])
        }} else {{
            None
        }}
    }}

    /// The resource location, e.g. `minecraft:stone`.
    pub const fn name(self) -> &'static str {{
        Self::NAMES[self as usize]
    }}

    /// Accepts the resource location with or without the `minecraft:` namespace.
    pub fn from_name(name: &str) -> Option<Self> {{
        {lookup}
    }}
}}

impl core::fmt::Display for {name} {{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{
        f.write_str(self.name())
    }}
}}
"#
        )?;
        Ok(())
    }
}

/// Orders `entries` by ID, failing unless the IDs are exactly `0..len`.
pub fn by_id<'a>(entries: impl IntoIterator<Item = (&'a str, u32)>) -> Result<Vec<&'a str>> {
    let mut entries: Vec<_> = entries.into_iter().collect();
    entries.sort_by_key(|&(_, id)| id);
    for (index, &(name, id)) in entries.iter().enumerate() {
        anyhow::ensure!(
            id as usize == index,
            "IDs are not dense: `{name}` has ID {id}, expected {index}"
        );
    }
    Ok(entries.into_iter().map(|(name, _)| name).collect())
}

/// Emits a fieldless enum backed by an arbitrary set of names (no dense
/// numeric ID), such as a block property's possible values.
pub fn emit_value_enum(out: &mut String, type_name: &str, values: &[String]) -> Result<()> {
    let variants = names::unique(values.iter().map(String::as_str), names::pascal)?;
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]"
    )?;
    writeln!(out, "pub enum {type_name} {{")?;
    for variant in &variants {
        writeln!(out, "    {variant},")?;
    }
    writeln!(out, "}}\n\nimpl {type_name} {{")?;
    let all: Vec<_> = variants.iter().map(|v| format!("Self::{v}")).collect();
    writeln!(
        out,
        "    pub const ALL: [Self; {}] = [{}];\n",
        all.len(),
        all.join(", ")
    )?;
    writeln!(
        out,
        "    pub const fn name(self) -> &'static str {{\n        match self {{"
    )?;
    for (variant, value) in variants.iter().zip(values) {
        writeln!(
            out,
            "            Self::{variant} => {},",
            str_literal(value)
        )?;
    }
    writeln!(
        out,
        "        }}\n    }}\n\n    pub fn from_name(name: &str) -> Option<Self> {{"
    )?;
    writeln!(out, "        Some(match name {{")?;
    for (variant, value) in variants.iter().zip(values) {
        writeln!(
            out,
            "            {} => Self::{variant},",
            str_literal(value)
        )?;
    }
    writeln!(
        out,
        "            _ => return None,\n        }})\n    }}\n}}\n"
    )?;
    writeln!(
        out,
        "impl core::fmt::Display for {type_name} {{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{
        f.write_str(self.name())
    }}
}}
"
    )?;
    Ok(())
}

/// `values` sorted and deduplicated, for [`emit_value_enum`].
pub fn unique_sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

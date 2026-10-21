//! One enum per built-in (static) registry. Registries under a path such as
//! `minecraft:worldgen/feature_type` are nested in matching modules.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::Result;

use super::enums::{IdEnum, by_id};
use crate::input::{Input, RegistryReport};
use crate::names::{pascal, snake, str_literal, strip_namespace};

pub fn generate(input: &Input) -> Result<String> {
    let mut root = Module::default();
    for (key, registry) in &input.registries {
        let mut segments: Vec<_> = strip_namespace(key).split('/').collect();
        segments.pop();
        let mut module = &mut root;
        for segment in segments {
            module = module.children.entry(snake(segment)).or_default();
        }
        module.registries.push((key, registry));
    }
    let mut out = String::new();
    root.emit(&mut out, 0)?;
    Ok(out)
}

/// The path of the enum generated for the static registry `key`, from the
/// crate root.
pub fn type_path(key: &str) -> String {
    let segments: Vec<_> = strip_namespace(key).split('/').collect();
    let (name, modules) = segments
        .split_last()
        .expect("split always yields a segment");
    let mut path = String::from("crate::registry::");
    for module in modules {
        path.push_str(&snake(module));
        path.push_str("::");
    }
    path.push_str(&pascal(name));
    path
}

/// Variant identifiers for the static registry `key`, keyed by entry name.
pub fn variants(registry: &RegistryReport) -> Result<BTreeMap<&str, String>> {
    let entries = sorted_entries(registry)?;
    let variants = registry_enum("", "", registry, entries.clone()).variants()?;
    Ok(entries.into_iter().zip(variants).collect())
}

/// Entry names of a static registry, ordered by protocol ID.
pub fn sorted_entries(registry: &RegistryReport) -> Result<Vec<&str>> {
    by_id(
        registry
            .entries
            .iter()
            .map(|(name, id)| (name.as_str(), id.protocol_id)),
    )
}

fn registry_enum<'a>(
    key: &str,
    name: &'a str,
    registry: &'a RegistryReport,
    entries: Vec<&'a str>,
) -> IdEnum<'a> {
    IdEnum {
        doc: format!("Entries of the built-in `{key}` registry, in protocol ID order."),
        name,
        id_type: "u32",
        entries,
        default: registry.default.as_deref(),
    }
}

#[derive(Default)]
struct Module<'a> {
    registries: Vec<(&'a String, &'a RegistryReport)>,
    children: BTreeMap<String, Module<'a>>,
}

impl Module<'_> {
    fn emit(&self, out: &mut String, depth: usize) -> Result<()> {
        let root = "super::".repeat(depth);
        for (key, registry) in &self.registries {
            let name = pascal(strip_namespace(key).rsplit('/').next().unwrap_or(key));
            registry_enum(key, &name, registry, sorted_entries(registry)?).emit(out)?;
            writeln!(
                out,
                r#"impl {root}Registry for {name} {{
    const KEY: &'static str = {key_literal};
    const PROTOCOL_ID: u32 = {protocol_id};
    const ALL: &'static [Self] = &Self::ALL;

    fn id(self) -> u32 {{
        Self::id(self)
    }}

    fn from_id(id: u32) -> Option<Self> {{
        Self::from_id(id)
    }}

    fn name(self) -> &'static str {{
        Self::name(self)
    }}

    fn from_name(name: &str) -> Option<Self> {{
        Self::from_name(name)
    }}
}}
"#,
                key_literal = str_literal(key),
                protocol_id = registry.protocol_id,
            )?;
        }
        for (name, child) in &self.children {
            writeln!(out, "pub mod {name} {{")?;
            child.emit(out, depth + 1)?;
            writeln!(out, "}}")?;
        }
        Ok(())
    }
}

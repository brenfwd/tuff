//! Tags from the built-in data pack, flattened so that nested `#tag`
//! references are already expanded. Tags of built-in registries hold typed
//! entries; tags of data-driven registries hold resource locations.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::registries;
use crate::input::Input;
use crate::names::{self, shouty, snake, str_literal, strip_namespace};

pub type Tags = BTreeMap<String, Vec<String>>;

pub fn generate(input: &Input) -> Result<String> {
    let mut root = Module::default();
    let mut registries = Vec::new();
    let mut paths = registry_paths(input);
    paths.sort();
    paths.dedup();
    for registry in paths {
        let resolved = resolved_registry_tags(input, &registry)?;
        if resolved.is_empty() {
            continue;
        }
        let mut module = &mut root;
        for segment in strip_namespace(&registry).split('/') {
            module = module.children.entry(snake(segment)).or_default();
        }
        module.registry = Some((registries.len(), registry.clone(), resolved));
        registries.push(registry);
    }

    let mut out = String::new();
    writeln!(
        out,
        "/// Every registry with built-in tags, indexed by [`Key::registry`]."
    )?;
    let literals: Vec<String> = registries.iter().map(|r| str_literal(r)).collect();
    writeln!(
        out,
        "pub const REGISTRIES: &[&str] = &[{}];",
        literals.join(", ")
    )?;
    root.emit(&mut out, input)?;
    emit_raw(&mut out, input)?;
    Ok(out)
}

/// Every tag file as written, `#` marking nested tags, so data packs can
/// merge their own entries before nested tags are expanded.
fn emit_raw(out: &mut String, input: &Input) -> Result<()> {
    writeln!(out, "pub mod raw {{")?;
    writeln!(
        out,
        "/// `(registry, [(tag, entries)])`; an entry starting with `#` is a nested tag."
    )?;
    writeln!(
        out,
        "pub type RegistryTags = (&'static str, &'static [(&'static str, &'static [&'static str])]);"
    )?;
    writeln!(out, "pub const ALL: &[RegistryTags] = &[")?;
    let mut paths = registry_paths(input);
    paths.sort();
    paths.dedup();
    for registry in paths {
        let raw = raw_registry_tags(input, &registry)?;
        if raw.is_empty() {
            continue;
        }
        writeln!(out, "    ({}, &[", str_literal(&registry))?;
        for (tag, entries) in &raw {
            let entries: Vec<String> = entries.iter().map(|e| str_literal(e)).collect();
            writeln!(
                out,
                "        ({}, &[{}]),",
                str_literal(tag),
                entries.join(", ")
            )?;
        }
        writeln!(out, "    ]),")?;
    }
    writeln!(out, "];")?;
    writeln!(out, "}}")?;
    Ok(())
}

/// Every registry key (`minecraft:...`) that can have tags: built-in
/// registries plus data-driven ones the built-in data pack tags.
fn registry_paths(input: &Input) -> Vec<String> {
    input
        .registries
        .keys()
        .chain(
            input
                .datapack
                .registries
                .iter()
                .filter(|(_, r)| r.tags)
                .map(|(key, _)| key),
        )
        .cloned()
        .collect()
}

/// The tags of one registry, with nested `#tag` references already expanded.
/// Entries are resource locations; for a built-in registry the caller
/// resolves them into typed variants (see [`emit_registry`]).
pub fn resolved_registry_tags(input: &Input, registry: &str) -> Result<Tags> {
    let raw = raw_registry_tags(input, registry)?;
    let mut resolved = Tags::new();
    for tag in raw.keys() {
        resolve(tag, &raw, &mut resolved, &mut Vec::new())?;
    }
    Ok(resolved)
}

/// The tag files of one registry, entries normalised but nested `#tag`
/// references kept.
fn raw_registry_tags(input: &Input, registry: &str) -> Result<Tags> {
    let mut paths = registry_paths(input);
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    let path = strip_namespace(registry);

    let mut raw = Tags::new();
    for (file, json) in &input.data_json {
        let Some((namespace, rest)) = file.split_once('/') else {
            continue;
        };
        let Some(rest) = rest
            .strip_prefix("tags/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let Some(tag) = rest.strip_prefix(path).and_then(|t| t.strip_prefix('/')) else {
            continue;
        };
        // A tag under a longer registry path (e.g. `worldgen/biome`) is not
        // one of this shorter registry's tags (e.g. `worldgen`), even though
        // the prefix matches; skip it if a longer registry also claims it.
        if paths.iter().any(|p| {
            p.len() > path.len()
                && rest
                    .strip_prefix(p.as_str())
                    .is_some_and(|t| t.starts_with('/'))
        }) {
            continue;
        }
        let values = tag_values(json).with_context(|| format!("data/{file}"))?;
        raw.insert(format!("{namespace}:{tag}"), values);
    }
    Ok(raw)
}

fn tag_values(json: &Value) -> Result<Vec<String>> {
    ensure!(
        json.get("replace").and_then(Value::as_bool) != Some(true),
        "replace is unsupported"
    );
    let values = json
        .get("values")
        .and_then(Value::as_array)
        .context("missing values")?;
    values
        .iter()
        .map(|value| {
            let (id, required) = match value {
                Value::String(id) => (id.as_str(), true),
                Value::Object(entry) => (
                    entry
                        .get("id")
                        .and_then(Value::as_str)
                        .context("tag entry without id")?,
                    entry
                        .get("required")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                ),
                _ => bail!("unexpected tag entry {value}"),
            };
            ensure!(required, "optional tag entries are unsupported: {id}");
            let (prefix, id) = id.strip_prefix('#').map_or(("", id), |id| ("#", id));
            Ok(if id.contains(':') {
                format!("{prefix}{id}")
            } else {
                format!("{prefix}minecraft:{id}")
            })
        })
        .collect()
}

fn resolve<'a>(
    tag: &'a str,
    tags: &'a Tags,
    resolved: &mut Tags,
    stack: &mut Vec<&'a str>,
) -> Result<Vec<String>> {
    if let Some(entries) = resolved.get(tag) {
        return Ok(entries.clone());
    }
    ensure!(!stack.contains(&tag), "tag cycle through {tag}");
    stack.push(tag);
    let mut entries = Vec::new();
    for value in tags
        .get(tag)
        .with_context(|| format!("unknown tag #{tag}"))?
    {
        let expanded = match value.strip_prefix('#') {
            Some(nested) => resolve(nested, tags, resolved, stack)?,
            None => vec![value.clone()],
        };
        for entry in expanded {
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    stack.pop();
    resolved.insert(tag.to_owned(), entries.clone());
    Ok(entries)
}

#[derive(Default)]
struct Module {
    registry: Option<(usize, String, Tags)>,
    children: BTreeMap<String, Module>,
}

impl Module {
    fn emit(&self, out: &mut String, input: &Input) -> Result<()> {
        if let Some((slot, registry, tags)) = &self.registry {
            emit_registry(out, input, *slot, registry, tags)?;
        }
        for (name, child) in &self.children {
            writeln!(out, "pub mod {name} {{")?;
            child.emit(out, input)?;
            writeln!(out, "}}")?;
        }
        Ok(())
    }
}

fn emit_registry(
    out: &mut String,
    input: &Input,
    slot: usize,
    registry: &str,
    tags: &Tags,
) -> Result<()> {
    writeln!(out, "//! Tags of `{registry}`.")?;
    let variants = match input.registries.get(registry) {
        Some(report) => Some(registries::variants(report)?),
        None => None,
    };
    let (entry_type, key_type) = match &variants {
        Some(_) => {
            let path = registries::type_path(registry);
            writeln!(out, "use {path};")?;
            (path.rsplit("::").next().unwrap_or(&path).to_owned(), path)
        }
        None => ("&str".to_owned(), "&'static str".to_owned()),
    };
    let entry = |name: &str| -> Result<String> {
        Ok(match &variants {
            Some(variants) => {
                let variant = variants
                    .get(name)
                    .with_context(|| format!("{name} is not in {registry}"))?;
                format!("{entry_type}::{variant}")
            }
            None => str_literal(name),
        })
    };
    let consts = names::unique(tags.keys().map(String::as_str), shouty)?;
    for ((tag, entries), name) in tags.iter().zip(&consts) {
        let entries = entries
            .iter()
            .map(|e| entry(e))
            .collect::<Result<Vec<_>>>()?;
        writeln!(out, "/// `#{tag}`")?;
        writeln!(
            out,
            "pub const {name}: &[{entry_type}] = &[{}];",
            entries.join(", ")
        )?;
    }
    writeln!(out, "/// Every tag, keyed by resource location.")?;
    writeln!(out, "pub const ALL: &[(&str, &[{entry_type}])] = &[")?;
    for (tag, name) in tags.keys().zip(&consts) {
        writeln!(out, "    ({}, {name}),", str_literal(tag))?;
    }
    writeln!(out, "];")?;
    writeln!(out, "/// Every tag as a [`crate::tag::Key`].")?;
    writeln!(out, "pub mod key {{")?;
    writeln!(out, "use crate::tag::Key;")?;
    for (index, (tag, name)) in tags.keys().zip(&consts).enumerate() {
        writeln!(
            out,
            "pub const {name}: Key<{key_type}> = Key {{ registry: {slot}, index: {index}, name: {}, members: super::{name} }};",
            str_literal(tag)
        )?;
    }
    writeln!(out, "}}")?;
    Ok(())
}

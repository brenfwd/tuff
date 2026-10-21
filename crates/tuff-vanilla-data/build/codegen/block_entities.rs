//! Block entity data from `build/BlockEntityProperties.java`: the block
//! entity type of every block that has one, and per type the saved and
//! client data of a new block entity, its item slots, whether clients get
//! its data on block changes, whether only operators may place it with
//! block entity data, and the item components it takes over; plus the
//! items that change sign text when used on a sign. Validated
//! strictly: every registered block entity type must have a block, and
//! every type some block creates must have its new data.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::registries::{self, sorted_entries};
use crate::input::{Input, RegistryReport};
use crate::names::str_literal;

#[derive(Default)]
struct TypeData<'a> {
    blocks: Vec<&'a str>,
    default: Option<&'a str>,
    client: Option<&'a str>,
    size: Option<u16>,
    updates: bool,
    op_only: bool,
    implicit: Vec<&'a str>,
    discard: Vec<&'a str>,
}

fn registry<'a>(input: &'a Input, key: &str) -> Result<&'a RegistryReport> {
    input
        .registries
        .get(key)
        .with_context(|| format!("registries.json has no {key}"))
}

fn snbt(value: &str, what: &str) -> Result<()> {
    ensure!(
        value.starts_with('{') && value.ends_with('}'),
        "block entity properties: {what} is not a compound: {value}"
    );
    Ok(())
}

pub fn generate(input: &Input) -> Result<String> {
    let block_registry = registry(input, "minecraft:block")?;
    let type_registry = registry(input, "minecraft:block_entity_type")?;
    let component_registry = registry(input, "minecraft:data_component_type")?;
    let blocks = registries::variants(block_registry)?;
    let types = registries::variants(type_registry)?;
    let components = registries::variants(component_registry)?;

    let mut data: BTreeMap<&str, TypeData> = BTreeMap::new();
    let mut block_types: BTreeMap<&str, (&str, bool)> = BTreeMap::new();
    let mut keeps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut applicators: Vec<&str> = Vec::new();
    let items = registries::variants(registry(input, "minecraft:item")?)?;
    for line in input.block_entity_properties.lines() {
        let mut parts = line.splitn(3, ' ');
        let (kind, name, rest) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next(),
        );
        if kind == "block" {
            let rest = rest.context("block entity properties: block line without a type")?;
            let (entity, created) = rest
                .split_once(' ')
                .context("block entity properties: block line without a created flag")?;
            ensure!(
                blocks.contains_key(name),
                "block entity properties: unknown block {name}"
            );
            ensure!(
                types.contains_key(entity),
                "block entity properties: {name} has unknown type {entity}"
            );
            let created = match created {
                "0" => false,
                "1" => true,
                other => bail!("block entity properties: bad created flag {other:?}"),
            };
            ensure!(
                block_types.insert(name, (entity, created)).is_none(),
                "block entity properties: {name} listed twice"
            );
            data.entry(entity).or_default().blocks.push(name);
            continue;
        }
        if kind == "sign_applicator" {
            ensure!(
                rest.is_none() && items.contains_key(name),
                "block entity properties: bad sign applicator line {line:?}"
            );
            applicators.push(name);
            continue;
        }
        if kind == "keep" {
            let old = rest.context("block entity properties: keep line without an old block")?;
            keeps.entry(name).or_default().push(old);
            continue;
        }
        ensure!(
            types.contains_key(name),
            "block entity properties: unknown type {name} in {line:?}"
        );
        let entry = data.entry(name).or_default();
        match (kind, rest) {
            ("default", Some(value)) => {
                snbt(value, "default")?;
                ensure!(entry.default.is_none(), "{name}: two defaults");
                entry.default = Some(value);
            }
            ("update", Some(value)) => {
                snbt(value, "update")?;
                ensure!(entry.client.is_none(), "{name}: two updates");
                entry.client = Some(value);
            }
            ("size", Some(value)) => entry.size = Some(value.parse()?),
            ("updates", None) => entry.updates = true,
            ("op_only", None) => entry.op_only = true,
            ("implicit", Some(component)) => {
                ensure!(
                    components.contains_key(component),
                    "block entity properties: {name} takes unknown component {component}"
                );
                entry.implicit.push(component);
            }
            ("discard", Some(key)) => entry.discard.push(key),
            _ => bail!("block entity properties: unknown line {line:?}"),
        }
    }

    let type_entries = sorted_entries(type_registry)?;
    for name in &type_entries {
        let entry = data
            .get(name)
            .with_context(|| format!("block entity properties: no block has type {name}"))?;
        ensure!(
            !entry.blocks.is_empty(),
            "block entity properties: no block has type {name}"
        );
        let created = entry.blocks.iter().any(|block| block_types[block].1);
        ensure!(
            entry.default.is_some() == created && entry.client.is_some() == created,
            "block entity properties: {name} new data does not match whether blocks create it"
        );
    }
    ensure!(
        data.len() == type_entries.len(),
        "block entity properties: {} types dumped, {} registered",
        data.len(),
        type_entries.len()
    );

    for (block, olds) in &keeps {
        for old in olds.iter().chain([block]) {
            let (entity, _) = block_types.get(old).with_context(|| {
                format!("block entity properties: {block} keeps the block entity of {old}, which has none")
            })?;
            ensure!(
                *entity == block_types[block].0,
                "block entity properties: {block} keeps a block entity of another type from {old}"
            );
        }
    }

    let mut out = String::new();
    let block_entries = sorted_entries(block_registry)?;
    writeln!(
        out,
        "static BLOCK_ENTITY_TYPES: [Option<(BlockEntityType, bool)>; {}] = [",
        block_entries.len()
    )?;
    for block in &block_entries {
        match block_types.get(block) {
            Some((entity, created)) => writeln!(
                out,
                "    Some((BlockEntityType::{}, {created})),",
                types[entity]
            )?,
            None => writeln!(out, "    None,")?,
        }
    }
    writeln!(out, "];")?;
    ensure!(
        !applicators.is_empty(),
        "block entity properties: no sign applicators"
    );
    let applicators: Vec<String> = applicators
        .iter()
        .map(|item| format!("Item::{}", items[item]))
        .collect();
    writeln!(
        out,
        "static SIGN_APPLICATORS: [Item; {}] = [{}];",
        applicators.len(),
        applicators.join(", ")
    )?;
    writeln!(out, "static KEEPS: [&[Block]; {}] = [", block_entries.len())?;
    for block in &block_entries {
        let olds: Vec<String> = keeps
            .get(block)
            .into_iter()
            .flatten()
            .map(|old| format!("Block::{}", blocks[old]))
            .collect();
        writeln!(out, "    &[{}],", olds.join(", "))?;
    }
    writeln!(out, "];")?;

    writeln!(
        out,
        "static TYPE_DATA: [TypeData; {}] = [",
        type_entries.len()
    )?;
    for name in &type_entries {
        let entry = &data[name];
        let option = |value: Option<&str>| match value {
            Some(value) => format!("Some({})", str_literal(value)),
            None => "None".to_string(),
        };
        let size = match entry.size {
            Some(size) => format!("Some({size})"),
            None => "None".to_string(),
        };
        let blocks_list: Vec<String> = entry
            .blocks
            .iter()
            .map(|block| format!("Block::{}", blocks[block]))
            .collect();
        let implicit: Vec<String> = entry
            .implicit
            .iter()
            .map(|component| format!("DataComponentType::{}", components[component]))
            .collect();
        let discard: Vec<String> = entry.discard.iter().map(|key| str_literal(key)).collect();
        writeln!(
            out,
            "    TypeData {{ blocks: &[{}], default: {}, client: {}, size: {size}, updates: {}, op_only: {}, implicit: &[{}], discard: &[{}] }},",
            blocks_list.join(", "),
            option(entry.default),
            option(entry.client),
            entry.updates,
            entry.op_only,
            implicit.join(", "),
            discard.join(", "),
        )?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

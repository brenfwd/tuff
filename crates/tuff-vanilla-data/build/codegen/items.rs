//! Per-item values from each item's default data components.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use super::array_body;
use super::registries::{self, sorted_entries};
use super::tags::{Tags, resolved_registry_tags};
use crate::input::Input;

const TOOL_FIELDS: [&str; 4] = [
    "rules",
    "default_mining_speed",
    "damage_per_block",
    "can_destroy_blocks_in_creative",
];
const RULE_FIELDS: [&str; 3] = ["blocks", "speed", "correct_for_drops"];
const NO_TOOL: usize = u8::MAX as usize;

pub fn generate(input: &Input) -> Result<String> {
    let registry = &input.registries["minecraft:item"];
    let items = sorted_entries(registry)?;
    let mut max_stack_size = Vec::new();
    let mut max_damage = Vec::new();
    let mut tools = Tools::new(input)?;
    let mut item_tool = Vec::new();
    for item in &items {
        let components = input
            .item_components
            .get(*item)
            .with_context(|| format!("no component report for {item}"))?;
        let number = |key: &str| components.get(key).and_then(|v| v.as_u64());
        max_stack_size.push(
            number("minecraft:max_stack_size")
                .with_context(|| format!("{item} has no max_stack_size"))?,
        );
        max_damage.push(number("minecraft:max_damage").unwrap_or(0));
        item_tool.push(match components.get("minecraft:tool") {
            Some(tool) => tools.add(tool).with_context(|| format!("{item}'s tool"))?,
            None => NO_TOOL,
        });
    }

    let mut out = String::new();
    write!(out, "static MAX_STACK_SIZE: [u8; {}] = [", items.len())?;
    array_body(&mut out, max_stack_size);
    writeln!(out, "];")?;
    write!(out, "static MAX_DAMAGE: [u32; {}] = [", items.len())?;
    array_body(&mut out, max_damage);
    writeln!(out, "];")?;
    tools.emit(&mut out)?;
    write!(out, "static ITEM_TOOL: [u8; {}] = [", items.len())?;
    array_body(&mut out, item_tool);
    writeln!(out, "];")?;
    Ok(out)
}

/// Distinct `minecraft:tool` components, with each rule's block set resolved
/// through tags and sorted by block ID so the runtime can binary-search it.
struct Tools<'a> {
    block_ids: HashMap<&'a str, u32>,
    block_variants: BTreeMap<&'a str, String>,
    block_tags: Tags,
    block_tag_consts: HashMap<String, String>,
    block_sets: Vec<Vec<&'a str>>,
    block_set_ids: HashMap<Vec<&'a str>, usize>,
    tools: Vec<String>,
    tool_ids: HashMap<String, usize>,
}

impl<'a> Tools<'a> {
    fn new(input: &'a Input) -> Result<Self> {
        let registry = &input.registries["minecraft:block"];
        Ok(Self {
            block_ids: registry
                .entries
                .iter()
                .map(|(name, id)| (name.as_str(), id.protocol_id))
                .collect(),
            block_variants: registries::variants(registry)?,
            block_tag_consts: {
                let tags = resolved_registry_tags(input, "minecraft:block")?;
                let consts =
                    crate::names::unique(tags.keys().map(String::as_str), crate::names::shouty)?;
                tags.into_keys().zip(consts).collect()
            },
            block_tags: resolved_registry_tags(input, "minecraft:block")?,
            block_sets: Vec::new(),
            block_set_ids: HashMap::new(),
            tools: Vec::new(),
            tool_ids: HashMap::new(),
        })
    }

    fn add(&mut self, tool: &Value) -> Result<usize> {
        let tool = fields(tool, &TOOL_FIELDS)?;
        let default_mining_speed = match tool.get("default_mining_speed") {
            Some(speed) => float(speed)?,
            None => "1.0_f32".to_owned(),
        };
        let damage_per_block = match tool.get("damage_per_block") {
            Some(damage) => damage.as_u64().context("bad damage_per_block")?,
            None => 1,
        };
        let can_destroy_blocks_in_creative = match tool.get("can_destroy_blocks_in_creative") {
            Some(value) => value
                .as_bool()
                .context("bad can_destroy_blocks_in_creative")?,
            None => true,
        };
        let mut rules = Vec::new();
        for rule in tool
            .get("rules")
            .and_then(Value::as_array)
            .context("tool without rules")?
        {
            let rule = fields(rule, &RULE_FIELDS)?;
            let reference = rule.get("blocks").context("rule without blocks")?;
            let blocks = self.block_set(reference)?;
            let tag = match reference.as_str().and_then(|r| r.strip_prefix('#')) {
                Some(tag) => format!(
                    "Some(crate::tag::block::key::{})",
                    self.block_tag_consts
                        .get(tag)
                        .with_context(|| format!("unknown block tag #{tag}"))?
                ),
                None => "None".to_owned(),
            };
            let speed = match rule.get("speed") {
                Some(speed) => format!("Some({})", float(speed)?),
                None => "None".to_owned(),
            };
            let correct_for_drops = match rule.get("correct_for_drops") {
                Some(value) => format!(
                    "Some({})",
                    value.as_bool().context("bad correct_for_drops")?
                ),
                None => "None".to_owned(),
            };
            rules.push(format!(
                "ToolRule {{ blocks: &TOOL_BLOCKS_{blocks}, tag: {tag}, speed: {speed}, \
                 correct_for_drops: {correct_for_drops} }}"
            ));
        }
        let code = format!(
            "Tool {{ rules: &[{}], default_mining_speed: {default_mining_speed}, \
             damage_per_block: {damage_per_block}, \
             can_destroy_blocks_in_creative: {can_destroy_blocks_in_creative} }}",
            rules.join(", ")
        );
        let next = self.tools.len();
        let id = *self.tool_ids.entry(code.clone()).or_insert(next);
        if id == next {
            ensure!(id < NO_TOOL, "too many distinct tools");
            self.tools.push(code);
        }
        Ok(id)
    }

    fn block_set(&mut self, blocks: &Value) -> Result<usize> {
        let references: Vec<&str> = match blocks {
            Value::String(reference) => vec![reference],
            Value::Array(list) => list
                .iter()
                .map(|v| v.as_str().context("non-string block in list"))
                .collect::<Result<_>>()?,
            _ => bail!("bad blocks {blocks}"),
        };
        let mut set = BTreeSet::new();
        for reference in references {
            let names: Vec<&str> = match reference.strip_prefix('#') {
                Some(tag) => self
                    .block_tags
                    .get(tag)
                    .with_context(|| format!("unknown block tag {reference}"))?
                    .iter()
                    .map(String::as_str)
                    .collect(),
                None => vec![reference],
            };
            for name in names {
                let (&name, &id) = self
                    .block_ids
                    .get_key_value(name)
                    .with_context(|| format!("unknown block {name}"))?;
                set.insert((id, name));
            }
        }
        let set: Vec<&str> = set.into_iter().map(|(_, name)| name).collect();
        let next = self.block_sets.len();
        let id = *self.block_set_ids.entry(set.clone()).or_insert(next);
        if id == next {
            self.block_sets.push(set);
        }
        Ok(id)
    }

    fn emit(&self, out: &mut String) -> Result<()> {
        for (id, set) in self.block_sets.iter().enumerate() {
            let blocks: Vec<String> = set
                .iter()
                .map(|name| format!("Block::{}", self.block_variants[name]))
                .collect();
            writeln!(
                out,
                "static TOOL_BLOCKS_{id}: [Block; {}] = [{}];",
                blocks.len(),
                blocks.join(", ")
            )?;
        }
        writeln!(out, "static TOOLS: [Tool; {}] = [", self.tools.len())?;
        for tool in &self.tools {
            writeln!(out, "    {tool},")?;
        }
        writeln!(out, "];")?;
        Ok(())
    }
}

fn fields<'v>(value: &'v Value, known: &[&str]) -> Result<&'v Map<String, Value>> {
    let object = value
        .as_object()
        .with_context(|| format!("expected an object, got {value}"))?;
    for key in object.keys() {
        ensure!(known.contains(&key.as_str()), "unknown field {key:?}");
    }
    Ok(object)
}

/// A float literal for the value as the game reads it (single precision).
fn float(value: &Value) -> Result<String> {
    let value = value
        .as_f64()
        .with_context(|| format!("expected a number, got {value}"))? as f32;
    ensure!(value.is_finite(), "non-finite float {value}");
    Ok(format!("{value:?}_f32"))
}

//! Attribute data from `build/AttributeProperties.java`: each attribute's
//! default and range, the base values of every living entity type, and the
//! modifiers mob effects apply. Validated strictly: every registered
//! attribute must appear exactly once and every name must resolve.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::registries::{sorted_entries, variants};
use crate::input::Input;
use crate::names::{pascal, str_literal};

use super::components::{OPERATIONS, SLOT_GROUPS};

fn number(value: &str) -> Result<f64> {
    let parsed: f64 = value
        .parse()
        .with_context(|| format!("attribute properties: bad number {value:?}"))?;
    ensure!(
        parsed.is_finite(),
        "attribute properties: non-finite {value}"
    );
    Ok(parsed)
}

fn ids<'a>(input: &'a Input, registry: &str) -> Result<HashMap<&'a str, usize>> {
    let entries = sorted_entries(&input.registries[registry])?;
    Ok(entries
        .into_iter()
        .enumerate()
        .map(|(id, name)| (name, id))
        .collect())
}

pub fn generate(input: &Input) -> Result<String> {
    let attributes = ids(input, "minecraft:attribute")?;
    let entity_types = ids(input, "minecraft:entity_type")?;
    let effects = ids(input, "minecraft:mob_effect")?;
    let attribute_variants = variants(&input.registries["minecraft:attribute"])?;
    let attribute_variant = |name: &str| -> Result<&String> {
        attribute_variants
            .get(name)
            .with_context(|| format!("attribute properties: unknown attribute {name}"))
    };
    let resolve = |table: &HashMap<&str, usize>, kind: &str, name: &str| -> Result<usize> {
        table
            .get(name)
            .copied()
            .with_context(|| format!("attribute properties: unknown {kind} {name}"))
    };

    let mut ranges: BTreeMap<usize, (f64, f64, f64)> = BTreeMap::new();
    let mut bases: BTreeMap<usize, BTreeMap<usize, f64>> = BTreeMap::new();
    let mut modifiers: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut networks: BTreeMap<&str, BTreeMap<u32, &str>> = BTreeMap::new();
    for line in input.attribute_properties.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["attribute", name, default, min, max] => {
                let id = resolve(&attributes, "attribute", name)?;
                let (default, min, max) = (number(default)?, number(min)?, number(max)?);
                ensure!(
                    min <= default && default <= max,
                    "attribute properties: {name} default outside its range"
                );
                ensure!(
                    ranges.insert(id, (default, min, max)).is_none(),
                    "attribute properties: {name} listed twice"
                );
            }
            ["base", entity, attribute, value] => {
                let entity = resolve(&entity_types, "entity type", entity)?;
                let attribute_id = resolve(&attributes, "attribute", attribute)?;
                ensure!(
                    bases
                        .entry(entity)
                        .or_default()
                        .insert(attribute_id, number(value)?)
                        .is_none(),
                    "attribute properties: {attribute} listed twice for one entity"
                );
            }
            ["effect", effect, attribute, id, amount, operation] => {
                let effect = resolve(&effects, "mob effect", effect)?;
                let variant = attribute_variant(attribute)?;
                ensure!(
                    OPERATIONS.contains(operation),
                    "attribute properties: unknown operation {operation}"
                );
                modifiers.entry(effect).or_default().push(format!(
                    "EffectModifier {{ attribute: Attribute::{}, id: {}, amount: {:?}, \
                     operation: ModifierOperation::{} }}",
                    variant,
                    str_literal(id),
                    number(amount)?,
                    pascal(operation),
                ));
            }
            ["network", kind, name, id] => {
                ensure!(
                    networks
                        .entry(kind)
                        .or_default()
                        .insert(id.parse()?, name)
                        .is_none(),
                    "attribute properties: {kind} id {id} listed twice"
                );
            }
            _ => bail!("attribute properties: unknown line {line:?}"),
        }
    }
    ensure!(
        ranges.len() == attributes.len(),
        "attribute properties: {} attributes dumped, {} registered",
        ranges.len(),
        attributes.len()
    );
    ensure!(
        bases.contains_key(&entity_types["minecraft:player"]),
        "attribute properties: no player attributes"
    );

    let by_id = |kind: &str| -> Result<Vec<&str>> {
        let ids = networks
            .get(kind)
            .with_context(|| format!("attribute properties: no {kind} ids"))?;
        ensure!(
            ids.keys().copied().eq(0..ids.len() as u32),
            "attribute properties: {kind} ids are not contiguous"
        );
        Ok(ids.values().copied().collect())
    };
    ensure!(
        by_id("slot_group")? == SLOT_GROUPS,
        "attribute properties: slot group network ids changed"
    );
    ensure!(
        by_id("operation")? == OPERATIONS,
        "attribute properties: operation network ids changed"
    );
    let displays = by_id("display")?;

    let mut out = String::new();
    writeln!(
        out,
        "/// Attribute modifier display types, by network id.\npub const MODIFIER_DISPLAYS: [&str; {}] = [{}];",
        displays.len(),
        displays
            .iter()
            .map(|name| str_literal(name))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    writeln!(out, "static RANGES: [AttributeRange; {}] = [", ranges.len())?;
    for (default, min, max) in ranges.values() {
        writeln!(
            out,
            "    AttributeRange {{ default: {default:?}, min: {min:?}, max: {max:?} }},"
        )?;
    }
    writeln!(out, "];")?;

    let attribute_names = sorted_entries(&input.registries["minecraft:attribute"])?;
    let attribute_names: Vec<&String> = attribute_names
        .into_iter()
        .map(attribute_variant)
        .collect::<Result<_>>()?;
    let entity_names = sorted_entries(&input.registries["minecraft:entity_type"])?;
    writeln!(
        out,
        "static BASES: [&[(Attribute, f64)]; {}] = [",
        entity_names.len()
    )?;
    for id in 0..entity_names.len() {
        let values = bases.get(&id).map_or(Vec::new(), |values| {
            values
                .iter()
                .map(|(attribute, value)| {
                    format!("(Attribute::{}, {value:?})", attribute_names[*attribute])
                })
                .collect()
        });
        writeln!(out, "    &[{}],", values.join(", "))?;
    }
    writeln!(out, "];")?;

    writeln!(
        out,
        "static EFFECT_MODIFIERS: [&[EffectModifier]; {}] = [",
        effects.len()
    )?;
    for id in 0..effects.len() {
        let list = modifiers
            .get(&id)
            .map_or(String::new(), |list| list.join(", "));
        writeln!(out, "    &[{list}],")?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

//! Item data from `build/ItemProperties.java`: crafting remainders, dye
//! colors, banner colors and firework shapes, validated strictly so a changed
//! dump format fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::{array_body, registries};
use crate::input::Input;
use crate::names::{pascal, str_literal};

struct Dye {
    name: String,
    diffuse: i32,
    firework: i32,
}

pub fn generate(input: &Input) -> Result<String> {
    let registry = &input.registries["minecraft:item"];
    let items = registries::variants(registry)?;
    let order = registries::sorted_entries(registry)?;
    let index: BTreeMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, name)| (*name, i))
        .collect();

    let mut remainders: Vec<Option<(String, u8)>> = vec![None; order.len()];
    let mut dyes: Vec<Dye> = Vec::new();
    let mut banners: Vec<Option<String>> = vec![None; order.len()];
    let mut shapes: Vec<String> = Vec::new();
    let blocks = registries::variants(&input.registries["minecraft:block"])?;
    let sounds = registries::variants(&input.registries["minecraft:sound_event"])?;
    let block = |name: &str| -> Result<String> {
        blocks
            .get(name)
            .map(|variant| format!("Block::{variant}"))
            .with_context(|| format!("item properties: unknown block {name}"))
    };
    let block_order: BTreeMap<&str, usize> =
        registries::sorted_entries(&input.registries["minecraft:block"])?
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, i))
            .collect();
    let mut waxables: BTreeMap<usize, (String, String)> = BTreeMap::new();
    let mut buckets: BTreeMap<usize, String> = BTreeMap::new();
    let mut boats: BTreeMap<usize, String> = BTreeMap::new();
    let stat_types = registries::variants(&input.registries["minecraft:stat_type"])?;
    let mut stat_registries: BTreeMap<String, String> = BTreeMap::new();
    let entity_types = registries::variants(&input.registries["minecraft:entity_type"])?;
    for line in input.item_properties.lines() {
        let values: Vec<&str> = line.split(' ').collect();
        match values.as_slice() {
            ["remainder", item, remainder, count] => {
                let slot = *index
                    .get(item)
                    .with_context(|| format!("item properties: unknown item {item}"))?;
                ensure!(
                    items.contains_key(*remainder),
                    "item properties: unknown remainder {remainder}"
                );
                let count: u8 = count.parse()?;
                ensure!(
                    count >= 1,
                    "item properties: bad remainder count in {line:?}"
                );
                ensure!(
                    remainders[slot].is_none(),
                    "item properties: {item} has two remainders"
                );
                remainders[slot] = Some((remainder.to_string(), count));
            }
            ["dye", name, id, diffuse, firework] => {
                let id: usize = id.parse()?;
                ensure!(id == dyes.len(), "item properties: dye {name} out of order");
                dyes.push(Dye {
                    name: name.to_string(),
                    diffuse: diffuse.parse()?,
                    firework: firework.parse()?,
                });
            }
            ["banner", item, color] => {
                let slot = *index
                    .get(item)
                    .with_context(|| format!("item properties: unknown item {item}"))?;
                banners[slot] = Some(color.to_string());
            }
            ["firework_shape", name, id] => {
                let id: usize = id.parse()?;
                ensure!(
                    id == shapes.len(),
                    "item properties: shape {name} out of order"
                );
                shapes.push(name.to_string());
            }
            ["wax", from, to] => {
                let position = *block_order
                    .get(from)
                    .with_context(|| format!("item properties: unknown block {from}"))?;
                ensure!(
                    waxables
                        .insert(position, (block(from)?, block(to)?))
                        .is_none(),
                    "item properties: a block waxes two ways"
                );
            }
            ["bucket", item, fluid, holds_mob, mob_type] => {
                let slot = *index
                    .get(item)
                    .with_context(|| format!("item properties: unknown item {item}"))?;
                let fluid = match *fluid {
                    "-" => "None".to_string(),
                    fluid => format!("Some({})", block(fluid)?),
                };
                let holds_mob = match *holds_mob {
                    "0" => false,
                    "1" => true,
                    _ => bail!("item properties: bad bucket line {line:?}"),
                };
                let mob = match (*mob_type, holds_mob) {
                    ("-", false) => "None".to_string(),
                    (kind, true) => {
                        let kind = entity_types.get(kind).with_context(|| {
                            format!("item properties: unknown entity type {kind}")
                        })?;
                        format!("Some(crate::registry::EntityType::{kind})")
                    }
                    (kind, false) => {
                        bail!("item properties: bucket line has entity type {kind} but no mob flag")
                    }
                };
                buckets.insert(
                    slot,
                    format!("Bucket::Fluid {{ fluid: {fluid}, mob: {mob} }}"),
                );
            }
            ["stat_type", kind, registry] => {
                let variant = stat_types
                    .get(*kind)
                    .with_context(|| format!("item properties: unknown stat type {kind}"))?;
                let registry = match *registry {
                    "minecraft:block" => "Block",
                    "minecraft:item" => "Item",
                    "minecraft:entity_type" => "EntityType",
                    "minecraft:custom_stat" => "CustomStat",
                    other => bail!("item properties: stat type {kind} counts {other}"),
                };
                ensure!(
                    stat_registries
                        .insert(variant.clone(), registry.to_owned())
                        .is_none(),
                    "item properties: stat type {kind} twice"
                );
            }
            ["boat", item, kind] => {
                let slot = *index
                    .get(item)
                    .with_context(|| format!("item properties: unknown item {item}"))?;
                let kind = entity_types
                    .get(*kind)
                    .with_context(|| format!("item properties: unknown entity type {kind}"))?;
                ensure!(
                    boats
                        .insert(slot, format!("crate::registry::EntityType::{kind}"))
                        .is_none(),
                    "item properties: {item} places two boats"
                );
            }
            ["solid_bucket", item, placed, sound] => {
                let slot = *index
                    .get(item)
                    .with_context(|| format!("item properties: unknown item {item}"))?;
                let sound = sounds
                    .get(*sound)
                    .with_context(|| format!("item properties: unknown sound {sound}"))?;
                buckets.insert(
                    slot,
                    format!(
                        "Bucket::Solid {{ block: {}, place_sound: crate::registry::SoundEvent::{sound} }}",
                        block(placed)?
                    ),
                );
            }
            _ => bail!("item properties: bad line {line:?}"),
        }
    }
    ensure!(!waxables.is_empty(), "item properties: nothing waxes");
    ensure!(
        buckets.values().any(|b| b.contains("fluid: None")),
        "item properties: no empty bucket"
    );
    ensure!(!boats.is_empty(), "item properties: no boats");
    ensure!(
        stat_registries.len() == stat_types.len(),
        "item properties: {} of {} stat types listed",
        stat_registries.len(),
        stat_types.len()
    );
    ensure!(!dyes.is_empty(), "item properties: no dye colors");
    ensure!(!shapes.is_empty(), "item properties: no firework shapes");
    ensure!(
        remainders.iter().any(Option::is_some),
        "item properties: no crafting remainders"
    );
    for color in banners.iter().flatten() {
        ensure!(
            dyes.iter().any(|dye| dye.name == *color),
            "item properties: unknown banner color {color}"
        );
    }

    let mut out = String::new();
    write!(
        out,
        "static WAXABLES: [(Block, Block); {}] = [",
        waxables.len()
    )?;
    array_body(
        &mut out,
        waxables
            .values()
            .map(|(from, to)| format!("({from}, {to})")),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_BUCKET: [Option<Bucket>; {}] = [",
        order.len()
    )?;
    array_body(
        &mut out,
        (0..order.len()).map(|slot| {
            buckets
                .get(&slot)
                .map_or("None".to_string(), |b| format!("Some({b})"))
        }),
    );
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// The registry a statistic type counts entries of.\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum StatRegistry {{ Block, Item, EntityType, CustomStat }}"
    )?;
    writeln!(
        out,
        "impl crate::registry::StatType {{\n    pub const fn registry(self) -> StatRegistry {{\n        match self {{"
    )?;
    for (variant, registry) in &stat_registries {
        writeln!(
            out,
            "            Self::{variant} => StatRegistry::{registry},"
        )?;
    }
    writeln!(out, "        }}\n    }}\n}}")?;
    write!(
        out,
        "static ITEM_BOAT: [Option<crate::registry::EntityType>; {}] = [",
        order.len()
    )?;
    array_body(
        &mut out,
        (0..order.len()).map(|slot| {
            boats
                .get(&slot)
                .map_or("None".to_string(), |b| format!("Some({b})"))
        }),
    );
    writeln!(out, "];")?;
    emit_enum(
        &mut out,
        "DyeColor",
        "A dye color, in the game's ID order.",
        &dyes.iter().map(|dye| dye.name.as_str()).collect::<Vec<_>>(),
    )?;
    writeln!(out, "impl DyeColor {{")?;
    write!(out, "    const DIFFUSE: [i32; {}] = [", dyes.len())?;
    array_body(&mut out, dyes.iter().map(|dye| dye.diffuse));
    writeln!(out, "    ];")?;
    write!(out, "    const FIREWORK: [i32; {}] = [", dyes.len())?;
    array_body(&mut out, dyes.iter().map(|dye| dye.firework));
    writeln!(out, "    ];")?;
    writeln!(
        out,
        "    /// The opaque ARGB color that dyeing mixes in.\n    pub const fn diffuse_color(self) -> i32 {{ Self::DIFFUSE[self as usize] }}"
    )?;
    writeln!(
        out,
        "    /// The RGB color a firework star takes from this dye.\n    pub const fn firework_color(self) -> i32 {{ Self::FIREWORK[self as usize] }}"
    )?;
    writeln!(out, "}}")?;
    emit_enum(
        &mut out,
        "FireworkShape",
        "A firework explosion shape, in the game's ID order.",
        &shapes.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    write!(
        out,
        "static ITEM_CRAFTING_REMAINDER: [Option<(Item, u8)>; {}] = [",
        order.len()
    )?;
    array_body(
        &mut out,
        remainders.iter().map(|remainder| match remainder {
            None => "None".to_string(),
            Some((item, count)) => format!("Some((Item::{}, {count}))", items[item.as_str()]),
        }),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_BANNER_COLOR: [Option<DyeColor>; {}] = [",
        order.len()
    )?;
    array_body(
        &mut out,
        banners.iter().map(|color| match color {
            None => "None".to_string(),
            Some(color) => format!("Some(DyeColor::{})", pascal(color)),
        }),
    );
    writeln!(out, "];")?;
    Ok(out)
}

fn emit_enum(out: &mut String, name: &str, doc: &str, values: &[&str]) -> Result<()> {
    writeln!(out, "#[doc = {}]", str_literal(doc))?;
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n#[repr(u8)]\npub enum {name} {{"
    )?;
    for value in values {
        writeln!(out, "    #[doc = \"`{value}`\"]\n    {},", pascal(value))?;
    }
    writeln!(out, "}}")?;
    writeln!(out, "impl {name} {{")?;
    write!(out, "    pub const ALL: [Self; {}] = [", values.len())?;
    array_body(out, values.iter().map(|v| format!("Self::{}", pascal(v))));
    writeln!(out, "    ];")?;
    write!(out, "    const NAMES: [&'static str; {}] = [", values.len())?;
    array_body(out, values.iter().map(|v| str_literal(v)));
    writeln!(out, "    ];")?;
    writeln!(
        out,
        "    pub const fn id(self) -> u32 {{ self as u32 }}\n    \
         pub fn from_id(id: u32) -> Option<Self> {{ Self::ALL.get(id as usize).copied() }}\n    \
         pub const fn name(self) -> &'static str {{ Self::NAMES[self as usize] }}\n    \
         pub fn from_name(name: &str) -> Option<Self> {{\n        \
         let name = name.strip_prefix(\"minecraft:\").unwrap_or(name);\n        \
         Self::ALL.iter().copied().find(|v| v.name() == name)\n    }}\n}}"
    )?;
    Ok(())
}

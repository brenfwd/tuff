//! Random tick, growth, bone meal and fire data from
//! `build/GrowthProperties.java`, validated strictly so a changed dump
//! format fails the build.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::{array_body, registries};
use crate::input::Input;
use crate::names::{pascal, str_literal};

const NONE: u8 = u8::MAX;
const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

struct Grower {
    name: String,
    shortest: Option<String>,
    trees: Vec<(u32, String)>,
    mega: Vec<(u32, String)>,
    flower: Vec<(u32, String)>,
}

fn state_set(ranges: &str, state_count: usize) -> Result<Vec<u64>> {
    let mut set = vec![0u64; state_count.div_ceil(64)];
    if ranges == "-" {
        return Ok(set);
    }
    for range in ranges.split(',') {
        let (start, end) = range.split_once('-').unwrap_or((range, range));
        let (start, end): (usize, usize) = (start.parse()?, end.parse()?);
        ensure!(
            start <= end && end < state_count,
            "growth properties: bad state range {range}"
        );
        for id in start..=end {
            set[id / 64] |= 1 << (id % 64);
        }
    }
    Ok(set)
}

fn kind_index(kinds: &mut Vec<String>, kind: &str) -> u8 {
    if kind == "-" {
        return NONE;
    }
    match kinds.iter().position(|k| k == kind) {
        Some(index) => index as u8,
        None => {
            kinds.push(kind.to_string());
            (kinds.len() - 1) as u8
        }
    }
}

pub fn generate(input: &Input) -> Result<String> {
    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let block_registry = input
        .registries
        .get("minecraft:block")
        .context("registries.json has no minecraft:block")?;
    let variants = registries::variants(block_registry)?;
    let order = registries::sorted_entries(block_registry)?;
    let block_index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, name)| (*name, i))
        .collect();
    let block = |name: &str| -> Result<String> {
        let variant = variants
            .get(name)
            .with_context(|| format!("growth properties: unknown block {name}"))?;
        Ok(format!("Block::{variant}"))
    };
    let index = |name: &str| -> Result<usize> {
        block_index
            .get(name)
            .copied()
            .with_context(|| format!("growth properties: unknown block {name}"))
    };

    let mut ticking = None;
    let mut fluid_ticking = None;
    let mut lava_ignites = None;
    let mut grown: BTreeMap<usize, String> = BTreeMap::new();
    let mut fires: BTreeMap<usize, f32> = BTreeMap::new();
    let mut tick_kinds: Vec<String> = Vec::new();
    let mut block_tick = vec![NONE; order.len()];
    let mut bonemeal_kinds: Vec<String> = Vec::new();
    let mut block_bonemeal = vec![NONE; order.len()];
    let mut flammable = vec![(0u8, 0u8); order.len()];
    let mut growers: Vec<Grower> = Vec::new();
    let mut saplings: Vec<(usize, String)> = Vec::new();
    let mut features: BTreeMap<usize, String> = BTreeMap::new();
    let mut stems: Vec<[String; 5]> = Vec::new();
    let mut attached: Vec<[String; 3]> = Vec::new();
    let mut growing: Vec<(String, String, &str, f64)> = Vec::new();
    let mut weathering: BTreeMap<usize, (u8, u8, f32, Option<String>)> = BTreeMap::new();
    let mut dead: BTreeMap<usize, String> = BTreeMap::new();

    for line in input.growth_properties.lines() {
        let mut words = line.split(' ');
        let tag = words.next().unwrap_or_default();
        let values: Vec<&str> = words.collect();
        match (tag, values.as_slice()) {
            ("ticking", [ranges]) => ticking = Some(state_set(ranges, state_count)?),
            ("fluid_ticking", [ranges]) => fluid_ticking = Some(state_set(ranges, state_count)?),
            ("lava_ignites", [ranges]) => lava_ignites = Some(state_set(ranges, state_count)?),
            ("fire", [name, damage]) => {
                fires.insert(index(name)?, damage.parse()?);
            }
            ("grown", [name, tall]) => {
                grown.insert(index(name)?, block(tall)?);
            }
            ("tick", [name, kind]) => block_tick[index(name)?] = kind_index(&mut tick_kinds, kind),
            ("bonemeal", [name, kind]) => {
                block_bonemeal[index(name)?] = kind_index(&mut bonemeal_kinds, kind)
            }
            ("flammable", [name, ignite, burn]) => {
                flammable[index(name)?] = (ignite.parse()?, burn.parse()?);
            }
            ("grower", [name, shortest]) => growers.push(Grower {
                name: name.to_string(),
                shortest: (*shortest != "-").then(|| shortest.to_string()),
                trees: Vec::new(),
                mega: Vec::new(),
                flower: Vec::new(),
            }),
            ("grower_tree", [name, list, weight, feature]) => {
                let grower = growers
                    .iter_mut()
                    .find(|g| g.name == *name)
                    .with_context(|| {
                        format!("growth properties: tree before grower in {line:?}")
                    })?;
                let entry = (weight.parse()?, feature.to_string());
                match *list {
                    "trees" => grower.trees.push(entry),
                    "mega" => grower.mega.push(entry),
                    "flower" => grower.flower.push(entry),
                    _ => bail!("growth properties: bad tree list in {line:?}"),
                }
            }
            ("sapling", [name, grower]) => saplings.push((index(name)?, grower.to_string())),
            ("feature", [name, feature]) => {
                features.insert(index(name)?, feature.to_string());
            }
            ("stem", [name, fruit, attached_stem, stem_support, fruit_support]) => {
                ensure!(
                    stem_support.starts_with('#') && fruit_support.starts_with('#'),
                    "growth properties: stem supports are tags in {line:?}"
                );
                stems.push([
                    block(name)?,
                    block(fruit)?,
                    block(attached_stem)?,
                    str_literal(&stem_support[1..]),
                    str_literal(&fruit_support[1..]),
                ]);
            }
            ("attached_stem", [name, stem, fruit]) => {
                attached.push([block(name)?, block(stem)?, block(fruit)?]);
            }
            ("growing_plant", [head, body, direction, chance]) => {
                let direction = DIRECTIONS
                    .iter()
                    .find(|d| *d == direction)
                    .with_context(|| format!("growth properties: bad direction in {line:?}"))?;
                growing.push((block(head)?, block(body)?, direction, chance.parse()?));
            }
            ("weathering", [name, group, age, modifier, next]) => {
                let next = if *next == "-" {
                    None
                } else {
                    Some(block(next)?)
                };
                weathering.insert(
                    index(name)?,
                    (group.parse()?, age.parse()?, modifier.parse()?, next),
                );
            }
            ("dead", [name, dead_block]) => {
                dead.insert(index(name)?, block(dead_block)?);
            }
            _ => bail!("growth properties: unknown line {line:?}"),
        }
    }
    let ticking = ticking.context("growth properties: no ticking line")?;
    let fluid_ticking = fluid_ticking.context("growth properties: no fluid_ticking line")?;
    let lava_ignites = lava_ignites.context("growth properties: no lava_ignites line")?;
    ensure!(!growers.is_empty(), "growth properties: no tree growers");
    ensure!(
        flammable.iter().any(|f| f.0 > 0),
        "growth properties: nothing flammable"
    );
    ensure!(
        tick_kinds.len() < NONE as usize && bonemeal_kinds.len() < NONE as usize,
        "growth properties: too many kinds"
    );
    let mut block_grower = vec![NONE; order.len()];
    for (block_id, name) in &saplings {
        let grower = growers
            .iter()
            .position(|g| &g.name == name)
            .with_context(|| format!("growth properties: unknown grower {name}"))?;
        block_grower[*block_id] = grower as u8;
    }

    let mut out = String::new();
    writeln!(
        out,
        "pub(crate) static RANDOM_TICKING: [u64; {}] = [",
        ticking.len()
    )?;
    array_body(&mut out, &ticking);
    writeln!(out, "];")?;
    writeln!(
        out,
        "pub(crate) static FLUID_RANDOM_TICKING: [u64; {}] = [",
        fluid_ticking.len()
    )?;
    array_body(&mut out, &fluid_ticking);
    writeln!(out, "];")?;
    writeln!(
        out,
        "static LAVA_IGNITES: [u64; {}] = [",
        lava_ignites.len()
    )?;
    array_body(&mut out, &lava_ignites);
    writeln!(out, "];")?;
    writeln!(out, "static GROWN: &[(u32, Block)] = &[")?;
    for (block_id, target) in &grown {
        writeln!(out, "    ({block_id}, {target}),")?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// Random tick kinds, indexed by `BLOCK_TICK`.\npub const TICK_KINDS: &[&str] = &[{}];",
        tick_kinds
            .iter()
            .map(|k| str_literal(k))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    writeln!(out, "static BLOCK_TICK: [u8; {}] = [", order.len())?;
    array_body(&mut out, &block_tick);
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// Bone meal kinds, indexed by `BLOCK_BONEMEAL`.\npub const BONEMEAL_KINDS: &[&str] = &[{}];",
        bonemeal_kinds
            .iter()
            .map(|k| str_literal(k))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    writeln!(out, "static BLOCK_BONEMEAL: [u8; {}] = [", order.len())?;
    array_body(&mut out, &block_bonemeal);
    writeln!(out, "];")?;
    writeln!(out, "static FLAMMABLE: [(u8, u8); {}] = [", order.len())?;
    array_body(
        &mut out,
        flammable
            .iter()
            .map(|(ignite, burn)| format!("({ignite}, {burn})")),
    );
    writeln!(out, "];")?;

    writeln!(out, "pub static GROWERS: &[TreeGrower] = &[")?;
    let list = |entries: &[(u32, String)]| {
        entries
            .iter()
            .map(|(weight, feature)| format!("({}, {weight})", str_literal(feature)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    for grower in &growers {
        writeln!(
            out,
            "    TreeGrower {{ name: {}, shortest: {}, trees: &[{}], mega: &[{}], flower: &[{}] }},",
            str_literal(&grower.name),
            grower
                .shortest
                .as_deref()
                .map_or("None".to_string(), |s| format!("Some({})", str_literal(s))),
            list(&grower.trees),
            list(&grower.mega),
            list(&grower.flower),
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static BLOCK_GROWER: [u8; {}] = [", order.len())?;
    array_body(&mut out, &block_grower);
    writeln!(out, "];")?;

    writeln!(
        out,
        "/// Blocks that grow into a configured feature, by block id.\nstatic FEATURES: &[(u32, &str)] = &["
    )?;
    for (block_id, feature) in &features {
        writeln!(out, "    ({block_id}, {}),", str_literal(feature))?;
    }
    writeln!(out, "];")?;

    writeln!(out, "pub static STEMS: &[Stem] = &[")?;
    for [stem, fruit, attached_stem, stem_support, fruit_support] in &stems {
        writeln!(
            out,
            "    Stem {{ stem: {stem}, fruit: {fruit}, attached: {attached_stem}, stem_support: {stem_support}, fruit_support: {fruit_support} }},"
        )?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// Attached stems as `(attached, stem, fruit)`.\npub static ATTACHED_STEMS: &[(Block, Block, Block)] = &["
    )?;
    for [block, stem, fruit] in &attached {
        writeln!(out, "    ({block}, {stem}, {fruit}),")?;
    }
    writeln!(out, "];")?;

    writeln!(out, "pub static GROWING_PLANTS: &[GrowingPlant] = &[")?;
    for (head, body, direction, chance) in &growing {
        writeln!(
            out,
            "    GrowingPlant {{ head: {head}, body: {body}, direction: crate::block::Facing::{}, chance: {chance:?} }},",
            pascal(direction)
        )?;
    }
    writeln!(out, "];")?;

    writeln!(out, "static WEATHERING: &[(u32, Weathering)] = &[")?;
    for (block_id, (group, age, modifier, next)) in &weathering {
        writeln!(
            out,
            "    ({block_id}, Weathering {{ group: {group}, age: {age}, chance_modifier: {modifier:?}, next: {} }}),",
            next.as_deref()
                .map_or("None".to_string(), |n| format!("Some({n})"))
        )?;
    }
    writeln!(out, "];")?;
    ensure!(!fires.is_empty(), "growth properties: no fire blocks");
    writeln!(out, "static FIRE_DAMAGE: &[(u32, f32)] = &[")?;
    for (block_id, damage) in &fires {
        writeln!(out, "    ({block_id}, {damage:?}),")?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static DEAD: &[(u32, Block)] = &[")?;
    for (block_id, dead_block) in &dead {
        writeln!(out, "    ({block_id}, {dead_block}),")?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

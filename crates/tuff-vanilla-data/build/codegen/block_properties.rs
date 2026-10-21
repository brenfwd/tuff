//! Block and block item data from `build/BlockProperties.java`, validated
//! strictly so a changed dump format fails the build.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::{array_body, registries};
use crate::input::Input;
use crate::names::pascal;

/// Everything about a state other than its tool flag and hardness, deduped
/// because most states share it.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Info {
    sturdy: u32,
    flags: u16,
    full_faces: u16,
    sound: usize,
    shape: usize,
    outline: usize,
}

/// A block's sound events plus volume and pitch, as Rust literals.
type SoundLiteral = ([String; 5], String, String);

struct Toggle {
    open: String,
    close: String,
    by_hand: bool,
}

struct Button {
    ticks: u32,
    press: String,
    release: String,
}

struct Pickup {
    kind: &'static str,
    item: String,
    sound: Option<String>,
}

type PickStateLine = (bool, Vec<String>, Vec<(String, String)>);

const PICKUP_KINDS: [&str; 3] = ["liquid", "waterlogged", "whole"];

pub fn generate(input: &Input) -> Result<String> {
    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let blocks = registries::variants(registry(input, "minecraft:block")?)?;
    let items = registries::variants(registry(input, "minecraft:item")?)?;
    let sound_events = registries::variants(registry(input, "minecraft:sound_event")?)?;
    let block_index: HashMap<&str, usize> =
        registries::sorted_entries(registry(input, "minecraft:block")?)?
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, i))
            .collect();
    let item_order = registries::sorted_entries(registry(input, "minecraft:item")?)?;
    let item_index: HashMap<&str, usize> = item_order
        .iter()
        .enumerate()
        .map(|(i, name)| (*name, i))
        .collect();

    let mut requires_tool = vec![0u64; state_count.div_ceil(64)];
    let mut speeds: Vec<f32> = Vec::new();
    let mut speed_index = Vec::with_capacity(state_count);
    let mut sounds: Vec<SoundLiteral> = Vec::new();
    let instruments: Vec<&str> = input
        .blocks
        .get("minecraft:note_block")
        .and_then(|block| block.properties.get("instrument"))
        .context("block properties: note blocks have no instrument property")?
        .iter()
        .map(String::as_str)
        .collect();
    let mut instrument_info: Vec<Option<(String, bool, bool, bool)>> =
        vec![None; instruments.len()];
    let mut block_resistance: Vec<Option<f32>> = vec![None; block_index.len()];
    let mut block_instrument: Vec<Option<usize>> = vec![None; block_index.len()];
    let mut toggles: BTreeMap<usize, Toggle> = BTreeMap::new();
    let mut buttons: BTreeMap<usize, Button> = BTreeMap::new();
    let mut pickups: BTreeMap<usize, Pickup> = BTreeMap::new();
    let mut beds: BTreeMap<usize, (String, bool)> = BTreeMap::new();
    let sound_variant = |name: &str| -> Result<String> {
        sound_events
            .get(name)
            .map(|variant| format!("crate::registry::SoundEvent::{variant}"))
            .with_context(|| format!("block properties: unknown sound {name}"))
    };
    let block_slot = |name: &str| -> Result<usize> {
        block_index
            .get(name)
            .copied()
            .with_context(|| format!("block properties: unknown block {name}"))
    };
    let mut shapes: Vec<String> = Vec::new();
    let mut infos: Vec<Info> = Vec::new();
    let mut info_index = Vec::with_capacity(state_count);
    let mut block_kinds: Vec<Option<String>> = vec![None; block_index.len()];
    let mut block_loot: Vec<Option<String>> = vec![None; block_index.len()];
    let mut block_experience: Vec<String> = vec!["None".to_string(); block_index.len()];
    let mut kinds_seen = BTreeSet::new();
    let mut seen_blocks = 0;
    let mut grounds: Vec<Vec<u64>> = Vec::new();
    let mut block_ground: Vec<Option<usize>> = vec![None; block_index.len()];
    let mut item_blocks: BTreeMap<usize, (String, Option<(String, String)>)> = BTreeMap::new();

    let mut transforms: Vec<[u32; 4]> = Vec::new();
    let mut block_entities: Vec<Option<(String, bool)>> = vec![None; block_index.len()];
    let mut falling = vec![false; block_index.len()];
    let mut falling_delay = vec![0u8; block_index.len()];
    let mut push = vec!["PushPull"; block_index.len()];
    let mut infested: BTreeMap<usize, usize> = BTreeMap::new();
    let mut bases: BTreeMap<usize, usize> = BTreeMap::new();
    let mut campfires: BTreeMap<usize, u32> = BTreeMap::new();
    let mut offsets: BTreeMap<usize, (f32, f32, bool, bool)> = BTreeMap::new();
    let mut offset_samples: Vec<String> = Vec::new();
    let mut picks: BTreeMap<u32, Option<String>> = BTreeMap::new();
    let mut pick_states: BTreeMap<usize, PickStateLine> = BTreeMap::new();
    let mut count = 0;
    for line in input.block_properties.lines() {
        let (tag, rest) = line
            .split_once(' ')
            .with_context(|| format!("block properties: bad line {line:?}"))?;
        let values: Vec<&str> = rest.split(' ').collect();
        match tag {
            "state" => {
                let [
                    id,
                    requires,
                    speed,
                    sturdy,
                    flags,
                    full_faces,
                    break_sound,
                    step_sound,
                    place_sound,
                    hit_sound,
                    fall_sound,
                    volume,
                    pitch,
                    boxes,
                    outline,
                ] = values.as_slice()
                else {
                    bail!("block properties: bad state line {line:?}");
                };
                let id: usize = id.parse()?;
                ensure!(id == count, "block properties: state {id} out of order");
                ensure!(
                    id < state_count,
                    "block properties: more states than blocks.json's {state_count}"
                );
                match *requires {
                    "1" => requires_tool[id / 64] |= 1 << (id % 64),
                    "0" => {}
                    _ => bail!("block properties: bad flag in {line:?}"),
                }
                let speed: f32 = speed
                    .parse()
                    .with_context(|| format!("block properties: bad destroy speed in {line:?}"))?;
                ensure!(
                    speed == -1.0 || (speed.is_finite() && speed >= 0.0),
                    "block properties: destroy speed out of range in {line:?}"
                );
                speed_index.push(u8::try_from(intern(&mut speeds, speed))?);

                let sturdy: u32 = sturdy.parse()?;
                ensure!(
                    sturdy < 1 << 18,
                    "block properties: bad sturdy faces in {line:?}"
                );
                let flags: u16 = flags.parse()?;
                ensure!(flags < 1 << 15, "block properties: bad flags in {line:?}");
                let full_faces: u16 = full_faces.parse()?;
                ensure!(
                    full_faces < 1 << 14,
                    "block properties: bad full faces in {line:?}"
                );
                let events = [break_sound, step_sound, place_sound, hit_sound, fall_sound]
                    .map(|name| sound_variant(name));
                let [a, b, c, d, e] = events;
                let sound = intern(
                    &mut sounds,
                    (
                        [a?, b?, c?, d?, e?],
                        f32_literal(volume)?,
                        f32_literal(pitch)?,
                    ),
                );
                let shape = intern(&mut shapes, shape_literal(boxes)?);
                let outline = intern(&mut shapes, shape_literal(outline)?);
                let info = Info {
                    sturdy,
                    flags,
                    full_faces,
                    sound,
                    shape,
                    outline,
                };
                info_index.push(u16::try_from(intern(&mut infos, info))?);
                count += 1;
            }
            "block" => {
                let [name, kind, loot, experience, resistance, instrument] = values.as_slice()
                else {
                    bail!("block properties: bad block line {line:?}");
                };
                let index = *block_index
                    .get(name)
                    .with_context(|| format!("block properties: unknown block {name}"))?;
                ensure!(
                    block_kinds[index].is_none(),
                    "block properties: {name} listed twice"
                );
                ensure!(
                    kind.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-'),
                    "block properties: bad kind in {line:?}"
                );
                let kind = match *kind {
                    "-" => "none".to_string(),
                    kind => kind.to_string(),
                };
                kinds_seen.insert(kind.clone());
                block_kinds[index] = Some(kind);
                if *loot != "-" {
                    ensure!(
                        loot.split_once(':')
                            .is_some_and(|(_, path)| path.starts_with("blocks/")),
                        "block properties: bad loot table in {line:?}"
                    );
                    block_loot[index] = Some(loot.to_string());
                }
                block_experience[index] = experience_literal(experience)
                    .with_context(|| format!("block properties: bad experience in {line:?}"))?;
                let resistance: f32 = resistance.parse()?;
                ensure!(
                    resistance.is_finite() && resistance >= 0.0,
                    "block properties: bad explosion resistance in {line:?}"
                );
                block_resistance[index] = Some(resistance);
                block_instrument[index] = Some(
                    instruments
                        .iter()
                        .position(|i| i == instrument)
                        .with_context(|| format!("block properties: bad instrument in {line:?}"))?,
                );
                seen_blocks += 1;
            }
            "falling" => {
                let [name, delay] = values.as_slice() else {
                    bail!("block properties: bad falling line {line:?}");
                };
                let index = *block_index
                    .get(name)
                    .with_context(|| format!("block properties: unknown block {name}"))?;
                falling[index] = true;
                falling_delay[index] = delay
                    .parse()
                    .with_context(|| format!("block properties: bad falling delay in {line:?}"))?;
            }
            "push" => {
                let [name, reaction] = values.as_slice() else {
                    bail!("block properties: bad push line {line:?}");
                };
                let index = *block_index
                    .get(name)
                    .with_context(|| format!("block properties: unknown block {name}"))?;
                push[index] = match *reaction {
                    "push" => "Push",
                    "popped" => "Popped",
                    "immoveable" => "Immovable",
                    "ignore_entity" => "IgnoreEntity",
                    _ => bail!("block properties: unknown push reaction in {line:?}"),
                };
            }
            "pick" => {
                let [id, item] = values.as_slice() else {
                    bail!("block properties: bad pick line {line:?}");
                };
                let id: u32 = id
                    .parse()
                    .with_context(|| format!("block properties: bad pick state in {line:?}"))?;
                ensure!(
                    (id as usize) < state_count,
                    "block properties: bad pick state {id}"
                );
                let item = match *item {
                    "-" => None,
                    name => Some(
                        items
                            .get(name)
                            .with_context(|| format!("block properties: unknown item {name}"))?
                            .clone(),
                    ),
                };
                ensure!(
                    picks.insert(id, item).is_none(),
                    "block properties: state {id} picked twice"
                );
            }
            "pick_state" => {
                let [name, always, properties, prototype] = values.as_slice() else {
                    bail!("block properties: bad pick_state line {line:?}");
                };
                let always = match *always {
                    "0" => false,
                    "1" => true,
                    _ => bail!("block properties: bad pick_state flag in {line:?}"),
                };
                let properties = match *properties {
                    "-" => Vec::new(),
                    list => list.split(',').map(str::to_string).collect(),
                };
                let prototype = match *prototype {
                    "-" => Vec::new(),
                    pairs => pairs
                        .split(',')
                        .map(|pair| {
                            pair.split_once('=')
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                .with_context(|| {
                                    format!("block properties: bad pick_state {line:?}")
                                })
                        })
                        .collect::<Result<_>>()?,
                };
                ensure!(
                    pick_states
                        .insert(block_slot(name)?, (always, properties, prototype))
                        .is_none(),
                    "block properties: {name} listed twice in pick_state"
                );
            }
            "infested" => {
                let [name, host] = values.as_slice() else {
                    bail!("block properties: bad infested line {line:?}");
                };
                ensure!(
                    infested
                        .insert(block_slot(name)?, block_slot(host)?)
                        .is_none(),
                    "block properties: {name} listed twice as infested"
                );
            }
            "base" => {
                let [name, base] = values.as_slice() else {
                    bail!("block properties: bad base line {line:?}");
                };
                ensure!(
                    bases.insert(block_slot(name)?, block_slot(base)?).is_none(),
                    "block properties: {name} listed twice as having a base"
                );
            }
            "offset" => {
                let [name, horizontal, vertical, collision, outline] = values.as_slice() else {
                    bail!("block properties: bad offset line {line:?}");
                };
                let flag = |v: &str| match v {
                    "0" => Ok(false),
                    "1" => Ok(true),
                    _ => bail!("block properties: bad offset flag in {line:?}"),
                };
                let entry = (
                    horizontal.parse()?,
                    vertical.parse()?,
                    flag(collision)?,
                    flag(outline)?,
                );
                ensure!(
                    offsets.insert(block_slot(name)?, entry).is_none(),
                    "block properties: {name} listed twice with an offset"
                );
            }
            "offset_sample" => {
                let [name, x, z, dx, dy, dz] = values.as_slice() else {
                    bail!("block properties: bad offset sample line {line:?}");
                };
                block_slot(name)?;
                let coord = |v: &str| -> Result<f64> { Ok(v.parse()?) };
                offset_samples.push(format!(
                    "(Block::{}, {}i32, {}i32, [{:?}, {:?}, {:?}])",
                    blocks
                        .get(*name)
                        .with_context(|| format!("block properties: unknown block {name}"))?,
                    x.parse::<i32>()?,
                    z.parse::<i32>()?,
                    coord(dx)?,
                    coord(dy)?,
                    coord(dz)?
                ));
            }
            "campfire" => {
                let [name, damage] = values.as_slice() else {
                    bail!("block properties: bad campfire line {line:?}");
                };
                ensure!(
                    campfires
                        .insert(block_slot(name)?, damage.parse()?)
                        .is_none(),
                    "block properties: {name} listed twice as a campfire"
                );
            }
            "entity" => {
                let [name, kind, randomizable] = values.as_slice() else {
                    bail!("block properties: bad entity line {line:?}");
                };
                let index = *block_index
                    .get(name)
                    .with_context(|| format!("block properties: unknown block {name}"))?;
                ensure!(
                    input
                        .registries
                        .get("minecraft:block_entity_type")
                        .is_some_and(|r| r.entries.contains_key(*kind)),
                    "block properties: unknown block entity type {kind}"
                );
                let randomizable = match *randomizable {
                    "1" => true,
                    "0" => false,
                    _ => bail!("block properties: bad entity line {line:?}"),
                };
                block_entities[index] = Some((kind.to_string(), randomizable));
            }
            "transform" => {
                let ids = values
                    .iter()
                    .map(|v| v.parse::<u32>())
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("block properties: bad transform line {line:?}"))?;
                let [id, rotated, left_right, front_back] = ids.as_slice() else {
                    bail!("block properties: bad transform line {line:?}");
                };
                ensure!(
                    [id, rotated, left_right, front_back]
                        .iter()
                        .all(|&&v| (v as usize) < state_count),
                    "block properties: transform out of range in {line:?}"
                );
                ensure!(
                    transforms.last().is_none_or(|last| last[0] < *id),
                    "block properties: transforms out of order"
                );
                transforms.push([*id, *rotated, *left_right, *front_back]);
            }
            "ground" => {
                let [name, ranges] = values.as_slice() else {
                    bail!("block properties: bad ground line {line:?}");
                };
                let index = *block_index
                    .get(name)
                    .with_context(|| format!("block properties: unknown block {name}"))?;
                let mut set = vec![0u64; state_count.div_ceil(64)];
                if *ranges != "-" {
                    for range in ranges.split(',') {
                        let (start, end) = range.split_once('-').unwrap_or((range, range));
                        let (start, end): (usize, usize) = (start.parse()?, end.parse()?);
                        ensure!(
                            start <= end && end < state_count,
                            "block properties: bad ground range {range}"
                        );
                        for id in start..=end {
                            set[id / 64] |= 1 << (id % 64);
                        }
                    }
                }
                block_ground[index] = Some(intern(&mut grounds, set));
            }
            "item" => {
                let (item, block, wall) = match values.as_slice() {
                    [item, block] => (item, block, None),
                    [item, block, wall, attach] => {
                        ensure!(
                            ["down", "up", "north", "south", "west", "east"].contains(attach),
                            "block properties: bad attachment in {line:?}"
                        );
                        (item, block, Some((wall, attach)))
                    }
                    _ => bail!("block properties: bad item line {line:?}"),
                };
                let index = *item_index
                    .get(item)
                    .with_context(|| format!("block properties: unknown item {item}"))?;
                for name in std::iter::once(block).chain(wall.map(|(w, _)| w)) {
                    ensure!(
                        blocks.contains_key(*name),
                        "block properties: unknown block {name}"
                    );
                }
                item_blocks.insert(
                    index,
                    (
                        block.to_string(),
                        wall.map(|(w, a)| (w.to_string(), a.to_string())),
                    ),
                );
            }
            "instrument" => {
                let [name, sound, tunable, above, custom] = values.as_slice() else {
                    bail!("block properties: bad instrument line {line:?}");
                };
                let index = instruments
                    .iter()
                    .position(|i| i == name)
                    .with_context(|| format!("block properties: unknown instrument {name}"))?;
                ensure!(
                    instrument_info[index].is_none(),
                    "block properties: instrument {name} listed twice"
                );
                instrument_info[index] = Some((
                    sound_variant(sound)?,
                    flag(tunable)?,
                    flag(above)?,
                    flag(custom)?,
                ));
            }
            "toggle" => {
                let [name, open, close, by_hand] = values.as_slice() else {
                    bail!("block properties: bad toggle line {line:?}");
                };
                let toggle = Toggle {
                    open: sound_variant(open)?,
                    close: sound_variant(close)?,
                    by_hand: flag(by_hand)?,
                };
                ensure!(
                    toggles.insert(block_slot(name)?, toggle).is_none(),
                    "block properties: {name} toggles twice"
                );
            }
            "button" => {
                let [name, ticks, press, release] = values.as_slice() else {
                    bail!("block properties: bad button line {line:?}");
                };
                let button = Button {
                    ticks: ticks.parse()?,
                    press: sound_variant(press)?,
                    release: sound_variant(release)?,
                };
                ensure!(
                    button.ticks > 0,
                    "block properties: button {name} never releases"
                );
                ensure!(
                    buttons.insert(block_slot(name)?, button).is_none(),
                    "block properties: {name} listed twice as a button"
                );
            }
            "pickup" => {
                let [name, kind, item, sound] = values.as_slice() else {
                    bail!("block properties: bad pickup line {line:?}");
                };
                let kind = PICKUP_KINDS
                    .into_iter()
                    .find(|k| k == kind)
                    .with_context(|| format!("block properties: bad pickup kind in {line:?}"))?;
                let item = items
                    .get(*item)
                    .with_context(|| format!("block properties: unknown item {item}"))?;
                let pickup = Pickup {
                    kind,
                    item: format!("Item::{item}"),
                    sound: match *sound {
                        "-" => None,
                        sound => Some(sound_variant(sound)?),
                    },
                };
                ensure!(
                    pickups.insert(block_slot(name)?, pickup).is_none(),
                    "block properties: {name} listed twice as a pickup"
                );
            }
            "bed" => {
                let [name, attribute, destruction] = values.as_slice() else {
                    bail!("block properties: bad bed line {line:?}");
                };
                let explodes = match *destruction {
                    "explode" => true,
                    "remove" => false,
                    _ => bail!("block properties: bad bed destruction in {line:?}"),
                };
                let attribute = attribute
                    .strip_prefix("minecraft:")
                    .with_context(|| format!("block properties: bad bed rule in {line:?}"))?;
                ensure!(
                    ["gameplay/bed_rule", "gameplay/straw_bed_rule"].contains(&attribute),
                    "block properties: unknown bed rule attribute in {line:?}"
                );
                beds.insert(block_slot(name)?, (attribute.to_string(), explodes));
            }
            _ => bail!("block properties: bad line {line:?}"),
        }
    }
    ensure!(
        instrument_info.iter().all(Option::is_some),
        "block properties: an instrument has no sound"
    );
    ensure!(
        !toggles.is_empty() && !buttons.is_empty() && !pickups.is_empty() && !beds.is_empty(),
        "block properties: no toggles, buttons, bucket pickups or beds"
    );
    ensure!(
        count == state_count,
        "block properties: {count} states, but blocks.json has {state_count}"
    );
    ensure!(
        seen_blocks == block_index.len(),
        "block properties: {seen_blocks} blocks, but the registry has {}",
        block_index.len()
    );
    ensure!(!item_blocks.is_empty(), "block properties: no block items");

    let mut out = String::new();
    write!(
        out,
        "static STATE_REQUIRES_CORRECT_TOOL_FOR_DROPS: [u64; {}] = [",
        requires_tool.len()
    )?;
    array_body(&mut out, requires_tool.iter().map(|w| format!("{w:#x}")));
    writeln!(out, "];")?;
    write!(out, "static DESTROY_SPEEDS: [f32; {}] = [", speeds.len())?;
    array_body(&mut out, speeds.iter().map(|s| format!("{s:?}")));
    writeln!(out, "];")?;
    write!(out, "static STATE_DESTROY_SPEED: [u8; {state_count}] = [")?;
    array_body(&mut out, &speed_index);
    writeln!(out, "];")?;

    write!(out, "static SOUNDS: [SoundType; {}] = [", sounds.len())?;
    array_body(
        &mut out,
        sounds
            .iter()
            .map(|([brk, step, place, hit, fall], volume, pitch)| {
                format!(
                    "SoundType {{ break_sound: {brk}, step_sound: {step}, place_sound: {place}, \
                 hit_sound: {hit}, fall_sound: {fall}, volume: {volume}, pitch: {pitch} }}"
                )
            }),
    );
    writeln!(out, "];")?;
    write!(out, "static SHAPES: [&[[f32; 6]]; {}] = [", shapes.len())?;
    array_body(&mut out, &shapes);
    writeln!(out, "];")?;
    write!(out, "static INFOS: [Info; {}] = [", infos.len())?;
    array_body(
        &mut out,
        infos.iter().map(|i| {
            format!(
                "Info {{ sturdy: {}, flags: {}, full_faces: {}, sound: {}, shape: {}, outline: {} }}",
                i.sturdy, i.flags, i.full_faces, i.sound, i.shape, i.outline
            )
        }),
    );
    writeln!(out, "];")?;
    ensure!(
        !transforms.is_empty(),
        "block properties: no state transforms"
    );
    write!(
        out,
        "/// `[state, rotated clockwise, mirrored left-right, mirrored front-back]`\n\
         /// for every state a rotation or mirror changes, sorted by state.\n\
         static STATE_TRANSFORMS: [[u32; 4]; {}] = [",
        transforms.len()
    )?;
    array_body(
        &mut out,
        transforms
            .iter()
            .map(|[a, b, c, d]| format!("[{a}, {b}, {c}, {d}]")),
    );
    writeln!(out, "];")?;
    ensure!(
        block_entities.iter().any(Option::is_some),
        "block properties: no block entities"
    );
    write!(
        out,
        "/// Per block: the block entity type it gets when placed, and whether\n\
         /// that is a container that can hold a loot table.\n\
         static BLOCK_ENTITY: [Option<(&str, bool)>; {}] = [",
        block_entities.len()
    )?;
    array_body(
        &mut out,
        block_entities.iter().map(|entity| match entity {
            Some((kind, randomizable)) => {
                format!(
                    "Some(({}, {randomizable}))",
                    crate::names::str_literal(kind)
                )
            }
            None => "None".to_string(),
        }),
    );
    writeln!(out, "];")?;
    ensure!(
        falling.iter().any(|f| *f),
        "block properties: no falling blocks"
    );
    write!(out, "static FALLING: [bool; {}] = [", falling.len())?;
    array_body(&mut out, &falling);
    writeln!(out, "];")?;
    write!(
        out,
        "static FALLING_DELAY: [u8; {}] = [",
        falling_delay.len()
    )?;
    array_body(&mut out, &falling_delay);
    writeln!(out, "];")?;
    ensure!(
        push.contains(&"Immovable") && push.contains(&"Popped"),
        "block properties: no push reactions"
    );
    writeln!(
        out,
        "/// How a piston moving a block treats it.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
         pub enum PushReaction {{ PushPull, Push, Popped, Immovable, IgnoreEntity }}"
    )?;
    write!(
        out,
        "static PUSH_REACTION: [PushReaction; {}] = [",
        push.len()
    )?;
    array_body(&mut out, push.iter().map(|p| format!("PushReaction::{p}")));
    writeln!(out, "];")?;
    write!(out, "static STATE_INFO: [u16; {state_count}] = [")?;
    array_body(&mut out, &info_index);
    writeln!(out, "];")?;

    writeln!(
        out,
        "/// A block's placement and survival behavior family.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum BlockKind {{"
    )?;
    for kind in &kinds_seen {
        writeln!(out, "    {},", pascal(kind))?;
    }
    writeln!(out, "}}")?;
    write!(
        out,
        "static BLOCK_KIND: [BlockKind; {}] = [",
        block_kinds.len()
    )?;
    array_body(
        &mut out,
        block_kinds
            .iter()
            .map(|k| format!("BlockKind::{}", pascal(k.as_deref().unwrap_or("none")))),
    );
    writeln!(out, "];")?;

    write!(
        out,
        "static BLOCK_LOOT_TABLE: [Option<&str>; {}] = [",
        block_loot.len()
    )?;
    array_body(
        &mut out,
        block_loot.iter().map(|loot| match loot {
            Some(loot) => format!("Some({})", crate::names::str_literal(loot)),
            None => "None".to_string(),
        }),
    );
    writeln!(out, "];")?;

    write!(
        out,
        "static BLOCK_EXPERIENCE: [Option<BlockExperience>; {}] = [",
        block_experience.len()
    )?;
    array_body(&mut out, &block_experience);
    writeln!(out, "];")?;

    write!(
        out,
        "static GROUNDS: [[u64; {}]; {}] = [",
        state_count.div_ceil(64),
        grounds.len()
    )?;
    for set in &grounds {
        out.push('[');
        array_body(&mut out, set.iter().map(|w| format!("{w:#x}")));
        out.push_str("],");
    }
    writeln!(out, "];")?;
    write!(out, "static BLOCK_GROUND: [u8; {}] = [", block_ground.len())?;
    array_body(
        &mut out,
        block_ground
            .iter()
            .map(|g| g.map_or(Ok(u8::MAX), u8::try_from))
            .collect::<Result<Vec<_>, _>>()
            .context("block properties: too many ground sets")?,
    );
    writeln!(out, "];")?;

    write!(
        out,
        "static BLOCK_EXPLOSION_RESISTANCE: [f32; {}] = [",
        block_resistance.len()
    )?;
    array_body(
        &mut out,
        block_resistance
            .iter()
            .map(|r| r.map(|r| format!("{r:?}")))
            .collect::<Option<Vec<_>>>()
            .context("block properties: a block has no explosion resistance")?,
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static BLOCK_INSTRUMENT: [Instrument; {}] = [",
        block_instrument.len()
    )?;
    array_body(
        &mut out,
        block_instrument
            .iter()
            .map(|i| i.map(|i| format!("Instrument::{}", pascal(instruments[i]))))
            .collect::<Option<Vec<_>>>()
            .context("block properties: a block has no instrument")?,
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static INSTRUMENTS: [InstrumentInfo; {}] = [",
        instrument_info.len()
    )?;
    array_body(
        &mut out,
        instrument_info.iter().flatten().map(|(sound, tunable, above, custom)| {
            format!(
                "InstrumentInfo {{ sound: {sound}, tunable: {tunable}, works_above_note_block: {above}, \
                 custom_sound: {custom} }}"
            )
        }),
    );
    writeln!(out, "];")?;
    let lookup =
        |out: &mut String, name: &str, ty: &str, entries: Vec<(usize, String)>| -> Result<()> {
            write!(out, "static {name}: [(Block, {ty}); {}] = [", entries.len())?;
            array_body(
                out,
                entries
                    .iter()
                    .map(|(index, value)| format!("(Block::ALL[{index}], {value})")),
            );
            writeln!(out, "];")?;
            Ok(())
        };
    ensure!(!picks.is_empty(), "block properties: no pick overrides");
    write!(
        out,
        "static PICK_OVERRIDES: [(u16, Option<Item>); {}] = [",
        picks.len()
    )?;
    array_body(
        &mut out,
        picks.iter().map(|(id, item)| {
            format!(
                "({id}, {})",
                item.as_ref()
                    .map_or("None".to_string(), |i| format!("Some(Item::{i})"))
            )
        }),
    );
    writeln!(out, "];")?;
    ensure!(
        !pick_states.is_empty(),
        "block properties: no pick_state blocks"
    );
    lookup(
        &mut out,
        "PICK_STATES",
        "PickState",
        pick_states
            .iter()
            .map(|(index, (always, properties, prototype))| {
                (
                    *index,
                    format!(
                        "PickState {{ always: {always}, properties: &[{}], item_default: &[{}] }}",
                        properties
                            .iter()
                            .map(|p| format!("{p:?}"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        prototype
                            .iter()
                            .map(|(k, v)| format!("({k:?}, {v:?})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })
            .collect(),
    )?;
    ensure!(!infested.is_empty(), "block properties: no infested blocks");
    lookup(
        &mut out,
        "INFESTED_HOSTS",
        "Block",
        infested
            .iter()
            .map(|(index, host)| (*index, format!("Block::ALL[{host}]")))
            .collect(),
    )?;
    ensure!(!bases.is_empty(), "block properties: no base blocks");
    lookup(
        &mut out,
        "BASE_BLOCKS",
        "Block",
        bases
            .iter()
            .map(|(index, base)| (*index, format!("Block::ALL[{base}]")))
            .collect(),
    )?;
    ensure!(
        offsets.len() >= 2,
        "block properties: only {} blocks with a shape offset",
        offsets.len()
    );
    lookup(
        &mut out,
        "SHAPE_OFFSETS",
        "ShapeOffset",
        offsets
            .iter()
            .map(|(index, (h, v, collision, outline))| {
                (
                    *index,
                    format!(
                        "ShapeOffset {{ horizontal: {h:?}, vertical: {v:?}, collision: {collision}, outline: {outline} }}"
                    ),
                )
            })
            .collect(),
    )?;
    ensure!(
        !offset_samples.is_empty(),
        "block properties: no offset samples"
    );
    writeln!(
        out,
        "/// The game's own shape offsets at a few columns, for tests.\n#[doc(hidden)]\npub static SHAPE_OFFSET_SAMPLES: [(Block, i32, i32, [f64; 3]); {}] = [{}];",
        offset_samples.len(),
        offset_samples.join(", ")
    )?;
    ensure!(!campfires.is_empty(), "block properties: no campfires");
    lookup(
        &mut out,
        "CAMPFIRE_DAMAGE",
        "f32",
        campfires
            .iter()
            .map(|(index, damage)| (*index, format!("{damage}.0")))
            .collect(),
    )?;
    lookup(
        &mut out,
        "TOGGLES",
        "Toggle",
        toggles
            .iter()
            .map(|(index, t)| {
                (
                    *index,
                    format!(
                        "Toggle {{ open_sound: {}, close_sound: {}, opens_by_hand: {} }}",
                        t.open, t.close, t.by_hand
                    ),
                )
            })
            .collect(),
    )?;
    lookup(
        &mut out,
        "BUTTONS",
        "Button",
        buttons
            .iter()
            .map(|(index, b)| {
                (
                    *index,
                    format!(
                        "Button {{ press_ticks: {}, press_sound: {}, release_sound: {} }}",
                        b.ticks, b.press, b.release
                    ),
                )
            })
            .collect(),
    )?;
    lookup(
        &mut out,
        "PICKUPS",
        "BucketPickup",
        pickups
            .iter()
            .map(|(index, p)| {
                (
                    *index,
                    format!(
                        "BucketPickup {{ kind: PickupKind::{}, item: {}, sound: {} }}",
                        pascal(p.kind),
                        p.item,
                        p.sound
                            .as_ref()
                            .map_or("None".to_string(), |s| format!("Some({s})"))
                    ),
                )
            })
            .collect(),
    )?;
    lookup(
        &mut out,
        "BEDS",
        "Bed",
        beds.iter()
            .map(|(index, (attribute, explodes))| {
                let rule = if attribute == "gameplay/bed_rule" {
                    "BedRuleAttribute::Bed"
                } else {
                    "BedRuleAttribute::StrawBed"
                };
                (
                    *index,
                    format!("Bed {{ rule: {rule}, explodes: {explodes} }}"),
                )
            })
            .collect(),
    )?;

    let mut block_items: Vec<Option<&str>> = vec![None; block_index.len()];
    write!(
        out,
        "static ITEM_BLOCKS: [Option<ItemBlock>; {}] = [",
        item_order.len()
    )?;
    let mut entries = Vec::with_capacity(item_order.len());
    for (index, item) in item_order.iter().enumerate() {
        entries.push(match item_blocks.get(&index) {
            None => "None".to_string(),
            Some((block, wall)) => {
                for name in std::iter::once(block).chain(wall.as_ref().map(|(w, _)| w)) {
                    block_items[block_index[name.as_str()]].get_or_insert(item);
                }
                let wall = match wall {
                    None => "None".to_string(),
                    Some((wall, attach)) => {
                        format!(
                            "Some((Block::{}, Facing::{}))",
                            blocks[wall.as_str()],
                            pascal(attach)
                        )
                    }
                };
                format!("Some((Block::{}, {wall}))", blocks[block.as_str()])
            }
        });
    }
    array_body(&mut out, entries);
    writeln!(out, "];")?;
    write!(
        out,
        "static BLOCK_ITEMS: [Option<Item>; {}] = [",
        block_items.len()
    )?;
    array_body(
        &mut out,
        block_items
            .iter()
            .map(|i| i.map_or("None".to_string(), |i| format!("Some(Item::{})", items[i]))),
    );
    writeln!(out, "];")?;
    Ok(out)
}

/// `-`, `c<n>`, `u<min>-<max>` or `s<base>+<bound>+...` as a
/// `BlockExperience` literal.
fn experience_literal(spec: &str) -> Result<String> {
    let (kind, rest) = spec.split_at(spec.len().min(1));
    Ok(match kind {
        "-" if rest.is_empty() => "None".to_string(),
        "c" => format!("Some(BlockExperience::Constant({}))", rest.parse::<i32>()?),
        "u" => {
            let (min, max) = rest
                .split_once('-')
                .context("uniform experience needs min-max")?;
            let (min, max): (i32, i32) = (min.parse()?, max.parse()?);
            ensure!(min <= max, "uniform experience range is reversed");
            format!("Some(BlockExperience::Uniform {{ min: {min}, max: {max} }})")
        }
        "s" => {
            let mut terms = rest.split('+');
            let base: i32 = terms.next().context("sum needs a base")?.parse()?;
            let bounds = terms
                .map(|bound| bound.parse::<i32>())
                .collect::<Result<Vec<_>, _>>()?;
            ensure!(!bounds.is_empty(), "sum needs random terms");
            format!(
                "Some(BlockExperience::Sum {{ base: {base}, bounds: &[{}] }})",
                bounds
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        _ => bail!("unknown experience spec {spec}"),
    })
}

fn flag(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("block properties: bad flag {value}"),
    }
}

fn registry<'a>(input: &'a Input, key: &str) -> Result<&'a crate::input::RegistryReport> {
    input
        .registries
        .get(key)
        .with_context(|| format!("registries.json has no {key}"))
}

fn intern<T: PartialEq>(values: &mut Vec<T>, value: T) -> usize {
    match values.iter().position(|v| *v == value) {
        Some(index) => index,
        None => {
            values.push(value);
            values.len() - 1
        }
    }
}

fn f32_literal(value: &str) -> Result<String> {
    let value: f32 = value.parse()?;
    ensure!(value.is_finite(), "block properties: bad number {value}");
    Ok(format!("{value:?}"))
}

fn shape_literal(boxes: &str) -> Result<String> {
    if boxes == "-" {
        return Ok("&[]".to_string());
    }
    let mut out = String::from("&[");
    for aabb in boxes.split(';') {
        let values: Vec<&str> = aabb.split(',').collect();
        ensure!(values.len() == 6, "block properties: bad box {aabb}");
        out.push('[');
        for value in values {
            let value: f32 = value.parse()?;
            ensure!(
                value.is_finite() && (-1.0..=2.0).contains(&value),
                "block properties: box out of range {aabb}"
            );
            write!(out, "{value:?},")?;
        }
        out.push_str("],");
    }
    out.push(']');
    Ok(out)
}

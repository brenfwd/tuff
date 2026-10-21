//! Fluid rules and the block state properties the fluid simulation reads,
//! from `build/FluidProperties.java` and the dimension types' `fast_lava`
//! attribute.
//!
//! Behaviour that is code rather than data (lava's slower rise, lava turning
//! water below it into stone, what each fluid does to the blocks it
//! replaces) is chosen per fluid by its overridden methods, which must match
//! a modeled set exactly, so a version that changes them fails the build.
//! Collision faces become ids with a precomputed "together cover the face"
//! matrix, as for light.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::array_body;
use super::registries;
use super::tags::resolved_registry_tags;
use crate::input::Input;
use crate::names::str_literal;

const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
const EMPTY_FACE: usize = 0;
const BLOCK_FACE: usize = 1;

/// The flowing fluid methods each modeled behaviour overrides. The flowing
/// kind adds its `createFluidStateDefinition` (the level property).
const WATER_OVERRIDES: &[&str] = &[
    "beforeDestroyingBlock",
    "canConvertToSource",
    "getAmount",
    "getDropOff",
    "getFlowing",
    "getSlopeFindDistance",
    "getSource",
];
const LAVA_OVERRIDES: &[&str] = &[
    "beforeDestroyingBlock",
    "canConvertToSource",
    "getAmount",
    "getDropOff",
    "getFlowing",
    "getSlopeFindDistance",
    "getSource",
    "getSpreadDelay",
    "spreadTo",
];

struct Rules {
    variant: String,
    source: String,
    flowing: String,
    tick_delay: [u32; 2],
    drop_off: [u8; 2],
    slope_find_distance: [u8; 2],
    behaviour: &'static str,
}

struct FluidStateLine {
    fluid: String,
    amount: u8,
    falling: bool,
    source: bool,
    own_height: f32,
    block: u32,
}

pub fn generate(input: &Input) -> Result<String> {
    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let fluid_registry = input
        .registries
        .get("minecraft:fluid")
        .context("registries.json has no minecraft:fluid")?;
    let fluids = registries::variants(fluid_registry)?;
    let fluid_order = registries::sorted_entries(fluid_registry)?;
    let sounds = registries::variants(
        input
            .registries
            .get("minecraft:sound_event")
            .context("registries.json has no minecraft:sound_event")?,
    )?;
    let fluid_tags = resolved_registry_tags(input, "minecraft:fluid")?;
    let tagged = |tag: &str, fluid: &str| {
        fluid_tags
            .get(tag)
            .is_some_and(|members| members.iter().any(|m| m == fluid))
    };
    let fluid_variant = |name: &str| -> Result<String> {
        fluids
            .get(name)
            .map(|v| format!("Fluid::{v}"))
            .with_context(|| format!("fluid properties: unknown fluid {name}"))
    };

    let mut lines = input.fluid_properties.lines().peekable();
    let directions: Vec<&str> = field(
        lines.next().context("fluid properties: empty")?,
        "directions",
    )?
    .split(' ')
    .collect();
    ensure!(
        directions == DIRECTIONS,
        "fluid properties: direction order is {directions:?}, expected {DIRECTIONS:?}"
    );
    let fast_lava_line: Vec<&str> = field(
        lines.next().context("fluid properties: no fast_lava")?,
        "fast_lava",
    )?
    .split(' ')
    .collect();
    let [fast_lava_attribute, fast_lava_default] = fast_lava_line.as_slice() else {
        bail!("fluid properties: bad fast_lava line");
    };
    let fast_lava_default: bool = fast_lava_default.parse()?;

    let mut rules: Vec<Rules> = Vec::new();
    let mut states: Vec<FluidStateLine> = Vec::new();
    let mut replaceable: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    let mut face_count = None;
    let mut covers: Vec<u64> = Vec::new();
    let mut covers_rows = 0;
    let mut state_fluid: Vec<u8> = Vec::with_capacity(state_count);
    let mut state_flags: Vec<u8> = Vec::with_capacity(state_count);
    let mut state_faces: Vec<[usize; 6]> = Vec::with_capacity(state_count);
    let mut places: Vec<String> = Vec::new();

    for line in lines {
        let (tag, rest) = line
            .split_once(' ')
            .with_context(|| format!("fluid properties: bad line {line:?}"))?;
        let values: Vec<&str> = rest.split(' ').collect();
        match tag {
            "fluid" => {
                let [
                    name,
                    source,
                    source_fluid,
                    flowing_fluid,
                    tick,
                    tick_fast,
                    drop,
                    drop_fast,
                    slope,
                    slope_fast,
                    overrides,
                ] = values.as_slice()
                else {
                    bail!("fluid properties: bad fluid line {line:?}");
                };
                let mut overridden: Vec<&str> = overrides
                    .split(',')
                    .filter(|m| *m != "-" && *m != "createFluidStateDefinition")
                    .collect();
                overridden.sort_unstable();
                let behaviour = if overridden == WATER_OVERRIDES {
                    ensure!(
                        tagged("minecraft:water", name),
                        "fluid properties: water-like {name} is not in #water"
                    );
                    "Water"
                } else if overridden == LAVA_OVERRIDES {
                    ensure!(
                        tagged("minecraft:lava", name),
                        "fluid properties: lava-like {name} is not in #lava"
                    );
                    "Lava"
                } else {
                    bail!("fluid properties: {name} overrides unmodeled behaviour {overridden:?}");
                };
                ensure!(
                    (*source == "1") == (name == source_fluid),
                    "fluid properties: {name} source flag disagrees"
                );
                rules.push(Rules {
                    variant: fluid_variant(name)?,
                    source: fluid_variant(source_fluid)?,
                    flowing: fluid_variant(flowing_fluid)?,
                    tick_delay: [tick.parse()?, tick_fast.parse()?],
                    drop_off: [drop.parse()?, drop_fast.parse()?],
                    slope_find_distance: [slope.parse()?, slope_fast.parse()?],
                    behaviour,
                });
            }
            "fluid_state" => {
                let [id, fluid, amount, falling, source, height, block] = values.as_slice() else {
                    bail!("fluid properties: bad fluid state {line:?}");
                };
                ensure!(
                    id.parse::<usize>()? == states.len(),
                    "fluid properties: fluid state {id} out of order"
                );
                let block: u32 = block.parse()?;
                ensure!(
                    (block as usize) < state_count,
                    "fluid properties: bad block state in {line:?}"
                );
                let amount: u8 = amount.parse()?;
                ensure!(amount <= 8, "fluid properties: bad amount in {line:?}");
                states.push(FluidStateLine {
                    fluid: fluid_variant(fluid)?,
                    amount,
                    falling: *falling == "1",
                    source: *source == "1",
                    own_height: height.parse()?,
                    block,
                });
            }
            "replaceable" => {
                let [id, same, bits] = values.as_slice() else {
                    bail!("fluid properties: bad replaceable line {line:?}");
                };
                ensure!(
                    bits.len() == fluid_order.len() * 6,
                    "fluid properties: replaceable row of the wrong width"
                );
                let mut row = 0u32;
                for (i, bit) in bits.bytes().enumerate() {
                    match bit {
                        b'1' => row |= 1 << i,
                        b'0' => {}
                        _ => bail!("fluid properties: bad replaceable bit"),
                    }
                }
                replaceable.insert((id.parse()?, same.parse()?), row);
            }
            "faces" => {
                let count: usize = rest.parse()?;
                ensure!(count > BLOCK_FACE, "fluid properties: too few faces");
                face_count = Some(count);
                covers = vec![0; (count * count).div_ceil(64)];
            }
            "covers" => {
                let count = face_count.context("fluid properties: covers before faces")?;
                ensure!(
                    rest.len() == count,
                    "fluid properties: covers row of the wrong width"
                );
                for (b, bit) in rest.bytes().enumerate() {
                    match bit {
                        b'1' => {
                            let index = covers_rows * count + b;
                            covers[index / 64] |= 1 << (index % 64);
                        }
                        b'0' => {}
                        _ => bail!("fluid properties: bad covers entry"),
                    }
                }
                covers_rows += 1;
            }
            "state" => {
                let count = face_count.context("fluid properties: state before faces")?;
                let numbers = values
                    .iter()
                    .map(|v| v.parse::<usize>())
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("fluid properties: bad state {line:?}"))?;
                let [id, fluid_state, ice, container, liquid, mask, faces @ ..] =
                    numbers.as_slice()
                else {
                    bail!("fluid properties: short state {line:?}");
                };
                ensure!(
                    *id == state_fluid.len(),
                    "fluid properties: state {id} out of order"
                );
                ensure!(
                    *fluid_state < states.len()
                        && *ice <= 1
                        && *container <= 1
                        && *liquid <= 1
                        && *mask < 32,
                    "fluid properties: out of range values in {line:?}"
                );
                ensure!(
                    *container == 1 || *mask == 0,
                    "fluid properties: state {id} takes liquids but is no container"
                );
                let faces: [usize; 6] = faces
                    .try_into()
                    .with_context(|| format!("fluid properties: bad face count in {line:?}"))?;
                ensure!(
                    faces.iter().all(|f| *f < count),
                    "fluid properties: face id out of range in {line:?}"
                );
                state_fluid.push(*fluid_state as u8);
                state_flags.push((*ice | container << 1 | liquid << 2 | mask << 3) as u8);
                state_faces.push(faces);
            }
            "place" => {
                let [id, fluid, result, tick, sound] = values.as_slice() else {
                    bail!("fluid properties: bad place line {line:?}");
                };
                if *result == "-" {
                    continue;
                }
                let result: usize = result.parse()?;
                ensure!(
                    result < state_count,
                    "fluid properties: bad result {line:?}"
                );
                let tick = match *tick {
                    "-" => "None".to_string(),
                    name => format!("Some({})", fluid_variant(name)?),
                };
                let mut sound_literals = Vec::new();
                if *sound != "-" {
                    for entry in sound.split(';') {
                        let parts: Vec<&str> = entry.split(',').collect();
                        let [name, volume, pitch] = parts.as_slice() else {
                            bail!("fluid properties: bad sound {entry:?}");
                        };
                        let variant = sounds
                            .get(*name)
                            .with_context(|| format!("fluid properties: unknown sound {name}"))?;
                        let (volume, pitch): (f32, f32) = (volume.parse()?, pitch.parse()?);
                        sound_literals.push(format!(
                            "(crate::registry::SoundEvent::{variant}, {volume:?}, {pitch:?})"
                        ));
                    }
                }
                places.push(format!(
                    "({id}, {}, LiquidPlacement {{ result: BlockState::raw({result}), tick: {tick}, sounds: &[{}] }})",
                    fluid_variant(fluid)?,
                    sound_literals.join(", ")
                ));
            }
            _ => bail!("fluid properties: unknown line {line:?}"),
        }
    }

    let face_count = face_count.context("fluid properties: no faces")?;
    ensure!(
        covers_rows == face_count,
        "fluid properties: {covers_rows} covers rows for {face_count} faces"
    );
    let covers_at = |a: usize, b: usize| {
        let index = a * face_count + b;
        covers[index / 64] >> (index % 64) & 1 == 1
    };
    ensure!(
        !covers_at(EMPTY_FACE, EMPTY_FACE),
        "fluid properties: face {EMPTY_FACE} is not empty"
    );
    ensure!(
        (0..face_count).all(|b| covers_at(BLOCK_FACE, b) && covers_at(b, BLOCK_FACE)),
        "fluid properties: face {BLOCK_FACE} is not the full block"
    );
    ensure!(
        state_fluid.len() == state_count,
        "fluid properties: {} states, but blocks.json has {state_count}",
        state_fluid.len()
    );
    ensure!(
        states.first().is_some_and(|s| s.fluid == "Fluid::Empty"),
        "fluid properties: fluid state 0 is not empty"
    );
    ensure!(
        rules.len() + 1 == fluid_order.len(),
        "fluid properties: every fluid but the empty one needs rules"
    );
    for id in 0..states.len() {
        for same in 0..2 {
            ensure!(
                replaceable.contains_key(&(id, same)),
                "fluid properties: no replaceable row for {id}/{same}"
            );
        }
    }

    let mut fast_lava = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("dimension_type/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let value = match json
            .get("attributes")
            .and_then(|a| a.get(*fast_lava_attribute))
        {
            None => fast_lava_default,
            Some(Value::Bool(value)) => *value,
            Some(other) => bail!("data/{path}: bad {fast_lava_attribute} {other}"),
        };
        fast_lava.push((format!("{namespace}:{name}"), value));
    }
    ensure!(
        !fast_lava.is_empty(),
        "fluid properties: no dimension types"
    );
    fast_lava.sort();

    let mut face_sets: Vec<[usize; 6]> = Vec::new();
    let mut face_set_ids: HashMap<[usize; 6], usize> = HashMap::new();
    let state_face_set: Vec<usize> = state_faces
        .iter()
        .map(|faces| {
            *face_set_ids.entry(*faces).or_insert_with(|| {
                face_sets.push(*faces);
                face_sets.len() - 1
            })
        })
        .collect();
    ensure!(face_count <= 1 << 8, "fluid properties: too many faces");
    ensure!(
        face_sets.len() <= 1 << 16,
        "fluid properties: too many face sets"
    );

    let mut out = String::new();
    writeln!(
        out,
        "const FAST_LAVA_DEFAULT: bool = {fast_lava_default};\n\
         static FAST_LAVA: &[(&str, bool)] = &["
    )?;
    for (name, value) in &fast_lava {
        writeln!(out, "    ({}, {value}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static RULES: &[FluidRules] = &[")?;
    for rule in &rules {
        writeln!(
            out,
            "    FluidRules {{ fluid: {}, source: {}, flowing: {}, tick_delay: {:?}, drop_off: {:?}, \
             slope_find_distance: {:?}, behaviour: FluidBehaviour::{} }},",
            rule.variant,
            rule.source,
            rule.flowing,
            rule.tick_delay,
            rule.drop_off,
            rule.slope_find_distance,
            rule.behaviour
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out, "static FLUID_STATES: &[FluidStateData] = &[")?;
    for state in &states {
        writeln!(
            out,
            "    FluidStateData {{ fluid: {}, amount: {}, falling: {}, source: {}, own_height: {:?}, \
             block: BlockState::raw({}) }},",
            state.fluid, state.amount, state.falling, state.source, state.own_height, state.block
        )?;
    }
    writeln!(out, "];")?;
    write!(
        out,
        "/// Per fluid state and \"same fluid above\": bit `other * 6 + direction`.\n\
         static REPLACEABLE: [u32; {}] = [",
        states.len() * 2
    )?;
    array_body(&mut out, replaceable.values().map(|v| format!("{v:#x}")));
    writeln!(out, "];")?;
    write!(
        out,
        "\npub(crate) static STATE_FLUID: [u8; crate::block::BlockState::COUNT] = ["
    )?;
    array_body(&mut out, state_fluid);
    writeln!(out, "];")?;
    write!(
        out,
        "\n/// Bit 0 ice, bit 1 liquid container, bit 2 liquid block, bits 3.. the\n\
         /// fluids it takes.\n\
         static STATE_FLUID_FLAGS: [u8; crate::block::BlockState::COUNT] = ["
    )?;
    array_body(&mut out, state_flags);
    writeln!(out, "];")?;
    writeln!(out, "const COLLISION_FACE_COUNT: usize = {face_count};")?;
    write!(
        out,
        "\nstatic STATE_COLLISION_FACES: [u16; crate::block::BlockState::COUNT] = ["
    )?;
    array_body(&mut out, state_face_set);
    writeln!(out, "];")?;
    write!(
        out,
        "\nstatic COLLISION_FACE_SETS: [[u8; 6]; {}] = [",
        face_sets.len()
    )?;
    array_body(
        &mut out,
        face_sets.iter().map(|set| {
            let ids: Vec<String> = set.iter().map(usize::to_string).collect();
            format!("[{}]", ids.join(", "))
        }),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "\nstatic COLLISION_COVERS: [u64; {}] = [",
        covers.len()
    )?;
    array_body(&mut out, covers.iter().map(|w| format!("{w:#x}")));
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// What placing a fluid's source into a liquid container does, by\n\
         /// state id; containers not listed refuse it.\n\
         static PLACEMENTS: &[(u32, Fluid, LiquidPlacement)] = &["
    )?;
    for place in &places {
        writeln!(out, "    {place},")?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

fn field<'a>(line: &'a str, name: &str) -> Result<&'a str> {
    line.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .with_context(|| format!("fluid properties: expected {name:?}, got {line:?}"))
}

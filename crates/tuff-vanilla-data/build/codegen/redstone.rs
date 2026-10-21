//! Redstone data from `build/RedstoneProperties.java`, validated strictly
//! so a changed dump format fails the build.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::{array_body, registries};
use crate::input::Input;
use crate::names::pascal;

const COMPONENTS: [&str; 22] = [
    "wire",
    "wall_torch",
    "torch",
    "repeater",
    "comparator",
    "observer",
    "lever",
    "button",
    "pressure_plate",
    "power_block",
    "lamp",
    "door",
    "trapdoor",
    "fence_gate",
    "note_block",
    "tnt",
    "hopper",
    "bell",
    "dropper",
    "dispenser",
    "daylight_detector",
    "target",
];
const ANALOG_SOURCES: [&str; 3] = ["state", "container", "unsupported"];

struct Plate {
    sensitivity: &'static str,
    max_weight: u32,
    ticks: u32,
    on: String,
    off: String,
}

fn nibbles(text: &str) -> Result<u32> {
    ensure!(
        text.len() == 6,
        "redstone properties: bad signal list {text}"
    );
    text.chars().enumerate().try_fold(0u32, |packed, (i, c)| {
        let value = c
            .to_digit(16)
            .with_context(|| format!("redstone properties: bad signal {text}"))?;
        Ok(packed | value << (4 * i))
    })
}

pub fn generate(input: &Input) -> Result<String> {
    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let registry = |key: &str| {
        input
            .registries
            .get(key)
            .with_context(|| format!("registries.json has no {key}"))
    };
    let sound_events = registries::variants(registry("minecraft:sound_event")?)?;
    let block_index: HashMap<&str, usize> =
        registries::sorted_entries(registry("minecraft:block")?)?
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, i))
            .collect();
    let block_slot = |name: &str| -> Result<usize> {
        block_index
            .get(name)
            .copied()
            .with_context(|| format!("redstone properties: unknown block {name}"))
    };
    let sound = |name: &str| -> Result<String> {
        sound_events
            .get(name)
            .map(|variant| format!("crate::registry::SoundEvent::{variant}"))
            .with_context(|| format!("redstone properties: unknown sound {name}"))
    };

    let mut flags: Vec<Option<u8>> = vec![None; state_count];
    let mut signals: Vec<(usize, u32, u32)> = Vec::new();
    let mut components: BTreeMap<usize, &str> = BTreeMap::new();
    let mut analog: BTreeMap<usize, &str> = BTreeMap::new();
    let mut analog_states: BTreeMap<usize, u8> = BTreeMap::new();
    let mut plates: BTreeMap<usize, Plate> = BTreeMap::new();
    let mut arrow_buttons: BTreeSet<usize> = BTreeSet::new();
    let items = registries::variants(registry("minecraft:item")?)?;
    let mut dispense: BTreeMap<String, &str> = BTreeMap::new();
    for line in input.redstone_properties.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        match words.as_slice() {
            ["state", id, bits, rest @ ..] => {
                let id: usize = id.parse()?;
                let bits: u8 = bits.parse()?;
                ensure!(
                    id < state_count,
                    "redstone properties: state {id} out of range"
                );
                ensure!(bits < 8, "redstone properties: unknown flags {line}");
                ensure!(flags[id].is_none(), "redstone properties: state {id} twice");
                flags[id] = Some(bits);
                match rest {
                    [] => ensure!(bits & 2 == 0, "redstone properties: no signals: {line}"),
                    [weak, strong] => {
                        ensure!(
                            bits & 2 != 0,
                            "redstone properties: signals of a non-source"
                        );
                        let (weak, strong) = (nibbles(weak)?, nibbles(strong)?);
                        if weak != 0 || strong != 0 {
                            signals.push((id, weak, strong));
                        }
                    }
                    _ => bail!("redstone properties: bad state line {line}"),
                }
            }
            ["component", block, component] => {
                let component = COMPONENTS
                    .iter()
                    .find(|c| *c == component)
                    .with_context(|| format!("redstone properties: unknown component {line}"))?;
                ensure!(
                    components.insert(block_slot(block)?, component).is_none(),
                    "redstone properties: {block} has two components"
                );
            }
            ["analog", block, source] => {
                let source = ANALOG_SOURCES
                    .iter()
                    .find(|s| *s == source)
                    .with_context(|| {
                        format!("redstone properties: unknown analog source {line}")
                    })?;
                analog.insert(block_slot(block)?, source);
            }
            ["analog_state", id, value] => {
                let id: usize = id.parse()?;
                let value: u8 = value.parse()?;
                ensure!(
                    id < state_count && value <= 15,
                    "redstone properties: {line}"
                );
                analog_states.insert(id, value);
            }
            ["plate", block, sensitivity, max_weight, ticks, on, off] => {
                let sensitivity = ["everything", "mobs", "weighted"]
                    .into_iter()
                    .find(|s| s == sensitivity)
                    .with_context(|| format!("redstone properties: unknown sensitivity {line}"))?;
                let max_weight: u32 = max_weight.parse()?;
                ensure!(
                    (sensitivity == "weighted") == (max_weight > 0),
                    "redstone properties: weight of a plate {line}"
                );
                plates.insert(
                    block_slot(block)?,
                    Plate {
                        sensitivity,
                        max_weight,
                        ticks: ticks.parse()?,
                        on: sound(on)?,
                        off: sound(off)?,
                    },
                );
            }
            ["arrow_button", block] => {
                arrow_buttons.insert(block_slot(block)?);
            }
            ["dispense", item, kind] => {
                let kind = ["default", "projectile", "special"]
                    .into_iter()
                    .find(|k| k == kind)
                    .with_context(|| {
                        format!("redstone properties: unknown dispense kind {line}")
                    })?;
                let variant = items
                    .get(item)
                    .with_context(|| format!("redstone properties: unknown item {item}"))?;
                dispense.insert(variant.clone(), kind);
            }
            _ => bail!("unknown redstone properties line: {line}"),
        }
    }
    let flags: Vec<u8> = flags
        .into_iter()
        .enumerate()
        .map(|(id, f)| f.with_context(|| format!("redstone properties: state {id} missing")))
        .collect::<Result<_>>()?;
    for (component, name) in [
        ("wire", "minecraft:redstone_wire"),
        ("comparator", "minecraft:comparator"),
    ] {
        ensure!(
            components.get(&block_slot(name)?) == Some(&component),
            "redstone properties: {name} is not the {component} component"
        );
    }
    ensure!(
        !plates.is_empty(),
        "redstone properties: no pressure plates"
    );
    ensure!(
        !analog_states.is_empty(),
        "redstone properties: no state comparator outputs"
    );
    ensure!(
        analog.values().any(|s| *s == "container"),
        "redstone properties: no container comparator outputs"
    );
    for id in analog_states.keys() {
        ensure!(
            flags[*id] & 4 != 0,
            "redstone properties: analog value for state {id} without output"
        );
    }

    let mut out = String::new();
    writeln!(
        out,
        "/// A block's redstone behaviour.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
         pub enum Component {{"
    )?;
    for component in COMPONENTS {
        writeln!(out, "    {},", pascal(component))?;
    }
    writeln!(out, "}}")?;
    writeln!(
        out,
        "impl Component {{\n    pub const ALL: [Self; {}] = [{}];\n}}",
        COMPONENTS.len(),
        COMPONENTS
            .iter()
            .map(|c| format!("Self::{}", pascal(c)))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    write!(out, "static REDSTONE_FLAGS: [u8; {state_count}] = [")?;
    array_body(&mut out, &flags);
    writeln!(out, "];")?;
    write!(
        out,
        "static SIGNALS: [(u16, u32, u32); {}] = [",
        signals.len()
    )?;
    array_body(
        &mut out,
        signals
            .iter()
            .map(|(id, weak, strong)| format!("({id}, {weak:#x}, {strong:#x})")),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static COMPONENTS: [(Block, Component); {}] = [",
        components.len()
    )?;
    array_body(
        &mut out,
        components
            .iter()
            .map(|(block, c)| format!("(Block::ALL[{block}], Component::{})", pascal(c))),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ANALOG_SOURCES: [(Block, AnalogSource); {}] = [",
        analog.len()
    )?;
    array_body(
        &mut out,
        analog
            .iter()
            .map(|(block, s)| format!("(Block::ALL[{block}], AnalogSource::{})", pascal(s))),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ANALOG_STATES: [(u16, u8); {}] = [",
        analog_states.len()
    )?;
    array_body(
        &mut out,
        analog_states.iter().map(|(id, v)| format!("({id}, {v})")),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static PLATES: [(Block, PressurePlate); {}] = [",
        plates.len()
    )?;
    array_body(
        &mut out,
        plates.iter().map(|(block, plate)| {
            let sensitivity = match plate.sensitivity {
                "everything" => "Sensitivity::Everything".to_string(),
                "mobs" => "Sensitivity::Mobs".to_string(),
                _ => format!("Sensitivity::Weighted({})", plate.max_weight),
            };
            format!(
                "(Block::ALL[{block}], PressurePlate {{ sensitivity: {sensitivity}, pressed_ticks: {}, \
                 click_on: {}, click_off: {} }})",
                plate.ticks, plate.on, plate.off
            )
        }),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ARROW_BUTTONS: [Block; {}] = [",
        arrow_buttons.len()
    )?;
    array_body(
        &mut out,
        arrow_buttons
            .iter()
            .map(|block| format!("Block::ALL[{block}]")),
    );
    writeln!(out, "];")?;
    writeln!(
        out,
        "fn dispense_table(item: Item) -> Option<Dispense> {{\n    match item {{"
    )?;
    for (variant, kind) in &dispense {
        writeln!(
            out,
            "        Item::{variant} => Some(Dispense::{}),",
            pascal(kind)
        )?;
    }
    writeln!(out, "        _ => None,\n    }}\n}}")?;
    Ok(out)
}

//! Potion and mob effect data from `build/PotionProperties.java`: every
//! potion's effects and every mob effect's category, color and particle.
//! Validated strictly: each registered potion and effect must appear
//! exactly once and every name must resolve.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::registries::{sorted_entries, variants};
use crate::input::Input;
use crate::names::{pascal, str_literal};

fn flag(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("potion properties: bad flag {value:?}"),
    }
}

pub fn generate(input: &Input) -> Result<String> {
    let effect_names = sorted_entries(&input.registries["minecraft:mob_effect"])?;
    let effect_variants = variants(&input.registries["minecraft:mob_effect"])?;
    let potion_names = sorted_entries(&input.registries["minecraft:potion"])?;
    let particle_variants = variants(&input.registries["minecraft:particle_type"])?;
    let effect_variant = |name: &str| -> Result<String> {
        effect_variants
            .get(name)
            .map(|v| format!("MobEffect::{v}"))
            .with_context(|| format!("potion properties: unknown mob effect {name}"))
    };

    let mut effects: BTreeMap<&str, String> = BTreeMap::new();
    let mut potions: BTreeMap<&str, (String, usize, Vec<String>)> = BTreeMap::new();
    let mut ambient_alpha = None;
    for line in input.potion_properties.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["effect", name, category, color, instant, particle] => {
                ensure!(
                    effect_variants.contains_key(name),
                    "potion properties: unknown mob effect {name}"
                );
                let category = match *category {
                    "beneficial" | "harmful" | "neutral" => pascal(category),
                    _ => bail!("potion properties: unknown category {category}"),
                };
                let color: i32 = color.parse()?;
                let particle = match *particle {
                    "color" => "None".to_owned(),
                    particle => format!(
                        "Some(ParticleType::{})",
                        particle_variants.get(particle).with_context(|| format!(
                            "potion properties: unknown particle {particle}"
                        ))?
                    ),
                };
                let previous = effects.insert(
                    name,
                    format!(
                        "EffectInfo {{ category: EffectCategory::{category}, color: {color}, instantaneous: {}, particle: {particle} }}",
                        flag(instant)?
                    ),
                );
                ensure!(previous.is_none(), "potion properties: {name} twice");
            }
            ["ambient_alpha", alpha] => {
                let alpha: u8 = alpha.parse()?;
                ensure!(
                    ambient_alpha.is_none(),
                    "potion properties: two ambient alphas"
                );
                ambient_alpha = Some(alpha);
            }
            ["potion", name, base, count] => {
                ensure!(
                    potion_names.contains(name),
                    "potion properties: unknown potion {name}"
                );
                let previous = potions.insert(name, (base.to_string(), count.parse()?, Vec::new()));
                ensure!(previous.is_none(), "potion properties: {name} twice");
            }
            [
                "potion_effect",
                name,
                effect,
                duration,
                amplifier,
                ambient,
                visible,
                icon,
            ] => {
                let (_, _, list) = potions
                    .get_mut(name)
                    .with_context(|| format!("potion properties: effect before potion {name}"))?;
                let duration: i64 = duration.parse()?;
                let amplifier: u64 = amplifier.parse()?;
                list.push(format!(
                    "EffectInstance {{ effect: {}, duration: {duration}, amplifier: {amplifier}, ambient: {}, show_particles: {}, show_icon: {} }}",
                    effect_variant(effect)?,
                    flag(ambient)?,
                    flag(visible)?,
                    flag(icon)?
                ));
            }
            _ => bail!("potion properties: unrecognized line {line:?}"),
        }
    }

    let mut out = String::new();
    writeln!(
        out,
        "/// The alpha of an ambient effect's tinted particle.\npub const AMBIENT_ALPHA: u8 = {};",
        ambient_alpha.context("potion properties: no ambient alpha")?
    )?;
    writeln!(
        out,
        "static EFFECTS: [EffectInfo; {}] = [",
        effect_names.len()
    )?;
    for name in &effect_names {
        let info = effects
            .get(name)
            .with_context(|| format!("potion properties: mob effect {name} missing"))?;
        writeln!(out, "    {info},")?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "static POTIONS: [PotionInfo; {}] = [",
        potion_names.len()
    )?;
    for name in &potion_names {
        let (base, count, list) = potions
            .get(name)
            .with_context(|| format!("potion properties: potion {name} missing"))?;
        ensure!(
            *count == list.len(),
            "potion properties: {name} lists {} of {count} effects",
            list.len()
        );
        writeln!(
            out,
            "    PotionInfo {{ name: {}, effects: &[{}] }},",
            str_literal(base),
            list.join(", ")
        )?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

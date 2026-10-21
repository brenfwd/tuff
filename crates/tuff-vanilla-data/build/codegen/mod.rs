mod advancements;
mod attributes;
mod biomes;
mod block_entities;
mod block_properties;
mod block_transformers;
mod blocks;
mod commands;
mod components;
mod damage_types;
mod data_registries;
mod dimension_types;
mod dragon;
mod enchantments;
mod entities;
mod enums;
mod fluids;
mod game_rules;
mod growth;
mod item_properties;
mod items;
mod json;
mod lang;
mod light;
mod loot_tables;
mod maps;
mod packets;
mod potions;
mod providers;
mod recipes;
mod redstone;
mod registries;
mod structures;
mod sulfur_cube;
mod tags;
mod timelines;
mod trial_spawners;
mod variants;
mod version;
mod villagers;
mod world;
mod worldgen;

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::input::Input;

pub fn generate(input: &Input, out_dir: &Path) -> Result<()> {
    let files = [
        ("version.rs", version::generate(input)?),
        ("packets.rs", packets::generate(input)?),
        ("registries.rs", registries::generate(input)?),
        ("blocks.rs", blocks::generate(input)?),
        ("light.rs", light::generate(input)?),
        ("fluids.rs", fluids::generate(input)?),
        ("block_properties.rs", block_properties::generate(input)?),
        ("block_entities.rs", block_entities::generate(input)?),
        ("items.rs", items::generate(input)?),
        ("item_properties.rs", item_properties::generate(input)?),
        ("providers.rs", providers::generate(input)?),
        ("components.rs", components::generate(input)?),
        ("tags.rs", tags::generate(input)?),
        ("data_registries.rs", data_registries::generate(input)?),
        ("dimension_types.rs", dimension_types::generate(input)?),
        ("damage_types.rs", damage_types::generate(input)?),
        ("biomes.rs", biomes::generate(input)?),
        ("recipes.rs", recipes::generate(input)?),
        ("advancements.rs", advancements::generate(input)?),
        ("loot_tables.rs", loot_tables::generate(input)?),
        ("commands.rs", commands::generate(input)?),
        ("timelines.rs", timelines::generate(input)?),
        ("trial_spawners.rs", trial_spawners::generate(input)?),
        ("sulfur_cube.rs", sulfur_cube::generate(input)?),
        ("game_rules.rs", game_rules::generate(input)?),
        ("enchantments.rs", enchantments::generate(input)?),
        (
            "block_transformers.rs",
            block_transformers::generate(input)?,
        ),
        ("world.rs", world::generate(input)?),
        ("entities.rs", entities::generate(input)?),
        ("dragon.rs", dragon::generate(input)?),
        ("redstone.rs", redstone::generate(input)?),
        ("growth.rs", growth::generate(input)?),
        ("potions.rs", potions::generate(input)?),
        ("maps.rs", maps::generate(input)?),
        ("variants.rs", variants::generate(input)?),
        ("attributes.rs", attributes::generate(input)?),
        ("worldgen.rs", worldgen::generate(input)?),
        ("structures.rs", structures::generate(input)?),
        ("villagers.rs", villagers::generate(input)?),
        ("lang.rs", lang::generate(input)?),
    ];
    for (name, code) in files {
        fs::write(out_dir.join(name), code)?;
    }
    let blob: Vec<u8> = structures::templates(input)
        .into_iter()
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect();
    fs::write(out_dir.join("structure_templates.bin"), blob)?;
    fs::write(
        out_dir.join("worldgen_pack.json"),
        serde_json::to_vec(&worldgen::pack_json(input))?,
    )?;
    Ok(())
}

/// Writes `values` as the body of an array literal, several per line.
pub fn array_body(out: &mut String, values: impl IntoIterator<Item = impl std::fmt::Display>) {
    use std::fmt::Write;
    for (i, value) in values.into_iter().enumerate() {
        out.push_str(if i % 16 == 0 { "\n    " } else { " " });
        let _ = write!(out, "{value},");
    }
    out.push('\n');
}

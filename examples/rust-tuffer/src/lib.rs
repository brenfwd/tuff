// SPDX-License-Identifier: MIT
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]
#![deny(deprecated)]
//! tuffer: a dimension of floating islands under a fixed pale sky, reached
//! through glowstone frames lit with a water bucket. One tuffer block is
//! 128 overworld blocks. Needs the `cap.dimension` grant.

use tuff_plugin_sdk::prelude::*;

mod island;
mod portal;

const BIOME: &str = "tuffer:islands";
const TYPE: &str = "tuffer:tuffer_type";

fn dimension_type() -> DimensionType {
    DimensionType::new()
        .ambient_light(0.35)
        .range(0, 256)
        .has_fixed_time(true)
        .bed_unusable()
        .attribute_bool("minecraft:gameplay/respawn_anchor_works", false)
        .attribute_bool("minecraft:gameplay/water_evaporates", false)
        .attribute_color("minecraft:visual/ambient_light_color", "#6f86c2")
        .attribute_color("minecraft:visual/fog_color", "#b9c7ee")
        .attribute_color("minecraft:visual/sky_color", "#8fa4dc")
        .attribute_color("minecraft:visual/cloud_color", "#e9eeff")
        .ambient_particles(&[("minecraft:white_ash", 0.004)])
        .skybox("none")
}

fn biome() -> BiomeDef {
    BiomeDef::new()
        .has_precipitation(false)
        .temperature(0.4)
        .downfall(0.0)
        .water_color("#4f7fd8")
}

struct Tuffer;

impl Plugin for Tuffer {
    fn register() -> Result<(), String> {
        on::<ev::BlockBreak>(portal::broken).map_err(|e| format!("{e:?}"))?;
        on::<ev::RightClick>(portal::ignite).map_err(|e| format!("{e:?}"))?;
        on::<ev::InsideBlock>(portal::travel).map_err(|e| format!("{e:?}"))?;
        watch_block("minecraft:end_gateway").map_err(|e| format!("{e:?}"))?;
        intercept_click(
            ClickKind::Right,
            "minecraft:water_bucket",
            "",
            "minecraft:glowstone",
        )
        .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    fn worldgen(world: &mut Worldgen) {
        let def = DimensionDef::new(portal::TUFFER)
            .dimension_type(TYPE, &dimension_type())
            .biome(BIOME, biome())
            .spawn(0, island::SPAWN_Y, 0);
        world.dimension(def, |a| {
            Ok(Chunk {
                palette: island::PALETTE.iter().map(|s| (*s).to_owned()).collect(),
                blocks: island::generate(a.seed, a.chunk_x, a.chunk_z, a.min_y, a.height),
                biome: BIOME.into(),
            })
        });
    }
}

#[cfg(target_arch = "wasm32")]
plugin!(Tuffer {
    id: "tuffer",
    version: "1.0.0",
    description: "Floating islands reached through glowstone portals",
    requests: [
        "cap.dimension = tuffer:tuffer -- adds the tuffer dimension",
        "cap.world-write = all -- places the portal blocks",
        "cap.player-write = all -- moves travelers between dimensions",
    ],
});

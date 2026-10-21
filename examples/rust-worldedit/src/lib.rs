// SPDX-License-Identifier: MIT
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]
#![deny(deprecated)]
//! worldedit: region edits that never fail on unloaded chunks, as a
//! runtime plugin. Edits are split into slices applied one per server tick,
//! recorded for undo and redo, and bounded by a per-player block limit.
//!
//! Every command is `/tuff we <name>` and also `//<name>` (and WorldEdit's
//! other names for it); `//help` lists them. A wooden axe selects: left
//! click pos1, right click pos2.

use tuff_plugin_sdk::prelude::{ev, on};
use tuff_plugin_sdk::{ClickKind, Plugin, events, plugin, types};

mod brush;
mod clipboard;
mod commands;
mod cui;
mod edit;
mod expr;
mod history;
mod jobs;
mod mask;
mod pattern;
mod plan;
mod region;
mod schematic;
mod selection;
mod session;

use session::{model, owner};

const WAND: &str = "minecraft:wooden_axe";

fn clicked(right: bool, click: &types::Click) {
    let at = [click.pos.x, click.pos.y, click.pos.z];
    if brush::click(right, at, &click.dimension) {
        return;
    }
    if tuff_plugin_sdk::world::acting::has_permission(commands::BASE_NODE) != Some(true)
        || tuff_plugin_sdk::world::acting::has_permission("worldedit.selection.pos") != Some(true)
    {
        return;
    }
    let Some(id) = owner() else {
        return;
    };
    if !model(|m| m.players[&id].wand) {
        return;
    }
    selection::set_pos(id, right, at, &click.dimension);
}

struct WorldEdit;

impl Plugin for WorldEdit {
    fn register() -> Result<(), String> {
        commands::register()?;
        events::intercept_click(ClickKind::Left, WAND, "", "")?;
        events::intercept_click(ClickKind::Right, WAND, "", "")?;
        events::intercept_click(ClickKind::Left, "", brush::TAG, "")?;
        events::intercept_click(ClickKind::Right, "", brush::TAG, "")?;
        on::<ev::LeftClick>(|c| clicked(false, c))?;
        on::<ev::RightClick>(|c| clicked(true, c))?;
        events::register_channel(cui::CHANNEL)?;
        on::<ev::Payload>(cui::on_payload)?;
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
plugin!(WorldEdit {
    id: "worldedit",
    version: "1.0.0",
    description: "Asynchronous region edits that work across unloaded chunks",
    requests: [
        "cap.world-write = all -- edits regions",
        "cap.player-write = all -- teleports and gives the wand",
        "cap.player-read = all -- reads the held item and selection",
    ],
});

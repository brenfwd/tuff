// SPDX-License-Identifier: MIT
//! Everything a typical plugin uses: `use tuff_plugin_sdk::prelude::*;`.

pub use crate::background;
pub use crate::command::{Ctx, Outcome, arg, argument, command, literal, parser};
pub use crate::config::Setting;
pub use crate::dimensions::{
    ChunkBuilder, ContentBuilder, DimensionDef, GenArgs, ItemDef, Worldgen,
};
pub use crate::events::{
    ev, intercept_click, intercept_inventory, intercept_item, intercept_state, listen, on,
    watch_block, watch_region,
};
pub use crate::items::{self, ItemDetails};
pub use crate::messaging::{self, CallError};
pub use crate::pack::Pack;
pub use crate::panel::{self, Page, PanelUnit};
pub use crate::plugin_state;
pub use crate::registry::{BiomeDef, DimensionType};
pub use crate::reply::Replier;
pub use crate::schedule::{Repeat, after, every};
pub use crate::state::State;
pub use crate::store::{Stored, StoredMap};
pub use crate::text::{Color, Span, Style, Styled};
pub use crate::types::{BlockPos, Player, Uuid, Vec3};
pub use crate::world::{self, EntityRef, Particle, PlayerExt, World, acting, pos, vec3};
pub use crate::world::{Sidebar, Team, Title, declare_permission};
pub use crate::{
    Biome, Chunk, ClickKind, Dimension, block_get, block_set, teleport_entity, teleport_player,
    teleport_self,
};
pub use crate::{Level, Plugin, args, broadcast, log, plugin, sleep, spawn, text};

use crate::world::dimension::DimensionId;
use crate::world::menu::MenuKind;
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_world_model::BlockPos;

use super::*;

/// A container menu as the region hands it to a player.
#[derive(Debug)]
pub struct MenuOpen {
    pub dimension: DimensionId,
    pub region: RegionHandle,
    pub kind: MenuKind,
    pub title: TextComponent,
    pub source: MenuSource,
    /// The statistic opening this block counts towards.
    pub stat: Option<tuff_vanilla_data::registry::CustomStat>,
}

#[derive(Debug)]
pub enum MenuSource {
    /// Block containers: the block entities backing the container section
    /// in slot order, each with its slot count, their contents, and the
    /// container data slots (a furnace's progress).
    Blocks {
        parts: Vec<(BlockPos, usize)>,
        contents: Vec<Option<ItemStack>>,
        data: Vec<i32>,
        /// The part owned by another region (the far half of a double
        /// chest across a region border) and that region.
        remote: Option<(BlockPos, RegionHandle)>,
    },
    /// The ender chest block at `pos`; the contents are the player's own.
    EnderChest { pos: BlockPos },
    /// A crafting station at `pos` (crafting table, stonecutter, smithing
    /// table): its slots exist only while the player has it open.
    /// `bookcases` is the enchanting table's shelf count, 0 elsewhere.
    /// `data` is the beacon's levels and encoded effects, empty elsewhere.
    Station {
        pos: BlockPos,
        bookcases: i32,
        data: Vec<i32>,
    },
    /// The container of the region entity with this id (a chest boat).
    Entity {
        entity: i32,
        contents: Vec<Option<ItemStack>>,
    },
}

/// The far half of a double chest as its region opened it for a viewer.
#[derive(Debug)]
pub struct OpenedHalf {
    pub pos: BlockPos,
    pub region: RegionHandle,
    pub contents: Vec<Option<ItemStack>>,
    pub custom_name: Option<TextComponent>,
}

/// One slot of an entity container a transaction changes, as
/// [`ContainerChange`] is for blocks.
#[derive(tuff_item::item::DupClone, Debug, PartialEq)]
pub struct EntitySlotChange {
    pub slot: usize,
    pub expected: Option<ItemStack>,
    pub new: Option<ItemStack>,
}

/// One slot of a block container a transaction changes: what the session
/// expects it to hold and what it should hold afterwards.
#[derive(Debug, PartialEq)]
pub struct ContainerChange {
    pub pos: BlockPos,
    pub slot: usize,
    pub expected: Option<ItemStack>,
    pub new: Option<ItemStack>,
}

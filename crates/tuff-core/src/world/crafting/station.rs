//! The stations' vocabulary: the blocks that cook and what a smithing
//! table's slots must hold.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::recipe::{CookingCategory, Ingredient};
use tuff_vanilla_data::registry::RecipeBookCategory;

use crate::world::block_entity::ContainerKind;

/// The blocks that cook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CookingStation {
    Furnace,
    BlastFurnace,
    Smoker,
    Campfire,
}

impl CookingStation {
    /// The station a container is, if it cooks.
    pub fn of(kind: ContainerKind) -> Option<Self> {
        Some(match kind {
            ContainerKind::Furnace => Self::Furnace,
            ContainerKind::BlastFurnace => Self::BlastFurnace,
            ContainerKind::Smoker => Self::Smoker,
            ContainerKind::Campfire => Self::Campfire,
            _ => return None,
        })
    }

    /// The recipe property set listing what it accepts.
    pub fn property_set(self) -> &'static str {
        match self {
            Self::Furnace => "furnace_input",
            Self::BlastFurnace => "blast_furnace_input",
            Self::Smoker => "smoker_input",
            Self::Campfire => "campfire_input",
        }
    }

    pub fn icon(self) -> Item {
        match self {
            Self::Furnace => Item::Furnace,
            Self::BlastFurnace => Item::BlastFurnace,
            Self::Smoker => Item::Smoker,
            Self::Campfire => Item::Campfire,
        }
    }

    pub fn book_category(self, category: Option<CookingCategory>) -> RecipeBookCategory {
        match (self, category) {
            (Self::Furnace, Some(CookingCategory::Blocks)) => RecipeBookCategory::FurnaceBlocks,
            (Self::Furnace, Some(CookingCategory::Food)) => RecipeBookCategory::FurnaceFood,
            (Self::Furnace, _) => RecipeBookCategory::FurnaceMisc,
            (Self::BlastFurnace, Some(CookingCategory::Blocks)) => {
                RecipeBookCategory::BlastFurnaceBlocks
            }
            (Self::BlastFurnace, _) => RecipeBookCategory::BlastFurnaceMisc,
            (Self::Smoker, _) => RecipeBookCategory::SmokerFood,
            (Self::Campfire, _) => RecipeBookCategory::Campfire,
        }
    }

    /// Ticks one cooking takes unless a recipe says otherwise. The blast
    /// furnace and smoker get their speed from their fuel's multiplier.
    pub const fn default_time(self) -> u64 {
        match self {
            Self::Campfire => 600,
            _ => 200,
        }
    }
}

/// What a smithing table's template, base and addition slots must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmithingSlots {
    pub template: Option<Ingredient>,
    pub base: Ingredient,
    pub addition: Option<Ingredient>,
}

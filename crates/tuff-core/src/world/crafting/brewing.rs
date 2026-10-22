//! The brewing stand as a pure function of its slots and progress: fuel
//! from each item's `brewing_fuel` component, the brew timer and the
//! registered brewing recipes.

use tuff_vanilla_data::recipe::PotionIngredient;

use tuff_item::item::ItemStack;

pub use crate::world::block_entity::{BREW_TIME, BrewTick, Brewing, DEFAULT_FUEL_USES};

/// Whether `stack` is the item, and potion if any, `ingredient` asks for.
pub fn accepts(ingredient: &PotionIngredient, stack: &ItemStack) -> bool {
    if !ingredient.ingredient.matches(stack.item()) {
        return false;
    }
    let Some(potions) = &ingredient.potions else {
        return true;
    };
    stack
        .components
        .potion_contents
        .as_ref()
        .and_then(|contents| contents.potion)
        .is_some_and(|potion| potions.contains(&potion))
}

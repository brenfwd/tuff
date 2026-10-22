//! The furnace family and campfires as pure functions of their slots and
//! progress: fuel lighting, cook timing, output stacking and the fuel's
//! remainder, driven by the recipe data and each fuel's `cooking_fuel`
//! component.

use tuff_vanilla_data::recipe::CookingRecipe;
use tuff_vanilla_data::{BlockState, Item};

use super::{CookingStation, RecipeSet};
use crate::world::block_entity::{Campfire, ContainerKind, Furnace, furnace};
use tuff_item::item::ItemStack;

/// The cook time a furnace assumes for an input it has no recipe for.
pub const DEFAULT_COOK_TIME: i32 = 200;

pub use crate::world::block_entity::FurnaceTick;

/// The recipe `kind` cooks `item` with.
pub fn recipe_for(
    recipes: &RecipeSet,
    kind: ContainerKind,
    item: Item,
) -> Option<(&str, &CookingRecipe)> {
    recipes.cooking_recipe(CookingStation::of(kind)?, item)
}

pub fn burn_time(fuel: Item, block: BlockState) -> i32 {
    fuel.cooking_fuel()
        .map_or(0, |fuel| fuel.burn_time(Some(block)))
}

pub fn speed_multiplier(fuel: Item, block: BlockState) -> f32 {
    fuel.cooking_fuel()
        .map_or(1.0, |fuel| fuel.speed_multiplier(Some(block)))
}

pub fn total_cook_time(recipe: &CookingRecipe, speed: f32) -> i32 {
    let ticks = recipe.cooking_time as i32;
    if speed > 0.0 {
        (ticks as f32 / speed).ceil() as i32
    } else {
        ticks
    }
}

/// The total cook time for what is in the input slot.
pub fn cook_time_for_input(
    recipes: &RecipeSet,
    kind: ContainerKind,
    items: &[Option<ItemStack>],
    furnace: &Furnace,
) -> i32 {
    items[furnace::INPUT]
        .as_ref()
        .and_then(|input| recipe_for(recipes, kind, input.item()))
        .map_or(DEFAULT_COOK_TIME, |(_, recipe)| {
            total_cook_time(recipe, furnace.speed_multiplier)
        })
}

/// The input slot now holds a different item: progress restarts.
pub fn input_changed(
    recipes: &RecipeSet,
    kind: ContainerKind,
    items: &[Option<ItemStack>],
    furnace: &mut Furnace,
) {
    furnace.cooking_total_time = cook_time_for_input(recipes, kind, items, furnace);
    furnace.cooking_timer = 0;
}

/// Puts one of `stack` on the first free campfire slot if it has a
/// campfire recipe, returning that slot.
pub fn place_food(
    recipes: &RecipeSet,
    items: &mut [Option<ItemStack>],
    campfire: &mut Campfire,
    stack: ItemStack,
) -> Result<usize, ItemStack> {
    let Some(slot) = items.iter().position(Option::is_none) else {
        return Err(stack);
    };
    let Some((_, recipe)) = recipe_for(recipes, ContainerKind::Campfire, stack.item()) else {
        return Err(stack);
    };
    campfire.cooking_time[slot] = recipe.cooking_time as i32;
    campfire.cooking_progress[slot] = 0;
    items[slot] = Some(stack);
    Ok(slot)
}

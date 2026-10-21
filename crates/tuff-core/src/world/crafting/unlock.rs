//! What every recipe kind shares: the book tab of a crafting category and
//! the advancement that teaches a recipe.

use tuff_vanilla_data::advancement::{Criterion, Rewards};
use tuff_vanilla_data::recipe::{CraftingCategory, Ingredient};
use tuff_vanilla_data::registry::{RecipeBookCategory, TriggerType};
use tuff_vanilla_data::{Frozen, Item};

use super::Recipe;
use crate::registry::Registries;
use crate::world::advancement::Definition;
use tuff_item::load::LoadError;

/// The recipe book tab a crafting recipe of this category is listed under.
pub fn crafting_category(category: Option<CraftingCategory>) -> RecipeBookCategory {
    match category {
        Some(CraftingCategory::Building) => RecipeBookCategory::CraftingBuildingBlocks,
        Some(CraftingCategory::Equipment) => RecipeBookCategory::CraftingEquipment,
        Some(CraftingCategory::Redstone) => RecipeBookCategory::CraftingRedstone,
        _ => RecipeBookCategory::CraftingMisc,
    }
}

/// The advancement that teaches `name` once the player holds an item one of
/// `ingredients` accepts (or already knows the recipe), as the data pack's
/// recipe advancements do. Named `<namespace>:recipes/<path>`.
pub fn unlock_advancement(name: &'static str, ingredients: &[Ingredient]) -> Definition {
    let mut items: Vec<Item> = ingredients
        .iter()
        .flat_map(Ingredient::items)
        .copied()
        .collect();
    items.sort_by_key(|item| item.id());
    items.dedup();
    let item_names: Vec<&str> = items.iter().map(|item| item.name()).collect();
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    Definition {
        name: leak(format!("{namespace}:recipes/{path}")),
        parent: None,
        display: None,
        criteria: Box::leak(Box::new([
            Criterion {
                name: "has_the_recipe",
                trigger: TriggerType::RecipeUnlocked.name(),
                conditions: Some(leak(serde_json::json!({ "recipes": name }).to_string())),
            },
            Criterion {
                name: "has_ingredient",
                trigger: TriggerType::InventoryChanged.name(),
                conditions: Some(leak(
                    serde_json::json!({ "items": [{ "items": item_names }] }).to_string(),
                )),
            },
        ])),
        requirements: &[&["has_the_recipe", "has_ingredient"]],
        rewards: Rewards {
            recipes: Box::leak(Box::new([name])),
            ..Rewards::NONE
        },
        sends_telemetry_event: false,
        position: [0.0, 0.0],
    }
}

/// A name a feature registers for good: compiled in, or kept for the life of
/// the process (features register once, at startup).
pub fn static_name(name: &Frozen<str>) -> &'static str {
    name.as_static()
        .unwrap_or_else(|| Box::leak(name.to_string().into_boxed_str()))
}

pub fn register_unlocked(
    feature: &'static str,
    registries: &mut Registries,
    name: &'static str,
    recipe: impl Recipe + 'static,
    ingredients: &[Ingredient],
) -> Result<(), LoadError> {
    registries.recipe_entries.register(feature, name, recipe)?;
    let advancement = unlock_advancement(name, ingredients);
    registries
        .advancement_definitions
        .register(feature, advancement.name, advancement)
}

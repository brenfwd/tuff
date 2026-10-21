//! The data pack's recipes as core recipes: the plain data kinds use
//! core's own types, special crafting kinds and armor trims run the
//! vanilla logic in this crate.

use std::sync::{Arc, OnceLock};

use tuff_vanilla_data::recipe::{ALL, CookingRecipe, Recipe, RecipeEntry};
use tuff_vanilla_data::registry::RecipeBookCategory;

use super::display::{category_of, displays_of, group_of};
use super::{Brewing, Cooking, Shaped, Shapeless, SmithingTransform, Stonecutting};
use tuff_core::protocol::recipe::RecipeDisplay;
use tuff_core::world::crafting::{
    self, CookingStation, CraftingInput, Placement, RecipeSet, Registry, SmithingSlots,
};
use tuff_item::item::ItemStack;
use tuff_item::load::LoadError;

/// A data pack recipe of a kind core does not know.
pub struct Vanilla(pub RecipeEntry);

impl crafting::Recipe for Vanilla {
    fn matches(&self, input: &CraftingInput) -> bool {
        super::is_crafting(&self.0.recipe) && super::matches(&self.0.recipe, input)
    }

    fn assemble(&self, input: &CraftingInput) -> Option<ItemStack> {
        super::assemble(&self.0.recipe, input)
    }

    fn remainders(&self, input: &CraftingInput) -> Vec<Option<ItemStack>> {
        super::remaining_items(&self.0.recipe, input)
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        displays_of(&self.0.recipe)
    }

    fn category(&self) -> RecipeBookCategory {
        category_of(&self.0.recipe)
    }

    fn group(&self) -> Option<&str> {
        group_of(&self.0.recipe)
    }

    fn placement(&self) -> Option<Placement> {
        super::placement_ingredients(&self.0.recipe).map(Placement::shapeless)
    }

    fn is_special(&self) -> bool {
        super::is_special(&self.0.recipe)
    }

    fn show_notification(&self) -> bool {
        super::show_notification(&self.0.recipe)
    }

    fn smithing(&self) -> Option<SmithingSlots> {
        match &self.0.recipe {
            Recipe::SmithingTrim(trim) => Some(SmithingSlots {
                template: Some(trim.template.clone()),
                base: trim.base.clone(),
                addition: Some(trim.addition.clone()),
            }),
            _ => None,
        }
    }

    fn smith(
        &self,
        _template: Option<&ItemStack>,
        base: &ItemStack,
        addition: Option<&ItemStack>,
    ) -> Option<ItemStack> {
        match &self.0.recipe {
            Recipe::SmithingTrim(trim) => super::apply_trim(trim, base, addition?),
            _ => None,
        }
    }
}

/// A data pack recipe as a core recipe: the plain data kinds as core's
/// own types, the rest through [`Vanilla`].
pub fn data_recipe(entry: &RecipeEntry) -> Arc<dyn crafting::Recipe> {
    let name = entry.name.clone();
    let cooking = |station, recipe: &CookingRecipe| -> Arc<dyn crafting::Recipe> {
        Arc::new(Cooking {
            name: name.clone(),
            station,
            recipe: recipe.clone(),
        })
    };
    match &entry.recipe {
        Recipe::CraftingShaped(recipe) => Arc::new(Shaped {
            name,
            recipe: recipe.clone(),
        }),
        Recipe::CraftingShapeless(recipe) => Arc::new(Shapeless {
            name,
            recipe: recipe.clone(),
        }),
        Recipe::Smelting(recipe) => cooking(CookingStation::Furnace, recipe),
        Recipe::Blasting(recipe) => cooking(CookingStation::BlastFurnace, recipe),
        Recipe::Smoking(recipe) => cooking(CookingStation::Smoker, recipe),
        Recipe::CampfireCooking(recipe) => cooking(CookingStation::Campfire, recipe),
        Recipe::Stonecutting(recipe) => Arc::new(Stonecutting {
            name,
            recipe: recipe.clone(),
        }),
        Recipe::SmithingTransform(recipe) => Arc::new(SmithingTransform {
            name,
            recipe: recipe.clone(),
        }),
        Recipe::Brewing(recipe) => Arc::new(Brewing {
            name,
            recipe: recipe.clone(),
        }),
        _ => Arc::new(Vanilla(entry.clone())),
    }
}

/// Turns the recipes data packs define into recipes.
pub struct PackRecipes;

impl crafting::DataRecipes for PackRecipes {
    fn recipe(&self, entry: &RecipeEntry) -> Arc<dyn crafting::Recipe> {
        data_recipe(entry)
    }
}

/// Registers every data pack recipe under its name.
pub fn register(registry: &mut Registry, feature: &'static str) -> Result<(), LoadError> {
    for (_, entry) in ALL {
        registry.register_shared(feature, entry.name.clone(), data_recipe(entry))?;
    }
    Ok(())
}

/// The data pack's recipes alone, for code that answers vanilla questions
/// without a server (and tests).
pub fn vanilla_set() -> &'static Arc<RecipeSet> {
    static SET: OnceLock<Arc<RecipeSet>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut registry = Registry::default();
        register(&mut registry, "vanilla-recipes").expect("data pack recipe names are unique");
        Arc::new(RecipeSet::from_registry(&registry))
    })
}

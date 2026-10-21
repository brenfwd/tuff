//! Data recipes for the stations: cooking, stonecutting, smithing
//! transforms and brewing.

use tuff_core::protocol::recipe::{RecipeDisplay, SlotDisplay};
use tuff_core::registry::Registries;
use tuff_core::world::crafting::{
    CookingStation, Placement, Recipe, SmithingSlots, register_unlocked, result_stack, static_name,
    with_input_components,
};
use tuff_item::item::ItemStack;
use tuff_item::load::LoadError;
use tuff_vanilla_data::recipe::{
    BrewingRecipe, CookingCategory, CookingRecipe, Ingredient, PotionIngredient, RecipeResult,
    SmithingTransformRecipe, StonecuttingRecipe,
};
use tuff_vanilla_data::registry::RecipeBookCategory;
use tuff_vanilla_data::{Frozen, Item};

const fn result(item: Item) -> RecipeResult {
    RecipeResult {
        item,
        count: 1,
        components: None,
    }
}

fn slot(ingredient: Ingredient) -> SlotDisplay {
    SlotDisplay::Ingredient(ingredient)
}

fn optional_slot(ingredient: Option<Ingredient>) -> SlotDisplay {
    ingredient.map_or(SlotDisplay::Empty, slot)
}

/// A recipe a furnace, blast furnace, smoker or campfire cooks.
///
/// ```ignore
/// const LEATHER: Cooking = Cooking::new(
///     "example:leather_from_rotten_flesh",
///     CookingStation::Smoker,
///     Ingredient::Item(Item::RottenFlesh),
///     Item::Leather,
/// )
/// .experience(0.35);
/// LEATHER.register(ID, cx.registries())?;
/// ```
#[derive(Clone, Debug)]
pub struct Cooking {
    pub name: Frozen<str>,
    pub station: CookingStation,
    pub recipe: CookingRecipe,
}

impl Cooking {
    pub const fn new(
        name: &'static str,
        station: CookingStation,
        ingredient: Ingredient,
        result_item: Item,
    ) -> Self {
        Self {
            name: Frozen::Static(name),
            station,
            recipe: CookingRecipe {
                category: None,
                group: None,
                ingredient,
                result: result(result_item),
                experience: 0.0,
                cooking_time: station.default_time(),
                show_notification: true,
            },
        }
    }

    pub const fn count(mut self, count: u64) -> Self {
        self.recipe.result.count = count;
        self
    }

    /// Ticks one cooking takes.
    pub const fn time(mut self, ticks: u64) -> Self {
        self.recipe.cooking_time = ticks;
        self
    }

    /// Experience each cooking earns when the result is taken.
    pub const fn experience(mut self, experience: f32) -> Self {
        self.recipe.experience = experience;
        self
    }

    pub const fn category(mut self, category: CookingCategory) -> Self {
        self.recipe.category = Some(category);
        self
    }

    pub const fn group(mut self, group: &'static str) -> Self {
        crate::crafting::set_const(&mut self.recipe.group, Some(Frozen::Static(group)));
        self
    }

    /// Registers the recipe with an advancement that teaches it once the
    /// player holds its ingredient.
    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let ingredient = self.recipe.ingredient.clone();
        let name = static_name(&self.name);
        register_unlocked(feature, registries, name, self, &[ingredient])
    }
}

impl Recipe for Cooking {
    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Furnace {
            ingredient: slot(self.recipe.ingredient.clone()),
            fuel: SlotDisplay::AnyFuel,
            result: SlotDisplay::template(result_stack(&self.recipe.result)),
            station: SlotDisplay::Item(self.station.icon()),
            duration: self.recipe.cooking_time as u32,
            experience: self.recipe.experience,
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        self.station.book_category(self.recipe.category)
    }

    fn group(&self) -> Option<&str> {
        self.recipe.group.as_deref()
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::shapeless(vec![self.recipe.ingredient.clone()]))
    }

    fn cooking(&self) -> Option<(CookingStation, &CookingRecipe)> {
        Some((self.station, &self.recipe))
    }
}

/// A recipe the stonecutter offers for its input.
#[derive(Clone, Debug)]
pub struct Stonecutting {
    pub name: Frozen<str>,
    pub recipe: StonecuttingRecipe,
}

impl Stonecutting {
    pub const fn new(name: &'static str, ingredient: Ingredient, result_item: Item) -> Self {
        Self {
            name: Frozen::Static(name),
            recipe: StonecuttingRecipe {
                ingredient,
                result: result(result_item),
                show_notification: true,
            },
        }
    }

    pub const fn count(mut self, count: u64) -> Self {
        self.recipe.result.count = count;
        self
    }

    /// Registers the recipe with an advancement that teaches it once the
    /// player holds its ingredient.
    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let ingredient = self.recipe.ingredient.clone();
        let name = static_name(&self.name);
        register_unlocked(feature, registries, name, self, &[ingredient])
    }
}

impl Recipe for Stonecutting {
    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Stonecutter {
            input: slot(self.recipe.ingredient.clone()),
            result: SlotDisplay::template(result_stack(&self.recipe.result)),
            station: SlotDisplay::Item(Item::Stonecutter),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        RecipeBookCategory::Stonecutter
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::shapeless(vec![self.recipe.ingredient.clone()]))
    }

    fn stonecutting(&self) -> Option<&StonecuttingRecipe> {
        Some(&self.recipe)
    }
}

/// base's components (enchantments, damage, name).
#[derive(Clone, Debug)]
pub struct SmithingTransform {
    pub name: Frozen<str>,
    pub recipe: SmithingTransformRecipe,
}

impl SmithingTransform {
    pub const fn new(
        name: &'static str,
        template: Ingredient,
        base: Ingredient,
        addition: Ingredient,
        result_item: Item,
    ) -> Self {
        Self {
            name: Frozen::Static(name),
            recipe: SmithingTransformRecipe {
                template: Some(template),
                base,
                addition: Some(addition),
                result: result(result_item),
                show_notification: true,
            },
        }
    }

    /// Registers the recipe with an advancement that teaches it once the
    /// player holds any of its inputs.
    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let r = self.recipe.clone();
        let name = static_name(&self.name);
        let ingredients: Vec<Ingredient> = r
            .template
            .into_iter()
            .chain([r.base])
            .chain(r.addition)
            .collect();
        register_unlocked(feature, registries, name, self, &ingredients)
    }
}

impl Recipe for SmithingTransform {
    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Smithing {
            template: optional_slot(self.recipe.template.clone()),
            base: slot(self.recipe.base.clone()),
            addition: optional_slot(self.recipe.addition.clone()),
            result: SlotDisplay::template(result_stack(&self.recipe.result)),
            station: SlotDisplay::Item(Item::SmithingTable),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        RecipeBookCategory::Smithing
    }

    fn placement(&self) -> Option<Placement> {
        let r = &self.recipe;
        Some(Placement::shapeless(
            r.template
                .iter()
                .chain([&r.base])
                .chain(r.addition.iter())
                .cloned()
                .collect(),
        ))
    }

    fn smithing(&self) -> Option<SmithingSlots> {
        Some(SmithingSlots {
            template: self.recipe.template.clone(),
            base: self.recipe.base.clone(),
            addition: self.recipe.addition.clone(),
        })
    }

    fn smith(
        &self,
        _template: Option<&ItemStack>,
        base: &ItemStack,
        _addition: Option<&ItemStack>,
    ) -> Option<ItemStack> {
        Some(with_input_components(Some(&self.recipe.result), base, 0))
    }
}

/// A brewing stand recipe: what a bottle turns into with a reagent. The
/// recipe book never lists brewing.
#[derive(Clone, Debug)]
pub struct Brewing {
    pub name: Frozen<str>,
    pub recipe: BrewingRecipe,
}

impl Brewing {
    /// `output` usually carries a `minecraft:potion_contents` component.
    pub const fn new(
        name: &'static str,
        input: PotionIngredient,
        reagent: PotionIngredient,
        output: RecipeResult,
    ) -> Self {
        Self {
            name: Frozen::Static(name),
            recipe: BrewingRecipe {
                input,
                reagent,
                output,
            },
        }
    }

    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let name = static_name(&self.name);
        registries.recipe_entries.register(feature, name, self)
    }
}

impl Recipe for Brewing {
    fn is_special(&self) -> bool {
        true
    }

    fn brewing(&self) -> Option<&BrewingRecipe> {
        Some(&self.recipe)
    }
}

//! Shaped and shapeless crafting recipes: the data recipes features (and
//! the data pack) describe with ingredients and a result.

use tuff_core::protocol::recipe::{RecipeDisplay, SlotDisplay};
use tuff_core::registry::Registries;
use tuff_core::world::crafting::{
    CraftingInput, Placement, Recipe, crafting_category, register_unlocked, result_stack,
    static_name,
};
use tuff_item::item::ItemStack;
use tuff_item::load::LoadError;
use tuff_vanilla_data::recipe::{
    CraftingCategory, Ingredient, PatternRows, RecipeResult, ShapedCraftingRecipe,
    ShapelessCraftingRecipe,
};
use tuff_vanilla_data::registry::RecipeBookCategory;
use tuff_vanilla_data::{Frozen, Item};

/// A shaped recipe's pattern with empty rows and columns trimmed away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedPattern {
    pub width: usize,
    pub height: usize,
    pub ingredients: Vec<Option<Ingredient>>,
}

impl ShapedPattern {
    pub fn of(recipe: &ShapedCraftingRecipe) -> Self {
        let rows: Vec<&[u8]> = recipe.pattern.iter().map(|row| row.as_bytes()).collect();
        let non_space = |row: &[u8]| {
            let first = row.iter().position(|b| *b != b' ');
            let last = row.iter().rposition(|b| *b != b' ');
            first.zip(last)
        };
        let mut left = usize::MAX;
        let mut right = 0;
        let mut top = 0;
        let mut trailing = 0;
        for (i, row) in rows.iter().enumerate() {
            match non_space(row) {
                Some((first, last)) => {
                    left = left.min(first);
                    right = right.max(last);
                    trailing = 0;
                }
                None => {
                    if top == i {
                        top += 1;
                    }
                    trailing += 1;
                }
            }
        }
        if trailing == rows.len() {
            return Self {
                width: 0,
                height: 0,
                ingredients: Vec::new(),
            };
        }
        let height = rows.len() - trailing - top;
        let width = right + 1 - left;
        let mut ingredients = Vec::with_capacity(width * height);
        for row in &rows[top..top + height] {
            for &symbol in &row[left..=right] {
                ingredients.push(match symbol {
                    b' ' => None,
                    symbol => recipe.ingredient(symbol as char),
                });
            }
        }
        Self {
            width,
            height,
            ingredients,
        }
    }

    fn count(&self) -> usize {
        self.ingredients.iter().flatten().count()
    }

    fn symmetrical(&self) -> bool {
        (0..self.height)
            .all(|y| (0..self.width).all(|x| self.at(x, y) == self.at(self.width - 1 - x, y)))
    }

    fn at(&self, x: usize, y: usize) -> Option<&Ingredient> {
        self.ingredients[x + y * self.width].as_ref()
    }

    /// Whether `input` holds the pattern, as is or mirrored.
    pub fn matches(&self, input: &CraftingInput) -> bool {
        if input.ingredient_count() != self.count()
            || input.width != self.width
            || input.height != self.height
        {
            return false;
        }
        (!self.symmetrical() && self.matches_flipped(input, true))
            || self.matches_flipped(input, false)
    }

    fn matches_flipped(&self, input: &CraftingInput, flip: bool) -> bool {
        (0..self.height).all(|y| {
            (0..self.width).all(|x| {
                let expected = if flip {
                    self.at(self.width - 1 - x, y)
                } else {
                    self.at(x, y)
                };
                match (expected, input.get(x, y)) {
                    (Some(ingredient), Some(stack)) => ingredient.matches(stack.item),
                    (None, None) => true,
                    _ => false,
                }
            })
        })
    }
}

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

/// A crafting recipe whose ingredients must sit in a pattern, which may be
/// mirrored and placed anywhere in the grid.
///
/// ```ignore
/// const DIAMOND_BLOCK: Shaped = Shaped::new(
///     "example:diamond_block_from_dirt",
///     &["DD", "DD"],
///     &[('D', Ingredient::Item(Item::Dirt))],
///     Item::DiamondBlock,
/// );
/// DIAMOND_BLOCK.register(ID, cx.registries())?;
/// ```
#[derive(Clone, Debug)]
pub struct Shaped {
    pub name: Frozen<str>,
    pub recipe: ShapedCraftingRecipe,
}

impl Shaped {
    /// `pattern` rows use `key`'s symbols; a space is an empty cell.
    pub const fn new(
        name: &'static str,
        pattern: &'static [&'static str],
        key: &'static [(char, Ingredient)],
        result_item: Item,
    ) -> Self {
        Self {
            name: Frozen::Static(name),
            recipe: ShapedCraftingRecipe {
                category: None,
                group: None,
                key: Frozen::Static(key),
                pattern: PatternRows::Static(pattern),
                result: result(result_item),
                show_notification: true,
            },
        }
    }

    pub const fn count(mut self, count: u64) -> Self {
        self.recipe.result.count = count;
        self
    }

    pub const fn category(mut self, category: CraftingCategory) -> Self {
        self.recipe.category = Some(category);
        self
    }

    pub const fn group(mut self, group: &'static str) -> Self {
        crate::crafting::set_const(&mut self.recipe.group, Some(Frozen::Static(group)));
        self
    }

    /// Registers the recipe with an advancement that teaches it once the
    /// player holds any of its ingredients.
    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let ingredients: Vec<Ingredient> = self.recipe.key.iter().map(|(_, i)| i.clone()).collect();
        let name = static_name(&self.name);
        register_unlocked(feature, registries, name, self, &ingredients)
    }

    fn pattern(&self) -> ShapedPattern {
        ShapedPattern::of(&self.recipe)
    }
}

impl Recipe for Shaped {
    fn matches(&self, input: &CraftingInput) -> bool {
        self.pattern().matches(input)
    }

    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(result_stack(&self.recipe.result))
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        let pattern = self.pattern();
        vec![RecipeDisplay::Shaped {
            width: pattern.width as u32,
            height: pattern.height as u32,
            ingredients: pattern
                .ingredients
                .iter()
                .map(|cell| cell.clone().map_or(SlotDisplay::Empty, slot))
                .collect(),
            result: SlotDisplay::template(tuff_core::world::crafting::result_template(
                &self.recipe.result,
            )),
            station: SlotDisplay::Item(Item::CraftingTable),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        crafting_category(self.recipe.category)
    }

    fn group(&self) -> Option<&str> {
        self.recipe.group.as_deref()
    }

    fn placement(&self) -> Option<Placement> {
        let pattern = self.pattern();
        Some(Placement::shaped(
            pattern.width,
            pattern.height,
            &pattern.ingredients,
        ))
    }

    fn show_notification(&self) -> bool {
        self.recipe.show_notification
    }
}

/// A crafting recipe whose ingredients may sit anywhere in the grid.
///
/// ```ignore
/// const DIAMOND: Shapeless = Shapeless::new(
///     "example:diamond_from_dirt",
///     &[Ingredient::Item(Item::Dirt)],
///     Item::Diamond,
/// );
/// DIAMOND.register(ID, cx.registries())?;
/// ```
#[derive(Clone, Debug)]
pub struct Shapeless {
    pub name: Frozen<str>,
    pub recipe: ShapelessCraftingRecipe,
}

impl Shapeless {
    pub const fn new(
        name: &'static str,
        ingredients: &'static [Ingredient],
        result_item: Item,
    ) -> Self {
        Self {
            name: Frozen::Static(name),
            recipe: ShapelessCraftingRecipe {
                category: None,
                group: None,
                ingredients: Frozen::Static(ingredients),
                result: result(result_item),
                show_notification: true,
            },
        }
    }

    pub const fn count(mut self, count: u64) -> Self {
        self.recipe.result.count = count;
        self
    }

    pub const fn category(mut self, category: CraftingCategory) -> Self {
        self.recipe.category = Some(category);
        self
    }

    pub const fn group(mut self, group: &'static str) -> Self {
        crate::crafting::set_const(&mut self.recipe.group, Some(Frozen::Static(group)));
        self
    }

    /// Registers the recipe with an advancement that teaches it once the
    /// player holds any of its ingredients.
    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let ingredients = self.recipe.ingredients.clone();
        let name = static_name(&self.name);
        register_unlocked(feature, registries, name, self, &ingredients)
    }
}

impl Recipe for Shapeless {
    fn matches(&self, input: &CraftingInput) -> bool {
        let ingredients = &self.recipe.ingredients;
        if input.ingredient_count() != ingredients.len() {
            return false;
        }
        if input.items.len() == 1 && ingredients.len() == 1 {
            return input.items[0]
                .as_ref()
                .is_some_and(|stack| ingredients[0].matches(stack.item));
        }
        input.stacked().try_pick(ingredients, 1).is_some()
    }

    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(result_stack(&self.recipe.result))
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Shapeless {
            ingredients: self.recipe.ingredients.iter().cloned().map(slot).collect(),
            result: SlotDisplay::template(tuff_core::world::crafting::result_template(
                &self.recipe.result,
            )),
            station: SlotDisplay::Item(Item::CraftingTable),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        crafting_category(self.recipe.category)
    }

    fn group(&self) -> Option<&str> {
        self.recipe.group.as_deref()
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::shapeless(self.recipe.ingredients.to_vec()))
    }
}

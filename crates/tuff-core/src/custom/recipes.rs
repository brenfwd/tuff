//! Crafting recipes over custom items: ingredients that match a custom
//! item by identity and results built from a definition. Matching is
//! exact: a [`CustomIngredient::Item`] takes only plain stacks of its
//! items, a [`CustomIngredient::Custom`] only that custom item.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::recipe::Ingredient;
use tuff_vanilla_data::registry::RecipeBookCategory;

use super::{CustomItemDef, item_id};
use crate::protocol::recipe::{RecipeDisplay, SlotDisplay};
use crate::registry::Registries;
use crate::world::crafting::{CraftingInput, Placement, Recipe, unlock_advancement};
use tuff_item::item::{ItemSnapshot, ItemStack, MintReason};
use tuff_item::load::LoadError;

/// One grid cell's requirement.
#[derive(Clone, Debug, PartialEq)]
pub enum CustomIngredient {
    /// A plain (not custom) stack of one of these items.
    Item(Ingredient),
    /// The custom item with this id; `display` is how the book shows it.
    Custom { id: String, display: ItemSnapshot },
}

impl CustomIngredient {
    pub fn custom(def: &CustomItemDef) -> Self {
        Self::Custom {
            id: def.id.clone(),
            display: def.template(),
        }
    }

    pub fn matches(&self, stack: &ItemStack) -> bool {
        match self {
            Self::Item(ingredient) => item_id(stack).is_none() && ingredient.matches(stack.item()),
            Self::Custom { id, .. } => item_id(stack) == Some(id.as_str()),
        }
    }

    fn display(&self) -> SlotDisplay {
        match self {
            Self::Item(ingredient) => SlotDisplay::Ingredient(ingredient.clone()),
            Self::Custom { display, .. } => SlotDisplay::Stack(display.clone()),
        }
    }

    /// The items this ingredient is made of, for unlocking.
    fn base(&self) -> Ingredient {
        match self {
            Self::Item(ingredient) => ingredient.clone(),
            Self::Custom { display, .. } => Ingredient::Item(display.item()),
        }
    }
}

impl From<Ingredient> for CustomIngredient {
    fn from(ingredient: Ingredient) -> Self {
        Self::Item(ingredient)
    }
}

impl From<Item> for CustomIngredient {
    fn from(item: Item) -> Self {
        Self::Item(Ingredient::Item(item))
    }
}

impl From<&CustomItemDef> for CustomIngredient {
    fn from(def: &CustomItemDef) -> Self {
        Self::custom(def)
    }
}

fn placement_ingredients(cells: &[&CustomIngredient]) -> Option<Vec<Ingredient>> {
    cells
        .iter()
        .map(|cell| match cell {
            CustomIngredient::Item(ingredient) => Some(ingredient.clone()),
            CustomIngredient::Custom { .. } => None,
        })
        .collect()
}

fn register(
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

/// A shaped crafting recipe over custom ingredients, matched as is or
/// mirrored anywhere in the grid.
#[derive(Debug)]
pub struct CustomShaped {
    pub name: &'static str,
    width: usize,
    height: usize,
    cells: Vec<Option<CustomIngredient>>,
    pub result: ItemStack,
    pub category: RecipeBookCategory,
    pub group: Option<&'static str>,
}

impl CustomShaped {
    /// `pattern` rows use `key`'s symbols; a space is an empty cell. Empty
    /// outer rows and columns are trimmed. `None` for a symbol missing from
    /// `key`, rows of different lengths, or a pattern over 3x3.
    pub fn new(
        name: &'static str,
        pattern: &[&str],
        key: &[(char, CustomIngredient)],
        result: ItemStack,
    ) -> Option<Self> {
        let rows: Vec<Vec<char>> = pattern.iter().map(|row| row.chars().collect()).collect();
        let row_width = rows.first().map_or(0, Vec::len);
        if rows.len() > 3 || row_width > 3 || rows.iter().any(|row| row.len() != row_width) {
            return None;
        }
        let used = |x: usize, y: usize| rows[y][x] != ' ';
        let columns: Vec<usize> = (0..row_width)
            .filter(|x| (0..rows.len()).any(|y| used(*x, y)))
            .collect();
        let lines: Vec<usize> = (0..rows.len())
            .filter(|y| (0..row_width).any(|x| used(x, *y)))
            .collect();
        let (Some(left), Some(right), Some(top), Some(bottom)) =
            (columns.first(), columns.last(), lines.first(), lines.last())
        else {
            return None;
        };
        let mut cells = Vec::new();
        for row in &rows[*top..=*bottom] {
            for symbol in &row[*left..=*right] {
                cells.push(match symbol {
                    ' ' => None,
                    symbol => Some(key.iter().find(|(s, _)| s == symbol)?.1.clone()),
                });
            }
        }
        Some(Self {
            name,
            width: right - left + 1,
            height: bottom - top + 1,
            cells,
            result,
            category: RecipeBookCategory::CraftingMisc,
            group: None,
        })
    }

    fn at(&self, x: usize, y: usize) -> Option<&CustomIngredient> {
        self.cells[x + y * self.width].as_ref()
    }

    fn matches_flipped(&self, input: &CraftingInput, flip: bool) -> bool {
        (0..self.height).all(|y| {
            (0..self.width).all(|x| {
                let column = if flip { self.width - 1 - x } else { x };
                match (self.at(column, y), input.get(x, y)) {
                    (Some(ingredient), Some(stack)) => ingredient.matches(stack),
                    (None, None) => true,
                    _ => false,
                }
            })
        })
    }

    fn ingredients(&self) -> Vec<&CustomIngredient> {
        self.cells.iter().flatten().collect()
    }

    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let bases: Vec<Ingredient> = self.ingredients().iter().map(|i| i.base()).collect();
        register(feature, registries, self.name, self, &bases)
    }
}

impl Recipe for CustomShaped {
    fn matches(&self, input: &CraftingInput) -> bool {
        input.width == self.width
            && input.height == self.height
            && (self.matches_flipped(input, false) || self.matches_flipped(input, true))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "crafting-style result (assemble)"
    )]
    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(ItemStack::mint_like(
            &self.result,
            self.result.count(),
            MintReason::Crafting,
        ))
    }

    fn accepts_custom_items(&self) -> bool {
        true
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Shaped {
            width: self.width as u32,
            height: self.height as u32,
            ingredients: self
                .cells
                .iter()
                .map(|cell| cell.as_ref().map_or(SlotDisplay::Empty, |i| i.display()))
                .collect(),
            result: SlotDisplay::Stack(ItemSnapshot::of(&self.result)),
            station: SlotDisplay::Item(Item::CraftingTable),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        self.category
    }

    fn group(&self) -> Option<&'static str> {
        self.group
    }

    /// Only recipes of plain items can be filled by the book, which picks
    /// stacks by item alone.
    fn placement(&self) -> Option<Placement> {
        placement_ingredients(&self.ingredients())?;
        let cells: Vec<Option<Ingredient>> = self
            .cells
            .iter()
            .map(|cell| cell.as_ref().map(CustomIngredient::base))
            .collect();
        Some(Placement::shaped(self.width, self.height, &cells))
    }
}

/// A shapeless crafting recipe over custom ingredients.
#[derive(Debug)]
pub struct CustomShapeless {
    pub name: &'static str,
    pub ingredients: Vec<CustomIngredient>,
    pub result: ItemStack,
    pub category: RecipeBookCategory,
    pub group: Option<&'static str>,
}

impl CustomShapeless {
    pub fn new(name: &'static str, ingredients: Vec<CustomIngredient>, result: ItemStack) -> Self {
        Self {
            name,
            ingredients,
            result,
            category: RecipeBookCategory::CraftingMisc,
            group: None,
        }
    }

    pub fn register(
        self,
        feature: &'static str,
        registries: &mut Registries,
    ) -> Result<(), LoadError> {
        let bases: Vec<Ingredient> = self.ingredients.iter().map(|i| i.base()).collect();
        register(feature, registries, self.name, self, &bases)
    }
}

/// Whether every stack can take a distinct ingredient (augmenting paths).
fn assign(stacks: &[&ItemStack], ingredients: &[CustomIngredient]) -> bool {
    fn place(
        stack: usize,
        stacks: &[&ItemStack],
        ingredients: &[CustomIngredient],
        owner: &mut [Option<usize>],
        seen: &mut [bool],
    ) -> bool {
        for (index, ingredient) in ingredients.iter().enumerate() {
            if seen[index] || !ingredient.matches(stacks[stack]) {
                continue;
            }
            seen[index] = true;
            if owner[index].is_none_or(|other| place(other, stacks, ingredients, owner, seen)) {
                owner[index] = Some(stack);
                return true;
            }
        }
        false
    }
    let mut owner = vec![None; ingredients.len()];
    (0..stacks.len()).all(|stack| {
        let mut seen = vec![false; ingredients.len()];
        place(stack, stacks, ingredients, &mut owner, &mut seen)
    })
}

impl Recipe for CustomShapeless {
    fn matches(&self, input: &CraftingInput) -> bool {
        let stacks: Vec<&ItemStack> = input.stacks().collect();
        stacks.len() == self.ingredients.len() && assign(&stacks, &self.ingredients)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "crafting-style result (assemble)"
    )]
    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(ItemStack::mint_like(
            &self.result,
            self.result.count(),
            MintReason::Crafting,
        ))
    }

    fn accepts_custom_items(&self) -> bool {
        true
    }

    fn displays(&self) -> Vec<RecipeDisplay> {
        vec![RecipeDisplay::Shapeless {
            ingredients: self.ingredients.iter().map(|i| i.display()).collect(),
            result: SlotDisplay::Stack(ItemSnapshot::of(&self.result)),
            station: SlotDisplay::Item(Item::CraftingTable),
        }]
    }

    fn category(&self) -> RecipeBookCategory {
        self.category
    }

    fn group(&self) -> Option<&'static str> {
        self.group
    }

    fn placement(&self) -> Option<Placement> {
        let cells: Vec<&CustomIngredient> = self.ingredients.iter().collect();
        Some(Placement::shapeless(placement_ingredients(&cells)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom::Definitions;
    use crate::world::crafting::RecipeSet;
    use std::sync::Arc;
    use tuff_item::item::MintReason;

    fn grid(cells: &[Option<ItemStack>], width: usize) -> CraftingInput<'_> {
        CraftingInput::positioned(width, cells.len() / width, cells).input
    }

    fn ruby() -> CustomItemDef {
        CustomItemDef::new("ex:ruby", Item::Emerald).with_model("ex:ruby")
    }

    #[tuff_core::watchdog]
    #[test]
    fn shaped_matches_custom_ingredients_exactly() {
        let ruby = ruby();
        let crown = CustomItemDef::new("ex:crown", Item::GoldenHelmet).stack(1);
        let recipe = CustomShaped::new(
            "ex:crown",
            &["GRG", "G G"],
            &[('G', Item::GoldIngot.into()), ('R', (&ruby).into())],
            crown.test_copy(),
        )
        .unwrap();
        let gold = || Some(ItemStack::mint(Item::GoldIngot, 1, MintReason::Test));
        let cells = [
            gold(),
            Some(ruby.stack(1)),
            gold(),
            gold(),
            None,
            gold(),
            None,
            None,
            None,
        ];
        assert!(recipe.matches(&grid(&cells, 3)));
        assert_eq!(recipe.assemble(&grid(&cells, 3)), Some(crown));
        let mut plain = cells
            .iter()
            .map(|s| s.as_ref().map(ItemStack::test_copy))
            .collect::<Vec<_>>();
        plain[1] = Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test));
        assert!(!recipe.matches(&grid(&plain, 3)));
        let mut custom_gold = cells
            .iter()
            .map(|s| s.as_ref().map(ItemStack::test_copy))
            .collect::<Vec<_>>();
        custom_gold[0] = Some(CustomItemDef::new("ex:fool", Item::GoldIngot).stack(1));
        assert!(!recipe.matches(&grid(&custom_gold, 3)));
        assert!(recipe.placement().is_none());
        assert!(matches!(
            &recipe.displays()[0],
            RecipeDisplay::Shaped { ingredients, .. } if matches!(ingredients[1], SlotDisplay::Stack(_))
        ));
        assert!(
            CustomShaped::new(
                "ex:bad",
                &["X"],
                &[],
                ItemStack::mint(Item::Stone, 1, MintReason::Test)
            )
            .is_none()
        );
        assert!(
            CustomShaped::new(
                "ex:bad",
                &["AB", "A"],
                &[],
                ItemStack::mint(Item::Stone, 1, MintReason::Test)
            )
            .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn shaped_mirrors_and_trims() {
        let recipe = CustomShaped::new(
            "ex:hook",
            &["  ", "AB"],
            &[('A', Item::Stick.into()), ('B', Item::String.into())],
            ItemStack::mint(Item::TripwireHook, 1, MintReason::Test),
        )
        .unwrap();
        let mirrored = [
            Some(ItemStack::mint(Item::String, 1, MintReason::Test)),
            Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)),
        ];
        assert!(recipe.matches(&grid(&mirrored, 2)));
        assert!(recipe.placement().is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn shapeless_assigns_each_stack_one_ingredient() {
        let ruby = ruby();
        let recipe = CustomShapeless::new(
            "ex:rubies",
            vec![
                (&ruby).into(),
                Ingredient::any_of(&[Item::Emerald, Item::Diamond]).into(),
            ],
            ItemStack::mint(Item::Diamond, 2, MintReason::Test),
        );
        let both = [
            Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test)),
            Some(ruby.stack(1)),
        ];
        assert!(recipe.matches(&grid(&both, 2)));
        let two_rubies = [Some(ruby.stack(1)), Some(ruby.stack(1))];
        assert!(!recipe.matches(&grid(&two_rubies, 2)));
        assert!(!recipe.matches(&grid(&[Some(ruby.stack(1)), None], 2)));
        assert!(recipe.placement().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_recipes_refuse_suppressed_custom_items() {
        use crate::world::crafting::test_recipe::Shapeless;
        let mut defs = Definitions::default();
        defs.register_item("ex", ruby()).unwrap();
        defs.register_item(
            "ex",
            CustomItemDef::new("ex:gem", Item::Emerald).inheriting(),
        )
        .unwrap();
        let content = defs.resolve().unwrap();
        let block = Shapeless::new(
            "ex:emerald_dust",
            &[Ingredient::Item(Item::Emerald)],
            Item::GreenDye,
        );
        let set = RecipeSet::new([("ex:emerald_dust".into(), Arc::new(block) as Arc<dyn Recipe>)])
            .with_custom(content.clone());
        let input = |stack: ItemStack| grid(vec![Some(stack)].leak(), 1);
        assert!(
            set.find(&input(ItemStack::mint(Item::Emerald, 1, MintReason::Test)))
                .is_some()
        );
        assert!(
            set.find(&input(content.stack("ex:ruby", 1).unwrap()))
                .is_none()
        );
        assert!(
            set.find(&input(content.stack("ex:gem", 1).unwrap()))
                .is_some()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn shapeless_display_shows_custom_stacks() {
        let ruby = ruby();
        let result = CustomItemDef::new("ex:crown", Item::GoldenHelmet).stack(1);
        let recipe = CustomShapeless::new(
            "ex:crown",
            vec![Item::GoldIngot.into(), (&ruby).into()],
            ItemStack::mint_like(&result, result.count(), MintReason::Test),
        );
        let displays = recipe.displays();
        let [
            RecipeDisplay::Shapeless {
                ingredients,
                result: shown,
                ..
            },
        ] = displays.as_slice()
        else {
            panic!("one shapeless display");
        };
        assert_eq!(shown, &SlotDisplay::Stack(ItemSnapshot::of(&result)));
        assert!(matches!(ingredients[0], SlotDisplay::Ingredient(_)));
        assert_eq!(ingredients[1], SlotDisplay::Stack(ruby.template()));
    }
}

//! Every recipe registered in the built-in data pack, one Rust variant per
//! `minecraft:recipe_serializer` (see [`Recipe`]).

use std::sync::Arc;

use crate::Frozen;
use crate::Frozen::Static;
use crate::registry::{Item, Potion};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ingredient {
    Item(Item),
    /// Matches any of these items: an inline list of item names.
    AnyOf(Frozen<[Item]>),
    /// A `#tag` reference: the tag and its members when the recipe loaded.
    Tag(Frozen<str>, Frozen<[Item]>),
}

impl Ingredient {
    /// Any of these compiled-in items.
    pub const fn any_of(items: &'static [Item]) -> Self {
        Self::AnyOf(Frozen::Static(items))
    }

    pub fn matches(&self, item: Item) -> bool {
        match self {
            Self::Item(single) => *single == item,
            Self::AnyOf(items) | Self::Tag(_, items) => items.contains(&item),
        }
    }

    /// The items this matches.
    pub fn items(&self) -> &[Item] {
        match self {
            Self::Item(single) => std::slice::from_ref(single),
            Self::AnyOf(items) | Self::Tag(_, items) => items,
        }
    }
}

impl Recipe {
    /// Calls `f` on every ingredient the recipe holds, in place.
    pub fn ingredients_mut(&mut self, f: &mut dyn FnMut(&mut Ingredient)) {
        fn each<T: Clone>(list: &mut Frozen<[T]>, mut f: impl FnMut(&mut T)) {
            let mut owned = list.to_vec();
            owned.iter_mut().for_each(&mut f);
            *list = owned.into();
        }
        match self {
            Self::CraftingShaped(r) => each(&mut r.key, |(_, i)| f(i)),
            Self::CraftingShapeless(r) => each(&mut r.ingredients, |i| f(i)),
            Self::Smelting(r) | Self::Blasting(r) | Self::Smoking(r) | Self::CampfireCooking(r) => {
                f(&mut r.ingredient)
            }
            Self::Stonecutting(r) => f(&mut r.ingredient),
            Self::Brewing(r) => {
                f(&mut r.input.ingredient);
                f(&mut r.reagent.ingredient);
            }
            Self::SmithingTransform(r) => {
                if let Some(template) = &mut r.template {
                    f(template);
                }
                f(&mut r.base);
                if let Some(addition) = &mut r.addition {
                    f(addition);
                }
            }
            Self::SmithingTrim(r) => {
                f(&mut r.template);
                f(&mut r.base);
                f(&mut r.addition);
            }
            Self::CraftingTransmute(r) => {
                f(&mut r.input);
                f(&mut r.material);
            }
            Self::CraftingDye(r) => {
                f(&mut r.dye);
                f(&mut r.target);
            }
            Self::CraftingSpecialBannerDuplicate(r) => f(&mut r.banner),
            Self::CraftingSpecialBookCloning(r) => {
                f(&mut r.source);
                f(&mut r.material);
            }
            Self::CraftingSpecialRepairItem => {}
            Self::CraftingSpecialFireworkRocket(r) => {
                f(&mut r.fuel);
                f(&mut r.shell);
                f(&mut r.star);
            }
            Self::CraftingSpecialFireworkStar(r) => {
                f(&mut r.dye);
                f(&mut r.fuel);
                f(&mut r.trail);
                f(&mut r.twinkle);
                each(&mut r.shapes, |(_, i)| f(i));
            }
            Self::CraftingSpecialFireworkStarFade(r) => {
                f(&mut r.dye);
                f(&mut r.target);
            }
            Self::CraftingSpecialMapExtending(r) => {
                f(&mut r.map);
                f(&mut r.material);
            }
            Self::CraftingSpecialShieldDecoration(r) => {
                f(&mut r.banner);
                f(&mut r.target);
            }
            Self::CraftingDecoratedPot(r) => {
                f(&mut r.back);
                f(&mut r.front);
                f(&mut r.left);
                f(&mut r.right);
            }
            Self::CraftingImbue(r) => {
                f(&mut r.source);
                f(&mut r.material);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeResult {
    pub item: Item,
    pub count: u64,
    /// Raw JSON of a `minecraft:*` data component override map, if the
    /// recipe overrides one of the result's default components.
    pub components: Option<Frozen<str>>,
}

#[derive(Clone, Debug)]
pub struct RecipeEntry {
    pub name: Frozen<str>,
    pub recipe: Recipe,
}

impl RecipeEntry {
    pub fn get(name: &str) -> Option<&'static RecipeEntry> {
        crate::lookup(ALL, name)
    }
}

#[derive(Clone, Debug)]
pub enum Recipe {
    CraftingShaped(ShapedCraftingRecipe),
    CraftingShapeless(ShapelessCraftingRecipe),
    Smelting(CookingRecipe),
    Blasting(CookingRecipe),
    Smoking(CookingRecipe),
    CampfireCooking(CookingRecipe),
    Stonecutting(StonecuttingRecipe),
    Brewing(BrewingRecipe),
    SmithingTransform(SmithingTransformRecipe),
    SmithingTrim(SmithingTrimRecipe),
    CraftingTransmute(TransmuteRecipe),
    CraftingDye(DyeRecipe),
    CraftingSpecialBannerDuplicate(BannerDuplicateRecipe),
    CraftingSpecialBookCloning(BookCloningRecipe),
    /// Repairs two items of the same kind by combining their durability.
    /// Takes no parameters: which items qualify is fixed game logic, not data.
    CraftingSpecialRepairItem,
    CraftingSpecialFireworkRocket(FireworkRocketRecipe),
    CraftingSpecialFireworkStar(FireworkStarRecipe),
    CraftingSpecialFireworkStarFade(FireworkStarFadeRecipe),
    CraftingSpecialMapExtending(MapExtendingRecipe),
    CraftingSpecialShieldDecoration(ShieldDecorationRecipe),
    CraftingDecoratedPot(DecoratedPotRecipe),
    CraftingImbue(ImbueRecipe),
}

#[derive(Clone, Debug)]
pub struct ShapedCraftingRecipe {
    /// `None` when the recipe does not specify one; the game then uses `misc`.
    pub category: Option<CraftingCategory>,
    pub group: Option<Frozen<str>>,
    /// `pattern` symbol to ingredient, e.g. `('X', Ingredient::Item(Item::Stick))`.
    pub key: Frozen<[(char, Ingredient)]>,
    /// Each row of the crafting grid, top to bottom; a space is an empty cell.
    pub pattern: PatternRows,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

impl ShapedCraftingRecipe {
    /// The ingredient a `pattern` symbol maps to, or `None` for a space.
    pub fn ingredient(&self, symbol: char) -> Option<Ingredient> {
        self.key
            .iter()
            .find(|(s, _)| *s == symbol)
            .map(|(_, i)| i.clone())
    }
}

#[derive(Clone, Debug)]
pub struct ShapelessCraftingRecipe {
    /// `None` when the recipe does not specify one; the game then uses `misc`.
    pub category: Option<CraftingCategory>,
    pub group: Option<Frozen<str>>,
    pub ingredients: Frozen<[Ingredient]>,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct CookingRecipe {
    /// `None` when the recipe does not specify one; the game then uses `misc`.
    pub category: Option<CookingCategory>,
    pub group: Option<Frozen<str>>,
    pub ingredient: Ingredient,
    pub result: RecipeResult,
    pub experience: f32,
    pub cooking_time: u64,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct StonecuttingRecipe {
    pub ingredient: Ingredient,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct BrewingRecipe {
    /// What a brewing stand's bottle slot must hold.
    pub input: PotionIngredient,
    /// What its top slot must hold.
    pub reagent: PotionIngredient,
    pub output: RecipeResult,
}

/// An item, optionally only with one of the listed potions.
#[derive(Clone, Debug)]
pub struct PotionIngredient {
    pub ingredient: Ingredient,
    pub potions: Option<Frozen<[Potion]>>,
}

#[derive(Clone, Debug)]
pub struct SmithingTransformRecipe {
    pub template: Option<Ingredient>,
    pub base: Ingredient,
    pub addition: Option<Ingredient>,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct SmithingTrimRecipe {
    pub template: Ingredient,
    pub base: Ingredient,
    pub addition: Ingredient,
    /// A `minecraft:trim_pattern` entry name. This is a data-driven registry
    /// (see [`crate::data_registry`]), so it is referenced by name rather
    /// than a fixed enum.
    pub pattern: &'static str,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct TransmuteRecipe {
    /// `None` when the recipe does not specify one; the game then uses `misc`.
    pub category: Option<CraftingCategory>,
    pub group: Option<Frozen<str>>,
    pub input: Ingredient,
    pub material: Ingredient,
    /// How many `material` items the grid may hold; `None` means exactly one.
    pub material_count: Option<CountRange>,
    /// Whether the result's count grows by the number of `material` items used.
    pub add_material_count_to_result: bool,
    /// `None` when the output is the `input` item itself, with its data
    /// components carried over (e.g. `map_cloning`).
    pub result: Option<RecipeResult>,
    /// Count of the output when `result` is `None` (the result object had no id).
    pub keep_count: u64,
    /// Component overrides applied to the kept input item when `result` is `None`.
    pub keep_components: Option<Frozen<str>>,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountRange {
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Debug)]
pub struct DyeRecipe {
    pub category: Option<CraftingCategory>,
    pub group: Option<Frozen<str>>,
    pub dye: Ingredient,
    pub target: Ingredient,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

#[derive(Clone, Debug)]
pub struct BannerDuplicateRecipe {
    pub banner: Ingredient,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct BookCloningRecipe {
    pub source: Ingredient,
    pub material: Ingredient,
    /// Written book generations that may be copied.
    pub allowed_generations: CountRange,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct FireworkRocketRecipe {
    pub fuel: Ingredient,
    pub shell: Ingredient,
    pub star: Ingredient,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct FireworkStarRecipe {
    pub dye: Ingredient,
    pub fuel: Ingredient,
    pub trail: Ingredient,
    pub twinkle: Ingredient,
    /// `(shape name, ingredient)`, e.g. `("star", Ingredient::Item(Item::GoldNugget))`.
    pub shapes: Frozen<[(Frozen<str>, Ingredient)]>,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct FireworkStarFadeRecipe {
    pub dye: Ingredient,
    pub target: Ingredient,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct MapExtendingRecipe {
    pub map: Ingredient,
    pub material: Ingredient,
}

#[derive(Clone, Debug)]
pub struct ShieldDecorationRecipe {
    pub banner: Ingredient,
    pub target: Ingredient,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct DecoratedPotRecipe {
    pub back: Ingredient,
    pub front: Ingredient,
    pub left: Ingredient,
    pub right: Ingredient,
    pub result: RecipeResult,
}

#[derive(Clone, Debug)]
pub struct ImbueRecipe {
    pub category: Option<CraftingCategory>,
    pub group: Option<Frozen<str>>,
    pub source: Ingredient,
    pub material: Ingredient,
    pub result: RecipeResult,
    /// Whether unlocking this recipe shows a toast notification.
    pub show_notification: bool,
}

/// A shaped recipe's grid rows, top to bottom.
#[derive(Clone)]
pub enum PatternRows {
    Static(&'static [&'static str]),
    Shared(Arc<[Box<str>]>),
}

impl PatternRows {
    pub fn len(&self) -> usize {
        match self {
            Self::Static(rows) => rows.len(),
            Self::Shared(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, row: usize) -> Option<&str> {
        match self {
            Self::Static(rows) => rows.get(row).copied(),
            Self::Shared(rows) => rows.get(row).map(|row| &**row),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).filter_map(|row| self.get(row))
    }
}

impl std::ops::Index<usize> for PatternRows {
    type Output = str;

    fn index(&self, row: usize) -> &str {
        self.get(row).expect("pattern row out of range")
    }
}

impl std::fmt::Debug for PatternRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl From<Vec<String>> for PatternRows {
    fn from(rows: Vec<String>) -> Self {
        Self::Shared(rows.into_iter().map(String::into_boxed_str).collect())
    }
}

include!(concat!(env!("OUT_DIR"), "/recipes.rs"));

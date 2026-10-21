//! Every recipe a server knows, by name: the crafting grid's matching and
//! consumption, the recipe book's entries and click-to-fill, and what the
//! furnaces, campfires, stonecutter, smithing table and brewing stand
//! make. Recipes are [`Recipe`] trait objects features register into
//! [`Registry`]; the concrete kinds live in the vanilla-recipes feature.

pub mod brewing;
pub mod place;
pub mod smelting;
pub mod stacked;
mod station;
mod unlock;

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

use tuff_vanilla_data::recipe::{
    BrewingRecipe, CookingRecipe, Ingredient, RecipeResult, StonecuttingRecipe,
};
use tuff_vanilla_data::registry::RecipeBookCategory;
use tuff_vanilla_data::{Frozen, Item, tag};

use crate::protocol::recipe::{DisplayEntry, RecipeDisplay, SlotDisplay};
use crate::world::block_entity::{Campfire, ContainerKind, Furnace};
use stacked::StackedContents;
use tuff_item::item::{Components, ItemStack};
use tuff_item::load::{LoadError, LoadErrorKind};
use tuff_nbt::Compound;

pub use station::{CookingStation, SmithingSlots};
pub use unlock::{crafting_category, register_unlocked, static_name, unlock_advancement};

const MAX_RESULT_COUNT: u64 = 99;

/// One recipe. A crafting grid recipe overrides [`Recipe::matches`] and
/// [`Recipe::assemble`]; a station recipe returns its data from the
/// station's method; the rest describes it to the recipe book.
pub trait Recipe: Send + Sync {
    /// Whether a crafting grid holding `input` makes this recipe.
    fn matches(&self, _input: &CraftingInput) -> bool {
        false
    }

    /// What `input` makes; `None` when it matches but makes nothing.
    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        None
    }

    /// What each cell of `input` leaves behind once the result is taken.
    /// One item of every occupied cell is always used up first.
    fn remainders(&self, input: &CraftingInput) -> Vec<Option<ItemStack>> {
        input.crafting_remainders()
    }

    /// Its recipe book entries; none keeps it out of the book.
    fn displays(&self) -> Vec<RecipeDisplay> {
        Vec::new()
    }

    fn category(&self) -> RecipeBookCategory {
        RecipeBookCategory::CraftingMisc
    }

    /// Recipes sharing a group share one book entry.
    fn group(&self) -> Option<&str> {
        None
    }

    /// How the recipe book fills a grid with it; `None` when it cannot.
    fn placement(&self) -> Option<Placement> {
        None
    }

    /// Recipes the book never learns (their inputs are item logic, not a
    /// fixed ingredient list).
    fn is_special(&self) -> bool {
        false
    }

    /// Whether learning it shows a toast.
    fn show_notification(&self) -> bool {
        true
    }

    /// What a furnace, blast furnace, smoker or campfire cooks with it.
    fn cooking(&self) -> Option<(CookingStation, &CookingRecipe)> {
        None
    }

    /// What a stonecutter cuts with it.
    fn stonecutting(&self) -> Option<&StonecuttingRecipe> {
        None
    }

    /// What a smithing table's three slots must hold for it.
    fn smithing(&self) -> Option<SmithingSlots> {
        None
    }

    /// What a smithing table makes from slots [`Recipe::smithing`] accepts;
    /// `None` when it matches but makes nothing.
    fn smith(
        &self,
        _template: Option<&ItemStack>,
        _base: &ItemStack,
        _addition: Option<&ItemStack>,
    ) -> Option<ItemStack> {
        None
    }

    /// What a brewing stand brews with it.
    fn brewing(&self) -> Option<&BrewingRecipe> {
        None
    }

    /// Whether a grid holding custom items whose base behaviour is off
    /// may make it (it matches custom items itself).
    fn accepts_custom_items(&self) -> bool {
        false
    }
}

/// Turns a data pack recipe into a recipe, for recipes packs define at
/// runtime; filled by the feature that knows the special crafting kinds.
pub trait DataRecipes: Send + Sync {
    fn recipe(&self, entry: &tuff_vanilla_data::recipe::RecipeEntry) -> Arc<dyn Recipe>;
}

/// The occupied part of a crafting grid, trimmed to its bounding box the
/// way the game presents grids to recipes.
#[derive(Debug, Clone, PartialEq)]
pub struct CraftingInput {
    pub width: usize,
    pub height: usize,
    pub items: Vec<Option<ItemStack>>,
}

/// What a crafter made.
#[derive(Debug, Clone)]
pub struct Crafted {
    pub recipe: Frozen<str>,
    pub result: ItemStack,
    pub remainders: Vec<ItemStack>,
}

/// A trimmed input plus where it sat in the full grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Positioned {
    pub input: CraftingInput,
    pub left: usize,
    pub top: usize,
}

impl CraftingInput {
    pub const EMPTY: Self = Self {
        width: 0,
        height: 0,
        items: Vec::new(),
    };

    pub fn positioned(width: usize, height: usize, slots: &[Option<ItemStack>]) -> Positioned {
        let empty = Positioned {
            input: Self::EMPTY,
            left: 0,
            top: 0,
        };
        if width == 0 || height == 0 {
            return empty;
        }
        let (mut left, mut right, mut top, mut bottom) = (width - 1, 0, height - 1, 0);
        let mut any = false;
        for y in 0..height {
            let mut row_empty = true;
            for x in 0..width {
                if slots[x + y * width].is_some() {
                    left = left.min(x);
                    right = right.max(x);
                    row_empty = false;
                }
            }
            if !row_empty {
                top = top.min(y);
                bottom = bottom.max(y);
                any = true;
            }
        }
        if !any {
            return empty;
        }
        let (new_width, new_height) = (right - left + 1, bottom - top + 1);
        let items = if (new_width, new_height) == (width, height) {
            slots.to_vec()
        } else {
            let mut items = Vec::with_capacity(new_width * new_height);
            for y in 0..new_height {
                for x in 0..new_width {
                    items.push(slots[x + left + (y + top) * width].clone());
                }
            }
            items
        };
        Positioned {
            input: Self {
                width: new_width,
                height: new_height,
                items,
            },
            left,
            top,
        }
    }

    pub fn get(&self, x: usize, y: usize) -> Option<&ItemStack> {
        self.items[x + y * self.width].as_ref()
    }

    pub fn ingredient_count(&self) -> usize {
        self.items.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.ingredient_count() == 0
    }

    pub fn stacks(&self) -> impl Iterator<Item = &ItemStack> {
        self.items.iter().flatten()
    }

    /// One of each occupied cell's item, for matching shapeless lists.
    pub fn stacked(&self) -> StackedContents {
        let mut contents = StackedContents::default();
        for stack in self.stacks() {
            contents.account(stack.item, 1);
        }
        contents
    }

    /// Each cell's item crafting remainder (a bucket for a milk bucket).
    pub fn crafting_remainders(&self) -> Vec<Option<ItemStack>> {
        self.items
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .and_then(|stack| stack.item.crafting_remainder())
                    .map(|(item, count)| ItemStack::new(item, count))
            })
            .collect()
    }
}

/// How the recipe book lays a recipe's ingredients into a grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The ingredients the book must find, in the order they fill the grid.
    pub ingredients: Vec<Ingredient>,
    /// Cells, row by row over `size` (or one per ingredient), naming the
    /// ingredient each takes; `None` cells stay empty.
    pub layout: Vec<Option<usize>>,
    /// A shaped recipe's width and height.
    pub size: Option<(usize, usize)>,
}

impl Placement {
    pub fn shapeless(ingredients: Vec<Ingredient>) -> Self {
        Self {
            layout: (0..ingredients.len()).map(Some).collect(),
            ingredients,
            size: None,
        }
    }

    pub fn shaped(width: usize, height: usize, cells: &[Option<Ingredient>]) -> Self {
        let mut ingredients = Vec::new();
        let layout = cells
            .iter()
            .map(|cell| {
                cell.as_ref().map(|ingredient| {
                    ingredients.push(ingredient.clone());
                    ingredients.len() - 1
                })
            })
            .collect();
        Self {
            ingredients,
            layout,
            size: Some((width, height)),
        }
    }
}

/// The stack a data recipe's result describes.
pub fn result_stack(result: &RecipeResult) -> ItemStack {
    let count = result.count.clamp(1, MAX_RESULT_COUNT) as u8;
    let components = result
        .components
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| tuff_item::registries::codecs::json_to_nbt(&value))
        .and_then(|tag| tag.as_compound().map(Components::from_nbt))
        .unwrap_or_default();
    ItemStack::new(result.item, count).with_components(components)
}

/// A result stack built over an input's components: the input's component
/// patch first, the recipe result's own components on top. With no
/// result, the input item itself.
pub fn with_input_components(
    result: Option<&RecipeResult>,
    input: &ItemStack,
    extra_count: u64,
) -> ItemStack {
    let (item, count, overrides) = match result {
        Some(result) => (
            result.item,
            result.count + extra_count,
            result_stack(result).components,
        ),
        None => (input.item, 1 + extra_count, Components::default()),
    };
    let mut components = input.components.clone();
    for (key, value) in overrides.to_nbt() {
        components.other.remove(&key);
        if let Some(id) = key.strip_prefix('!') {
            components.removed.push(id.to_owned());
        } else {
            let mut patch = Components::from_nbt(&Compound::new().with(key, value));
            let other = std::mem::take(&mut patch.other);
            merge_typed(&mut components, patch);
            for (key, value) in other {
                components.other.insert(key, value);
            }
        }
    }
    components.removed.sort();
    components.removed.dedup();
    ItemStack::new(item, count.clamp(1, MAX_RESULT_COUNT) as u8).with_components(components)
}

fn merge_typed(into: &mut Components, from: Components) {
    let from = from.into_patch();
    macro_rules! take {
        ($($field:ident),*) => {
            $(if from.$field.is_some() {
                into.$field = from.$field;
            })*
        };
    }
    take!(
        damage,
        max_damage,
        custom_name,
        enchantments,
        stored_enchantments,
        potion_contents,
        repair_cost,
        container,
        dyed_color,
        base_color,
        banner_patterns,
        trim,
        fireworks,
        firework_explosion,
        writable_book,
        written_book,
        suspicious_stew_effects
    );
    into.unbreakable |= from.unbreakable;
}

struct Entry {
    recipe: Arc<dyn Recipe>,
    registered_by: &'static str,
}

/// Every recipe features registered, by name.
#[derive(Default)]
pub struct Registry {
    entries: BTreeMap<Frozen<str>, Entry>,
    frozen: bool,
}

impl Registry {
    /// Adds a recipe. Errors if `name` is taken; use [`Self::replace`] to
    /// override one on purpose.
    pub fn register(
        &mut self,
        feature: &'static str,
        name: impl Into<Frozen<str>>,
        recipe: impl Recipe + 'static,
    ) -> Result<(), LoadError> {
        self.register_shared(feature, name, Arc::new(recipe))
    }

    /// [`Self::register`] for a recipe already behind an `Arc`.
    pub fn register_shared(
        &mut self,
        feature: &'static str,
        name: impl Into<Frozen<str>>,
        recipe: Arc<dyn Recipe>,
    ) -> Result<(), LoadError> {
        let name = name.into();
        self.check_open(feature)?;
        if let Some(existing) = self.entries.get(&*name) {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Conflict {
                    registry: "recipes",
                    key: name.to_string(),
                    first_registered_by: existing.registered_by,
                },
            });
        }
        self.insert(feature, name, recipe);
        Ok(())
    }

    /// Adds a recipe, replacing any registered under `name`.
    pub fn replace(
        &mut self,
        feature: &'static str,
        name: impl Into<Frozen<str>>,
        recipe: impl Recipe + 'static,
    ) -> Result<(), LoadError> {
        self.check_open(feature)?;
        self.insert(feature, name.into(), Arc::new(recipe));
        Ok(())
    }

    /// Drops a recipe; true if one was registered under `name`.
    pub fn remove(&mut self, feature: &'static str, name: &str) -> Result<bool, LoadError> {
        self.check_open(feature)?;
        Ok(self.entries.remove(name).is_some())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every recipe by name, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&Frozen<str>, &Arc<dyn Recipe>)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name, &entry.recipe))
    }

    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    fn check_open(&self, feature: &'static str) -> Result<(), LoadError> {
        if self.frozen {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other("recipes are frozen; registration is closed".into()),
            });
        }
        Ok(())
    }

    fn insert(&mut self, feature: &'static str, name: Frozen<str>, recipe: Arc<dyn Recipe>) {
        self.entries.insert(
            name,
            Entry {
                recipe,
                registered_by: feature,
            },
        );
    }
}

struct Indexed {
    recipe: usize,
    displays: Range<usize>,
}

/// The recipe property sets clients are told about, in the game's order:
/// what each smithing slot, cooking block and brewing slot accepts.
const PROPERTY_SETS: [&str; 9] = [
    "smithing_base",
    "smithing_template",
    "smithing_addition",
    "furnace_input",
    "blast_furnace_input",
    "smoker_input",
    "campfire_input",
    "brewing_input",
    "brewing_reagent",
];

/// A server's recipes, numbered for the recipe book: display ids follow
/// recipe name order, so they are stable for one set of recipes. Station
/// lookups also go in name order, so the first matching recipe wins.
pub struct RecipeSet {
    recipes: Vec<(Frozen<str>, Arc<dyn Recipe>)>,
    displays: Vec<DisplayEntry>,
    by_name: HashMap<Frozen<str>, Indexed>,
    cooking: Vec<usize>,
    stonecutting: Vec<usize>,
    smithing: Vec<usize>,
    brewing: Vec<usize>,
    property_sets: Vec<(&'static str, Vec<Item>)>,
    custom: Arc<crate::custom::CustomContent>,
}

impl Default for RecipeSet {
    fn default() -> Self {
        Self::new(std::iter::empty())
    }
}

impl std::fmt::Debug for RecipeSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeSet")
            .field("recipes", &self.recipes.len())
            .field("displays", &self.displays.len())
            .finish()
    }
}

impl RecipeSet {
    pub fn new(recipes: impl IntoIterator<Item = (Frozen<str>, Arc<dyn Recipe>)>) -> Self {
        let mut recipes: Vec<_> = recipes.into_iter().collect();
        recipes.sort_by(|(a, _), (b, _)| a.cmp(b));
        recipes.dedup_by(|(a, _), (b, _)| a == b);
        let mut displays = Vec::new();
        let mut by_name = HashMap::with_capacity(recipes.len());
        let mut groups: HashMap<&str, u32> = HashMap::new();
        for (index, (name, recipe)) in recipes.iter().enumerate() {
            let group = recipe.group().filter(|g| !g.is_empty()).map(|group| {
                let next = groups.len() as u32;
                *groups.entry(group).or_insert(next)
            });
            let requirements = if recipe.is_special() {
                None
            } else {
                recipe.placement().map(|placement| placement.ingredients)
            };
            let category = recipe.category();
            let start = displays.len();
            for display in recipe.displays() {
                displays.push(DisplayEntry {
                    id: displays.len() as u32,
                    recipe: name.clone(),
                    display,
                    group,
                    category,
                    requirements: requirements.clone(),
                });
            }
            by_name.insert(
                name.clone(),
                Indexed {
                    recipe: index,
                    displays: start..displays.len(),
                },
            );
        }
        let indices = |has: fn(&dyn Recipe) -> bool| -> Vec<usize> {
            (0..recipes.len())
                .filter(|index| has(&*recipes[*index].1))
                .collect()
        };
        let cooking = indices(|r| r.cooking().is_some());
        let stonecutting = indices(|r| r.stonecutting().is_some());
        let smithing = indices(|r| r.smithing().is_some());
        let brewing = indices(|r| r.brewing().is_some());
        let mut set = Self {
            recipes,
            displays,
            by_name,
            cooking,
            stonecutting,
            smithing,
            brewing,
            property_sets: Vec::new(),
            custom: Arc::default(),
        };
        set.property_sets = set.collect_property_sets();
        set
    }

    fn collect_property_sets(&self) -> Vec<(&'static str, Vec<Item>)> {
        let mut sets: Vec<(&'static str, Vec<Item>)> = PROPERTY_SETS
            .iter()
            .map(|name| (*name, Vec::new()))
            .collect();
        let mut push = |name: &str, ingredient: &Ingredient| {
            let set = &mut sets.iter_mut().find(|(n, _)| *n == name).expect("listed").1;
            set.extend_from_slice(ingredient.items());
        };
        for (_, recipe) in &self.recipes {
            if let Some(slots) = recipe.smithing() {
                if let Some(template) = &slots.template {
                    push("smithing_template", template);
                }
                push("smithing_base", &slots.base);
                if let Some(addition) = &slots.addition {
                    push("smithing_addition", addition);
                }
            }
            if let Some(brewing) = recipe.brewing() {
                push("brewing_input", &brewing.input.ingredient);
                push("brewing_reagent", &brewing.reagent.ingredient);
            }
            if let Some((station, cooking)) = recipe.cooking() {
                push(station.property_set(), &cooking.ingredient);
            }
        }
        for (_, set) in &mut sets {
            set.sort_by_key(|item| item.id());
            set.dedup();
        }
        sets
    }

    fn recipes_at<'a>(
        &'a self,
        indices: &'a [usize],
    ) -> impl Iterator<Item = (&'a str, &'a dyn Recipe)> + 'a {
        indices.iter().map(|index| {
            let (name, recipe) = &self.recipes[*index];
            (&**name, &**recipe)
        })
    }

    pub fn from_registry(registry: &Registry) -> Self {
        Self::new(
            registry
                .iter()
                .map(|(name, recipe)| (name.clone(), recipe.clone())),
        )
    }

    /// The custom items grids are checked against: one whose base
    /// behaviour is off only makes recipes that accept custom items.
    pub fn with_custom(mut self, custom: Arc<crate::custom::CustomContent>) -> Self {
        self.custom = custom;
        self
    }

    /// Every recipe by name, in name order.
    pub fn entries(&self) -> impl Iterator<Item = (&Frozen<str>, &Arc<dyn Recipe>)> {
        self.recipes.iter().map(|(name, recipe)| (name, recipe))
    }

    pub fn custom(&self) -> &Arc<crate::custom::CustomContent> {
        &self.custom
    }

    pub fn get(&self, name: &str) -> Option<&dyn Recipe> {
        let index = self.by_name.get(name)?.recipe;
        Some(&*self.recipes[index].1)
    }

    /// The shared name of a recipe this set has.
    pub fn resolve(&self, name: &str) -> Option<Frozen<str>> {
        let index = self.by_name.get(name)?.recipe;
        Some(self.recipes[index].0.clone())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.recipes.iter().map(|(name, _)| &**name)
    }

    /// Every recipe book entry, indexed by display id.
    pub fn displays(&self) -> &[DisplayEntry] {
        &self.displays
    }

    pub fn display(&self, id: u32) -> Option<&DisplayEntry> {
        self.displays.get(id as usize)
    }

    pub fn displays_for(&self, name: &str) -> &[DisplayEntry] {
        self.by_name
            .get(name)
            .map_or(&[], |indexed| &self.displays[indexed.displays.clone()])
    }

    pub fn is_special(&self, name: &str) -> bool {
        self.get(name).is_some_and(|recipe| recipe.is_special())
    }

    pub fn show_notification(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|recipe| recipe.show_notification())
    }

    /// The first recipe, in name order, that a grid holding `input` makes.
    pub fn find(&self, input: &CraftingInput) -> Option<(&Frozen<str>, &dyn Recipe)> {
        if input.is_empty() {
            return None;
        }
        let custom_only = input
            .stacks()
            .any(|stack| self.custom.suppresses_base(stack));
        self.recipes
            .iter()
            .find(|(_, recipe)| {
                (!custom_only || recipe.accepts_custom_items()) && recipe.matches(input)
            })
            .map(|(name, recipe)| (name, &**recipe))
    }

    /// What a crafting grid `width` slots wide currently makes.
    pub fn grid_result(
        &self,
        grid: &[Option<ItemStack>],
        width: usize,
        height: usize,
    ) -> Option<ItemStack> {
        let positioned = CraftingInput::positioned(width, height, grid);
        let (_, recipe) = self.find(&positioned.input)?;
        recipe.assemble(&positioned.input)
    }

    /// The recipe a crafting grid matches.
    pub fn grid_recipe(
        &self,
        grid: &[Option<ItemStack>],
        width: usize,
        height: usize,
    ) -> Option<&str> {
        let positioned = CraftingInput::positioned(width, height, grid);
        self.find(&positioned.input).map(|(name, _)| &**name)
    }

    /// A crafter's craft over its 3x3 grid: the result and the remainders
    /// each cell leaves, with one item of every occupied cell used up. The
    /// grid is untouched when nothing matches.
    pub fn crafter_craft(&self, grid: &mut [Option<ItemStack>]) -> Option<Crafted> {
        let positioned = CraftingInput::positioned(3, 3, grid);
        let input = &positioned.input;
        let (recipe_name, recipe) = self.find(input)?;
        let result = recipe.assemble(input).filter(|result| result.count > 0)?;
        let remainders = recipe.remainders(input).into_iter().flatten().collect();
        for slot in grid.iter_mut() {
            if let Some(stack) = slot.as_mut() {
                stack.count -= 1;
                if stack.count == 0 {
                    *slot = None;
                }
            }
        }
        Some(Crafted {
            recipe: recipe_name.clone(),
            result,
            remainders,
        })
    }

    /// Uses up one of each ingredient after the result was taken, leaving
    /// remainders in the grid. Remainders that cannot stay in their slot
    /// are returned for the player's inventory.
    pub fn take_result(
        &self,
        grid: &mut [Option<ItemStack>],
        width: usize,
        height: usize,
    ) -> Vec<ItemStack> {
        let positioned = CraftingInput::positioned(width, height, grid);
        let input = &positioned.input;
        let mut remaining = match self.find(input) {
            Some((_, recipe)) => recipe.remainders(input),
            None => input.crafting_remainders(),
        };
        remaining.resize(input.items.len(), None);
        let mut leftovers = Vec::new();
        for y in 0..input.height {
            for x in 0..input.width {
                let slot = x + positioned.left + (y + positioned.top) * width;
                if let Some(stack) = grid[slot].as_mut() {
                    stack.count -= 1;
                    if stack.count == 0 {
                        grid[slot] = None;
                    }
                }
                let Some(mut replacement) = remaining[x + y * input.width].take() else {
                    continue;
                };
                match &grid[slot] {
                    None => grid[slot] = Some(replacement),
                    Some(current) if current.stacks_with(&replacement) => {
                        replacement.count += current.count;
                        grid[slot] = Some(replacement);
                    }
                    Some(_) => leftovers.push(replacement),
                }
            }
        }
        leftovers
    }
}

/// Recipe stations: what the furnaces, campfires, stonecutter, smithing
/// table and brewing stand accept and make.
impl RecipeSet {
    /// The items in a recipe property set.
    pub fn property_set(&self, name: &str) -> &[Item] {
        self.property_sets
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(&[], |(_, items)| items.as_slice())
    }

    /// Every property set, as the client is told them.
    pub fn property_sets(&self) -> &[(&'static str, Vec<Item>)] {
        &self.property_sets
    }

    /// The first recipe `station` cooks `item` with.
    pub fn cooking_recipe(
        &self,
        station: CookingStation,
        item: Item,
    ) -> Option<(&str, &CookingRecipe)> {
        self.recipes_at(&self.cooking).find_map(|(name, recipe)| {
            let (at, cooking) = recipe.cooking()?;
            (at == station && cooking.ingredient.matches(item)).then_some((name, cooking))
        })
    }

    pub fn can_cook(&self, kind: ContainerKind, item: Item) -> bool {
        CookingStation::of(kind).is_some_and(|station| self.cooking_recipe(station, item).is_some())
    }

    /// The experience one cooking of `recipe` earns, if it is a cooking
    /// recipe.
    pub fn cooking_experience(&self, recipe: &str) -> Option<f32> {
        self.get(recipe)?
            .cooking()
            .map(|(_, cooking)| cooking.experience)
    }

    /// A furnace's input slot changed: restarts its cook timing.
    pub fn furnace_input_changed(
        &self,
        kind: ContainerKind,
        items: &[Option<ItemStack>],
        furnace: &mut Furnace,
    ) {
        smelting::input_changed(self, kind, items, furnace);
    }

    /// Puts one of `stack` on a free campfire slot if it cooks there,
    /// returning the slot.
    pub fn place_food(
        &self,
        items: &mut [Option<ItemStack>],
        campfire: &mut Campfire,
        stack: &ItemStack,
    ) -> Option<usize> {
        smelting::place_food(self, items, campfire, stack)
    }

    /// The stonecutter's recipes in the order clients number them: each
    /// input and its result.
    pub fn stonecutter_list(&self) -> Vec<(Ingredient, SlotDisplay)> {
        self.recipes_at(&self.stonecutting)
            .filter_map(|(_, recipe)| recipe.stonecutting())
            .map(|cutting| {
                (
                    cutting.ingredient.clone(),
                    SlotDisplay::Stack(result_stack(&cutting.result)),
                )
            })
            .collect()
    }

    /// Numbers of the stonecutter recipes that cut `item`.
    pub fn stonecutter_options(&self, item: Item) -> Vec<usize> {
        self.recipes_at(&self.stonecutting)
            .enumerate()
            .filter(|(_, (_, recipe))| {
                recipe
                    .stonecutting()
                    .is_some_and(|cutting| cutting.ingredient.matches(item))
            })
            .map(|(option, _)| option)
            .collect()
    }

    /// Stonecutter recipe `option`: its name and result.
    pub fn stonecutter_recipe(&self, option: usize) -> Option<(&str, ItemStack)> {
        let (name, recipe) = &self.recipes[*self.stonecutting.get(option)?];
        Some((name, result_stack(&recipe.stonecutting()?.result)))
    }

    pub fn stonecutter_accepts(&self, item: Item) -> bool {
        self.recipes_at(&self.stonecutting).any(|(_, recipe)| {
            recipe
                .stonecutting()
                .is_some_and(|cutting| cutting.ingredient.matches(item))
        })
    }

    /// The smithing recipe these three slots match and its result.
    pub fn smithing(
        &self,
        template: Option<&ItemStack>,
        base: Option<&ItemStack>,
        addition: Option<&ItemStack>,
    ) -> Option<(&str, Option<ItemStack>)> {
        let base = base?;
        let slot_matches = |ingredient: &Option<Ingredient>, stack: Option<&ItemStack>| match (
            ingredient, stack,
        ) {
            (None, None) => true,
            (Some(ingredient), Some(stack)) => ingredient.matches(stack.item),
            _ => false,
        };
        self.recipes_at(&self.smithing).find_map(|(name, recipe)| {
            let slots = recipe.smithing()?;
            (slot_matches(&slots.template, template)
                && slots.base.matches(base.item)
                && slot_matches(&slots.addition, addition))
            .then(|| (name, recipe.smith(template, base, addition)))
        })
    }

    /// The first recipe that brews `input` with `reagent`.
    pub fn brewing_recipe(&self, input: &ItemStack, reagent: &ItemStack) -> Option<&BrewingRecipe> {
        self.recipes_at(&self.brewing)
            .filter_map(|(_, recipe)| recipe.brewing())
            .find(|recipe| {
                brewing::accepts(&recipe.input, input) && brewing::accepts(&recipe.reagent, reagent)
            })
    }

    /// Whether `stack` may go in a bottle slot.
    pub fn is_potion_input(&self, stack: &ItemStack) -> bool {
        self.property_set("brewing_input")
            .binary_search_by_key(&stack.item.id(), |item| item.id())
            .is_ok()
            || tuff_datapack::tags::is(tag::item::key::BREWING_POTION_INPUTS, stack.item)
    }

    /// Whether `stack` may go in the reagent slot.
    pub fn is_brewing_reagent(&self, stack: &ItemStack) -> bool {
        self.property_set("brewing_reagent")
            .binary_search_by_key(&stack.item.id(), |item| item.id())
            .is_ok()
    }

    pub fn is_brewing_fuel(&self, stack: &ItemStack) -> bool {
        stack.item.brewing_fuel().is_some()
    }
}

#[cfg(test)]
pub(crate) mod test_recipe {
    use super::*;
    use tuff_vanilla_data::recipe::ShapelessCraftingRecipe;

    /// A minimal shapeless data recipe for core's own tests.
    #[derive(Debug)]
    pub struct Shapeless {
        pub recipe: ShapelessCraftingRecipe,
    }

    impl Shapeless {
        pub fn new(_name: &'static str, ingredients: &'static [Ingredient], item: Item) -> Self {
            Self {
                recipe: ShapelessCraftingRecipe {
                    category: None,
                    group: None,
                    ingredients: Frozen::Static(ingredients),
                    result: RecipeResult {
                        item,
                        count: 1,
                        components: None,
                    },
                    show_notification: true,
                },
            }
        }
    }

    impl Recipe for Shapeless {
        fn matches(&self, input: &CraftingInput) -> bool {
            input.ingredient_count() == self.recipe.ingredients.len()
                && input
                    .stacked()
                    .try_pick(&self.recipe.ingredients, 1)
                    .is_some()
        }

        fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
            Some(result_stack(&self.recipe.result))
        }

        fn placement(&self) -> Option<Placement> {
            Some(Placement::shapeless(self.recipe.ingredients.to_vec()))
        }
    }
}

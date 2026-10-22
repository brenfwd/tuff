//! Recipe matching and assembly for the crafting grid, the stonecutter and
//! the smithing table, as pure functions of the built-in recipe data
//! (`tuff_vanilla_data::recipe`) and item stacks.

pub mod display;
pub mod registered;
mod shaped;
mod station;

pub use shaped::{Shaped, ShapedPattern, Shapeless};
pub use station::{Brewing, Cooking, SmithingTransform, Stonecutting};

/// Sets a field of a const builder. A const fn cannot drop the old value,
/// which builders only ever replace while it still holds compiled-in data.
const fn set_const<T>(field: &mut T, value: T) {
    std::mem::forget(std::mem::replace(field, value));
}

use tuff_nbt::Tag;
use tuff_vanilla_data::data_registry::TRIM_PATTERN;
use tuff_vanilla_data::item::{DyeColor, FireworkShape};
use tuff_vanilla_data::recipe::{
    Ingredient, Recipe, RecipeResult, SmithingTrimRecipe, TransmuteRecipe,
};
use tuff_vanilla_data::tag;
use tuff_vanilla_data::{Frozen, Item};

use tuff_core::world::crafting::Recipe as _;
pub use tuff_core::world::crafting::{
    CraftingInput, Positioned, result_stack, with_input_components,
};
use tuff_item::item::decor::{self, ArmorTrim, FireworkExplosion, Fireworks};
use tuff_item::item::{Enchantments, ItemStack, MintReason};

pub const MAX_GRID: usize = 3;
const MAX_FIREWORK_FUEL: usize = 3;
const MAX_BANNER_LAYERS_TO_COPY: usize = 6;
const REPAIR_BONUS_PERCENT: u32 = 5;
const DEFAULT_TRANSMUTE_MATERIALS: (u64, u64) = (1, 8);

fn accepts(ingredient: &Ingredient, stack: &ItemStack) -> bool {
    ingredient.matches(stack.item())
}

/// Whether a recipe belongs to the crafting grid.
pub fn is_crafting(recipe: &Recipe) -> bool {
    !matches!(
        recipe,
        Recipe::Smelting(_)
            | Recipe::Blasting(_)
            | Recipe::Smoking(_)
            | Recipe::CampfireCooking(_)
            | Recipe::Stonecutting(_)
            | Recipe::Brewing(_)
            | Recipe::SmithingTransform(_)
            | Recipe::SmithingTrim(_)
    )
}

/// Recipes the recipe book never lists: their inputs are item logic, not a
/// fixed ingredient list.
pub fn is_special(recipe: &Recipe) -> bool {
    matches!(
        recipe,
        Recipe::Brewing(_)
            | Recipe::CraftingSpecialBannerDuplicate(_)
            | Recipe::CraftingSpecialBookCloning(_)
            | Recipe::CraftingSpecialRepairItem
            | Recipe::CraftingSpecialFireworkRocket(_)
            | Recipe::CraftingSpecialFireworkStar(_)
            | Recipe::CraftingSpecialFireworkStarFade(_)
            | Recipe::CraftingSpecialMapExtending(_)
            | Recipe::CraftingSpecialShieldDecoration(_)
            | Recipe::CraftingDecoratedPot(_)
    )
}

pub fn show_notification(recipe: &Recipe) -> bool {
    match recipe {
        Recipe::CraftingShaped(r) => r.show_notification,
        Recipe::CraftingShapeless(r) => r.show_notification,
        Recipe::Smelting(r)
        | Recipe::Blasting(r)
        | Recipe::Smoking(r)
        | Recipe::CampfireCooking(r) => r.show_notification,
        Recipe::Stonecutting(r) => r.show_notification,
        Recipe::SmithingTransform(r) => r.show_notification,
        Recipe::SmithingTrim(r) => r.show_notification,
        Recipe::CraftingTransmute(r) => r.show_notification,
        Recipe::CraftingDye(r) => r.show_notification,
        Recipe::CraftingImbue(r) => r.show_notification,
        _ => true,
    }
}

fn transmute_bounds(recipe: &TransmuteRecipe) -> (u64, u64) {
    recipe
        .material_count
        .map_or(DEFAULT_TRANSMUTE_MATERIALS, |range| (range.min, range.max))
}

/// The ingredients the recipe book must find to place a recipe, in the
/// order they fill the grid; `None` for recipes it cannot place.
pub fn placement_ingredients(recipe: &Recipe) -> Option<Vec<Ingredient>> {
    Some(match recipe {
        Recipe::CraftingShaped(shaped) => ShapedPattern::of(shaped)
            .ingredients
            .into_iter()
            .flatten()
            .collect(),
        Recipe::CraftingShapeless(shapeless) => shapeless.ingredients.to_vec(),
        Recipe::CraftingTransmute(transmute) => {
            let (_, max) = transmute_bounds(transmute);
            std::iter::once(transmute.input.clone())
                .chain(std::iter::repeat_n(
                    transmute.material.clone(),
                    max as usize,
                ))
                .collect()
        }
        Recipe::CraftingDye(dye) => vec![dye.target.clone(), dye.dye.clone()],
        Recipe::CraftingImbue(imbue) => {
            let m = &imbue.material;
            let s = &imbue.source;
            [m, m, m, m, s, m, m, m, m].map(Ingredient::clone).to_vec()
        }
        Recipe::SmithingTrim(s) => vec![s.template.clone(), s.base.clone(), s.addition.clone()],
        _ => return None,
    })
}

pub fn matches(recipe: &Recipe, input: &CraftingInput) -> bool {
    match recipe {
        Recipe::CraftingShaped(recipe) => ShapedPattern::of(recipe).matches(input),
        Recipe::CraftingShapeless(recipe) => Shapeless {
            name: Frozen::Static(""),
            recipe: recipe.clone(),
        }
        .matches(input),
        Recipe::CraftingTransmute(transmute) => matches_transmute(transmute, input),
        Recipe::CraftingDye(dye) => {
            if input.ingredient_count() < 2 {
                return false;
            }
            let (mut has_target, mut has_dye) = (false, false);
            for stack in input.stacks() {
                if accepts(&dye.target, stack) {
                    if has_target {
                        return false;
                    }
                    has_target = true;
                } else if accepts(&dye.dye, stack) && stack.item().dye().is_some() {
                    has_dye = true;
                } else {
                    return false;
                }
            }
            has_target && has_dye
        }
        Recipe::CraftingSpecialRepairItem => repair_pair(input).is_some(),
        Recipe::CraftingSpecialBookCloning(cloning) => {
            if input.ingredient_count() < 2 {
                return false;
            }
            let (mut has_material, mut has_source) = (false, false);
            for stack in input.stacks() {
                if accepts(&cloning.source, stack) {
                    let generation = stack
                        .components
                        .written_book
                        .as_ref()
                        .map(|book| book.generation);
                    if !generation.is_some_and(|g| {
                        u64::try_from(g).is_ok_and(|g| {
                            (cloning.allowed_generations.min..=cloning.allowed_generations.max)
                                .contains(&g)
                        })
                    }) || has_source
                    {
                        return false;
                    }
                    has_source = true;
                } else if accepts(&cloning.material, stack) {
                    has_material = true;
                } else {
                    return false;
                }
            }
            has_source && has_material
        }
        Recipe::CraftingSpecialFireworkRocket(rocket) => {
            if input.ingredient_count() < 2 {
                return false;
            }
            let mut has_shell = false;
            let mut fuel = 0;
            for stack in input.stacks() {
                if accepts(&rocket.shell, stack) {
                    if has_shell {
                        return false;
                    }
                    has_shell = true;
                } else if accepts(&rocket.fuel, stack) {
                    fuel += 1;
                    if fuel > MAX_FIREWORK_FUEL {
                        return false;
                    }
                } else if !accepts(&rocket.star, stack) {
                    return false;
                }
            }
            has_shell && fuel >= 1
        }
        Recipe::CraftingSpecialFireworkStar(star) => {
            if input.ingredient_count() < 2 {
                return false;
            }
            let (mut has_fuel, mut has_dye, mut has_shape, mut has_trail, mut has_twinkle) =
                (false, false, false, false, false);
            for stack in input.stacks() {
                if accepts(&star.twinkle, stack) {
                    if has_twinkle {
                        return false;
                    }
                    has_twinkle = true;
                } else if accepts(&star.trail, stack) {
                    if has_trail {
                        return false;
                    }
                    has_trail = true;
                } else if accepts(&star.fuel, stack) {
                    if has_fuel {
                        return false;
                    }
                    has_fuel = true;
                } else if accepts(&star.dye, stack) && stack.item().dye().is_some() {
                    has_dye = true;
                } else {
                    let shape = star.shapes.iter().any(|(_, shape)| accepts(shape, stack));
                    if !shape || has_shape {
                        return false;
                    }
                    has_shape = true;
                }
            }
            has_fuel && has_dye
        }
        Recipe::CraftingSpecialFireworkStarFade(fade) => {
            if input.ingredient_count() < 2 {
                return false;
            }
            let (mut has_dye, mut has_target) = (false, false);
            for stack in input.stacks() {
                if accepts(&fade.dye, stack) && stack.item().dye().is_some() {
                    has_dye = true;
                } else {
                    if !accepts(&fade.target, stack) || has_target {
                        return false;
                    }
                    has_target = true;
                }
            }
            has_target && has_dye
        }
        Recipe::CraftingSpecialShieldDecoration(shield) => {
            if input.ingredient_count() != 2 {
                return false;
            }
            let (mut has_target, mut has_banner) = (false, false);
            for stack in input.stacks() {
                if accepts(&shield.banner, stack) && stack.item().banner_color().is_some() {
                    if has_banner {
                        return false;
                    }
                    has_banner = true;
                } else {
                    if !accepts(&shield.target, stack) || has_target {
                        return false;
                    }
                    if stack
                        .components
                        .banner_patterns
                        .as_ref()
                        .is_some_and(|layers| !layers.is_empty())
                    {
                        return false;
                    }
                    has_target = true;
                }
            }
            has_target && has_banner
        }
        Recipe::CraftingSpecialBannerDuplicate(duplicate) => {
            if input.ingredient_count() != 2 {
                return false;
            }
            let mut color = None;
            let (mut has_target, mut has_source) = (false, false);
            for stack in input.stacks() {
                let Some(banner) = stack
                    .item()
                    .banner_color()
                    .filter(|_| accepts(&duplicate.banner, stack))
                else {
                    return false;
                };
                if color.is_some_and(|c| c != banner) {
                    return false;
                }
                color = Some(banner);
                let layers = banner_layers(stack);
                if layers > MAX_BANNER_LAYERS_TO_COPY {
                    return false;
                }
                if layers > 0 {
                    if has_source {
                        return false;
                    }
                    has_source = true;
                } else {
                    if has_target {
                        return false;
                    }
                    has_target = true;
                }
            }
            has_source && has_target
        }
        Recipe::CraftingImbue(imbue) => {
            if input.width != MAX_GRID || input.height != MAX_GRID || input.ingredient_count() != 9
            {
                return false;
            }
            (0..MAX_GRID).all(|y| {
                (0..MAX_GRID).all(|x| {
                    let ingredient = if (x, y) == (1, 1) {
                        &imbue.source
                    } else {
                        &imbue.material
                    };
                    input
                        .get(x, y)
                        .is_some_and(|stack| accepts(ingredient, stack))
                })
            })
        }
        Recipe::CraftingDecoratedPot(pot) => {
            if input.width != MAX_GRID || input.height != MAX_GRID || input.ingredient_count() != 4
            {
                return false;
            }
            [
                (1, 0, &pot.back),
                (0, 1, &pot.left),
                (2, 1, &pot.right),
                (1, 2, &pot.front),
            ]
            .into_iter()
            .all(|(x, y, ingredient)| {
                input
                    .get(x, y)
                    .is_some_and(|stack| accepts(ingredient, stack))
            })
        }
        Recipe::CraftingSpecialMapExtending(extending) => {
            if input.width != MAX_GRID || input.height != MAX_GRID || input.ingredient_count() != 9
            {
                return false;
            }
            (0..MAX_GRID).all(|y| {
                (0..MAX_GRID).all(|x| {
                    let ingredient = if (x, y) == (1, 1) {
                        &extending.map
                    } else {
                        &extending.material
                    };
                    input
                        .get(x, y)
                        .is_some_and(|stack| accepts(ingredient, stack))
                })
            })
        }
        _ => false,
    }
}

fn banner_layers(stack: &ItemStack) -> usize {
    stack
        .components
        .banner_patterns
        .as_ref()
        .map_or(0, Vec::len)
}

fn matches_transmute(recipe: &TransmuteRecipe, input: &CraftingInput) -> bool {
    let (min, max) = transmute_bounds(recipe);
    let count = input.ingredient_count() as u64;
    if count < min + 1 || count > max + 1 {
        return false;
    }
    let mut found = None;
    let mut materials = 0u64;
    for stack in input.stacks() {
        if accepts(&recipe.input, stack) {
            if found.is_some() {
                return false;
            }
            found = Some(stack);
        } else if accepts(&recipe.material, stack) {
            materials += 1;
            if materials > max {
                return false;
            }
        } else {
            return false;
        }
    }
    let Some(found) = found else {
        return false;
    };
    if !(min..=max).contains(&materials) {
        return false;
    }
    if transmute_count(recipe, materials) != 1 {
        return true;
    }
    let result = transmute_result(recipe, found, 0);
    !(result.item() == found.item() && result.components == found.components)
}

fn transmute_count(recipe: &TransmuteRecipe, materials: u64) -> u64 {
    let base = recipe
        .result
        .as_ref()
        .map_or(recipe.keep_count, |r| r.count);
    if recipe.add_material_count_to_result {
        base + materials
    } else {
        base
    }
}

fn transmute_result(recipe: &TransmuteRecipe, input: &ItemStack, materials: u64) -> ItemStack {
    match &recipe.result {
        Some(result) => with_input_components(Some(result), input, materials),
        None if recipe.keep_count == 1 && recipe.keep_components.is_none() => {
            with_input_components(None, input, materials)
        }
        None => {
            let kept = RecipeResult {
                item: input.item(),
                count: recipe.keep_count,
                components: recipe.keep_components.clone(),
            };
            with_input_components(Some(&kept), input, materials)
        }
    }
}

fn repair_pair<'a>(input: &CraftingInput<'a>) -> Option<(&'a ItemStack, &'a ItemStack)> {
    if input.ingredient_count() != 2 {
        return None;
    }
    let mut stacks = input.stacks();
    let first = stacks.next()?;
    let second = stacks.next()?;
    let repairable = |stack: &ItemStack| {
        stack.count() == 1 && stack.max_damage().is_some() && stack.has_damage()
    };
    (first.item() == second.item() && repairable(first) && repairable(second))
        .then_some((first, second))
}

fn crafting_enchantments(stack: &ItemStack) -> Option<&Enchantments> {
    if stack.item() == Item::EnchantedBook {
        stack.components.stored_enchantments.as_ref()
    } else {
        stack.components.enchantments.as_ref()
    }
}

pub fn assemble(recipe: &Recipe, input: &CraftingInput) -> Option<ItemStack> {
    match recipe {
        Recipe::CraftingShaped(shaped) => Some(result_stack(&shaped.result)),
        Recipe::CraftingShapeless(shapeless) => Some(result_stack(&shapeless.result)),
        Recipe::CraftingTransmute(transmute) => {
            let mut materials = 0;
            let mut found = None;
            for stack in input.stacks() {
                if accepts(&transmute.input, stack) {
                    found = Some(stack);
                } else if accepts(&transmute.material, stack) {
                    materials += 1;
                }
            }
            let extra = if transmute.add_material_count_to_result {
                materials
            } else {
                0
            };
            found.map(|input| transmute_result(transmute, input, extra))
        }
        Recipe::CraftingDye(dye) => {
            let mut dyes = Vec::new();
            let mut target = None;
            for stack in input.stacks() {
                if accepts(&dye.target, stack) {
                    if target.is_some() {
                        return None;
                    }
                    target = Some(stack);
                } else if accepts(&dye.dye, stack) {
                    dyes.push(stack.item().dye().unwrap_or(DyeColor::White));
                } else {
                    return None;
                }
            }
            let target = target?;
            if dyes.is_empty() {
                return None;
            }
            let mut result = with_input_components(Some(&dye.result), target, 0);
            result.components.dyed_color =
                Some(decor::apply_dyes(target.components.dyed_color, &dyes));
            Some(result)
        }
        Recipe::CraftingSpecialRepairItem => {
            let (first, second) = repair_pair(input)?;
            let durability = first.max_damage()?.max(second.max_damage()?);
            let remaining = (first.max_damage()? - first.damage())
                + (second.max_damage()? - second.damage())
                + durability * REPAIR_BONUS_PERCENT / 100;
            let mut result = ItemStack::mint(first.item(), 1, MintReason::Crafting);
            if first.item().max_damage() != Some(durability) {
                result.components.max_damage = Some(durability);
            }
            result.set_damage(durability.saturating_sub(remaining));
            let mut curses = Vec::new();
            for enchantments in [first, second]
                .into_iter()
                .filter_map(crafting_enchantments)
            {
                for (id, level) in enchantments.entries() {
                    if tuff_datapack::tags::has(tag::enchantment::key::CURSE, id.as_str()) {
                        match curses
                            .iter_mut()
                            .find(|(other, _): &&mut (String, i32)| other == id)
                        {
                            Some((_, existing)) => *existing = (*existing).max(*level),
                            None => curses.push((id.clone(), *level)),
                        }
                    }
                }
            }
            if !curses.is_empty() {
                result.components.enchantments = Some(Enchantments::new(curses));
            }
            Some(result)
        }
        Recipe::CraftingSpecialBookCloning(cloning) => {
            let mut copies = 0;
            let mut source = None;
            for stack in input.stacks() {
                if accepts(&cloning.source, stack) && stack.components.written_book.is_some() {
                    if source.is_some() {
                        return None;
                    }
                    source = Some(stack);
                } else if accepts(&cloning.material, stack) {
                    copies += 1;
                } else {
                    return None;
                }
            }
            let source = source?;
            let book = source.components.written_book.as_ref()?;
            let mut result = with_input_components(Some(&cloning.result), source, copies - 1);
            result.components.written_book = Some(Box::new(book.craft_copy()));
            Some(result)
        }
        Recipe::CraftingSpecialFireworkRocket(rocket) => {
            let mut fuel = 0u8;
            let mut explosions = Vec::new();
            for stack in input.stacks() {
                if accepts(&rocket.fuel, stack) {
                    fuel += 1;
                } else if accepts(&rocket.star, stack)
                    && let Some(explosion) = &stack.components.firework_explosion
                {
                    explosions.push((**explosion).clone());
                }
            }
            let mut result = result_stack(&rocket.result);
            result.components.fireworks = Some(Box::new(Fireworks {
                flight_duration: fuel,
                explosions,
            }));
            Some(result)
        }
        Recipe::CraftingSpecialFireworkStar(star) => {
            let mut explosion = FireworkExplosion::default();
            for stack in input.stacks() {
                if let Some((name, _)) = star.shapes.iter().find(|(_, shape)| accepts(shape, stack))
                {
                    explosion.shape =
                        FireworkShape::from_name(name).unwrap_or(FireworkShape::SmallBall);
                } else if accepts(&star.twinkle, stack) {
                    explosion.has_twinkle = true;
                } else if accepts(&star.trail, stack) {
                    explosion.has_trail = true;
                } else if accepts(&star.dye, stack) {
                    explosion.colors.push(
                        stack
                            .item()
                            .dye()
                            .unwrap_or(DyeColor::White)
                            .firework_color(),
                    );
                }
            }
            let mut result = result_stack(&star.result);
            result.components.firework_explosion = Some(Box::new(explosion));
            Some(result)
        }
        Recipe::CraftingSpecialFireworkStarFade(fade) => {
            let mut colors = Vec::new();
            let mut target = None;
            for stack in input.stacks() {
                if accepts(&fade.dye, stack) {
                    colors.push(
                        stack
                            .item()
                            .dye()
                            .unwrap_or(DyeColor::White)
                            .firework_color(),
                    );
                } else if accepts(&fade.target, stack) {
                    target = Some(stack);
                }
            }
            let target = target?;
            if colors.is_empty() {
                return None;
            }
            let mut result = with_input_components(Some(&fade.result), target, 0);
            let mut explosion = target
                .components
                .firework_explosion
                .clone()
                .unwrap_or_default();
            explosion.fade_colors = colors;
            result.components.firework_explosion = Some(explosion);
            Some(result)
        }
        Recipe::CraftingSpecialShieldDecoration(shield) => {
            let mut patterns = None;
            let mut base_color = DyeColor::White;
            let mut target = None;
            for stack in input.stacks() {
                if accepts(&shield.banner, stack)
                    && let Some(color) = stack.item().banner_color()
                {
                    patterns = stack.components.banner_patterns.clone();
                    base_color = color;
                } else if accepts(&shield.target, stack) {
                    target = Some(stack);
                }
            }
            let target = target?;
            let mut result = with_input_components(Some(&shield.result), target, 0);
            result.components.banner_patterns = patterns;
            result.components.base_color = Some(base_color);
            Some(result)
        }
        Recipe::CraftingSpecialBannerDuplicate(duplicate) => input
            .stacks()
            .find(|stack| (1..=MAX_BANNER_LAYERS_TO_COPY).contains(&banner_layers(stack)))
            .map(|source| with_input_components(Some(&duplicate.result), source, 0)),
        Recipe::CraftingImbue(imbue) => {
            let source = input.get(1, 1)?;
            let mut result = result_stack(&imbue.result);
            result.components.potion_contents = source.components.potion_contents.clone();
            Some(result)
        }
        Recipe::CraftingDecoratedPot(pot) => {
            let side = |x, y| {
                input
                    .get(x, y)
                    .map(|stack| ItemStack::mint_like(stack, 1, MintReason::Crafting))
            };
            let mut result = result_stack(&pot.result);
            result.components.other.insert(
                "minecraft:pot_decorations",
                tuff_item::item::pot_decorations_to_nbt(&[
                    side(1, 0),
                    side(0, 1),
                    side(2, 1),
                    side(1, 2),
                ]),
            );
            Some(result)
        }
        Recipe::CraftingSpecialMapExtending(_) => {
            let mut result = ItemStack::mint_like(input.get(1, 1)?, 1, MintReason::Crafting);
            result
                .components
                .other
                .insert("minecraft:map_post_processing", Tag::Int(1));
            Some(result)
        }
        _ => None,
    }
}

/// What each slot of `input` leaves behind when the recipe is taken: the
/// item's crafting remainder, or for a few recipes one of the inputs itself.
pub fn remaining_items(recipe: &Recipe, input: &CraftingInput) -> Vec<Option<ItemStack>> {
    let mut remaining = input.crafting_remainders();
    match recipe {
        Recipe::CraftingSpecialBookCloning(_) => {
            if let Some(index) = input.items.iter().position(|slot| {
                slot.as_ref().is_some_and(|stack| {
                    stack.item().crafting_remainder().is_none()
                        && stack.components.written_book.is_some()
                })
            }) {
                remaining[index] =
                    input.items[index].map(|s| ItemStack::mint_like(s, 1, MintReason::Crafting));
            }
        }
        Recipe::CraftingSpecialBannerDuplicate(_) => {
            for (index, slot) in input.items.iter().enumerate() {
                if let Some(stack) = *slot
                    && stack.item().crafting_remainder().is_none()
                    && banner_layers(stack) > 0
                {
                    remaining[index] = Some(ItemStack::mint_like(stack, 1, MintReason::Remainder));
                }
            }
        }
        _ => {}
    }
    remaining
}

/// An armor trim applied to `base`, if `material` is a trim material and
/// the trim is new.
pub fn apply_trim(
    recipe: &SmithingTrimRecipe,
    base: &ItemStack,
    material: &ItemStack,
) -> Option<ItemStack> {
    let material = material.item().trim_material()?;
    if !TRIM_PATTERN.entries.contains(&recipe.pattern) {
        return None;
    }
    let trim = ArmorTrim::named(material, recipe.pattern);
    if base.components.trim.as_deref() == Some(&trim) {
        return None;
    }
    let mut result = ItemStack::mint_like(base, 1, MintReason::Crafting);
    result.components.trim = Some(Box::new(trim));
    Some(result)
}

#[cfg(test)]
mod set_tests;
#[cfg(test)]
mod tests;

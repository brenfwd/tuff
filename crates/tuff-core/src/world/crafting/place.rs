//! The recipe book's click-to-fill: moving ingredients from the inventory
//! into a crafting grid the way the game does, or reporting that they are
//! missing so the client shows the ghost recipe.

use tuff_vanilla_data::Item;

use super::stacked::{StackedContents, usable_for_crafting};
use super::{CraftingInput, Placement, Recipe};
use crate::world::inventory::Inventory;
use tuff_item::item::ItemStack;

const HOTBAR: std::ops::Range<usize> = 36..45;
const MAIN: std::ops::Range<usize> = 9..36;
const OFFHAND: usize = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostPlace {
    Nothing,
    /// The ingredients are missing: the client shows them greyed out.
    Ghost,
}

/// A crafting grid's input slots as the recipe book fills them.
pub struct Grid<'a> {
    pub width: usize,
    pub height: usize,
    pub slots: &'a mut [Option<ItemStack>],
}

/// The inventory list slots in the game's order: hotbar, then the main grid.
fn item_slots() -> impl Iterator<Item = usize> {
    HOTBAR.chain(MAIN)
}

fn stackable_into(existing: Option<&ItemStack>, stack: &ItemStack) -> bool {
    existing.is_some_and(|existing| {
        existing.stacks_with(stack)
            && existing.max_stack_size() > 1
            && existing.count() < existing.max_stack_size()
    })
}

fn slot_with_remaining_space(inventory: &Inventory, stack: &ItemStack) -> Option<usize> {
    let slots = inventory.slots();
    let selected = Inventory::hotbar_slot(inventory.selected());
    [selected, OFFHAND]
        .into_iter()
        .chain(item_slots())
        .find(|slot| stackable_into(slots[*slot].as_ref(), stack))
}

/// Whether everything in the grid could go back into the inventory, either
/// topping up a partial stack already there or taking an empty slot, where
/// returned items of one kind share an empty slot while they fit.
fn grid_fits_in_inventory(grid: &[Option<ItemStack>], inventory: &Inventory) -> bool {
    let mut empty = item_slots()
        .filter(|slot| inventory.slots()[*slot].is_none())
        .count();
    let mut claimed: Vec<(Item, u16, u16)> = Vec::new();
    for stack in grid.iter().flatten() {
        if slot_with_remaining_space(inventory, stack).is_some() {
            continue;
        }
        let amount = stack.count() as u16;
        let shared = claimed.iter_mut().find(|(item, held, cap)| {
            *item == stack.item() && *held < *cap && held + amount <= *cap
        });
        match shared {
            Some((_, held, _)) => *held += amount,
            None if empty == 0 => return false,
            None => {
                empty -= 1;
                claimed.push((stack.item(), amount, stack.max_stack_size() as u16));
            }
        }
    }
    true
}

fn clear_grid(
    grid: &mut [Option<ItemStack>],
    extra: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    drops: &mut Vec<ItemStack>,
) {
    for slot in grid.iter_mut().chain(extra.iter_mut()) {
        if let Some(stack) = slot.take()
            && let Some(leftover) = inventory.place_back(stack)
        {
            drops.push(leftover);
        }
    }
}

fn find_ingredient(
    inventory: &Inventory,
    item: Item,
    existing: Option<&ItemStack>,
) -> Option<usize> {
    let slots = inventory.slots();
    item_slots().find(|slot| {
        slots[*slot].as_ref().is_some_and(|stack| {
            stack.item() == item
                && usable_for_crafting(stack)
                && existing.is_none_or(|existing| existing.stacks_with(stack))
        })
    })
}

/// Moves up to `count` of `item` from the inventory into one grid slot,
/// returning what is still wanted, or `None` if the inventory has none.
fn move_to_grid(
    target: &mut Option<ItemStack>,
    inventory: &mut Inventory,
    item: Item,
    count: u32,
) -> Option<u32> {
    let source = find_ingredient(inventory, item, target.as_ref())?;
    let stack = inventory.slot_mut(source);
    let available = stack.as_ref().map_or(0, |s| s.count() as u32);
    let taken = count.min(available) as u8;
    let Some(moved) = ItemStack::take_from(stack, taken) else {
        return Some(count);
    };
    ItemStack::put_back(target, moved);
    Some(count - taken as u32)
}

/// Grid positions ingredients go to, as `(ingredient index, grid index)`
/// pairs: a shaped recipe is centred in a larger grid.
pub fn layout(
    grid_width: usize,
    grid_height: usize,
    recipe_size: Option<(usize, usize)>,
    entries: &[Option<usize>],
) -> Vec<(Option<usize>, usize)> {
    let (recipe_width, recipe_height) = recipe_size.unwrap_or((grid_width, grid_height));
    let mut placed = Vec::new();
    let mut entries = entries.iter().peekable();
    let mut grid_index = 0;
    let mut y = 0;
    while y < grid_height {
        let center = (recipe_height as f32) < grid_height as f32 / 2.0;
        let start = (grid_height as f32 / 2.0 - recipe_height as f32 / 2.0).floor() as usize;
        if center && start > y {
            grid_index += grid_width;
            y += 1;
        }
        let mut x = 0;
        while x < grid_width {
            if entries.peek().is_none() {
                return placed;
            }
            let center_x = (recipe_width as f32) < grid_width as f32 / 2.0;
            let start_x = (grid_width as f32 / 2.0 - recipe_width as f32 / 2.0).floor() as usize;
            let mut total_width = recipe_width;
            let mut add = x < recipe_width;
            if center_x {
                total_width = start_x + recipe_width;
                add = start_x <= x && x < start_x + recipe_width;
            }
            if add {
                placed.push((*entries.next().expect("peeked"), grid_index));
            } else if total_width == x {
                grid_index += grid_width - x;
                break;
            }
            grid_index += 1;
            x += 1;
        }
        y += 1;
    }
    placed
}

/// Fills `grid` with `recipe` from the inventory. `also_clear` are slots
/// outside the grid that clearing returns to the inventory too (a
/// furnace's result). `use_max_items` crafts as many as possible
/// (shift-click); `allow_dropping` lets the grid be cleared even when the
/// inventory cannot hold its contents (creative).
pub fn place_recipe(
    recipe: &dyn Recipe,
    grid: &mut Grid<'_>,
    also_clear: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    use_max_items: bool,
    allow_dropping: bool,
    drops: &mut Vec<ItemStack>,
) -> PostPlace {
    let Some(placement) = recipe.placement() else {
        return PostPlace::Nothing;
    };
    if placement.ingredients.is_empty() {
        return PostPlace::Nothing;
    }
    if !allow_dropping && !grid_fits_in_inventory(grid.slots, inventory) {
        return PostPlace::Nothing;
    }
    let mut available = StackedContents::default();
    for slot in item_slots() {
        if let Some(stack) = &inventory.slots()[slot] {
            available.account_simple(stack);
        }
    }
    for stack in grid.slots.iter().flatten() {
        available.account_simple(stack);
    }
    if available.try_pick(&placement.ingredients, 1).is_none() {
        clear_grid(grid.slots, also_clear, inventory, drops);
        return PostPlace::Ghost;
    }
    try_place(
        recipe,
        &placement,
        grid,
        also_clear,
        inventory,
        &available,
        use_max_items,
        drops,
    );
    PostPlace::Nothing
}

#[expect(
    clippy::too_many_arguments,
    reason = "independent inputs of one operation; a parameter struct would be built once"
)]
fn try_place(
    recipe: &dyn Recipe,
    placement: &Placement,
    grid: &mut Grid<'_>,
    also_clear: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    available: &StackedContents,
    use_max_items: bool,
    drops: &mut Vec<ItemStack>,
) {
    let positioned = CraftingInput::positioned(grid.width, grid.height, grid.slots);
    let ingredients = &placement.ingredients;
    let placed_matches = recipe.matches(&positioned.input);
    let (biggest, _) = available.biggest_craftable(ingredients, u32::MAX);
    if placed_matches {
        for stack in grid.slots.iter().flatten() {
            if biggest.min(stack.max_stack_size() as u32) < stack.count() as u32 + 1 {
                return;
            }
        }
    }
    let amount = if use_max_items {
        biggest
    } else if placed_matches {
        let smallest = grid
            .slots
            .iter()
            .flatten()
            .map(|stack| stack.count() as u32)
            .min();
        match smallest {
            Some(smallest) => smallest + 1,
            None => u32::MAX,
        }
    } else {
        1
    };
    let Some(mut used) = available.try_pick(ingredients, amount) else {
        return;
    };
    let adjusted = used.iter().fold(amount, |limit, item| {
        limit.min(item.max_stack_size() as u32)
    });
    if adjusted != amount {
        match available.try_pick(ingredients, adjusted) {
            Some(picked) => used = picked,
            None => return,
        }
    }
    clear_grid(grid.slots, also_clear, inventory, drops);
    for (ingredient, grid_index) in
        layout(grid.width, grid.height, placement.size, &placement.layout)
    {
        let Some(ingredient) = ingredient else {
            continue;
        };
        let item = used[ingredient];
        let mut remaining = adjusted;
        while remaining > 0 {
            match move_to_grid(&mut grid.slots[grid_index], inventory, item, remaining) {
                Some(left) => remaining = left,
                None => return,
            }
        }
    }
}

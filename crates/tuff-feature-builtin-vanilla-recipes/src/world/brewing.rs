//! Brewing stand ticking.

use tuff_core::protocol::play::level_event::SOUND_BREWING_STAND_BREW;
use tuff_core::world::block_entity::{
    BREW_TIME, BlockEntityTicker, BrewTick, Brewing, State, TickOutcome, TickView, brewing,
};
use tuff_core::world::crafting::{RecipeSet, result_stack};
use tuff_item::item::BurnReason;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::BlockState;

fn brewable(recipes: &RecipeSet, items: &[Option<ItemStack>]) -> bool {
    let Some(reagent) = &items[brewing::INGREDIENT] else {
        return false;
    };
    recipes.is_brewing_reagent(reagent)
        && items[..brewing::BOTTLES]
            .iter()
            .flatten()
            .any(|bottle| recipes.brewing_recipe(bottle, reagent).is_some())
}

fn shrink_with_remainder(items: &mut [Option<ItemStack>], slot: usize, drops: &mut Vec<ItemStack>) {
    let Some(stack) = items[slot].as_mut() else {
        return;
    };
    let remainder = stack.item().crafting_remainder();
    stack.shrink(1, BurnReason::CraftingInput);
    let empty = stack.count() == 0;
    match remainder {
        #[expect(
            clippy::disallowed_methods,
            reason = "item left over after use (shrink_with_remainder)"
        )]
        Some((item, count)) if empty => {
            items[slot] = Some(ItemStack::mint(item, count, MintReason::Remainder))
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "item left over after use (shrink_with_remainder)"
        )]
        Some((item, count)) => drops.push(ItemStack::mint(item, count, MintReason::Remainder)),
        None if empty => items[slot] = None,
        None => {}
    }
}

fn brew(recipes: &RecipeSet, items: &mut [Option<ItemStack>], drops: &mut Vec<ItemStack>) {
    let reagent = items[brewing::INGREDIENT]
        .take()
        .expect("a reagent is brewing");
    for slot in &mut items[..brewing::BOTTLES] {
        if let Some(bottle) = slot
            && let Some(recipe) = recipes.brewing_recipe(bottle, &reagent)
            && let Some(old) = slot.replace(result_stack(&recipe.output))
        {
            old.burn(BurnReason::Transform);
        }
    }
    items[brewing::INGREDIENT] = Some(reagent);
    shrink_with_remainder(items, brewing::INGREDIENT, drops);
}

/// One tick of a brewing stand in block state `block`.
pub fn tick(
    recipes: &RecipeSet,
    block: BlockState,
    items: &mut [Option<ItemStack>],
    stand: &mut Brewing,
) -> BrewTick {
    let mut tick = BrewTick {
        changed: false,
        brewed: false,
        bottles: None,
        drops: Vec::new(),
    };
    if stand.fuel <= 0
        && let Some(fuel) = items[brewing::FUEL]
            .as_ref()
            .and_then(|stack| stack.item().brewing_fuel())
    {
        stand.fuel = fuel.uses(Some(block));
        stand.total_fuel = stand.fuel;
        stand.speed_multiplier = fuel.speed_multiplier(Some(block));
        shrink_with_remainder(items, brewing::FUEL, &mut tick.drops);
        tick.changed = true;
    }
    let brewable = brewable(recipes, items);
    let reagent = items[brewing::INGREDIENT]
        .as_ref()
        .map(|stack| stack.item());
    if stand.brew_time > 0 {
        stand.brew_time -= 1;
        if stand.brew_time == 0 && brewable {
            brew(recipes, items, &mut tick.drops);
            tick.brewed = true;
        } else if !brewable || reagent != stand.ingredient {
            stand.brew_time = 0;
        }
        tick.changed = true;
    } else if brewable && stand.fuel > 0 {
        let speed = if stand.speed_multiplier > 0.0 {
            stand.speed_multiplier
        } else {
            1.0
        };
        stand.fuel -= 1;
        stand.brew_time = (BREW_TIME as f32 / speed).ceil() as i32;
        stand.total_brew_time = stand.brew_time;
        stand.ingredient = reagent;
        tick.changed = true;
    }
    let bottles = [0, 1, 2].map(|slot| items[slot].is_some());
    if stand.bottles != Some(bottles) {
        stand.bottles = Some(bottles);
        tick.bottles = Some(bottles);
    }
    tick
}

pub struct BrewingTicker;

impl BlockEntityTicker for BrewingTicker {
    fn tick(&self, view: &mut TickView) -> TickOutcome {
        let State::Brewing(stand) = &mut *view.data else {
            return TickOutcome::default();
        };
        let tick = tick(view.recipes, view.state, view.items, stand);
        let mut set_state = None;
        if let Some(bottles) = tick.bottles {
            let mut next = view.state;
            for (slot, has) in bottles.into_iter().enumerate() {
                let value = if has { "true" } else { "false" };
                if let Some(with) = next.with(&format!("has_bottle_{slot}"), value) {
                    next = with;
                }
            }
            if next != view.state {
                set_state = Some(next);
            }
        }
        TickOutcome {
            set_state,
            drops: tick.drops,
            level_event: tick.brewed.then_some((SOUND_BREWING_STAND_BREW, 0)),
            ..TickOutcome::default()
        }
    }
}

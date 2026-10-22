//! Furnace-family and campfire ticking.

use tuff_core::world::block_entity::{
    BlockEntityTicker, CAMPFIRE_SLOTS, Campfire, ContainerKind, Furnace, FurnaceTick, State,
    TickOutcome, TickView, furnace,
};
use tuff_core::world::crafting::smelting::{
    burn_time, recipe_for, speed_multiplier, total_cook_time,
};
use tuff_core::world::crafting::{RecipeSet, result_stack};
use tuff_item::item::BurnReason;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::{BlockState, Item};

const COOL_SPEED: i32 = 2;
const CONTAINER_MAX_STACK: u8 = 99;

fn can_burn(items: &[Option<ItemStack>], result: &ItemStack) -> bool {
    match &items[furnace::RESULT] {
        None => true,
        Some(current) => {
            current.stacks_with(result)
                && current.count() as u32 + result.count() as u32
                    <= CONTAINER_MAX_STACK.min(result.max_stack_size()) as u32
        }
    }
}

fn consume_fuel(items: &mut [Option<ItemStack>], drops: &mut Vec<ItemStack>) {
    let Some(fuel) = items[furnace::FUEL].as_mut() else {
        return;
    };
    let remainder = fuel.item().crafting_remainder();
    fuel.shrink(1, BurnReason::Consumed);
    let empty = fuel.count() == 0;
    match remainder {
        Some((item, count)) if empty => {
            items[furnace::FUEL] = Some(ItemStack::mint(item, count, MintReason::Remainder))
        }
        Some((item, count)) => drops.push(ItemStack::mint(item, count, MintReason::Remainder)),
        None if empty => items[furnace::FUEL] = None,
        None => {}
    }
}

fn burn(items: &mut [Option<ItemStack>], result: &ItemStack) {
    match &mut items[furnace::RESULT] {
        None => {
            items[furnace::RESULT] = Some(ItemStack::mint_like(
                result,
                result.count(),
                MintReason::Processing,
            ))
        }
        Some(current) => current.grow(result.count(), MintReason::Processing),
    }
    let input = items[furnace::INPUT]
        .as_mut()
        .expect("an ingredient is cooking");
    if input.item() == Item::WetSponge
        && items[furnace::FUEL]
            .as_ref()
            .is_some_and(|fuel| fuel.item() == Item::Bucket)
    {
        items[furnace::FUEL] = Some(ItemStack::mint(Item::WaterBucket, 1, MintReason::Remainder));
    }
    let input = items[furnace::INPUT]
        .as_mut()
        .expect("an ingredient is cooking");
    input.shrink(1, BurnReason::CraftingInput);
    if input.count() == 0 {
        items[furnace::INPUT] = None;
    }
}

/// One tick of a furnace of `kind` in block state `block`.
pub fn tick_furnace(
    recipes: &RecipeSet,
    kind: ContainerKind,
    block: BlockState,
    items: &mut [Option<ItemStack>],
    furnace: &mut Furnace,
) -> FurnaceTick {
    let mut tick = FurnaceTick {
        changed: false,
        lit: None,
        drops: Vec::new(),
    };
    let was_lit = furnace.lit_time_remaining > 0;
    let mut is_lit = false;
    if was_lit {
        furnace.lit_time_remaining -= 1;
        is_lit = furnace.lit_time_remaining > 0;
    }
    let ingredient = items[furnace::INPUT].as_ref().map(|stack| stack.item());
    let fuel = items[furnace::FUEL].as_ref().map(|stack| stack.item());
    if is_lit || (fuel.is_some() && ingredient.is_some()) {
        match ingredient {
            None => furnace.cooking_timer = 0,
            Some(ingredient) => {
                if let Some((name, recipe)) = recipe_for(recipes, kind, ingredient) {
                    let result = tuff_core::world::crafting::result_template(&recipe.result);
                    if result.count() > 0 && can_burn(items, &result) {
                        if !is_lit {
                            let (burn_ticks, speed) = fuel.map_or((0, 1.0), |fuel| {
                                (burn_time(fuel, block), speed_multiplier(fuel, block))
                            });
                            furnace.lit_time_remaining = burn_ticks;
                            furnace.lit_total_time = burn_ticks;
                            furnace.speed_multiplier = speed;
                            if furnace.cooking_total_time > 0
                                && furnace.cooking_timer < furnace.cooking_total_time
                            {
                                let ratio = furnace.cooking_timer as f32
                                    / furnace.cooking_total_time as f32;
                                furnace.cooking_total_time = total_cook_time(recipe, speed);
                                furnace.cooking_timer =
                                    (ratio * furnace.cooking_total_time as f32).ceil() as i32;
                            }
                            if burn_ticks > 0 {
                                consume_fuel(items, &mut tick.drops);
                                is_lit = true;
                                tick.changed = true;
                            }
                        }
                        if is_lit {
                            furnace.cooking_timer += 1;
                            if furnace.cooking_timer >= furnace.cooking_total_time {
                                furnace.cooking_timer = 0;
                                furnace.cooking_total_time =
                                    total_cook_time(recipe, furnace.speed_multiplier);
                                burn(items, &result);
                                *furnace.recipes_used.entry(name.to_owned()).or_insert(0) += 1;
                                tick.changed = true;
                            }
                        } else {
                            furnace.cooking_timer = 0;
                        }
                    } else {
                        furnace.cooking_timer = 0;
                    }
                }
            }
        }
    } else if furnace.cooking_timer > 0 {
        furnace.cooking_timer =
            (furnace.cooking_timer - COOL_SPEED).clamp(0, furnace.cooking_total_time);
    }
    if was_lit != is_lit {
        tick.changed = true;
        tick.lit = Some(is_lit);
    }
    tick
}

/// One tick of a campfire: lit ones cook, unlit ones lose progress.
/// Returns whether anything changed and the items that finished cooking.
pub fn tick_campfire(
    recipes: &RecipeSet,
    items: &mut [Option<ItemStack>],
    campfire: &mut Campfire,
    lit: bool,
) -> (bool, Vec<ItemStack>) {
    let mut changed = false;
    let mut popped = Vec::new();
    for slot in 0..CAMPFIRE_SLOTS {
        if lit {
            let Some(item) = items[slot].as_ref().map(|stack| stack.item()) else {
                continue;
            };
            changed = true;
            campfire.cooking_progress[slot] += 1;
            if campfire.cooking_progress[slot] >= campfire.cooking_time[slot] {
                let Some(input) = items[slot].take() else {
                    continue;
                };
                let result = match recipe_for(recipes, ContainerKind::Campfire, item) {
                    Some((_, recipe)) => {
                        input.burn(BurnReason::CraftingInput);
                        result_stack(&recipe.result)
                    }
                    None => input,
                };
                popped.push(result);
            }
        } else if campfire.cooking_progress[slot] > 0 {
            changed = true;
            campfire.cooking_progress[slot] = (campfire.cooking_progress[slot] - COOL_SPEED)
                .clamp(0, campfire.cooking_time[slot]);
        }
    }
    (changed, popped)
}

pub struct FurnaceTicker;

impl BlockEntityTicker for FurnaceTicker {
    fn tick(&self, view: &mut TickView) -> TickOutcome {
        let State::Furnace(furnace) = &mut *view.data else {
            return TickOutcome::default();
        };
        let tick = tick_furnace(view.recipes, view.kind, view.state, view.items, furnace);
        TickOutcome {
            set_state: tick
                .lit
                .and_then(|lit| view.state.with("lit", if lit { "true" } else { "false" })),
            drops: tick.drops,
            ..TickOutcome::default()
        }
    }
}

pub struct CampfireTicker;

impl BlockEntityTicker for CampfireTicker {
    fn tick(&self, view: &mut TickView) -> TickOutcome {
        let State::Campfire(campfire) = &mut *view.data else {
            return TickOutcome::default();
        };
        let lit = view.state.get("lit") == Some("true");
        let (_, popped) = tick_campfire(view.recipes, view.items, campfire, lit);
        TickOutcome {
            send_data: !popped.is_empty(),
            drops: popped,
            ..TickOutcome::default()
        }
    }
}

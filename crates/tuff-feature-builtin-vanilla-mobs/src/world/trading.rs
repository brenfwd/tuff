//! Merchant offers: what a villager or wandering trader sells, at which
//! price and how often, how offers are drawn from the data pack's trade
//! sets, and how they are saved and matched against what a player pays.

use rand::Rng;
use serde_json::Value;
use tuff_item::item::BurnReason;
use tuff_item::registries::trades::{Cost, IdSet, Trade, TradeSet, Trades};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::{MobEffect, Potion, VillagerType};

use tuff_item::item::enchanting::{
    enchant, enchant_randomly, enchantment_options, select_enchantments,
};
use tuff_item::item::{ItemStack, MintReason, SuspiciousEffect};

pub use tuff_core::world::trading::*;

/// Draws the offers of the trade set `set`, as the registries in effect
/// have it, for a merchant of type `merchant` and appends them.
pub fn add_offers_from_set(
    set: &str,
    merchant: Option<VillagerType>,
    offers: &mut Vec<MerchantOffer>,
    rng: &mut impl Rng,
) {
    let registries = tuff_item::registries::current();
    let trades = registries.trades();
    if let Some(set) = trades.sets.get(set) {
        add_offers(trades, set, merchant, offers, rng);
    }
}

/// Draws `set`'s offers from `trades`.
pub fn add_offers(
    trades: &Trades,
    set: &TradeSet,
    merchant: Option<VillagerType>,
    offers: &mut Vec<MerchantOffer>,
    rng: &mut impl Rng,
) {
    let wanted = set.amount.max(0) as usize;
    let mut candidates = trades.of_set(set);
    let mut found = 0;
    while found < wanted && !candidates.is_empty() {
        let roll = rng.gen_range(0..candidates.len());
        let trade = candidates[roll];
        if !set.allow_duplicates {
            candidates.remove(roll);
        }
        match offer_from_trade(trade, merchant, rng) {
            Some(offer) => {
                offers.push(offer);
                found += 1;
            }
            None if set.allow_duplicates => {
                candidates.remove(roll);
            }
            None => {}
        }
    }
}

/// The offer a trade makes, or `None` when the merchant does not offer it
/// or its item modifier discarded the result.
pub fn offer_from_trade(
    trade: &Trade,
    merchant: Option<VillagerType>,
    rng: &mut impl Rng,
) -> Option<MerchantOffer> {
    if let Some(types) = &trade.merchant_types
        && !merchant.is_some_and(|m| types.contains(&m))
    {
        return None;
    }
    let mut result = ItemStack::mint(
        trade.gives,
        trade.gives_count.clamp(1, 99) as u8,
        MintReason::Spawn,
    );
    let mut additional = 0;
    if let Some(modifier) = &trade.modifier {
        let (stack, cost) = apply_modifier(result, modifier, rng)?;
        result = stack;
        additional = cost;
    }
    if let Some(double) = &trade.double_price_enchantments
        && let Some(stored) = &result.components.stored_enchantments
        && stored.entries().iter().any(|(id, _)| match double {
            IdSet::Ids(ids) => ids.contains(id),
            IdSet::Tag(tag) => {
                tuff_datapack::tags::with(|tags| tags.contains("minecraft:enchantment", tag, id))
            }
        })
    {
        additional *= 2;
    }
    let buy = item_cost(&rolled(&trade.wants, rng)?, additional);
    if buy.count < 1 {
        return None;
    }
    let buy_b = match &trade.additional_wants {
        Some(cost) => Some(item_cost(&rolled(cost, rng)?, 0)),
        None => None,
    };
    if buy_b.as_ref().is_some_and(|c| c.count < 1) {
        return None;
    }
    let mut offer = MerchantOffer::new(buy, buy_b, result);
    offer.max_uses = (trade.max_uses as i32).max(1);
    offer.xp = (trade.xp as i32).max(0);
    offer.price_multiplier = trade.reputation_discount.max(0.0);
    Some(offer)
}

/// A cost with its count provider, if it has one, rolled.
/// What a rolled trade-table cost asks, `extra` added to its count.
fn item_cost(cost: &Cost, extra: i32) -> ItemCost {
    let count = (cost.count as i32)
        .saturating_add(extra)
        .clamp(0, cost.item.max_stack_size() as i32);
    let mut components = tuff_nbt::Compound::new();
    if let Some(potion) = cost.potion.as_deref() {
        components.insert(
            "minecraft:potion_contents",
            tuff_nbt::Compound::new().with("potion", potion),
        );
    }
    ItemCost {
        item: cost.item,
        count,
        components,
    }
}

fn rolled(cost: &Cost, rng: &mut impl Rng) -> Option<Cost> {
    let mut cost = cost.clone();
    if let Some(provider) = &cost.count_provider {
        cost.count = i64::from(int_provider(provider, rng)?);
    }
    Some(cost)
}

/// Applies a trade's item functions. Returns the stack and the extra cost
/// a function asked for, or `None` when the result was discarded.
fn apply_modifier(stack: ItemStack, json: &Value, rng: &mut impl Rng) -> Option<(ItemStack, i32)> {
    let functions: Vec<&Value> = match json {
        Value::Array(list) => list.iter().collect(),
        other => vec![other],
    };
    let mut stack = stack;
    let mut cost = None;
    for function in functions {
        let (next, extra) = apply_function(stack, function, rng)?;
        stack = next;
        if extra.is_some() {
            cost = extra;
        }
    }
    Some((stack, cost.unwrap_or(0)))
}

fn apply_function(
    mut stack: ItemStack,
    function: &Value,
    rng: &mut impl Rng,
) -> Option<(ItemStack, Option<i32>)> {
    let with_cost = function["include_additional_cost_component"].as_bool() == Some(true);
    match function["type"].as_str()? {
        "minecraft:enchant_randomly" => {
            let only_compatible = function["only_compatible"].as_bool().unwrap_or(true);
            let (stack, level) =
                enchant_randomly(stack, &function["options"], only_compatible, rng);
            let cost = level
                .filter(|_| with_cost)
                .map(|level| 2 + rng.gen_range(0..5 + level * 10) + 3 * level);
            Some((stack, cost))
        }
        "minecraft:enchant_with_levels" => {
            let levels = int_provider(&function["levels"], rng)?;
            let registry = tuff_item::enchantment::current();
            let options = enchantment_options(&registry, &function["options"]);
            let chosen = select_enchantments(&stack, levels, &options, rng);
            let stack = enchant(stack, &chosen);
            let cost = (with_cost && levels > 0).then_some(levels);
            Some((stack, cost))
        }
        "minecraft:filtered" => {
            let filter = &function["item_filter"];
            filter_passes(&stack, filter).then_some((stack, None))
        }
        "minecraft:exploration_map" => {
            let spec = tuff_core::world::exploration::ExplorationSpec::from_json(function)?;
            tuff_core::world::exploration::mark(&mut stack, &spec);
            Some((stack, None))
        }
        "minecraft:set_enchantments" => {
            let add = function["add"].as_bool().unwrap_or(false);
            if stack.item() == Item::Book {
                stack = ItemStack::mint(Item::EnchantedBook, stack.count(), MintReason::Spawn);
            }
            let target = if stack.item() == Item::EnchantedBook {
                &mut stack.components.stored_enchantments
            } else {
                &mut stack.components.enchantments
            };
            let mut entries: Vec<(String, i32)> = target
                .as_ref()
                .map(|e| e.entries().to_vec())
                .unwrap_or_default();
            for (id, provider) in function["enchantments"].as_object()? {
                let value = int_provider(provider, rng)?;
                let current = entries
                    .iter()
                    .find(|(entry, _)| entry == id)
                    .map_or(0, |(_, level)| *level);
                let level = if add { current + value } else { value }.clamp(0, 255);
                entries.retain(|(entry, _)| entry != id);
                if level > 0 {
                    entries.push((id.clone(), level));
                }
            }
            *target = (!entries.is_empty()).then(|| tuff_item::item::Enchantments::new(entries));
            Some((stack, None))
        }
        "minecraft:set_potion" => {
            let potion = Potion::from_name(function["id"].as_str()?)?;
            set_potion(&mut stack, potion);
            Some((stack, None))
        }
        "minecraft:set_random_potion" => {
            let options: Vec<Potion> = match function.get("options") {
                None => Potion::ALL.to_vec(),
                Some(value) => potion_options(value),
            };
            if !options.is_empty() {
                set_potion(&mut stack, options[rng.gen_range(0..options.len())]);
            }
            Some((stack, None))
        }
        "minecraft:set_stew_effect" => {
            let effects = function["effects"].as_array()?;
            if stack.item() != Item::SuspiciousStew || effects.is_empty() {
                return Some((stack, None));
            }
            let entry = &effects[rng.gen_range(0..effects.len())];
            let effect = MobEffect::from_name(entry["type"].as_str()?)?;
            let mut duration = int_provider(&entry["duration"], rng)?;
            if !effect.is_instantaneous() {
                duration *= 20;
            }
            stack
                .components
                .suspicious_stew_effects
                .get_or_insert_with(Vec::new)
                .push(SuspiciousEffect { effect, duration });
            Some((stack, None))
        }
        "minecraft:set_random_dyes" => {
            let rolls = int_provider(&function["number_of_dyes"], rng)?;
            if rolls <= 0 {
                return Some((stack, None));
            }
            let dyes: Vec<DyeColor> = (0..rolls)
                .map(|_| DyeColor::ALL[rng.gen_range(0..DyeColor::ALL.len())])
                .collect();
            let color = tuff_item::item::decor::apply_dyes(stack.components.dyed_color, &dyes);
            stack.resize(1, MintReason::Trade, BurnReason::Template);
            stack.components.dyed_color = Some(color);
            Some((stack, None))
        }
        _ => None,
    }
}

fn set_potion(stack: &mut ItemStack, potion: Potion) {
    let contents = stack
        .components
        .potion_contents
        .get_or_insert_with(Default::default);
    contents.potion = Some(potion);
}

fn potion_options(value: &Value) -> Vec<Potion> {
    match value {
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => {
                tuff_datapack::tags::with(|tags| tags.typed::<Potion>(tag)).unwrap_or_default()
            }
            None => Potion::from_name(s).into_iter().collect(),
        },
        Value::Array(list) => list
            .iter()
            .filter_map(|v| Potion::from_name(v.as_str()?))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a filter's item and component predicates accept `stack`. Only
/// "has the component" predicates are understood; anything more specific
/// fails.
fn filter_passes(stack: &ItemStack, filter: &Value) -> bool {
    if let Some(items) = filter.get("items") {
        let name = stack.item().name();
        let listed = match items {
            Value::String(s) => match s.strip_prefix('#') {
                Some(tag) => {
                    tuff_datapack::tags::with(|tags| tags.contains("minecraft:item", tag, name))
                }
                None => s == name,
            },
            Value::Array(list) => list.iter().any(|v| v.as_str() == Some(name)),
            _ => false,
        };
        if !listed {
            return false;
        }
    }
    let Some(predicates) = filter.get("predicates").and_then(Value::as_object) else {
        return true;
    };
    let any = |value: &Value| match value {
        Value::Array(list) => list
            .iter()
            .all(|v| v.as_object().is_some_and(|o| o.is_empty())),
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    predicates.iter().all(|(key, value)| {
        let c = &stack.components;
        match key.as_str() {
            "minecraft:enchantments" => enchantments_match(c.enchantments.as_ref(), value),
            "minecraft:stored_enchantments" => {
                enchantments_match(c.stored_enchantments.as_ref(), value)
            }
            "minecraft:dyed_color" => any(value) && c.dyed_color.is_some(),
            "minecraft:map_id" => {
                any(value)
                    && (c.map_id.is_some()
                        || tuff_core::world::exploration::pending(stack).is_some())
            }
            _ => false,
        }
    })
}

/// An enchantments item predicate: each entry (an optional enchantment set
/// and level bounds) must be met by one of the enchantments.
fn enchantments_match(
    enchantments: Option<&tuff_item::item::Enchantments>,
    wanted: &Value,
) -> bool {
    use tuff_core::world::predicate::{bounds, in_set};
    let Some(entries) = wanted.as_array() else {
        return false;
    };
    let have = enchantments.map_or(&[][..], |e| e.entries());
    !have.is_empty()
        && entries.iter().all(|entry| {
            let Some(entry) = entry.as_object() else {
                return false;
            };
            have.iter().any(|(id, level)| {
                entry.get("enchantments").is_none_or(|set| {
                    in_set(set, id, |tag| {
                        tuff_datapack::tags::with(|tags| {
                            tags.contains("minecraft:enchantment", tag, id)
                        })
                    })
                }) && entry
                    .get("levels")
                    .is_none_or(|b| bounds(b, f64::from(*level)))
            })
        })
}

/// An integer number provider: a constant, `uniform`, `binomial`,
/// `constant` or a sum of those (`add`).
fn int_provider(value: &Value, rng: &mut impl Rng) -> Option<i32> {
    if let Some(n) = value.as_f64() {
        return Some(n as i32);
    }
    match value["type"].as_str()? {
        "minecraft:constant" => Some(value["value"].as_f64()? as i32),
        "minecraft:uniform" => {
            let min = int_provider(&value["min"], rng)?;
            let max = int_provider(&value["max"], rng)?;
            Some(if max <= min {
                min
            } else {
                rng.gen_range(min..=max)
            })
        }
        "minecraft:binomial" => {
            let n = int_provider(&value["n"], rng)?;
            let p = value["p"].as_f64()? as f32;
            Some((0..n.max(0)).filter(|_| rng.r#gen::<f32>() < p).count() as i32)
        }
        "minecraft:add" => value["inputs"]
            .as_array()?
            .iter()
            .map(|input| int_provider(input, rng))
            .sum(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    #[tuff_core::watchdog]
    #[test]
    fn prices_follow_demand_and_special_prices() {
        let mut offer = MerchantOffer::new(
            ItemCost::new(Item::Wheat, 20),
            None,
            ItemStack::mint(Item::Emerald, 1, MintReason::Test),
        );
        offer.max_uses = 16;
        offer.price_multiplier = 0.05;
        assert_eq!(offer.cost_a(), 20);
        offer.uses = 16;
        offer.update_demand();
        assert_eq!(offer.demand, 16);
        assert_eq!(offer.cost_a(), 20 + 16);
        offer.special_price = -40;
        assert_eq!(offer.cost_a(), 1, "never below one");
        offer.special_price = 100;
        assert_eq!(offer.cost_a(), 64, "never above a stack");
        offer.demand = -5;
        offer.special_price = 0;
        assert_eq!(offer.cost_a(), 20, "negative demand does not lower prices");
    }

    #[tuff_core::watchdog]
    #[test]
    fn offers_are_paid_from_either_slot_order() {
        let offer = MerchantOffer::new(
            ItemCost::new(Item::Emerald, 5),
            Some(ItemCost::new(Item::Book, 1)),
            ItemStack::mint(Item::EnchantedBook, 1, MintReason::Test),
        );
        let emeralds = ItemStack::mint(Item::Emerald, 7, MintReason::Test);
        let book = ItemStack::mint(Item::Book, 1, MintReason::Test);
        assert!(offer.satisfied_by(Some(emeralds.view()), Some(book.view())));
        assert!(!offer.satisfied_by(Some(book.view()), Some(emeralds.view())));
        assert!(!offer.satisfied_by(Some(emeralds.view()), None));
        let (mut a, mut b) = (Some(emeralds), Some(book));
        assert!(offer.take(&mut a, &mut b));
        assert_eq!(a.map(|s| s.count()), Some(2));
        assert_eq!(b, None);
        let offers = [offer];
        let pay = ItemStack::mint(Item::Emerald, 5, MintReason::Test);
        let book = ItemStack::mint(Item::Book, 1, MintReason::Test);
        assert_eq!(
            recipe_for(&offers, Some(pay.view()), Some(book.view()), 0),
            Some(0)
        );
        assert_eq!(recipe_for(&offers, Some(pay.view()), None, 0), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn offers_save_with_the_game_keys() {
        let mut offer = MerchantOffer::new(
            ItemCost::new(Item::Emerald, 1),
            None,
            ItemStack::mint(Item::Bread, 6, MintReason::Test),
        );
        offer.max_uses = 16;
        offer.price_multiplier = 0.05;
        offer.uses = 3;
        let nbt = offers_to_nbt(std::slice::from_ref(&offer));
        let saved = nbt.get_list("Recipes").unwrap()[0].as_compound().unwrap();
        assert_eq!(saved.get_int("maxUses"), Some(16));
        assert_eq!(saved.get_int("uses"), Some(3));
        assert_eq!(saved.get_float("priceMultiplier"), Some(0.05));
        assert!(saved.get("xp").is_none(), "the default is left out");
        assert_eq!(saved.get_compound("buy").unwrap().get_int("count"), Some(1));
        assert_eq!(offers_from_nbt(&nbt), vec![offer]);
        assert!(offers_to_nbt(&[]).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_bottle_costs_need_the_potion() {
        let trade = Trade::builtin("minecraft:wandering_trader/water_bottle_emerald").unwrap();
        let offer = offer_from_trade(trade, None, &mut rng()).unwrap();
        let mut water = ItemStack::mint(Item::Potion, 1, MintReason::Test);
        set_potion(&mut water, Potion::Water);
        let mut awkward = ItemStack::mint(Item::Potion, 1, MintReason::Test);
        set_potion(&mut awkward, Potion::Awkward);
        assert!(offer.buy.matches(water.view()));
        assert!(!offer.buy.matches(awkward.view()));
        assert!(
            !offer
                .buy
                .matches(ItemStack::mint(Item::Potion, 1, MintReason::Test).view())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cost_components_match_as_a_subset() {
        let trade = Trade::builtin("minecraft:wandering_trader/water_bottle_emerald").unwrap();
        let offer = offer_from_trade(trade, None, &mut rng()).unwrap();
        let mut water = ItemStack::mint(Item::Potion, 1, MintReason::Test);
        set_potion(&mut water, Potion::Water);
        water.components.repair_cost = Some(3);
        assert!(offer.buy.matches(water.view()));
        let mut awkward = ItemStack::mint(Item::Potion, 1, MintReason::Test);
        set_potion(&mut awkward, Potion::Awkward);
        awkward.components.repair_cost = Some(3);
        assert!(!offer.buy.matches(awkward.view()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn trade_sets_draw_distinct_offers() {
        let set = "minecraft:farmer/level_1";
        for seed in 0..20 {
            let mut offers = Vec::new();
            add_offers_from_set(
                set,
                Some(VillagerType::Plains),
                &mut offers,
                &mut StdRng::seed_from_u64(seed),
            );
            assert_eq!(offers.len(), 2);
            assert_ne!(offers[0], offers[1]);
            assert!(offers.iter().all(|o| o.max_uses == 16));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn enchanted_books_cost_their_level() {
        let trade =
            Trade::builtin("minecraft:librarian/1/emerald_and_book_enchanted_book").unwrap();
        for seed in 0..50 {
            let offer = offer_from_trade(trade, None, &mut StdRng::seed_from_u64(seed)).unwrap();
            assert_eq!(offer.sell.item(), Item::EnchantedBook);
            let stored = offer.sell.components.stored_enchantments.as_ref().unwrap();
            let (id, level) = &stored.entries()[0];
            assert!(tuff_datapack::tags::has(
                tuff_vanilla_data::tag::enchantment::key::TRADEABLE,
                id.as_str()
            ));
            let base = 2 + 3 * level;
            let max = base + 4 + level * 10;
            let doubled = tuff_datapack::tags::has(
                tuff_vanilla_data::tag::enchantment::key::DOUBLE_TRADE_PRICE,
                id.as_str(),
            );
            let (low, high) = if doubled {
                (base * 2, max * 2)
            } else {
                (base, max)
            };
            let count = offer.buy.count;
            assert!(
                count == 64 || (low..=high).contains(&count),
                "{id} {level}: {count}"
            );
            assert_eq!(offer.buy_b.as_ref().unwrap().item, Item::Book);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn rebalanced_trades_set_their_enchantments_and_roll_their_price() {
        let book = Trade::builtin("minecraft:librarian/5/emerald_and_book_desert_enchanted_book")
            .expect("the trade_rebalance pack's trades are compiled in");
        assert!(offer_from_trade(book, Some(VillagerType::Plains), &mut rng()).is_none());
        let mut prices = std::collections::BTreeSet::new();
        let mut rng = rng();
        for _ in 0..40 {
            let offer = offer_from_trade(book, Some(VillagerType::Desert), &mut rng).unwrap();
            let stored = offer.sell.components.stored_enchantments.as_ref().unwrap();
            assert_eq!(stored.level("minecraft:efficiency"), 3);
            assert!((11..=46).contains(&offer.buy.count), "{}", offer.buy.count);
            prices.insert(offer.buy.count);
        }
        assert!(prices.len() > 1, "the price is rolled per offer");

        let boots =
            Trade::builtin("minecraft:armorer/4/emerald_enchanted_chainmail_boots_jungle").unwrap();
        let offer = offer_from_trade(boots, Some(VillagerType::Jungle), &mut rng).unwrap();
        let enchantments = offer.sell.components.enchantments.as_ref().unwrap();
        assert_eq!(enchantments.level("minecraft:unbreaking"), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn trade_sets_follow_the_villager_trade_tags_in_effect() {
        let set = TradeSet::builtin("minecraft:librarian/level_1").unwrap();
        let IdSet::Tag(tag) = &set.trades else {
            panic!("librarian trades come from a tag");
        };
        let registries = tuff_item::registries::Registries::vanilla();
        let trades = registries.trades();
        let generated = tuff_vanilla_data::villager::TradeSet::get(&set.name).unwrap();
        let built_in: Vec<&str> = trades.of_set(set).iter().map(|t| &*t.name).collect();
        assert_eq!(
            built_in,
            generated.trades.iter().map(|t| t.name).collect::<Vec<_>>()
        );
        let swapped = tuff_datapack::tags::vanilla_with(&[(
            "minecraft:villager_trade",
            tag,
            "minecraft:librarian/1/emerald_and_book_desert_enchanted_book",
        )]);
        let names: Vec<&str> = tuff_datapack::tags::sync_scope(swapped, || {
            trades.of_set(set).iter().map(|t| &*t.name).collect()
        });
        assert!(names.contains(&"minecraft:librarian/1/emerald_and_book_desert_enchanted_book"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn enchanted_gear_carries_enchantments() {
        let trade = Trade::builtin("minecraft:armorer/5/emerald_diamond_leggings")
            .or_else(|| {
                Trade::builtins()
                    .find(|t| t.gives == Item::DiamondLeggings && t.merchant_types.is_none())
            })
            .unwrap();
        let offer = offer_from_trade(trade, None, &mut rng()).unwrap();
        let enchantments = offer.sell.components.enchantments.as_ref().unwrap();
        assert!(!enchantments.entries().is_empty());
        assert!(offer.buy.count > trade.wants.count as i32);
    }

    #[tuff_core::watchdog]
    #[test]
    fn variant_trades_need_the_right_villager_type() {
        let trade = Trade::builtins()
            .find(|t| t.merchant_types.is_some() && t.modifier.is_none())
            .unwrap();
        let types = trade.merchant_types.as_ref().unwrap();
        let allowed = types[0];
        let other = VillagerType::ALL
            .iter()
            .copied()
            .find(|t| !types.contains(t))
            .unwrap();
        assert!(offer_from_trade(trade, Some(allowed), &mut rng()).is_some());
        assert!(offer_from_trade(trade, Some(other), &mut rng()).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn explorer_maps_are_offered_waiting_for_their_map() {
        let trade = Trade::builtins()
            .find(|t| {
                t.modifier
                    .as_ref()
                    .is_some_and(|m| m.to_string().contains("on_woodland_mansion_maps"))
            })
            .unwrap();
        let offer = offer_from_trade(trade, None, &mut rng()).unwrap();
        assert_eq!(offer.sell.item(), Item::WoodlandMansionMap);
        let spec = tuff_core::world::exploration::pending(&offer.sell).unwrap();
        assert_eq!(spec.destination, "#minecraft:on_woodland_mansion_maps");
        assert_eq!(spec.search_radius, 100);
    }

    #[tuff_core::watchdog]
    #[test]
    fn dyed_and_potion_results() {
        let dyed = Trade::builtins()
            .find(|t| {
                t.modifier
                    .as_ref()
                    .is_some_and(|m| m.to_string().contains("set_random_dyes"))
            })
            .unwrap();
        let offer = offer_from_trade(dyed, None, &mut rng()).unwrap();
        assert!(offer.sell.components.dyed_color.is_some());
        let arrows = Trade::builtins()
            .find(|t| {
                t.modifier
                    .as_ref()
                    .is_some_and(|m| m.to_string().contains("set_random_potion"))
            })
            .unwrap();
        let offer = offer_from_trade(arrows, None, &mut rng()).unwrap();
        let potion = offer
            .sell
            .components
            .potion_contents
            .as_ref()
            .unwrap()
            .potion;
        assert!(tuff_datapack::tags::is(
            tuff_vanilla_data::tag::potion::key::TRADEABLE,
            potion.unwrap()
        ));
    }
}

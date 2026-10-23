//! Enchanting the way loot functions and trades do it: picking from an
//! `options` set, and the enchanting table's level-based selection.

use rand::Rng;
use serde_json::Value;
use tuff_vanilla_data::Item;

use crate::enchantment::{self, EnchantmentDef, EnchantmentRegistry};
use crate::item::{Enchantments, ItemStack, MintReason, Mode};

/// One enchantment of a registry: its id and definition.
pub type Choice<'a> = (&'a str, &'a EnchantmentDef);

/// The enchantments an `options` field names: a tag, an id or a list;
/// every enchantment when absent.
pub fn enchantment_options<'a>(
    registry: &'a EnchantmentRegistry,
    value: &Value,
) -> Vec<Choice<'a>> {
    let lookup = |name: &str| {
        let index = registry.index_of(name)?;
        Some((registry.id(index)?, registry.get(name)?))
    };
    match value {
        Value::Null => registry
            .iter()
            .map(|(id, entry)| (id, entry.value()))
            .collect(),
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => tuff_datapack::tags::with(|tags| {
                tags.get("minecraft:enchantment", tag)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|name| lookup(name))
                    .collect()
            }),
            None => lookup(s).into_iter().collect(),
        },
        Value::Array(list) => list
            .iter()
            .filter_map(Value::as_str)
            .filter_map(lookup)
            .collect(),
        _ => Vec::new(),
    }
}

/// Adds enchantments, keeping the higher level of any it already has; a
/// book becomes an enchanted book holding them.
pub fn enchant<S: AsRef<str>>(mut stack: ItemStack, chosen: &[(S, i32)]) -> ItemStack {
    if chosen.is_empty() {
        return stack;
    }
    if stack.item == Item::Book {
        let count = stack.count;
        std::mem::replace(
            &mut stack,
            ItemStack::mint(Item::EnchantedBook, count, MintReason::Transform),
        )
        .burn(crate::item::BurnReason::Transform);
    }
    let target = if stack.item == Item::EnchantedBook {
        &mut stack.components.stored_enchantments
    } else {
        &mut stack.components.enchantments
    };
    let mut entries: Vec<(String, i32)> = target
        .as_ref()
        .map(|e| e.entries().to_vec())
        .unwrap_or_default();
    for (name, level) in chosen {
        let name = name.as_ref();
        match entries.iter_mut().find(|(id, _)| id == name) {
            Some((_, existing)) => *existing = (*existing).max(*level),
            None => entries.push((name.to_owned(), *level)),
        }
    }
    *target = Some(Enchantments::new(entries));
    stack
}

/// The enchanting table's choice for `cost` levels among `options`.
pub fn select_enchantments<M: Mode>(
    stack: &ItemStack<M>,
    cost: i32,
    options: &[Choice<'_>],
    rng: &mut impl Rng,
) -> Vec<(String, i32)> {
    let mut results: Vec<(Choice<'_>, i32)> = Vec::new();
    let Some(enchantability) = stack.item.enchantable() else {
        return Vec::new();
    };
    let quarter = enchantability as i32 / 4 + 1;
    let cost = cost + 1 + rng.gen_range(0..quarter) + rng.gen_range(0..quarter);
    let span = (rng.r#gen::<f32>() + rng.r#gen::<f32>() - 1.0) * 0.15;
    let mut cost = ((cost as f32 + cost as f32 * span).round() as i32).max(1);
    let book = stack.item == Item::Book;
    let mut available: Vec<(Choice<'_>, i32)> = options
        .iter()
        .filter(|(_, e)| book || e.is_primary(stack.item))
        .filter_map(|&(id, e)| {
            (1..=e.max_level as i32)
                .rev()
                .find(|level| {
                    cost >= e.min_cost.at_level(*level as u32)
                        && cost <= e.max_cost.at_level(*level as u32)
                })
                .map(|level| ((id, e), level))
        })
        .collect();
    if available.is_empty() {
        return Vec::new();
    }
    if let Some(pick) = weighted(&available, rng) {
        results.push(pick);
    }
    while rng.gen_range(0..50) <= cost {
        if let Some(&(last, _)) = results.last() {
            available.retain(|(e, _)| enchantment::compatible(*e, last));
        }
        if available.is_empty() {
            break;
        }
        if let Some(pick) = weighted(&available, rng) {
            results.push(pick);
        }
        cost /= 2;
    }
    results
        .into_iter()
        .map(|((id, _), level)| (id.to_owned(), level))
        .collect()
}

fn weighted<'a>(entries: &[(Choice<'a>, i32)], rng: &mut impl Rng) -> Option<(Choice<'a>, i32)> {
    let total: i32 = entries.iter().map(|((_, e), _)| e.weight as i32).sum();
    if total <= 0 {
        return None;
    }
    let mut roll = rng.gen_range(0..total);
    for entry in entries {
        roll -= (entry.0).1.weight as i32;
        if roll < 0 {
            return Some(*entry);
        }
    }
    None
}

/// One random enchantment from `options` at a random level, as
/// `enchant_randomly` does; with `only_compatible`, only those the item
/// takes (books take any). The level picked, if any.
pub fn enchant_randomly(
    stack: ItemStack,
    options: &Value,
    only_compatible: bool,
    rng: &mut impl Rng,
) -> (ItemStack, Option<i32>) {
    let book = stack.item == Item::Book;
    let registry = enchantment::current();
    let options: Vec<Choice<'_>> = enchantment_options(&registry, options)
        .into_iter()
        .filter(|(_, e)| book || !only_compatible || e.supports(stack.item))
        .collect();
    if options.is_empty() {
        return (stack, None);
    }
    let (id, enchantment) = options[rng.gen_range(0..options.len())];
    let level = rng.gen_range(1..=enchantment.max_level.max(1)) as i32;
    (enchant(stack, &[(id, level)]), Some(level))
}

/// Enchants `stack` with the enchantment provider `id` of the registries
/// in effect; `special_multiplier` is the local difficulty's. Unknown
/// providers leave the stack alone.
pub fn enchant_from_provider(
    stack: ItemStack,
    id: &str,
    special_multiplier: f32,
    rng: &mut impl Rng,
) -> ItemStack {
    let chosen = provider_enchantments(&stack, id, special_multiplier, rng);
    enchant(stack, &chosen)
}

/// The enchantments the provider `id` picks for `stack`; none for an
/// unknown provider.
pub fn provider_enchantments<M: Mode>(
    stack: &ItemStack<M>,
    id: &str,
    special_multiplier: f32,
    rng: &mut impl Rng,
) -> Vec<(String, i32)> {
    use crate::enchantment::{EnchantmentProvider, HolderSet};
    let data = enchantment::current();
    let Some(provider) = data.providers.get(id) else {
        return Vec::new();
    };
    let options = |set: &HolderSet| match set {
        HolderSet::Tag(tag) => enchantment_options(&data, &Value::String(format!("#{tag}"))),
        HolderSet::Ids(ids) => ids
            .iter()
            .filter_map(|id| {
                let index = data.index_of(id)?;
                Some((data.id(index)?, data.get(id)?))
            })
            .collect(),
    };
    match provider {
        EnchantmentProvider::ByCostWithDifficulty {
            enchantments,
            min_cost,
            max_cost_span,
        } => {
            let max = min_cost + (special_multiplier * *max_cost_span as f32) as i32;
            let cost = rng.gen_range(*min_cost..=max.max(*min_cost));
            select_enchantments(stack, cost, &options(enchantments), rng)
        }
        EnchantmentProvider::ByCost { enchantments, cost } => {
            let cost = cost.sample(rng);
            select_enchantments(stack, cost, &options(enchantments), rng)
        }
        EnchantmentProvider::Single { enchantment, level } => {
            let Some(def) = data.get(enchantment) else {
                return Vec::new();
            };
            let level = level.sample(rng).clamp(1, def.max_level as i32);
            vec![(enchantment.to_string(), level)]
        }
    }
}

/// The most bookshelves an enchanting table counts.
pub const TABLE_MAX_BOOKCASES: i32 = 15;

/// What an enchanting table's three buttons show for the item in its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableOffers {
    pub costs: [i32; 3],
    /// Registry index of each button's hint enchantment, -1 for none.
    pub clue: [i32; 3],
    pub level_clue: [i32; 3],
}

impl TableOffers {
    pub const NONE: Self = Self {
        costs: [0; 3],
        clue: [-1; 3],
        level_clue: [-1; 3],
    };
}

/// Whether the table can enchant `stack`: an enchantable item with no
/// enchantments yet.
pub fn table_accepts(stack: &ItemStack) -> bool {
    stack.item.enchantable().is_some()
        && stack
            .components
            .enchantments
            .as_ref()
            .is_none_or(|e| e.entries().is_empty())
}

fn seeded(seed: i32) -> rand::rngs::StdRng {
    rand::SeedableRng::seed_from_u64(seed as u32 as u64)
}

fn table_cost(rng: &mut impl Rng, slot: usize, bookcases: i32, stack: &ItemStack) -> i32 {
    if stack.item.enchantable().is_none_or(|v| v == 0) {
        return 0;
    }
    let bookcases = bookcases.clamp(0, TABLE_MAX_BOOKCASES);
    let selected = rng.gen_range(0..8) + 1 + (bookcases >> 1) + rng.gen_range(0..=bookcases);
    match slot {
        0 => (selected / 3).max(1),
        1 => selected * 2 / 3 + 1,
        _ => selected.max(bookcases * 2),
    }
}

/// The enchantments button `slot` applies for `cost` levels.
pub fn table_enchantments(
    stack: &ItemStack,
    seed: i32,
    slot: usize,
    cost: i32,
) -> Vec<(String, i32)> {
    let mut rng = seeded(seed.wrapping_add(slot as i32));
    table_list(&mut rng, stack, cost)
}

fn table_list(rng: &mut rand::rngs::StdRng, stack: &ItemStack, cost: i32) -> Vec<(String, i32)> {
    let registry = enchantment::current();
    let options = enchantment_options(
        &registry,
        &Value::String("#minecraft:in_enchanting_table".into()),
    );
    let mut list = select_enchantments(stack, cost, &options, rng);
    if stack.item == Item::Book && list.len() > 1 {
        let drop = rng.gen_range(0..list.len());
        list.remove(drop);
    }
    list
}

fn registry_index(name: &str) -> i32 {
    enchantment::current()
        .index_of(name)
        .map_or(-1, |index| index as i32)
}

/// The costs and hints the table shows for `stack` with `bookcases`
/// shelves around it, from the player's enchantment `seed`.
pub fn table_offers(stack: &ItemStack, seed: i32, bookcases: i32) -> TableOffers {
    if !table_accepts(stack) {
        return TableOffers::NONE;
    }
    let mut rng = seeded(seed);
    let mut offers = TableOffers::NONE;
    for (slot, cost) in offers.costs.iter_mut().enumerate() {
        *cost = table_cost(&mut rng, slot, bookcases, stack);
        if *cost < slot as i32 + 1 {
            *cost = 0;
        }
    }
    for slot in 0..3 {
        if offers.costs[slot] <= 0 {
            continue;
        }
        let mut rng = seeded(seed.wrapping_add(slot as i32));
        let list = table_list(&mut rng, stack, offers.costs[slot]);
        if list.is_empty() {
            continue;
        }
        let (name, level) = &list[rng.gen_range(0..list.len())];
        offers.clue[slot] = registry_index(name);
        let level = *level;
        offers.level_clue[slot] = level;
    }
    offers
}

#[cfg(test)]
mod table_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn nothing_is_offered_for_unenchantable_or_enchanted_items() {
        assert_eq!(
            table_offers(&ItemStack::mint(Item::Stick, 1, MintReason::Test), 7, 15),
            TableOffers::NONE
        );
        let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
        sword.components.enchantments =
            Some(Enchantments::new(vec![("minecraft:sharpness".into(), 1)]));
        assert_eq!(table_offers(&sword, 7, 15), TableOffers::NONE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn costs_scale_with_bookshelves_and_are_deterministic() {
        let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
        let none = table_offers(&sword, 12345, 0);
        assert_eq!(none, table_offers(&sword, 12345, 0));
        assert!(none.costs.iter().all(|c| *c <= 8));
        let full = table_offers(&sword, 12345, 15);
        assert!(full.costs[2] >= 30);
        assert!(full.costs[0] <= full.costs[2]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_clue_is_one_of_the_enchantments_the_button_applies() {
        let sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
        for seed in 0..40 {
            let offers = table_offers(&sword, seed, 15);
            for slot in 0..3 {
                if offers.costs[slot] == 0 {
                    continue;
                }
                let applied = table_enchantments(&sword, seed, slot, offers.costs[slot]);
                assert!(!applied.is_empty());
                assert!(applied.iter().any(|(name, level)| {
                    registry_index(name) == offers.clue[slot] && *level == offers.level_clue[slot]
                }));
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_book_always_gets_an_enchantment_at_thirty_levels() {
        let book = ItemStack::mint(Item::Book, 1, MintReason::Test);
        for seed in 0..60 {
            let list = table_enchantments(&book, seed, 2, 30);
            assert!(!list.is_empty());
        }
    }
}

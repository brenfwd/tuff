//! The vanilla anvil's repair, combine and rename arithmetic.

use tuff_core::world::anvil::AnvilOutcome;
use tuff_item::enchantment::{self, EnchantmentRegistry};
use tuff_item::item::enchanting::Choice;
use tuff_item::item::{Enchantments, ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::Item;

const TOO_EXPENSIVE: i64 = 40;
const REPAIR_CHANCE_DIVISOR: u32 = 4;
const COMBINE_BONUS_PERCENT: u32 = 12;

pub fn increased_repair_cost(base: u32) -> u32 {
    base.saturating_mul(2).saturating_add(1)
}

fn is_blank(name: &str) -> bool {
    name.chars().all(char::is_whitespace)
}

pub fn crafting_enchantments<'a>(
    registry: &'a EnchantmentRegistry,
    stack: &ItemStack,
) -> Vec<(Choice<'a>, i32)> {
    let list = if stack.item() == Item::EnchantedBook {
        &stack.components.stored_enchantments
    } else {
        &stack.components.enchantments
    };
    list.iter()
        .flat_map(|e| e.entries())
        .filter_map(|(id, level)| {
            let index = registry.index_of(id)?;
            Some(((registry.id(index)?, registry.get(id)?), *level))
        })
        .collect()
}

pub fn set_enchantments(stack: &mut ItemStack, list: &[(Choice<'_>, i32)]) {
    let value = (!list.is_empty())
        .then(|| Enchantments::new(list.iter().map(|((id, _), l)| ((*id).to_owned(), *l))));
    if stack.item() == Item::EnchantedBook {
        stack.components.stored_enchantments = value;
    } else {
        stack.components.enchantments = value;
    }
}

fn is_valid_repair_item(input: &ItemStack, addition: &ItemStack) -> bool {
    input.item().repairable_with().contains(&addition.item())
}

/// The stack an operation is working out; burned if it is abandoned.
struct Candidate(Option<ItemStack>);

impl Candidate {
    fn release(mut self) -> ItemStack {
        self.0.take().expect("held until released")
    }
}

impl std::ops::Deref for Candidate {
    type Target = ItemStack;

    fn deref(&self) -> &ItemStack {
        self.0.as_ref().expect("held until released")
    }
}

impl std::ops::DerefMut for Candidate {
    fn deref_mut(&mut self) -> &mut ItemStack {
        self.0.as_mut().expect("held until released")
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if let Some(stack) = self.0.take() {
            stack.burn(tuff_item::item::BurnReason::Preview);
        }
    }
}

pub fn compute(
    input: Option<&ItemStack>,
    addition: Option<&ItemStack>,
    name: Option<&str>,
    infinite: bool,
) -> AnvilOutcome {
    let Some(input) = input else {
        return AnvilOutcome::NOTHING;
    };
    let mut result = Candidate(Some(ItemStack::mint_like(
        input,
        input.count(),
        MintReason::Crafting,
    )));
    let registry = enchantment::current();
    let mut enchantments = crafting_enchantments(&registry, &result);
    let tax = i64::from(input.components.repair_cost.unwrap_or(0))
        + addition.map_or(0, |a| i64::from(a.components.repair_cost.unwrap_or(0)));
    let mut price: i64 = 0;
    let mut repair_count = 0u8;
    if let Some(addition) = addition {
        let using_book = addition.components.stored_enchantments.is_some();
        if result.is_damageable() && is_valid_repair_item(input, addition) {
            let max = result.max_damage().unwrap_or(0);
            let step = |damage: u32| damage.min(max / REPAIR_CHANCE_DIVISOR);
            let mut amount = step(result.damage());
            if amount == 0 {
                return AnvilOutcome::NOTHING;
            }
            let mut count = 0u8;
            while amount > 0 && count < addition.count() {
                let worn = result.damage() - amount;
                result.set_damage(worn);
                price += 1;
                amount = step(result.damage());
                count += 1;
            }
            repair_count = count;
        } else {
            if !using_book && (result.item() != addition.item() || !result.is_damageable()) {
                return AnvilOutcome::NOTHING;
            }
            if result.is_damageable() && !using_book {
                let max = result.max_damage().unwrap_or(0);
                let left_input = input
                    .max_damage()
                    .unwrap_or(0)
                    .saturating_sub(input.damage());
                let left_addition = addition
                    .max_damage()
                    .unwrap_or(0)
                    .saturating_sub(addition.damage());
                let remaining = left_input + left_addition + max * COMBINE_BONUS_PERCENT / 100;
                let damage = max.saturating_sub(remaining);
                if damage < result.damage() {
                    result.set_damage(damage);
                    price += 2;
                }
            }
            let mut any_compatible = false;
            let mut any_incompatible = false;
            for (enchantment, level) in crafting_enchantments(&registry, addition) {
                let current = enchantments
                    .iter()
                    .find(|(e, _)| e.0 == enchantment.0)
                    .map_or(0, |(_, l)| *l);
                let mut merged = if current == level {
                    level + 1
                } else {
                    level.max(current)
                };
                let mut compatible = infinite
                    || input.item() == Item::EnchantedBook
                    || enchantment.1.supports(input.item());
                for (other, _) in &enchantments {
                    if other.0 != enchantment.0 && !enchantment::compatible(*other, enchantment) {
                        compatible = false;
                        price += 1;
                    }
                }
                if !compatible {
                    any_incompatible = true;
                    continue;
                }
                any_compatible = true;
                merged = merged.min(enchantment.1.max_level as i32);
                match enchantments.iter_mut().find(|(e, _)| e.0 == enchantment.0) {
                    Some(entry) => entry.1 = merged,
                    None => enchantments.push((enchantment, merged)),
                }
                let mut fee = enchantment.1.anvil_cost as i64;
                if using_book {
                    fee = (fee / 2).max(1);
                }
                price += fee * i64::from(merged);
                if input.count() > 1 {
                    price = 40;
                }
            }
            if any_incompatible && !any_compatible {
                return AnvilOutcome::NOTHING;
            }
        }
    }
    let mut naming_cost = 0;
    let hover = input.components.custom_name.as_ref().map(|n| n.plain());
    match name.filter(|n| !is_blank(n)) {
        None => {
            if input.components.custom_name.is_some() {
                naming_cost = 1;
                price += 1;
                result.components.custom_name = None;
            }
        }
        Some(name) => {
            if hover.as_deref() != Some(name) {
                naming_cost = 1;
                price += 1;
                result.components.custom_name = Some(Box::new(TextComponent::new(name)));
            }
        }
    }
    let mut cost = if price <= 0 {
        0
    } else {
        (tax + price).clamp(0, i64::from(i32::MAX))
    };
    let mut keep = price > 0;
    let mut only_renaming = false;
    if naming_cost == price && naming_cost > 0 {
        if cost >= TOO_EXPENSIVE {
            cost = TOO_EXPENSIVE - 1;
        }
        only_renaming = true;
    }
    if cost >= TOO_EXPENSIVE && !infinite {
        keep = false;
    }
    if !keep {
        return AnvilOutcome {
            result: None,
            cost: cost as i32,
            repair_count,
            only_renaming,
        };
    }
    let mut base = result.components.repair_cost.unwrap_or(0);
    base = base.max(addition.map_or(0, |a| a.components.repair_cost.unwrap_or(0)));
    if naming_cost != price || naming_cost == 0 {
        base = increased_repair_cost(base);
    }
    result.components.repair_cost = Some(base);
    set_enchantments(&mut result, &enchantments);
    result.components.compact();
    AnvilOutcome {
        result: Some(result.release()),
        cost: cost as i32,
        repair_count,
        only_renaming,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;

    fn damaged(item: Item, damage: u32) -> ItemStack {
        let mut stack = ItemStack::mint(item, 1, MintReason::Test);
        stack.set_damage(damage);
        stack
    }

    fn enchanted(item: Item, id: &str, level: i32) -> ItemStack {
        let mut stack = ItemStack::mint(item, 1, MintReason::Test);
        let entries = vec![(id.to_owned(), level)];
        if item == Item::EnchantedBook {
            stack.components.stored_enchantments = Some(Enchantments::new(entries));
        } else {
            stack.components.enchantments = Some(Enchantments::new(entries));
        }
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn renaming_costs_one_level_and_does_not_raise_the_repair_cost() {
        let sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
        let out = compute(Some(&sword), None, Some("Sting"), false);
        assert_eq!(out.cost, 1);
        assert!(out.only_renaming);
        let result = out.result.expect("renamed");
        assert_eq!(
            result.components.custom_name.as_ref().unwrap().plain(),
            "Sting"
        );
        assert_eq!(result.components.repair_cost, Some(0));
        assert_eq!(compute(Some(&sword), None, None, false).result, None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn repairing_with_material_takes_quarter_steps_and_doubles_the_cost() {
        let max = Item::IronSword.max_damage().unwrap();
        let sword = damaged(Item::IronSword, max);
        let ingots = ItemStack::mint(Item::IronIngot, 5, MintReason::Test);
        let out = compute(Some(&sword), Some(&ingots), None, false);
        assert_eq!(out.repair_count, 5);
        assert_eq!(out.cost, 5);
        let result = out.result.unwrap();
        assert_eq!(result.damage(), 0);
        assert_eq!(result.components.repair_cost, Some(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn combining_books_merges_equal_levels_up_and_halves_the_fee() {
        let a = enchanted(Item::EnchantedBook, "minecraft:sharpness", 2);
        let b = enchanted(Item::EnchantedBook, "minecraft:sharpness", 2);
        let out = compute(Some(&a), Some(&b), None, false);
        let result = out.result.unwrap();
        let level = result
            .components
            .stored_enchantments
            .as_ref()
            .unwrap()
            .level("minecraft:sharpness");
        assert_eq!(level, 3);
        assert_eq!(out.cost, 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn incompatible_enchantments_yield_nothing_and_unsupported_ones_too() {
        let sword = enchanted(Item::DiamondSword, "minecraft:sharpness", 1);
        let smite = enchanted(Item::EnchantedBook, "minecraft:smite", 1);
        assert_eq!(
            compute(Some(&sword), Some(&smite), None, false).result,
            None
        );
        let efficiency = enchanted(Item::EnchantedBook, "minecraft:efficiency", 1);
        let plain = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
        assert_eq!(
            compute(Some(&plain), Some(&efficiency), None, false).result,
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn too_expensive_results_are_refused_unless_infinite() {
        let mut sword = enchanted(Item::DiamondSword, "minecraft:sharpness", 1);
        sword.components.repair_cost = Some(63);
        let book = enchanted(Item::EnchantedBook, "minecraft:sharpness", 3);
        let out = compute(Some(&sword), Some(&book), None, false);
        assert_eq!(out.result, None);
        assert!(out.cost >= 40);
        assert!(
            compute(Some(&sword), Some(&book), None, true)
                .result
                .is_some()
        );
    }
}

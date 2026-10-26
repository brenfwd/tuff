//! The grindstone's disenchanting and repair arithmetic.

use super::anvil::{crafting_enchantments, increased_repair_cost, set_enchantments};
use rand::Rng;
use tuff_core::world::menu::holds_enchantments;
use tuff_item::enchantment;
use tuff_item::item::enchanting::Choice;
use tuff_item::item::{BurnReason, ItemStack, MintReason};
use tuff_vanilla_data::Item;

const REPAIR_BONUS_PERCENT: u32 = 5;

fn is_curse(enchantment: &Choice<'_>) -> bool {
    tuff_datapack::tags::with(|tags| {
        tags.contains("minecraft:enchantment", "minecraft:curse", enchantment.0)
    })
}

fn without_non_curses(mut stack: ItemStack) -> ItemStack {
    let registry = enchantment::current();
    let mut kept = crafting_enchantments(&registry, &stack);
    kept.retain(|(enchantment, _)| is_curse(enchantment));
    set_enchantments(&mut stack, &kept);
    if stack.item() == Item::EnchantedBook && kept.is_empty() {
        stack.transform(Item::Book, MintReason::Transform, BurnReason::Transform);
    }
    let mut cost = 0;
    for _ in &kept {
        cost = increased_repair_cost(cost);
    }
    stack.components.repair_cost = Some(cost);
    stack
}

fn merged(input: &ItemStack, additional: &ItemStack) -> Option<ItemStack> {
    if input.item() != additional.item() {
        return None;
    }
    let durability = input
        .max_damage()
        .unwrap_or(0)
        .max(additional.max_damage().unwrap_or(0));
    let left = |stack: &ItemStack| {
        stack
            .max_damage()
            .unwrap_or(0)
            .saturating_sub(stack.damage())
    };
    let remaining = left(input) + left(additional) + durability * REPAIR_BONUS_PERCENT / 100;
    let mut count = 1;
    if !input.is_damageable() {
        if input.max_stack_size() < 2 || !input.stacks_with(additional) {
            return None;
        }
        count = 2;
    }
    #[expect(clippy::disallowed_methods, reason = "crafting-style result (merged)")]
    let mut result = ItemStack::mint_like(input, count, MintReason::Crafting);
    if result.is_damageable() {
        result.components.max_damage = Some(durability);
        result.set_damage(durability.saturating_sub(remaining));
    }
    let registry = enchantment::current();
    let mut enchantments = crafting_enchantments(&registry, &result);
    for (enchantment, level) in crafting_enchantments(&registry, additional) {
        if is_curse(&enchantment)
            && enchantments
                .iter()
                .any(|(existing, _)| existing.0 == enchantment.0)
        {
            continue;
        }
        match enchantments
            .iter_mut()
            .find(|(existing, _)| existing.0 == enchantment.0)
        {
            Some((_, existing)) => *existing = (*existing).max(level),
            None => enchantments.push((enchantment, level)),
        }
    }
    set_enchantments(&mut result, &enchantments);
    Some(without_non_curses(result))
}

/// What the grindstone offers for its two input slots.
pub fn compute(input: Option<&ItemStack>, additional: Option<&ItemStack>) -> Option<ItemStack> {
    if input.is_none() && additional.is_none() {
        return None;
    }
    if input.is_some_and(|s| s.count() > 1) || additional.is_some_and(|s| s.count() > 1) {
        return None;
    }
    match (input, additional) {
        (Some(input), Some(additional)) => merged(input, additional),
        #[expect(clippy::disallowed_methods, reason = "crafting-style result (compute)")]
        (Some(only), None) | (None, Some(only)) => holds_enchantments(only).then(|| {
            without_non_curses(ItemStack::mint_like(
                only,
                only.count(),
                MintReason::Crafting,
            ))
        }),
        (None, None) => None,
    }
}

/// The experience the enchantments on `stack` are worth, curses excluded.
pub fn experience_of(stack: &ItemStack) -> i32 {
    let registry = enchantment::current();
    crafting_enchantments(&registry, stack)
        .into_iter()
        .filter(|(enchantment, _)| !is_curse(enchantment))
        .map(|(enchantment, level)| enchantment.1.min_cost.at_level(level.max(0) as u32))
        .sum()
}

/// The experience dropped for grinding away `amount` worth of enchantments:
/// half of it rounded up, plus a random share of that half.
pub fn award(amount: i32, rng: &mut (impl Rng + ?Sized)) -> i32 {
    if amount <= 0 {
        return 0;
    }
    let half = (amount + 1) / 2;
    half + rng.gen_range(0..half)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::Enchantments;
    use tuff_item::item::MintReason;

    fn enchanted(item: Item, entries: &[(&str, i32)]) -> ItemStack {
        let mut stack = ItemStack::mint(item, 1, MintReason::Test);
        let list = Enchantments::new(entries.iter().map(|(id, level)| (id.to_string(), *level)));
        if item == Item::EnchantedBook {
            stack.components.stored_enchantments = Some(list);
        } else {
            stack.components.enchantments = Some(list);
        }
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn one_enchanted_item_loses_everything_but_curses() {
        let sword = enchanted(
            Item::IronSword,
            &[("minecraft:sharpness", 3), ("minecraft:vanishing_curse", 1)],
        );
        let out = compute(Some(&sword), None).expect("result");
        let kept = out.components.enchantments.as_ref().unwrap().entries();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, "minecraft:vanishing_curse");
        assert_eq!(out.components.repair_cost, Some(1));
        assert!(
            compute(
                Some(&ItemStack::mint(Item::IronSword, 1, MintReason::Test)),
                None
            )
            .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_ground_enchanted_book_becomes_a_book() {
        let book = enchanted(Item::EnchantedBook, &[("minecraft:mending", 1)]);
        let out = compute(None, Some(&book)).expect("result");
        assert_eq!(out.item(), Item::Book);
        assert_eq!(out.components.repair_cost, Some(0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn two_damaged_tools_merge_with_a_five_percent_bonus() {
        let mut a = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
        let mut b = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
        let max = a.max_damage().unwrap();
        a.set_damage(max - 10);
        b.set_damage(max - 20);
        let out = compute(Some(&a), Some(&b)).expect("result");
        assert_eq!(out.damage(), max - (30 + max * 5 / 100));
        assert!(
            compute(
                Some(&a),
                Some(&ItemStack::mint(Item::IronSword, 1, MintReason::Test))
            )
            .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn stacks_and_unequal_pairs_give_nothing() {
        let mut stack = enchanted(Item::IronSword, &[("minecraft:sharpness", 1)]);
        stack.grow(1, MintReason::Test);
        assert!(compute(Some(&stack), None).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn experience_skips_curses_and_awards_between_half_and_all() {
        let sword = enchanted(
            Item::IronSword,
            &[("minecraft:sharpness", 2), ("minecraft:binding_curse", 1)],
        );
        let amount = experience_of(&sword);
        assert!(amount > 0);
        let mut rng = rand::rngs::mock::StepRng::new(0, 1);
        let given = award(amount, &mut rng);
        assert!(given >= (amount + 1) / 2 && given <= amount + 1);
        assert_eq!(award(0, &mut rng), 0);
    }
}

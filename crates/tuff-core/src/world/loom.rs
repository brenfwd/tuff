//! The loom's slot vocabulary and pattern lists.

use tuff_vanilla_data::tag;

use tuff_item::item::ItemStack;

pub const MAX_LAYERS: usize = 6;

pub fn is_loom_dye(stack: &ItemStack) -> bool {
    tag::item::LOOM_DYES.contains(&stack.item()) && stack.item().dye().is_some()
}

fn pattern_tag(stack: &ItemStack) -> Option<&'static [&'static str]> {
    if !tag::item::LOOM_PATTERNS.contains(&stack.item()) {
        return None;
    }
    let name = stack.item().name();
    let kind = name
        .strip_prefix("minecraft:")?
        .strip_suffix("_banner_pattern")?;
    let tag_name = format!("minecraft:pattern_item/{kind}");
    tag::banner_pattern::ALL
        .iter()
        .find(|(name, _)| *name == tag_name)
        .map(|(_, members)| *members)
}

pub fn is_loom_pattern_item(stack: &ItemStack) -> bool {
    pattern_tag(stack).is_some()
}

/// The patterns a loom offers for the pattern slot's content.
pub fn selectable_patterns(pattern_item: Option<&ItemStack>) -> &'static [&'static str] {
    match pattern_item {
        None => tag::banner_pattern::NO_ITEM_REQUIRED,
        Some(stack) => pattern_tag(stack).unwrap_or(&[]),
    }
}

pub fn layer_count(banner: &ItemStack) -> usize {
    banner
        .components
        .banner_patterns
        .as_ref()
        .map_or(0, Vec::len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::menu::MenuKind;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    fn stack(item: Item) -> ItemStack {
        ItemStack::mint(item, 1, MintReason::Test)
    }

    #[tuff_core::watchdog]
    #[test]
    fn slot_rules_follow_the_loom() {
        let recipes = crate::world::crafting::RecipeSet::default();
        let kind = MenuKind::Loom;
        assert_eq!(kind.len(), 40);
        assert!(kind.may_place(&recipes, 0, &stack(Item::WhiteBanner)));
        assert!(!kind.may_place(&recipes, 0, &stack(Item::RedDye)));
        assert!(kind.may_place(&recipes, 1, &stack(Item::RedDye)));
        assert!(!kind.may_place(&recipes, 1, &stack(Item::WhiteBanner)));
        assert!(kind.may_place(&recipes, 2, &stack(Item::CreeperBannerPattern)));
        assert!(!kind.may_place(&recipes, 2, &stack(Item::RedDye)));
        assert!(!kind.may_place(&recipes, 3, &stack(Item::WhiteBanner)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn without_a_pattern_item_the_plain_patterns_are_offered() {
        let plain = selectable_patterns(None);
        assert!(plain.contains(&"minecraft:stripe_top"));
        assert!(!plain.contains(&"minecraft:creeper"));
        let creeper = stack(Item::CreeperBannerPattern);
        assert_eq!(selectable_patterns(Some(&creeper)), &["minecraft:creeper"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn taking_the_result_uses_one_banner_and_one_dye() {
        use crate::world::inventory::Inventory;
        use crate::world::menu::{LoomHook, ResultHook};
        let mut container = vec![
            Some(ItemStack::mint(Item::WhiteBanner, 2, MintReason::Test)),
            Some(stack(Item::RedDye)),
            None,
            None,
        ];
        let mut hook = LoomHook::default();
        hook.taken(&mut container, &mut Inventory::default(), &mut Vec::new());
        assert!(hook.took);
        assert_eq!(container[0].as_ref().unwrap().count(), 1);
        assert!(container[1].is_none());
    }
}

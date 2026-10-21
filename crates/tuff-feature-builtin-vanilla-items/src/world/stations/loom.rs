//! The banner a loom weaves and its selection as inputs change.

use tuff_core::world::loom::{MAX_LAYERS, layer_count, selectable_patterns};
use tuff_core::world::stations::LoomRefresh as Refreshed;
use tuff_item::item::ItemStack;
use tuff_item::item::decor::BannerLayer;

/// One banner with `pattern` added in the dye's color.
pub fn weave(banner: &ItemStack, dye: &ItemStack, pattern: &str) -> Option<ItemStack> {
    let color = dye.item.dye()?;
    let mut result = banner.clone();
    result.count = 1;
    result
        .components
        .banner_patterns
        .get_or_insert_with(Vec::new)
        .push(BannerLayer {
            pattern: pattern.to_owned(),
            inline: None,
            color,
        });
    Some(result)
}

pub fn refresh(
    previous: &[&'static str],
    selected: i32,
    banner: Option<&ItemStack>,
    dye: Option<&ItemStack>,
    pattern_item: Option<&ItemStack>,
) -> Refreshed {
    let (Some(banner), Some(dye)) = (banner, dye) else {
        return Refreshed {
            patterns: &[],
            selected: -1,
            result: None,
        };
    };
    let patterns = selectable_patterns(pattern_item);
    let chosen = if patterns.len() == 1 {
        Some(0)
    } else {
        usize::try_from(selected)
            .ok()
            .and_then(|index| previous.get(index))
            .and_then(|old| patterns.iter().position(|p| p == old))
    };
    let Some(index) = chosen else {
        return Refreshed {
            patterns,
            selected: -1,
            result: None,
        };
    };
    if layer_count(banner) >= MAX_LAYERS {
        return Refreshed {
            patterns,
            selected: -1,
            result: None,
        };
    }
    Refreshed {
        patterns,
        selected: index as i32,
        result: weave(banner, dye, patterns[index]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;

    fn stack(item: Item) -> ItemStack {
        ItemStack::new(item, 1)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_single_offered_pattern_is_selected_and_woven() {
        let banner = stack(Item::WhiteBanner);
        let dye = stack(Item::RedDye);
        let item = stack(Item::CreeperBannerPattern);
        let fresh = refresh(&[], -1, Some(&banner), Some(&dye), Some(&item));
        assert_eq!(fresh.selected, 0);
        let layers = fresh
            .result
            .unwrap()
            .components
            .banner_patterns
            .clone()
            .unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].pattern, "minecraft:creeper");
    }

    #[tuff_core::watchdog]
    #[test]
    fn many_patterns_wait_for_a_choice_and_keep_it_across_changes() {
        let banner = stack(Item::WhiteBanner);
        let dye = stack(Item::RedDye);
        let fresh = refresh(&[], -1, Some(&banner), Some(&dye), None);
        assert_eq!(fresh.selected, -1);
        assert!(fresh.result.is_none());
        let plain = selectable_patterns(None);
        let kept = refresh(plain, 3, Some(&banner), Some(&dye), None);
        assert_eq!(kept.selected, 3);
        assert_eq!(
            kept.result
                .unwrap()
                .components
                .banner_patterns
                .clone()
                .unwrap()[0]
                .pattern,
            plain[3]
        );
        let item = stack(Item::CreeperBannerPattern);
        let swapped = refresh(plain, 3, Some(&banner), Some(&dye), Some(&item));
        assert_eq!(swapped.selected, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_full_banner_or_missing_input_offers_nothing() {
        let mut banner = stack(Item::WhiteBanner);
        let dye = stack(Item::RedDye);
        let item = stack(Item::CreeperBannerPattern);
        for _ in 0..MAX_LAYERS {
            banner = weave(&banner, &dye, "minecraft:creeper").unwrap();
        }
        let full = refresh(&[], -1, Some(&banner), Some(&dye), Some(&item));
        assert_eq!(full.selected, -1);
        assert!(full.result.is_none());
        let missing = refresh(&[], -1, Some(&banner), None, Some(&item));
        assert!(missing.patterns.is_empty());
        assert!(missing.result.is_none());
    }
}

//! Vanilla anvil, grindstone and loom results.

pub mod anvil;
pub mod grindstone;
pub mod loom;

use rand::RngCore;
use tuff_core::world::anvil::AnvilOutcome;
use tuff_core::world::stations::{LoomRefresh, StationRules};
use tuff_item::item::ItemStack;

pub struct VanillaStations;

impl StationRules for VanillaStations {
    fn anvil_result(
        &self,
        input: Option<&ItemStack>,
        addition: Option<&ItemStack>,
        name: Option<&str>,
        infinite: bool,
    ) -> AnvilOutcome {
        anvil::compute(input, addition, name, infinite)
    }

    fn grindstone_result(
        &self,
        input: Option<&ItemStack>,
        additional: Option<&ItemStack>,
    ) -> Option<ItemStack> {
        grindstone::compute(input, additional)
    }

    fn grindstone_experience(&self, stack: &ItemStack) -> i32 {
        grindstone::experience_of(stack)
    }

    fn grindstone_award(&self, amount: i32, rng: &mut dyn RngCore) -> i32 {
        grindstone::award(amount, rng)
    }

    fn loom_weave(&self, banner: &ItemStack, dye: &ItemStack, pattern: &str) -> Option<ItemStack> {
        loom::weave(banner, dye, pattern)
    }

    fn loom_refresh(
        &self,
        previous: &[&'static str],
        selected: i32,
        banner: Option<&ItemStack>,
        dye: Option<&ItemStack>,
        pattern_item: Option<&ItemStack>,
    ) -> LoomRefresh {
        loom::refresh(previous, selected, banner, dye, pattern_item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tuff_core::world::stations::NoStationRules;
    use tuff_item::item::Enchantments;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_rules_grind_and_weave_where_the_inert_core_offers_nothing() {
        let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
        sword.components.enchantments =
            Some(Enchantments::new([("minecraft:sharpness".to_string(), 2)]));
        let banner = ItemStack::mint(Item::WhiteBanner, 1, MintReason::Test);
        let dye = ItemStack::mint(Item::RedDye, 1, MintReason::Test);
        let vanilla: Arc<dyn StationRules> = Arc::new(VanillaStations);
        let inert: Arc<dyn StationRules> = Arc::new(NoStationRules);
        assert!(vanilla.grindstone_result(Some(&sword), None).is_some());
        assert!(vanilla.grindstone_experience(&sword) > 0);
        assert!(
            vanilla
                .loom_weave(&banner, &dye, "minecraft:creeper")
                .is_some()
        );
        assert!(inert.grindstone_result(Some(&sword), None).is_none());
        assert_eq!(inert.grindstone_experience(&sword), 0);
        assert!(
            inert
                .loom_weave(&banner, &dye, "minecraft:creeper")
                .is_none()
        );
    }
}

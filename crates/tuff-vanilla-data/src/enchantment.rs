//! The data pack's `minecraft:enchantment` entries: levels, costs, which
//! items each enchantment goes on and which enchantments it excludes. This
//! is a data-driven registry (see [`crate::data_registry`]).

use crate::registry::Item;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Enchantment {
    pub name: &'static str,
    /// The display name, as raw JSON text component.
    pub description: &'static str,
    pub max_level: u32,
    pub weight: u32,
    pub anvil_cost: u32,
    pub min_cost: EnchantmentCost,
    pub max_cost: EnchantmentCost,
    /// The equipment slot groups the enchantment's effects apply in.
    pub slots: &'static [&'static str],
    /// Items the enchantment can be put on, sorted by registry order of the
    /// set they were resolved from.
    pub supported_items: &'static [Item],
    /// Items the enchanting table offers it for; `None` means the same as
    /// `supported_items`.
    pub primary_items: Option<&'static [Item]>,
    /// Enchantments that cannot share an item with this one, by name.
    pub exclusive_set: &'static [&'static str],
    /// `(effect component, raw JSON)` pairs; the effect mini-language is
    /// open-ended and left to the caller.
    pub effects: &'static [(&'static str, &'static str)],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnchantmentCost {
    pub base: i32,
    pub per_level_above_first: i32,
}

impl EnchantmentCost {
    pub fn at_level(&self, level: u32) -> i32 {
        self.base + self.per_level_above_first * (level as i32 - 1)
    }
}

impl Enchantment {
    pub fn get(name: &str) -> Option<&'static Enchantment> {
        crate::lookup(ALL, name)
    }

    /// Whether the enchantment can be put on `item`.
    pub fn supports(&self, item: Item) -> bool {
        self.supported_items.contains(&item)
    }

    /// Whether the enchanting table offers this enchantment for `item`.
    pub fn is_primary(&self, item: Item) -> bool {
        self.primary_items
            .unwrap_or(self.supported_items)
            .contains(&item)
    }

    /// Whether `self` and `other` can coexist on one item: different
    /// enchantments in neither one's exclusive set.
    pub fn compatible_with(&self, other: &Enchantment) -> bool {
        self.name != other.name
            && !self.exclusive_set.contains(&other.name)
            && !other.exclusive_set.contains(&self.name)
    }

    /// The raw JSON of one effect component, e.g. `minecraft:block_experience`.
    pub fn effect(&self, component: &str) -> Option<&'static str> {
        let (namespace, path) = component
            .split_once(':')
            .unwrap_or(("minecraft", component));
        self.effects
            .iter()
            .find(|(key, _)| *key == format!("{namespace}:{path}"))
            .map(|(_, json)| *json)
    }
}

include!(concat!(env!("OUT_DIR"), "/enchantments.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enchantments_carry_levels_items_and_exclusions() {
        let sharpness = Enchantment::get("minecraft:sharpness").unwrap();
        assert_eq!(sharpness.max_level, 5);
        assert!(sharpness.supports(Item::DiamondSword));
        assert!(sharpness.supports(Item::DiamondAxe));
        assert!(!sharpness.is_primary(Item::DiamondAxe));
        assert!(!sharpness.supports(Item::Stone));
        let smite = Enchantment::get("smite").unwrap();
        assert!(!sharpness.compatible_with(smite));
        let unbreaking = Enchantment::get("unbreaking").unwrap();
        assert!(sharpness.compatible_with(unbreaking));
        assert!(!sharpness.compatible_with(sharpness));
        let silk = Enchantment::get("minecraft:silk_touch").unwrap();
        assert!(silk.effect("block_experience").is_some());
        assert_eq!(silk.max_cost.at_level(1), 65);
        assert_eq!(sharpness.min_cost.at_level(3), 23);
        assert!(Enchantment::get("minecraft:nope").is_none());
        assert!(ALL.windows(2).all(|w| w[0].0 < w[1].0));
    }
}

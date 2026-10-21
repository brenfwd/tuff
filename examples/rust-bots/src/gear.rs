//! Choosing the best armor and tools from what a bot carries. Free of host
//! calls so the decisions are plain functions.

use crate::hands::Carried;
use crate::world::name_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmorSlot {
    Head,
    Chest,
    Legs,
    Feet,
}

const TIERS: &[&str] = &[
    "leather",
    "golden",
    "chainmail",
    "copper",
    "iron",
    "diamond",
    "netherite",
];

fn armor_part(name: &str) -> Option<(ArmorSlot, usize)> {
    let (tier, part) = name.split_once('_')?;
    let slot = match part {
        "helmet" => ArmorSlot::Head,
        "chestplate" => ArmorSlot::Chest,
        "leggings" => ArmorSlot::Legs,
        "boots" => ArmorSlot::Feet,
        _ => return None,
    };
    let rank = TIERS.iter().position(|t| *t == tier)?;
    Some((slot, rank))
}

/// The inventory slot of the best armor piece carried for each slot,
/// compared with the tier already worn (`None` when the slot is empty).
pub fn best_armor(
    carried: &[Carried],
    worn: impl Fn(ArmorSlot) -> Option<usize>,
) -> Vec<(ArmorSlot, u8)> {
    [
        ArmorSlot::Head,
        ArmorSlot::Chest,
        ArmorSlot::Legs,
        ArmorSlot::Feet,
    ]
    .into_iter()
    .filter_map(|slot| {
        let (menu, rank) = carried
            .iter()
            .filter(|(_, _, count)| *count > 0)
            .filter_map(|(menu, item, _)| match armor_part(name_of(item)) {
                Some((s, rank)) if s == slot => Some((*menu, rank)),
                _ => None,
            })
            .max_by_key(|(_, rank)| *rank)?;
        worn(slot).is_none_or(|w| rank > w).then_some((slot, menu))
    })
    .collect()
}

const TOOL_TIERS: &[&str] = &[
    "wooden",
    "golden",
    "stone",
    "copper",
    "iron",
    "diamond",
    "netherite",
];

/// The menu slot of the best tool of this kind (`pickaxe`, `axe`, `shovel`,
/// `sword`, `hoe`) in the inventory.
pub fn best_tool(carried: &[Carried], kind: &str) -> Option<u8> {
    carried
        .iter()
        .filter(|(_, _, count)| *count > 0)
        .filter_map(|(menu, item, _)| {
            let (tier, k) = name_of(item).split_once('_')?;
            if k != kind {
                return None;
            }
            let rank = TOOL_TIERS.iter().position(|t| *t == tier)?;
            Some((*menu, rank))
        })
        .max_by_key(|(_, rank)| *rank)
        .map(|(menu, _)| menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit(items: &[(u8, &str)]) -> Vec<Carried> {
        items
            .iter()
            .map(|(s, n)| (*s, format!("minecraft:{n}"), 1))
            .collect()
    }

    #[test]
    fn the_best_armor_per_slot_replaces_worse_worn_pieces() {
        let carried = kit(&[
            (9, "iron_helmet"),
            (10, "diamond_helmet"),
            (11, "leather_boots"),
            (12, "iron_chestplate"),
        ]);
        let worn = |s| match s {
            ArmorSlot::Chest => Some(4),
            ArmorSlot::Feet => Some(4),
            _ => None,
        };
        assert_eq!(best_armor(&carried, worn), vec![(ArmorSlot::Head, 10)]);
    }

    #[test]
    fn the_best_tool_of_a_kind_wins() {
        let carried = kit(&[
            (9, "stone_pickaxe"),
            (10, "iron_pickaxe"),
            (11, "diamond_axe"),
        ]);
        assert_eq!(best_tool(&carried, "pickaxe"), Some(10));
        assert_eq!(best_tool(&carried, "shovel"), None);
    }
}

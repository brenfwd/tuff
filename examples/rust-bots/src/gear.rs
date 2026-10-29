//! Choosing the best armor and tools from what a bot carries. Free of host
//! calls so the decisions are plain functions.

use crate::hands::{Carried, HOTBAR};
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

pub fn armor_part(name: &str) -> Option<(ArmorSlot, usize)> {
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

const ARMOR_SLOTS: [(ArmorSlot, u8); 4] = [
    (ArmorSlot::Head, 5),
    (ArmorSlot::Chest, 6),
    (ArmorSlot::Legs, 7),
    (ArmorSlot::Feet, 8),
];

/// The hotbar index of an armor piece to put on by using it: a better piece
/// than the worn one (or any piece for an empty slot). Anything else worn
/// in a slot, such as elytra, is left alone.
pub fn armor_to_wear(carried: &[Carried]) -> Option<u8> {
    let worn = |slot: ArmorSlot| {
        let menu = ARMOR_SLOTS.iter().find(|(s, _)| *s == slot)?.1;
        let (_, item, _) = carried.iter().find(|(m, _, c)| *m == menu && *c > 0)?;
        Some(armor_part(name_of(item)).map_or(usize::MAX, |(_, rank)| rank))
    };
    let hotbar: Vec<Carried> = carried
        .iter()
        .filter(|(m, _, _)| HOTBAR.contains(m))
        .cloned()
        .collect();
    best_armor(&hotbar, worn)
        .first()
        .map(|(_, menu)| menu - HOTBAR.start())
}

/// The kind of tool that breaks this block fastest, if any kind matters.
pub fn tool_kind(block: &str) -> Option<&'static str> {
    let name = name_of(block);
    const SHOVEL: &[&str] = &[
        "dirt",
        "grass_block",
        "coarse_dirt",
        "rooted_dirt",
        "podzol",
        "mycelium",
        "mud",
        "sand",
        "red_sand",
        "gravel",
        "clay",
        "snow",
        "snow_block",
        "soul_sand",
        "soul_soil",
        "farmland",
        "dirt_path",
    ];
    if SHOVEL.contains(&name) || name.ends_with("concrete_powder") {
        return Some("shovel");
    }
    const AXE_END: &[&str] = &["_log", "_wood", "_planks", "_stem", "_hyphae"];
    const AXE: &[&str] = &[
        "chest",
        "crafting_table",
        "bookshelf",
        "pumpkin",
        "melon",
        "barrel",
    ];
    if AXE_END.iter().any(|e| name.ends_with(e)) || AXE.contains(&name) {
        return Some("axe");
    }
    const PICK_PART: &[&str] = &[
        "stone",
        "ore",
        "cobble",
        "brick",
        "deepslate",
        "obsidian",
        "netherrack",
        "basalt",
        "concrete",
        "terracotta",
        "andesite",
        "diorite",
        "granite",
        "tuff",
        "prismarine",
        "quartz",
        "copper",
        "iron_block",
        "gold_block",
        "diamond_block",
        "furnace",
    ];
    PICK_PART
        .iter()
        .any(|p| name.contains(p))
        .then_some("pickaxe")
}

/// The hotbar index of the best tool for breaking this block.
pub fn tool_for(carried: &[Carried], block: &str) -> Option<u8> {
    let menu = best_tool(carried, tool_kind(block)?)?;
    HOTBAR.contains(&menu).then(|| menu - HOTBAR.start())
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

    #[test]
    fn a_better_hotbar_piece_is_worn_and_elytra_stay_on() {
        let carried = kit(&[
            (37, "iron_helmet"),
            (38, "iron_chestplate"),
            (5, "leather_helmet"),
        ]);
        assert_eq!(armor_to_wear(&carried), Some(1));
        let mut with_elytra = kit(&[(38, "iron_chestplate")]);
        with_elytra.push((6, "minecraft:elytra".into(), 1));
        assert_eq!(armor_to_wear(&with_elytra), None);
        let worn = kit(&[(37, "iron_helmet"), (5, "diamond_helmet")]);
        assert_eq!(armor_to_wear(&worn), None);
    }

    #[test]
    fn blocks_map_to_the_tool_that_breaks_them() {
        assert_eq!(tool_kind("minecraft:iron_ore"), Some("pickaxe"));
        assert_eq!(tool_kind("minecraft:sandstone"), Some("pickaxe"));
        assert_eq!(tool_kind("minecraft:sand"), Some("shovel"));
        assert_eq!(tool_kind("minecraft:oak_log[axis=y]"), Some("axe"));
        assert_eq!(tool_kind("minecraft:poppy"), None);
        let carried = kit(&[(36, "wooden_pickaxe"), (40, "iron_pickaxe")]);
        assert_eq!(tool_for(&carried, "minecraft:stone"), Some(4));
    }
}

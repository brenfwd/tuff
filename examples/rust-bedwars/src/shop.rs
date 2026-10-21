// SPDX-License-Identifier: MIT
//! The item shop and team upgrades as data, with prices checked against
//! what a player carries. Buying waits on host calls that take items from
//! a player and on plugin menus; `/bedwars shop` lists the offers.

use std::collections::BTreeMap;

use crate::economy::Resource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Blocks,
    Melee,
    Armor,
    Tools,
    Ranged,
    Potions,
    Utility,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Blocks,
        Category::Melee,
        Category::Armor,
        Category::Tools,
        Category::Ranged,
        Category::Potions,
        Category::Utility,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Blocks => "blocks",
            Category::Melee => "melee",
            Category::Armor => "armor",
            Category::Tools => "tools",
            Category::Ranged => "ranged",
            Category::Potions => "potions",
            Category::Utility => "utility",
        }
    }

    pub fn of(name: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub resource: Resource,
    pub amount: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    pub key: &'static str,
    pub name: &'static str,
    pub category: Category,
    /// What a purchase gives; wool and clay take the team's color.
    pub item: &'static str,
    pub count: u32,
    pub price: Price,
    /// Kept on death (armor, and tools at their tier).
    pub permanent: bool,
}

const fn offer(
    key: &'static str,
    name: &'static str,
    category: Category,
    item: &'static str,
    count: u32,
    resource: Resource,
    amount: u32,
) -> Offer {
    Offer {
        key,
        name,
        category,
        item,
        count,
        price: Price { resource, amount },
        permanent: false,
    }
}

const fn permanent(mut o: Offer) -> Offer {
    o.permanent = true;
    o
}

use Category::*;
use Resource::*;

pub const OFFERS: &[Offer] = &[
    offer(
        "wool",
        "Wool",
        Blocks,
        "minecraft:{color}_wool",
        16,
        Iron,
        4,
    ),
    offer(
        "clay",
        "Hardened Clay",
        Blocks,
        "minecraft:{color}_terracotta",
        16,
        Iron,
        12,
    ),
    offer(
        "glass",
        "Blast-Proof Glass",
        Blocks,
        "minecraft:{color}_stained_glass",
        4,
        Iron,
        12,
    ),
    offer(
        "end_stone",
        "End Stone",
        Blocks,
        "minecraft:end_stone",
        12,
        Iron,
        24,
    ),
    offer("ladder", "Ladder", Blocks, "minecraft:ladder", 8, Iron, 4),
    offer("wood", "Wood", Blocks, "minecraft:oak_planks", 16, Gold, 4),
    offer(
        "obsidian",
        "Obsidian",
        Blocks,
        "minecraft:obsidian",
        4,
        Emerald,
        4,
    ),
    offer(
        "stone_sword",
        "Stone Sword",
        Melee,
        "minecraft:stone_sword",
        1,
        Iron,
        10,
    ),
    offer(
        "iron_sword",
        "Iron Sword",
        Melee,
        "minecraft:iron_sword",
        1,
        Gold,
        7,
    ),
    offer(
        "diamond_sword",
        "Diamond Sword",
        Melee,
        "minecraft:diamond_sword",
        1,
        Emerald,
        4,
    ),
    offer(
        "kb_stick",
        "Knockback Stick",
        Melee,
        "minecraft:stick",
        1,
        Gold,
        5,
    ),
    permanent(offer(
        "chainmail",
        "Chainmail Armor",
        Armor,
        "minecraft:chainmail_boots",
        1,
        Iron,
        24,
    )),
    permanent(offer(
        "iron_armor",
        "Iron Armor",
        Armor,
        "minecraft:iron_boots",
        1,
        Gold,
        12,
    )),
    permanent(offer(
        "diamond_armor",
        "Diamond Armor",
        Armor,
        "minecraft:diamond_boots",
        1,
        Emerald,
        6,
    )),
    permanent(offer(
        "shears",
        "Shears",
        Tools,
        "minecraft:shears",
        1,
        Iron,
        20,
    )),
    permanent(offer(
        "pickaxe",
        "Wooden Pickaxe",
        Tools,
        "minecraft:wooden_pickaxe",
        1,
        Iron,
        10,
    )),
    permanent(offer(
        "axe",
        "Wooden Axe",
        Tools,
        "minecraft:wooden_axe",
        1,
        Iron,
        10,
    )),
    offer("arrows", "Arrow", Ranged, "minecraft:arrow", 6, Gold, 2),
    offer("bow", "Bow", Ranged, "minecraft:bow", 1, Gold, 12),
    offer(
        "power_bow",
        "Bow (Power I)",
        Ranged,
        "minecraft:bow",
        1,
        Gold,
        20,
    ),
    offer(
        "punch_bow",
        "Bow (Power I, Punch I)",
        Ranged,
        "minecraft:bow",
        1,
        Emerald,
        6,
    ),
    offer(
        "speed",
        "Speed II Potion (45 seconds)",
        Potions,
        "minecraft:potion",
        1,
        Emerald,
        1,
    ),
    offer(
        "jump",
        "Jump V Potion (45 seconds)",
        Potions,
        "minecraft:potion",
        1,
        Emerald,
        1,
    ),
    offer(
        "invisibility",
        "Invisibility Potion (30 seconds)",
        Potions,
        "minecraft:potion",
        1,
        Emerald,
        2,
    ),
    offer(
        "golden_apple",
        "Golden Apple",
        Utility,
        "minecraft:golden_apple",
        1,
        Gold,
        3,
    ),
    offer(
        "fireball",
        "Fireball",
        Utility,
        "minecraft:fire_charge",
        1,
        Iron,
        40,
    ),
    offer("tnt", "TNT", Utility, "minecraft:tnt", 1, Gold, 4),
    offer(
        "ender_pearl",
        "Ender Pearl",
        Utility,
        "minecraft:ender_pearl",
        1,
        Emerald,
        4,
    ),
    offer(
        "water",
        "Water Bucket",
        Utility,
        "minecraft:water_bucket",
        1,
        Gold,
        3,
    ),
    offer(
        "bridge_egg",
        "Bridge Egg",
        Utility,
        "minecraft:egg",
        1,
        Emerald,
        1,
    ),
    offer("sponge", "Sponge", Utility, "minecraft:sponge", 4, Gold, 3),
];

/// A team upgrade: one price in diamonds per tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upgrade {
    pub key: &'static str,
    pub name: &'static str,
    pub tiers: &'static [u32],
}

pub const UPGRADES: &[Upgrade] = &[
    Upgrade {
        key: "sharpness",
        name: "Sharpened Swords",
        tiers: &[4],
    },
    Upgrade {
        key: "protection",
        name: "Reinforced Armor",
        tiers: &[2, 4, 8, 16],
    },
    Upgrade {
        key: "haste",
        name: "Maniac Miner",
        tiers: &[2, 4],
    },
    Upgrade {
        key: "forge",
        name: "Forge",
        tiers: &[2, 4, 6, 8],
    },
    Upgrade {
        key: "heal_pool",
        name: "Heal Pool",
        tiers: &[1],
    },
    Upgrade {
        key: "trap",
        name: "It's a Trap!",
        tiers: &[1],
    },
];

pub fn find(key: &str) -> Option<&'static Offer> {
    OFFERS.iter().find(|o| o.key == key)
}

pub fn upgrade(key: &str) -> Option<&'static Upgrade> {
    UPGRADES.iter().find(|u| u.key == key)
}

/// The resources among `(item, count)` stacks.
pub fn wallet<'a>(stacks: impl IntoIterator<Item = (&'a str, u32)>) -> BTreeMap<Resource, u32> {
    let mut out = BTreeMap::new();
    for (item, count) in stacks {
        if let Some(resource) = Resource::of_item(item) {
            *out.entry(resource).or_insert(0) += count;
        }
    }
    out
}

/// What is still missing to pay `price`, if anything.
pub fn missing(wallet: &BTreeMap<Resource, u32>, price: Price) -> Option<u32> {
    let have = wallet.get(&price.resource).copied().unwrap_or(0);
    (have < price.amount).then(|| price.amount - have)
}

/// Pays `price` out of `wallet`.
pub fn pay(wallet: &mut BTreeMap<Resource, u32>, price: Price) -> Result<(), String> {
    if let Some(short) = missing(wallet, price) {
        return Err(format!("You need {short} more {}", price.resource.name()));
    }
    *wallet.entry(price.resource).or_insert(0) -= price.amount;
    Ok(())
}

/// The price of the next tier of `upgrade` once `owned` tiers are bought.
pub fn next_tier(upgrade: &Upgrade, owned: usize) -> Option<Price> {
    upgrade.tiers.get(owned).map(|&amount| Price {
        resource: Diamond,
        amount,
    })
}

/// The item an offer gives a team of dye color `color`.
pub fn item_for(offer: &Offer, color: &str) -> String {
    offer.item.replace("{color}", color)
}

pub const ARMOR_SLOTS: [u8; 4] = [5, 6, 7, 8];
pub const HOTBAR: std::ops::RangeInclusive<u8> = 36..=44;
pub const MAIN: std::ops::RangeInclusive<u8> = 9..=35;
pub const STARTER_SWORD: &str = "minecraft:wooden_sword";
const PIECES: [&str; 4] = ["helmet", "chestplate", "leggings", "boots"];
const DYES: [u32; 16] = [
    16383998, 16351261, 13061821, 3847130, 16701501, 8439583, 15961002, 4673362, 10329495, 1481884,
    8991416, 3949738, 8606770, 6192150, 11546150, 1908001,
];

/// The team upgrade levels that enchant gear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gear {
    pub sharpness: usize,
    pub protection: usize,
}

/// A stack as the `{id, count, components}` SNBT the host takes.
pub fn stack(id: &str, count: u32, components: &[String]) -> String {
    if components.is_empty() {
        return format!("{{id:\"{id}\",count:{count}}}");
    }
    format!(
        "{{id:\"{id}\",count:{count},components:{{{}}}}}",
        components.join(",")
    )
}

pub fn enchantments(levels: &[(&str, usize)]) -> String {
    let body: Vec<String> = levels
        .iter()
        .map(|(id, level)| format!("\"minecraft:{id}\":{level}"))
        .collect();
    format!("\"minecraft:enchantments\":{{{}}}", body.join(","))
}

fn dye(color: &str) -> Option<String> {
    let at = crate::map::COLORS.iter().position(|c| *c == color)?;
    Some(format!("\"minecraft:dyed_color\":{}", DYES[at]))
}

/// `id` as a stack carrying the team's enchantments for what it is.
pub fn geared(id: &str, count: u32, gear: Gear) -> String {
    let mut parts = Vec::new();
    if PIECES.iter().any(|p| id.ends_with(p)) && gear.protection > 0 {
        parts.push(enchantments(&[("protection", gear.protection)]));
    }
    if id.ends_with("_sword") && gear.sharpness > 0 {
        parts.push(enchantments(&[("sharpness", gear.sharpness)]));
    }
    stack(id, count, &parts)
}

pub fn is_armor(id: &str) -> bool {
    PIECES.iter().any(|p| id.ends_with(p))
}

/// The leather set a player starts with, dyed for the team.
pub fn leather_set(color: &str, gear: Gear) -> Vec<(u8, String)> {
    ARMOR_SLOTS
        .iter()
        .zip(PIECES)
        .map(|(slot, piece)| {
            let mut parts = Vec::new();
            parts.extend(dye(color));
            if gear.protection > 0 {
                parts.push(enchantments(&[("protection", gear.protection)]));
            }
            (
                *slot,
                stack(&format!("minecraft:leather_{piece}"), 1, &parts),
            )
        })
        .collect()
}

/// The four pieces of the armor offer whose boots are `boots`.
pub fn armor_set(boots: &str, gear: Gear) -> Vec<(u8, String)> {
    let base = boots.strip_suffix("boots").unwrap_or(boots);
    ARMOR_SLOTS
        .iter()
        .zip(PIECES)
        .map(|(slot, piece)| (*slot, geared(&format!("{base}{piece}"), 1, gear)))
        .collect()
}

/// Where a bought stack goes: a fixed slot, or the first free one.
pub enum Spot {
    Slot(u8),
    Free,
}

/// What buying `offer` puts where; empty for a plain item the host gives
/// like any pickup.
pub fn delivery(offer: &Offer, color: &str, gear: Gear) -> Vec<(Spot, String)> {
    let item = item_for(offer, color);
    if offer.category == Category::Armor {
        return armor_set(&item, gear)
            .into_iter()
            .map(|(slot, s)| (Spot::Slot(slot), s))
            .collect();
    }
    let custom = match offer.key {
        "power_bow" => Some(stack(&item, 1, &[enchantments(&[("power", 1)])])),
        "punch_bow" => Some(stack(
            &item,
            1,
            &[enchantments(&[("power", 1), ("punch", 1)])],
        )),
        "speed" => Some(potion(&item, "speed", 900, 1)),
        "jump" => Some(potion(&item, "jump_boost", 900, 4)),
        "invisibility" => Some(potion(&item, "invisibility", 600, 0)),
        "kb_stick" => Some(stack(&item, 1, &[enchantments(&[("knockback", 1)])])),
        _ if item.ends_with("_sword") && gear.sharpness > 0 => Some(geared(&item, 1, gear)),
        _ => None,
    };
    custom.map(|s| vec![(Spot::Free, s)]).unwrap_or_default()
}

fn potion(item: &str, effect: &str, duration: u32, amplifier: u32) -> String {
    let body = format!(
        "\"minecraft:potion_contents\":{{custom_effects:[{{id:\"minecraft:{effect}\",duration:{duration},amplifier:{amplifier}}}]}}"
    );
    stack(item, 1, &[body])
}

/// The id of a stack's SNBT.
pub fn snbt_id(stack: &str) -> Option<&str> {
    let start = stack.find("id:\"")? + 4;
    let end = stack[start..].find('"')?;
    Some(&stack[start..start + end])
}

/// The count of a stack's SNBT, 1 when missing.
pub fn snbt_count(stack: &str) -> u32 {
    let Some(start) = stack.find("count:") else {
        return 1;
    };
    let digits: String = stack[start + 6..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(1)
}

/// Whether a stack of `id` survives its owner's death.
pub fn survives(id: &str) -> bool {
    is_armor(id)
        || OFFERS
            .iter()
            .any(|o| o.permanent && o.category != Category::Armor && o.item == id)
}

pub fn category_icon(category: Category) -> &'static str {
    match category {
        Category::Blocks => "minecraft:terracotta",
        Category::Melee => "minecraft:golden_sword",
        Category::Armor => "minecraft:chainmail_boots",
        Category::Tools => "minecraft:stone_pickaxe",
        Category::Ranged => "minecraft:bow",
        Category::Potions => "minecraft:brewing_stand",
        Category::Utility => "minecraft:tnt",
    }
}

pub const UPGRADES_SLOT: u8 = 8;
pub const BACK_SLOT: u8 = 18;
const FIRST_OFFER: u8 = 9;

/// The slot of the category tab `category` in the shop menu.
pub fn tab_slot(category: Category) -> u8 {
    Category::ALL
        .iter()
        .position(|c| *c == category)
        .expect("listed") as u8
}

pub fn tab_at(slot: u8) -> Option<Category> {
    Category::ALL.get(slot as usize).copied()
}

/// The offers of `category` and the shop menu slots that show them.
pub fn offer_slots(category: Category) -> Vec<(u8, &'static Offer)> {
    OFFERS
        .iter()
        .filter(|o| o.category == category)
        .enumerate()
        .map(|(i, o)| (FIRST_OFFER + i as u8, o))
        .collect()
}

/// The upgrades menu slot of upgrade number `index`.
pub fn upgrade_slot(index: usize) -> u8 {
    10 + index as u8
}

pub fn upgrade_at(slot: u8) -> Option<&'static Upgrade> {
    (0..UPGRADES.len())
        .find(|i| upgrade_slot(*i) == slot)
        .map(|i| &UPGRADES[i])
}

/// A short price like `4 Iron`.
pub fn price_text(price: Price) -> String {
    format!("{} {}", price.amount, price.resource.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_are_unique_and_priced() {
        for (i, o) in OFFERS.iter().enumerate() {
            assert!(o.price.amount > 0 && o.count > 0, "{}", o.key);
            assert!(OFFERS[..i].iter().all(|p| p.key != o.key), "{}", o.key);
        }
        assert!(
            Category::ALL
                .iter()
                .all(|c| OFFERS.iter().any(|o| o.category == *c))
        );
        assert_eq!(Category::of("ranged"), Some(Category::Ranged));
        assert_eq!(
            item_for(find("wool").unwrap(), "lime"),
            "minecraft:lime_wool"
        );
        assert_eq!(item_for(find("tnt").unwrap(), "lime"), "minecraft:tnt");
    }

    #[test]
    fn paying_needs_the_whole_price() {
        let mut wallet = wallet([
            ("minecraft:iron_ingot", 20),
            ("minecraft:iron_ingot", 6),
            ("minecraft:gold_ingot", 3),
            ("minecraft:stone", 64),
        ]);
        assert_eq!(wallet.get(&Iron), Some(&26));
        let chainmail = find("chainmail").unwrap();
        assert!(chainmail.permanent);
        pay(&mut wallet, chainmail.price).unwrap();
        assert_eq!(wallet.get(&Iron), Some(&2));
        let err = pay(&mut wallet, find("wool").unwrap().price).unwrap_err();
        assert_eq!(err, "You need 2 more Iron");
        assert_eq!(
            missing(&wallet, find("ender_pearl").unwrap().price),
            Some(4)
        );
    }

    #[test]
    fn upgrades_get_dearer_and_run_out() {
        let armor = upgrade("protection").unwrap();
        assert_eq!(next_tier(armor, 0).unwrap().amount, 2);
        assert_eq!(next_tier(armor, 3).unwrap().amount, 16);
        assert_eq!(next_tier(armor, 4), None);
        assert_eq!(
            upgrade("forge").unwrap().tiers.len(),
            crate::economy::FORGE_TIERS.len() - 1
        );
    }
}

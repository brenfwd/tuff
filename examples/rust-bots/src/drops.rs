// SPDX-License-Identifier: MIT
//! What blocks drop, for the `collect` mode of the mine process: a small
//! hand-written table of the plain drop and the silk touch drop of the
//! blocks a bot is likely to be asked for. Fortune only changes how many
//! drop, which baseline counting already covers; chance-only drops
//! (leaves, flint) are left out.

use crate::world::name_of;

/// Which tool a drop needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Only a tool without silk touch drops it.
    Plain,
    /// Only a silk touch tool drops it.
    Silk,
    /// Any tool drops it.
    Either,
}

/// Block, plain drop, silk touch drop.
const TABLE: &[(&str, &str, &str)] = &[
    ("coal_ore", "coal", "coal_ore"),
    ("deepslate_coal_ore", "coal", "deepslate_coal_ore"),
    ("iron_ore", "raw_iron", "iron_ore"),
    ("deepslate_iron_ore", "raw_iron", "deepslate_iron_ore"),
    ("copper_ore", "raw_copper", "copper_ore"),
    ("deepslate_copper_ore", "raw_copper", "deepslate_copper_ore"),
    ("gold_ore", "raw_gold", "gold_ore"),
    ("deepslate_gold_ore", "raw_gold", "deepslate_gold_ore"),
    ("diamond_ore", "diamond", "diamond_ore"),
    ("deepslate_diamond_ore", "diamond", "deepslate_diamond_ore"),
    ("emerald_ore", "emerald", "emerald_ore"),
    ("deepslate_emerald_ore", "emerald", "deepslate_emerald_ore"),
    ("lapis_ore", "lapis_lazuli", "lapis_ore"),
    ("deepslate_lapis_ore", "lapis_lazuli", "deepslate_lapis_ore"),
    ("redstone_ore", "redstone", "redstone_ore"),
    (
        "deepslate_redstone_ore",
        "redstone",
        "deepslate_redstone_ore",
    ),
    ("nether_quartz_ore", "quartz", "nether_quartz_ore"),
    ("nether_gold_ore", "gold_nugget", "nether_gold_ore"),
    ("stone", "cobblestone", "stone"),
    ("deepslate", "cobbled_deepslate", "deepslate"),
    ("gravel", "gravel", "gravel"),
    ("clay", "clay_ball", "clay"),
    ("sand", "sand", "sand"),
    ("red_sand", "red_sand", "red_sand"),
    ("dirt", "dirt", "dirt"),
    ("grass_block", "dirt", "grass_block"),
    ("oak_log", "oak_log", "oak_log"),
    ("spruce_log", "spruce_log", "spruce_log"),
    ("birch_log", "birch_log", "birch_log"),
    ("jungle_log", "jungle_log", "jungle_log"),
    ("acacia_log", "acacia_log", "acacia_log"),
    ("dark_oak_log", "dark_oak_log", "dark_oak_log"),
    ("mangrove_log", "mangrove_log", "mangrove_log"),
    ("cherry_log", "cherry_log", "cherry_log"),
    ("glowstone", "glowstone_dust", "glowstone"),
    ("sea_lantern", "prismarine_crystals", "sea_lantern"),
    ("melon", "melon_slice", "melon"),
];

/// Every block that can drop `item`, by plain or silk touch drop.
pub fn blocks_dropping(item: &str) -> Vec<&'static str> {
    let item = name_of(item);
    TABLE
        .iter()
        .filter(|(_, plain, silk)| *plain == item || *silk == item)
        .map(|(block, _, _)| *block)
        .collect()
}

/// The tool `block` needs to drop `item`; none when it does not.
pub fn need_for(block: &str, item: &str) -> Option<Need> {
    let (block, item) = (name_of(block), name_of(item));
    let (_, plain, silk) = TABLE.iter().find(|(b, _, _)| *b == block)?;
    match (*plain == item, *silk == item) {
        (true, true) => Some(Need::Either),
        (true, false) => Some(Need::Plain),
        (false, true) => Some(Need::Silk),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_iron_comes_from_both_iron_ores_with_a_plain_tool() {
        assert_eq!(
            blocks_dropping("minecraft:raw_iron"),
            ["iron_ore", "deepslate_iron_ore"]
        );
        assert_eq!(need_for("iron_ore", "raw_iron"), Some(Need::Plain));
        assert_eq!(
            need_for("minecraft:iron_ore", "minecraft:raw_iron"),
            Some(Need::Plain)
        );
    }

    #[test]
    fn the_block_itself_needs_silk_touch() {
        assert_eq!(need_for("iron_ore", "iron_ore"), Some(Need::Silk));
        assert_eq!(need_for("stone", "cobblestone"), Some(Need::Plain));
        assert_eq!(need_for("stone", "stone"), Some(Need::Silk));
        assert_eq!(blocks_dropping("stone"), ["stone"]);
    }

    #[test]
    fn blocks_that_drop_themselves_take_any_tool() {
        assert_eq!(need_for("dirt", "dirt"), Some(Need::Either));
        assert_eq!(need_for("oak_log", "oak_log"), Some(Need::Either));
        assert_eq!(blocks_dropping("dirt"), ["dirt", "grass_block"]);
    }

    #[test]
    fn unknown_items_and_pairs_are_empty() {
        assert!(blocks_dropping("minecraft:bedrock").is_empty());
        assert_eq!(need_for("iron_ore", "diamond"), None);
        assert_eq!(need_for("bedrock", "raw_iron"), None);
    }
}

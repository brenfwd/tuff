//! Pure block registry queries behind the `block_*` registry imports.

use tuff_vanilla_data::block::BlockState;
use tuff_vanilla_data::registry::Block;

pub const TRAIT_SOLID: i32 = 1;
pub const TRAIT_AIR: i32 = 2;
pub const TRAIT_LIQUID: i32 = 4;
pub const TRAIT_REPLACEABLE: i32 = 8;

pub const OP_ROTATE_90: i32 = 1;
pub const OP_ROTATE_180: i32 = 2;
pub const OP_ROTATE_270: i32 = 3;
pub const OP_SWAP_NORTH_SOUTH: i32 = 4;
pub const OP_SWAP_EAST_WEST: i32 = 5;

/// A state or bare block id in command syntax, namespace optional.
pub fn state(text: &str) -> Option<BlockState> {
    crate::world::parse_state(text)
}

pub fn normalize(text: &str) -> Option<String> {
    state(text).map(|state| state.to_string())
}

/// One `name=value,value,...` line per property of the block, in state order.
pub fn properties(text: &str) -> Option<String> {
    let state = state(text)?;
    let mut out = String::new();
    for (name, _) in state.properties() {
        let mut values: Vec<&str> = Vec::new();
        for candidate in state.block().states() {
            if let Some(value) = candidate.get(name)
                && !values.contains(&value)
            {
                values.push(value);
            }
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(name);
        out.push('=');
        out.push_str(&values.join(","));
    }
    Some(out)
}

/// The members of a block tag, one block name per line; `None` when the
/// tag is unknown or empty.
pub fn tag_members(tag: &str) -> Option<String> {
    let tag = tag.strip_prefix('#').unwrap_or(tag);
    let members = tuff_vanilla_data::growth::block_tag(tag);
    if members.is_empty() {
        return None;
    }
    let names: Vec<&str> = members.iter().map(|block| block.name()).collect();
    Some(names.join("\n"))
}

/// The members of an item tag, one item name per line; `None` when the
/// tag is unknown or empty.
pub fn item_tag_members(tag: &str) -> Option<String> {
    let tag = crate::portal::qualified(tag.strip_prefix('#').unwrap_or(tag));
    let (_, members) = tuff_vanilla_data::tag::item::ALL
        .iter()
        .find(|(name, _)| *name == tag)?;
    if members.is_empty() {
        return None;
    }
    let names: Vec<&str> = members.iter().map(|item| item.name()).collect();
    Some(names.join("\n"))
}

pub fn transform(text: &str, op: i32) -> Result<String, i32> {
    let state = state(text).ok_or(-1)?;
    let turned = |turns: usize| (0..turns).fold(state, |s, _| s.rotated_clockwise());
    let out = match op {
        OP_ROTATE_90 => turned(1),
        OP_ROTATE_180 => turned(2),
        OP_ROTATE_270 => turned(3),
        OP_SWAP_NORTH_SOUTH => state.mirrored_left_right(),
        OP_SWAP_EAST_WEST => state.mirrored_front_back(),
        _ => return Err(-2),
    };
    Ok(out.to_string())
}

pub fn traits(text: &str) -> Option<i32> {
    let state = state(text)?;
    let mut bits = 0;
    if state.is_solid() {
        bits |= TRAIT_SOLID;
    }
    if matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir) {
        bits |= TRAIT_AIR;
    }
    if state.is_liquid() {
        bits |= TRAIT_LIQUID;
    }
    if state.is_replaceable() {
        bits |= TRAIT_REPLACEABLE;
    }
    Some(bits)
}

/// What the guest may ask about an item; none for an unknown item or air.
pub fn item(name: &str) -> Option<crate::wit::bindings::types::ItemDetails> {
    let item = tuff_vanilla_data::Item::from_name(&crate::portal::qualified(name))
        .filter(|item| *item != tuff_vanilla_data::Item::Air)?;
    Some(crate::wit::bindings::types::ItemDetails {
        item: item.name().to_owned(),
        max_stack: item.max_stack_size(),
        max_damage: item.max_damage(),
        consumable: item.consumable().is_some() || item.food().is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn normalizes_and_rejects() {
        assert_eq!(
            normalize("oak_stairs[facing=east]").unwrap(),
            "minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]"
        );
        assert!(normalize("nope").is_none());
        assert!(normalize("stone[facing=east]").is_none());
        assert!(normalize("oak_stairs[facing=up]").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lists_properties_and_tags() {
        let props = properties("minecraft:oak_log").unwrap();
        assert_eq!(props, "axis=x,y,z");
        assert!(
            properties("redstone_wire")
                .unwrap()
                .contains("north=up,side,none")
        );
        assert_eq!(properties("stone").unwrap(), "");
        assert!(
            tag_members("#minecraft:logs")
                .unwrap()
                .contains("minecraft:oak_log")
        );
        assert!(tag_members("logs").is_some());
        assert!(tag_members("minecraft:no_such_tag").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotates_and_mirrors() {
        let facing = |s: &str| {
            s.split("facing=").nth(1).unwrap()[..4]
                .trim_end_matches(',')
                .to_owned()
        };
        let stairs = "oak_stairs[facing=north]";
        assert!(
            transform(stairs, OP_ROTATE_90)
                .unwrap()
                .contains("facing=east")
        );
        assert!(
            transform(stairs, OP_ROTATE_180)
                .unwrap()
                .contains("facing=south")
        );
        assert!(
            transform(stairs, OP_ROTATE_270)
                .unwrap()
                .contains("facing=west")
        );
        assert!(
            transform(stairs, OP_SWAP_NORTH_SOUTH)
                .unwrap()
                .contains("facing=south")
        );
        assert!(
            transform(stairs, OP_SWAP_EAST_WEST)
                .unwrap()
                .contains("facing=north")
        );
        assert_eq!(
            facing(&transform("oak_door[facing=east]", OP_ROTATE_90).unwrap()),
            "sout"
        );
        assert_eq!(
            transform("oak_log[axis=x]", OP_ROTATE_90).unwrap(),
            "minecraft:oak_log[axis=z]"
        );
        assert!(
            transform("oak_sign[rotation=0]", OP_ROTATE_90)
                .unwrap()
                .contains("rotation=4")
        );
        assert!(
            transform("rail[shape=north_south]", OP_ROTATE_90)
                .unwrap()
                .contains("shape=east_west")
        );
        assert!(
            transform("redstone_wire[north=side]", OP_ROTATE_90)
                .unwrap()
                .contains("east=side")
        );
        assert_eq!(transform("nope", OP_ROTATE_90), Err(-1));
        assert_eq!(transform("stone", 9), Err(-2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn describes_items() {
        let pickaxe = item("diamond_pickaxe").unwrap();
        assert_eq!(pickaxe.item, "minecraft:diamond_pickaxe");
        assert_eq!(pickaxe.max_stack, 1);
        assert_eq!(pickaxe.max_damage, Some(1561));
        assert!(!pickaxe.consumable);
        let bread = item("minecraft:bread").unwrap();
        assert_eq!((bread.max_stack, bread.max_damage), (64, None));
        assert!(bread.consumable);
        assert_eq!(item("ender_pearl").unwrap().max_stack, 16);
        assert!(item("potion").unwrap().consumable);
        assert!(item("air").is_none());
        assert!(item("nope").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn reports_traits() {
        assert_eq!(traits("stone"), Some(TRAIT_SOLID));
        assert_eq!(traits("air").unwrap() & TRAIT_AIR, TRAIT_AIR);
        assert_eq!(traits("water").unwrap() & TRAIT_LIQUID, TRAIT_LIQUID);
        assert_eq!(
            traits("short_grass").unwrap() & TRAIT_REPLACEABLE,
            TRAIT_REPLACEABLE
        );
        assert_eq!(traits("nope"), None);
    }
}

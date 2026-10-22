//! Maps made at a crafting or cartography table: the result carries a
//! post-processing mark (lock or zoom out) that becomes a new map once the
//! player has taken it.

use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Tag;
use tuff_vanilla_data::Item;

use crate::data::{MAX_SCALE, MapData};
use crate::service::Derivation;

pub const POST_PROCESSING: &str = "minecraft:map_post_processing";
const LOCK: i32 = 0;
const SCALE: i32 = 1;

pub fn post_processing(stack: &ItemStack) -> Option<Derivation> {
    match stack.components.other.get(POST_PROCESSING)? {
        Tag::Int(LOCK) => Some(Derivation::Lock),
        Tag::Int(SCALE) => Some(Derivation::ZoomOut),
        _ => None,
    }
}

pub fn with_post_processing(mut stack: ItemStack, how: Derivation) -> ItemStack {
    let id = match how {
        Derivation::Lock => LOCK,
        Derivation::ZoomOut => SCALE,
    };
    stack.components.other.insert(POST_PROCESSING, Tag::Int(id));
    stack
}

pub fn take_post_processing(stack: &mut ItemStack) -> Option<Derivation> {
    let how = post_processing(stack);
    stack.components.remove_other(POST_PROCESSING);
    how
}

/// What a cartography table makes of `map` (whose saved data is `data`)
/// and the item in its second slot.
pub fn cartography_result(
    map: &ItemStack,
    additional: &ItemStack,
    data: &MapData,
) -> Option<ItemStack> {
    match additional.item() {
        Item::Paper
            if tuff_datapack::tags::is(
                tuff_vanilla_data::tag::item::key::EXTENDABLE_MAPS,
                map.item(),
            ) && !data.locked
                && data.scale < MAX_SCALE =>
        {
            Some(with_post_processing(
                ItemStack::mint_like(map, 1, MintReason::Crafting),
                Derivation::ZoomOut,
            ))
        }
        Item::GlassPane if !data.locked => Some(with_post_processing(
            ItemStack::mint_like(map, 1, MintReason::Crafting),
            Derivation::Lock,
        )),
        Item::Map => Some(ItemStack::mint_like(map, 2, MintReason::Crafting)),
        _ => None,
    }
}

/// Whether a crafted extending result may stand: its map exists and can
/// still grow.
pub fn extends(data: Option<&MapData>) -> bool {
    data.is_some_and(|data| data.scale < MAX_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::{BurnReason, MintReason};

    fn map(id: u32) -> ItemStack {
        let mut stack = ItemStack::mint(Item::FilledMap, 3, MintReason::Test);
        stack.components.map_id = Some(id);
        stack
    }

    fn data(scale: u8, locked: bool) -> MapData {
        let mut data = MapData::fresh(0.0, 0.0, scale, true, false, "minecraft:overworld");
        data.locked = locked;
        data
    }

    #[tuff_core::watchdog]
    #[test]
    fn paper_zooms_out_unlocked_maps_below_the_top_scale() {
        let paper = ItemStack::mint(Item::Paper, 1, MintReason::Test);
        let result = cartography_result(&map(4), &paper, &data(1, false)).unwrap();
        assert_eq!(result.count(), 1);
        assert_eq!(result.components.map_id, Some(4));
        assert_eq!(post_processing(&result), Some(Derivation::ZoomOut));
        assert!(cartography_result(&map(4), &paper, &data(MAX_SCALE, false)).is_none());
        assert!(cartography_result(&map(4), &paper, &data(0, true)).is_none());
        let mut explorer = map(4);
        explorer.transform(Item::WoodlandMansionMap, MintReason::Test, BurnReason::Test);
        assert!(cartography_result(&explorer, &paper, &data(0, false)).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn glass_locks_and_empty_maps_clone() {
        let pane = ItemStack::mint(Item::GlassPane, 1, MintReason::Test);
        let result = cartography_result(&map(2), &pane, &data(0, false)).unwrap();
        assert_eq!(post_processing(&result), Some(Derivation::Lock));
        assert!(cartography_result(&map(2), &pane, &data(0, true)).is_none());
        let empty = ItemStack::mint(Item::Map, 5, MintReason::Test);
        let copies = cartography_result(&map(2), &empty, &data(0, true)).unwrap();
        assert_eq!(copies.count(), 2);
        assert_eq!(post_processing(&copies), None);
        let stone = ItemStack::mint(Item::Stone, 1, MintReason::Test);
        assert!(cartography_result(&map(2), &stone, &data(0, false)).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_mark_is_taken_once() {
        let mut stack = with_post_processing(map(1), Derivation::Lock);
        assert_eq!(take_post_processing(&mut stack), Some(Derivation::Lock));
        assert_eq!(take_post_processing(&mut stack), None);
        assert!(extends(Some(&data(3, false))));
        assert!(!extends(Some(&data(MAX_SCALE, false))));
        assert!(!extends(None));
    }
}

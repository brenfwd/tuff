//! All-or-nothing edits of a player's inventory by uuid, online or not.
//! The coordinator owns the routing: an online player's session applies
//! the edit itself, and an offline player's stored data is edited in the
//! slot of their session chain, so a login waits for the edit to be saved
//! and an edit waits for a leaving session's save.

use crate::world::inventory::{Inventory, SLOTS};
use tuff_item::item::BurnReason;
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_item::item::MintReason;

/// Slots an edit looks at: armor, main, hotbar and off hand (never the
/// crafting grid or the cursor).
const OWNED: std::ops::Range<usize> = 5..SLOTS;

/// One edit; `stack` is one of the item, matched with its components.
#[derive(Clone, Debug, PartialEq)]
pub enum InventoryEdit {
    /// Removes `count` of `stack`, or nothing unless that many are held.
    Take { stack: ItemSnapshot, count: u32 },
    /// Adds `count` of `stack`, or nothing unless all of them fit.
    Give { stack: ItemSnapshot, count: u32 },
}

impl InventoryEdit {
    /// Applies the edit to `inventory`; false leaves it unchanged.
    pub fn apply(&self, inventory: &mut Inventory) -> bool {
        match self {
            InventoryEdit::Take { stack, count } => take(inventory, stack, *count),
            InventoryEdit::Give { stack, count } => give(inventory, stack, *count),
        }
    }
}

fn take(inventory: &mut Inventory, one: &ItemStack, count: u32) -> bool {
    let held: u32 = OWNED
        .filter_map(|slot| inventory.slots()[slot].as_ref())
        .filter(|stack| stack.stacks_with(one))
        .map(|stack| u32::from(stack.count()))
        .sum();
    if count == 0 || held < count {
        return false;
    }
    let mut remaining = count;
    for slot in OWNED.rev() {
        if remaining == 0 {
            break;
        }
        let held = inventory.slot_mut(slot);
        let Some(stack) = held.as_mut().filter(|stack| stack.stacks_with(one)) else {
            continue;
        };
        let taken = remaining.min(u32::from(stack.count()));
        remaining -= taken;
        stack.take(taken as u8).burn(BurnReason::Command);
        if stack.count() == 0 {
            *held = None;
        }
    }
    true
}

fn give(inventory: &mut Inventory, one: &ItemStack, count: u32) -> bool {
    if count == 0 {
        return false;
    }
    if inventory.total_room_for(one) < count {
        return false;
    }
    let chunk = u32::from(one.max_stack_size().max(1));
    let mut remaining = count;
    while remaining > 0 {
        let part = remaining.min(chunk);
        remaining -= part;
        if let Some(left) =
            inventory.add(ItemStack::mint_like(one, part as u8, MintReason::Command))
        {
            left.burn(BurnReason::Command);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn take_is_all_or_nothing_and_matches_components() {
        let mut inventory = Inventory::default();
        inventory.set_slot(
            36,
            Some(ItemStack::mint(Item::Diamond, 3, MintReason::Test)),
        );
        inventory.set_slot(
            9,
            Some(ItemStack::mint(Item::Diamond, 64, MintReason::Test)),
        );
        let one = ItemStack::mint(Item::Diamond, 1, MintReason::Test);
        assert!(
            !InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&one),
                count: 68
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[36].as_ref().unwrap().count(), 3);
        assert!(
            InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&one),
                count: 66
            }
            .apply(&mut inventory)
        );
        let left: u32 = inventory
            .slots()
            .iter()
            .flatten()
            .map(|s| u32::from(s.count()))
            .sum();
        assert_eq!(left, 1);
        let mut named = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
        named.set_damage(1);
        inventory.set_slot(
            10,
            Some(ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test)),
        );
        assert!(
            !InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&named),
                count: 1
            }
            .apply(&mut inventory)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn give_adds_everything_or_nothing() {
        let mut inventory = Inventory::default();
        for slot in 9..45 {
            inventory.set_slot(
                slot,
                Some(ItemStack::mint(Item::Stone, 64, MintReason::Test)),
            );
        }
        inventory.set_slot(9, Some(ItemStack::mint(Item::Dirt, 60, MintReason::Test)));
        let dirt = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
        assert!(
            !InventoryEdit::Give {
                stack: tuff_item::item::ItemSnapshot::of(&dirt),
                count: 5
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[9].as_ref().unwrap().count(), 60);
        assert!(
            InventoryEdit::Give {
                stack: tuff_item::item::ItemSnapshot::of(&dirt),
                count: 4
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[9].as_ref().unwrap().count(), 64);
    }
}

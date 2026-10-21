//! All-or-nothing edits of a player's inventory by uuid, online or not.
//! The coordinator owns the routing: an online player's session applies
//! the edit itself, and an offline player's stored data is edited in the
//! slot of their session chain, so a login waits for the edit to be saved
//! and an edit waits for a leaving session's save.

use crate::world::inventory::{Inventory, SLOTS};
use tuff_item::item::ItemStack;

/// Slots an edit looks at: armor, main, hotbar and off hand (never the
/// crafting grid or the cursor).
const OWNED: std::ops::Range<usize> = 5..SLOTS;

/// One edit; `stack` is one of the item, matched with its components.
#[derive(Clone, Debug, PartialEq)]
pub enum InventoryEdit {
    /// Removes `count` of `stack`, or nothing unless that many are held.
    Take { stack: ItemStack, count: u32 },
    /// Adds `count` of `stack`, or nothing unless all of them fit.
    Give { stack: ItemStack, count: u32 },
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
        .map(|stack| u32::from(stack.count))
        .sum();
    if count == 0 || held < count {
        return false;
    }
    let mut remaining = count;
    for slot in OWNED.rev() {
        if remaining == 0 {
            break;
        }
        let Some(stack) = inventory.slots()[slot].clone() else {
            continue;
        };
        if !stack.stacks_with(one) {
            continue;
        }
        let taken = remaining.min(u32::from(stack.count));
        remaining -= taken;
        let left = u32::from(stack.count) - taken;
        inventory.set_slot(slot, (left > 0).then(|| stack.with_count(left as u8)));
    }
    true
}

fn give(inventory: &mut Inventory, one: &ItemStack, count: u32) -> bool {
    if count == 0 {
        return false;
    }
    let mut trial = inventory.clone();
    let chunk = u32::from(one.max_stack_size().max(1));
    let mut remaining = count;
    while remaining > 0 {
        let part = remaining.min(chunk);
        remaining -= part;
        if trial.add(one.clone().with_count(part as u8)).is_some() {
            return false;
        }
    }
    *inventory = trial;
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
        inventory.set_slot(36, Some(ItemStack::new(Item::Diamond, 3)));
        inventory.set_slot(9, Some(ItemStack::new(Item::Diamond, 64)));
        let one = ItemStack::new(Item::Diamond, 1);
        assert!(
            !InventoryEdit::Take {
                stack: one.clone(),
                count: 68
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[36].as_ref().unwrap().count, 3);
        assert!(
            InventoryEdit::Take {
                stack: one,
                count: 66
            }
            .apply(&mut inventory)
        );
        let left: u32 = inventory
            .slots()
            .iter()
            .flatten()
            .map(|s| u32::from(s.count))
            .sum();
        assert_eq!(left, 1);
        let mut named = ItemStack::new(Item::IronPickaxe, 1);
        named.set_damage(1);
        inventory.set_slot(10, Some(ItemStack::new(Item::IronPickaxe, 1)));
        assert!(
            !InventoryEdit::Take {
                stack: named,
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
            inventory.set_slot(slot, Some(ItemStack::new(Item::Stone, 64)));
        }
        inventory.set_slot(9, Some(ItemStack::new(Item::Dirt, 60)));
        let dirt = ItemStack::new(Item::Dirt, 1);
        assert!(
            !InventoryEdit::Give {
                stack: dirt.clone(),
                count: 5
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[9].as_ref().unwrap().count, 60);
        assert!(
            InventoryEdit::Give {
                stack: dirt,
                count: 4
            }
            .apply(&mut inventory)
        );
        assert_eq!(inventory.slots()[9].as_ref().unwrap().count, 64);
    }
}

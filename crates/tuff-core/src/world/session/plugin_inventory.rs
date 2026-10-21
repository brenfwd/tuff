//! Inventory edits features queue for a player by uuid.

use super::*;
use tuff_item::item::BurnReason;
use tuff_item::item::ItemStack;

/// A per-player override of the `keep_inventory` rule, set by a feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeepInventory(pub bool);

/// One inventory edit a feature asked for.
#[derive(Debug, PartialEq)]
pub enum InventoryOp {
    /// Removes up to `count` of an item; with `all_or_nothing`, nothing
    /// unless that many are carried.
    Take {
        item: tuff_vanilla_data::Item,
        count: u32,
        all_or_nothing: bool,
    },
    /// Empties every slot (armor and off hand included) and the cursor.
    Clear,
    /// Replaces one slot (5 to 45 in menu numbering).
    SetSlot { slot: u8, stack: Option<ItemStack> },
    /// Overrides the `keep_inventory` rule for this player; `None` clears.
    KeepInventory(Option<bool>),
    /// Gives `count` of `stack`, dropping what does not fit.
    Give {
        stack: tuff_item::item::ItemSnapshot,
        count: i32,
    },
}

impl PlayerSession {
    pub fn keeps_inventory(&self) -> bool {
        self.ext
            .get::<KeepInventory>()
            .map_or_else(|| self.shared.rule_bool("keep_inventory"), |keep| keep.0)
    }

    /// Applies one feature call's edits in order. A failed all-or-nothing
    /// take skips the rest, so a take followed by a give is an exchange.
    pub async fn apply_inventory_ops(&mut self, ops: Vec<InventoryOp>) {
        for op in ops {
            if let InventoryOp::Give { stack, count } = op {
                self.give_items(stack.view(), count, tuff_item::item::MintReason::Plugin)
                    .await;
            } else if !self.apply_inventory_op(op) {
                break;
            }
        }
    }

    /// Applies one edit; false when an all-or-nothing take found too few.
    pub fn apply_inventory_op(&mut self, op: InventoryOp) -> bool {
        match op {
            InventoryOp::Take {
                item,
                count,
                all_or_nothing,
            } => {
                let held = u32::try_from(Self::count_in(&self.inventory, item)).unwrap_or(0);
                if all_or_nothing && held < count {
                    self.items_taken_hooks(item, count, 0);
                    return false;
                }
                let mut remaining = count.min(held);
                let carried_hit = remaining > 0
                    && self
                        .inventory
                        .carried()
                        .is_some_and(|stack| stack.item == item);
                for slot in (36..crate::world::inventory::SLOTS).chain(9..36) {
                    if remaining == 0 {
                        break;
                    }
                    burn_some(self.inventory.slot_mut(slot), item, &mut remaining);
                }
                if remaining > 0 && carried_hit {
                    burn_some(self.inventory.carried_mut(), item, &mut remaining);
                }
                self.send_inventory();
                self.items_taken_hooks(item, count, count.min(held) - remaining);
            }
            InventoryOp::Clear => {
                self.inventory.clear();
                for slot in 5..crate::world::inventory::SLOTS {
                    self.inventory.set_slot(slot, None);
                }
                self.send_inventory();
            }
            InventoryOp::SetSlot { slot, stack } => {
                if (5..crate::world::inventory::SLOTS).contains(&usize::from(slot)) {
                    self.inventory.set_slot(usize::from(slot), stack);
                    self.send_inventory();
                }
            }
            InventoryOp::KeepInventory(keep) => match keep {
                Some(keep) => *self.ext.entry::<KeepInventory>() = KeepInventory(keep),
                None => {
                    self.ext.remove::<KeepInventory>();
                }
            },
            InventoryOp::Give { .. } => {}
        }
        true
    }
}

fn burn_some(held: &mut Option<ItemStack>, item: tuff_vanilla_data::Item, remaining: &mut u32) {
    let Some(stack) = held.as_mut().filter(|stack| stack.item == item) else {
        return;
    };
    let take = (*remaining).min(u32::from(stack.count()));
    *remaining -= take;
    stack.take(take as u8).burn(BurnReason::Plugin);
    if stack.count() == 0 {
        *held = None;
    }
}

//! Inventory edits features queue for a player by uuid.

use super::*;
use crate::world::conservation::Transfer;
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
    /// Moves these stacks in, dropping what does not fit.
    GiveStacks(Vec<ItemStack>),
    /// [`InventoryOp::Take`], handing what is taken to `sink`.
    TakeInto {
        item: tuff_vanilla_data::Item,
        count: u32,
        all_or_nothing: bool,
        sink: TakenSink,
    },
}

type Deliver = Box<dyn FnOnce(Vec<ItemStack>) -> Result<(), Vec<ItemStack>> + Send + Sync>;

/// Where items a feature takes from a player go instead of being used
/// up: a plugin's stash, filled by a message to its host.
pub struct TakenSink {
    pub stash: u64,
    deliver: Option<Deliver>,
}

impl TakenSink {
    /// A sink for `stash` that nothing receives yet.
    pub fn new(stash: u64) -> Self {
        Self {
            stash,
            deliver: None,
        }
    }

    /// Sends what is taken with `deliver`, which hands the stacks back
    /// when no one can take them.
    pub fn attach(
        &mut self,
        deliver: impl FnOnce(Vec<ItemStack>) -> Result<(), Vec<ItemStack>> + Send + Sync + 'static,
    ) {
        self.deliver = Some(Box::new(deliver));
    }

    /// Hands `stacks` over; they come back when no one takes them.
    pub fn deliver(self, stacks: Vec<ItemStack>) -> Result<(), Vec<ItemStack>> {
        match self.deliver {
            Some(deliver) => deliver(stacks),
            None => Err(stacks),
        }
    }
}

impl std::fmt::Debug for TakenSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TakenSink")
            .field("stash", &self.stash)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TakenSink {
    fn eq(&self, other: &Self) -> bool {
        self.stash == other.stash
    }
}

impl InventoryOp {
    /// Lets go of an edit no player will apply: stacks it carries are used
    /// up.
    pub fn discard(self) {
        if let Self::GiveStacks(stacks) = self {
            if !stacks.is_empty() {
                log::warn!("a feature's items for an offline player were used up");
            }
            for stack in stacks {
                stack.burn(BurnReason::Plugin);
            }
        }
    }
}

impl PlayerSession {
    pub fn keeps_inventory(&self) -> bool {
        self.ext
            .get::<KeepInventory>()
            .map_or_else(|| self.shared.rule_bool("keep_inventory"), |keep| keep.0)
    }

    /// Applies one feature call's edits in order. A failed all-or-nothing
    /// take skips the rest, so a take followed by a give is an exchange.
    /// Refused by the conservation check, the op and the rest of the call
    /// do nothing: stacks they carry are dropped at the player.
    pub async fn apply_inventory_ops(&mut self, ops: Vec<InventoryOp>) {
        let mut ops = ops.into_iter();
        while let Some(op) = ops.next() {
            let at = BlockPos::containing(self.pose.position);
            if !self.may_transfer(Transfer::Plugin, None, at, None) {
                for op in std::iter::once(op).chain(ops) {
                    self.refuse_inventory_op(op).await;
                }
                return;
            }
            match op {
                InventoryOp::Give { stack, count } => {
                    self.give_items(&stack, count, tuff_item::item::MintReason::Plugin)
                        .await;
                }
                InventoryOp::GiveStacks(stacks) => self.receive_stacks(stacks).await,
                InventoryOp::TakeInto {
                    item,
                    count,
                    all_or_nothing,
                    sink,
                } => {
                    let Some(taken) = self.take_items(item, count, all_or_nothing) else {
                        break;
                    };
                    if let Err(back) = sink.deliver(taken) {
                        self.receive_stacks(back).await;
                    }
                }
                op => {
                    if !self.apply_inventory_op(op) {
                        break;
                    }
                }
            }
        }
    }

    /// Lets go of an op the conservation check refused: what it carries is
    /// dropped where the player stands, and a sink gets nothing.
    async fn refuse_inventory_op(&mut self, op: InventoryOp) {
        let stacks = match op {
            InventoryOp::GiveStacks(stacks) => stacks,
            InventoryOp::SetSlot {
                stack: Some(stack), ..
            } => vec![stack],
            _ => return,
        };
        let at = self.pose.position;
        for stack in stacks {
            self.spawn_item(stack, at, [0.0; 3], player::THROWN_PICKUP_DELAY)
                .await;
        }
    }

    /// Puts stacks into the inventory, throwing what does not fit.
    async fn receive_stacks(&mut self, stacks: Vec<ItemStack>) {
        let left: Vec<ItemStack> = stacks
            .into_iter()
            .filter_map(|stack| self.inventory.add(stack))
            .collect();
        for stack in left {
            self.throw_item(stack).await;
        }
        self.send_inventory();
    }

    /// Takes up to `count` of `item` out of the main slots, hotbar, off
    /// hand and cursor; `None` when an all-or-nothing take finds too few.
    fn take_items(
        &mut self,
        item: tuff_vanilla_data::Item,
        count: u32,
        all_or_nothing: bool,
    ) -> Option<Vec<ItemStack>> {
        let held = u32::try_from(Self::count_in(&self.inventory, item)).unwrap_or(0);
        if all_or_nothing && held < count {
            self.items_taken_hooks(item, count, 0);
            return None;
        }
        let mut remaining = count.min(held);
        let carried_hit = remaining > 0
            && self
                .inventory
                .carried()
                .is_some_and(|stack| stack.item() == item);
        let mut taken = Vec::new();
        for slot in (36..crate::world::inventory::SLOTS).chain(9..36) {
            if remaining == 0 {
                break;
            }
            taken.extend(take_some(
                self.inventory.slot_mut(slot),
                item,
                &mut remaining,
            ));
        }
        if remaining > 0 && carried_hit {
            taken.extend(take_some(
                self.inventory.carried_mut(),
                item,
                &mut remaining,
            ));
        }
        self.send_inventory();
        self.items_taken_hooks(item, count, count.min(held) - remaining);
        let after = u64::try_from(Self::count_in(&self.inventory, item)).unwrap_or(0);
        let sent = taken.iter().map(|stack| u64::from(stack.count())).sum();
        self.transfer_balanced(Transfer::Plugin, u64::from(held), 0, after, sent);
        Some(taken)
    }

    /// Applies one edit; false when an all-or-nothing take found too few.
    fn apply_inventory_op(&mut self, op: InventoryOp) -> bool {
        match op {
            InventoryOp::Take {
                item,
                count,
                all_or_nothing,
            } => {
                let Some(taken) = self.take_items(item, count, all_or_nothing) else {
                    return false;
                };
                for stack in taken {
                    stack.burn(BurnReason::Plugin);
                }
            }
            InventoryOp::Clear => {
                for stack in self.inventory.clear() {
                    stack.burn(tuff_item::item::BurnReason::Plugin);
                }
                self.send_inventory();
            }
            InventoryOp::SetSlot { slot, stack } => {
                if (5..crate::world::inventory::SLOTS).contains(&usize::from(slot)) {
                    let old = std::mem::replace(self.inventory.slot_mut(usize::from(slot)), stack);
                    if let Some(old) = old {
                        old.burn(tuff_item::item::BurnReason::Plugin);
                    }
                    self.send_inventory();
                } else if let Some(stack) = stack {
                    stack.burn(tuff_item::item::BurnReason::Plugin);
                }
            }
            InventoryOp::KeepInventory(keep) => match keep {
                Some(keep) => *self.ext.entry::<KeepInventory>() = KeepInventory(keep),
                None => {
                    self.ext.remove::<KeepInventory>();
                }
            },
            InventoryOp::Give { .. } => {}
            op @ (InventoryOp::GiveStacks(_) | InventoryOp::TakeInto { .. }) => op.discard(),
        }
        true
    }
}

fn take_some(
    held: &mut Option<ItemStack>,
    item: tuff_vanilla_data::Item,
    remaining: &mut u32,
) -> Option<ItemStack> {
    if held.as_ref().is_none_or(|stack| stack.item() != item) {
        return None;
    }
    let count = u32::from(held.as_ref().map_or(0, ItemStack::count));
    let take = (*remaining).min(count);
    *remaining -= take;
    ItemStack::take_from(held, take as u8)
}

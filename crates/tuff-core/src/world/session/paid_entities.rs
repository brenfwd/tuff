//! Items that make an entity are paid with the click: the one item leaves
//! the hand when the request is sent and is spent (or handed back) when the
//! region answers, so dropping the stack in between cannot keep both the
//! item and the entity.

use tuff_item::item::{BurnReason, ItemStack, MintReason};
use tuff_vanilla_data::Item;

use super::PlayerSession;
use crate::world::messages::{ActionOutcome, HandEffect};

/// An item in flight with a click, by the click's sequence.
#[derive(Debug, Default)]
pub struct Paying(Vec<(i32, usize, ItemStack)>);

impl PlayerSession {
    /// Whether using `item` creates an entity that the item pays for.
    pub(super) fn makes_an_entity(item: Item) -> bool {
        item.spawns_entity().is_some()
            || item.boat().is_some()
            || matches!(
                item,
                Item::ArmorStand
                    | Item::ItemFrame
                    | Item::GlowItemFrame
                    | Item::Painting
                    | Item::EndCrystal
            )
            || item.name().ends_with("minecart")
            || matches!(
                item.bucket(),
                Some(tuff_vanilla_data::item::Bucket::Fluid { mob: Some(_), .. })
            )
    }

    /// Takes the one item the click on `sequence` pays with out of the hand.
    pub(super) fn hold_payment(&mut self, off_hand: bool, sequence: i32) {
        if self.mode.abilities.instabuild {
            return;
        }
        let slot = self.inventory.hand_slot(off_hand);
        let pays = self
            .inventory
            .hand(off_hand)
            .is_some_and(|stack| Self::makes_an_entity(stack.item()));
        if pays && let Some(one) = ItemStack::take_from(self.inventory.slot_mut(slot), 1) {
            self.actions.paying.0.push((sequence, slot, one));
        }
    }

    /// Settles the payment held for `sequence` against the region's answer;
    /// true if it was spent, so the answer must not consume the hand again.
    pub(super) async fn settle_payment(&mut self, sequence: i32, outcome: &ActionOutcome) -> bool {
        let Some(at) = self
            .actions
            .paying
            .0
            .iter()
            .position(|(held, ..)| *held == sequence)
        else {
            return false;
        };
        let (_, slot, paid) = self.actions.paying.0.swap_remove(at);
        let spent = matches!(
            outcome,
            ActionOutcome::Placed { .. }
                | ActionOutcome::Used {
                    effect: HandEffect::Consume(_) | HandEffect::Emptied(_),
                    ..
                }
        );
        if !spent {
            self.return_one_payment(slot, paid).await;
            return false;
        }
        paid.burn(BurnReason::Consumed);
        if let ActionOutcome::Used {
            effect: HandEffect::Emptied(result),
            ..
        } = outcome
        {
            let stack = ItemStack::mint(*result, 1, MintReason::Transform);
            self.return_one_payment(slot, stack).await;
        }
        true
    }

    /// Returns every payment still in flight (the player is leaving).
    pub(super) async fn return_payments(&mut self) {
        for (_, slot, paid) in std::mem::take(&mut self.actions.paying.0) {
            self.return_one_payment(slot, paid).await;
        }
    }

    async fn return_one_payment(&mut self, slot: usize, stack: ItemStack) {
        if let Some(leftover) = self.inventory.put_back(slot, stack) {
            let at = self.pose.position;
            self.spawn_item(
                leftover,
                at,
                [0.0; 3],
                crate::world::player::THROWN_PICKUP_DELAY,
            )
            .await;
        }
        self.send_inventory();
    }
}

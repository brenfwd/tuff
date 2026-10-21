//! Stacks a block click moves to the clicked block's region.

use tuff_item::item::ItemStack;
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::BlockPos;

use super::PlayerSession;
use crate::world::inventory::Inventory;
use crate::world::messages::{RegionHandle, RegionRequest};
use crate::world::player;

impl PlayerSession {
    /// Sends a block click; if the region is gone, the stacks it offered
    /// go back where they came from.
    pub(super) async fn send_place(&mut self, region: &RegionHandle, request: RegionRequest) {
        let Err(refused) = region.try_request(request).await else {
            return;
        };
        let (RegionRequest::Place(place) | RegionRequest::PlaceOnWater(place)) = *refused else {
            return;
        };
        for (slot, stack) in place.offer.into_stacks() {
            if let Some(leftover) = self.inventory.put_back(slot, stack) {
                let at = self.pose.position;
                self.spawn_item(leftover, at, [0.0; 3], player::THROWN_PICKUP_DELAY)
                    .await;
            }
        }
        self.send_inventory();
    }

    /// The stacks a click on `target` moves to the region for the block to
    /// keep, judged from the block the client sees there: one item of the
    /// hand for blocks that take one (a lectern's book, a jukebox's disc,
    /// a firework launched off any block), the whole hand for a shelf
    /// swap, the hotbar for a powered shelf.
    pub(super) fn place_offer(
        &mut self,
        off_hand: bool,
        target: BlockPos,
        held: Option<Item>,
    ) -> crate::world::messages::PlaceOffer {
        use crate::world::messages::PlaceOffer;
        let creative = self.mode.abilities.instabuild;
        let Some(held) = held else {
            return PlaceOffer::new(creative, Vec::new());
        };
        let slot = self.inventory.hand_slot(off_hand);
        let block = self.view.mirror.block_at(target);
        let shelf = block
            .filter(|state| tuff_vanilla_data::tag::block::WOODEN_SHELVES.contains(&state.block()));
        let stacks = match shelf {
            Some(state) if !off_hand => {
                let slots: Vec<usize> = if state.get("powered") == Some("true") {
                    (0..9).map(Inventory::hotbar_slot).collect()
                } else {
                    vec![slot]
                };
                slots
                    .into_iter()
                    .filter_map(|slot| Some((slot, self.inventory.slot_mut(slot).take()?)))
                    .collect()
            }
            _ if held == Item::FireworkRocket
                || block.is_some_and(|state| keeps_one(state.block())) =>
            {
                ItemStack::take_from(self.inventory.slot_mut(slot), 1)
                    .map(|one| vec![(slot, one)])
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        PlaceOffer::new(creative, stacks)
    }
}

/// Blocks a click can leave one item of the hand in.
fn keeps_one(block: Block) -> bool {
    matches!(
        block,
        Block::Lectern
            | Block::Jukebox
            | Block::Campfire
            | Block::SoulCampfire
            | Block::ChiseledBookshelf
            | Block::DecoratedPot
            | Block::Composter
    )
}

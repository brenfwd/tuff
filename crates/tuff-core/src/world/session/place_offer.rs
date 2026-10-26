//! Stacks a block click moves to the clicked block's region.

use tuff_item::item::ItemStack;
use tuff_vanilla_data::{Block, BlockState, Item};
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
        self.answered(place.sequence);
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
    pub(super) async fn place_offer(
        &mut self,
        region: &RegionHandle,
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
        let block = match self.view.mirror.block_at(target) {
            Some(block) => Some(block),
            None => block_at(region, target).await,
        };
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
                || block.is_some_and(|state| self.keeps_one(state, held)) =>
            {
                ItemStack::take_from(self.inventory.slot_mut(slot), 1)
                    .map(|one| vec![(slot, one)])
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if stacks.is_empty()
            && !creative
            && (self.places_a_block_from_the_hand(held) || self.custom_item_places_blocks(off_hand))
            && !self.shared.block_rules.places_on_water(held)
            && let Some(one) = ItemStack::take_from(self.inventory.slot_mut(slot), 1)
        {
            return PlaceOffer::paying(creative, slot, one);
        }
        if stacks.is_empty()
            && !creative
            && self
                .inventory
                .hand(off_hand)
                .is_some_and(|held| held.max_damage().is_none())
            && let Some(one) = ItemStack::take_from(self.inventory.slot_mut(slot), 1)
        {
            return PlaceOffer::costing(creative, slot, one);
        }
        PlaceOffer::new(creative, stacks)
    }

    /// Whether the hand holds a custom item that places blocks from its
    /// own behaviour.
    fn custom_item_places_blocks(&self, off_hand: bool) -> bool {
        self.inventory
            .hand(off_hand)
            .is_some_and(|stack| self.shared.custom.places_blocks(stack))
    }

    /// Whether a click with `held` places a block that costs one item: the
    /// item rides along with the click and the region spends it as the
    /// block is written.
    pub(super) fn places_a_block_from_the_hand(&self, held: Item) -> bool {
        held.block().is_some()
            && !matches!(
                held.bucket(),
                Some(tuff_vanilla_data::item::Bucket::Fluid { .. })
            )
    }

    /// Whether a click with `held` on a block the client sees as `state`
    /// can leave one item in it, so the item rides along with the click.
    fn keeps_one(&self, state: BlockState, held: Item) -> bool {
        use tuff_vanilla_data::tag::item as item_tags;
        match state.block() {
            Block::Lectern => {
                state.get("has_book") != Some("true") && item_tags::LECTERN_BOOKS.contains(&held)
            }
            Block::Jukebox => {
                state.get("has_record") != Some("true") && held.jukebox_song_id().is_some()
            }
            Block::Campfire | Block::SoulCampfire => self
                .shared
                .recipes()
                .can_cook(crate::world::block_entity::ContainerKind::Campfire, held),
            Block::ChiseledBookshelf => item_tags::BOOKSHELF_BOOKS.contains(&held),
            Block::Composter => held.compostable().is_some(),
            Block::DecoratedPot => true,
            _ => false,
        }
    }
}

/// The block at `pos` as its region has it, for a click on a block the
/// client was never sent (a fake player).
async fn block_at(region: &RegionHandle, pos: BlockPos) -> Option<BlockState> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    region.request(RegionRequest::BlockAt { pos, reply }).await;
    answer.await.ok().flatten()
}

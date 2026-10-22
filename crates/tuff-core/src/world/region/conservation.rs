//! A region's side of item conservation: refused operations quarantine it
//! and the player involved, and a quarantined region neither saves nor
//! trades items with players until it unloads.

use super::RegionActor;
use crate::world::conservation::{ItemViolation, Subject, Transfer};
use crate::world::entity::RegionEntity;
use crate::world::messages::{PlayerEvent, PlayerHandle, RegionHandle};
use tuff_item::text::TextComponent;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

impl RegionActor {
    fn subject(&self) -> Subject {
        Subject::Region(self.dimension, self.pos)
    }

    /// Whether a refused item operation left this region's state out of
    /// what may be saved.
    pub(in crate::world::region) fn quarantined(&self) -> bool {
        self.shared.conservation.quarantined(self.subject())
    }

    /// The region reloads from its last consistent save next time.
    pub(in crate::world::region) fn end_quarantine(&self) {
        self.shared.conservation.release(self.subject());
    }

    /// Logs, audits and contains a refused item operation: this region and
    /// `player` are not saved until they reload, and the player is
    /// disconnected (with `tuff.items.conservationKick`) so its client
    /// reloads too.
    pub(in crate::world::region) fn item_violation(
        &mut self,
        player: Option<&PlayerHandle>,
        violation: ItemViolation,
    ) {
        tuff_item::item::conservation::violation(
            violation.operation,
            format_args!(
                "player {:?} at {:?} in {}: {}",
                violation.player,
                violation.pos,
                self.dimension().key,
                violation.detail
            ),
        );
        self.shared.conservation.quarantine(self.subject());
        if let Some(player) = player {
            self.shared
                .conservation
                .quarantine(Subject::Player(player.id));
            let kick = self
                .shared
                .config
                .get_bool(crate::config::ITEMS_CONSERVATION_KICK)
                .unwrap_or(true);
            if kick {
                player.send(PlayerEvent::Kick(TextComponent::from(
                    "An item operation was refused; reconnect to reload your inventory",
                )));
            }
        }
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.item_violation(self, &violation);
        }
    }

    /// Whether a `transfer` at `pos` may start, sending to region `to`
    /// when another region takes part: never while either is quarantined
    /// (nothing taken from unsaved state reaches saved state, and nothing
    /// goes where it would not be saved), and an injected failure is a
    /// refused violation. Refused, nothing moves.
    pub(in crate::world::region) fn may_transfer(
        &mut self,
        transfer: Transfer,
        pos: BlockPos,
        to: Option<RegionPos>,
    ) -> bool {
        let conservation = &self.shared.conservation;
        if self.quarantined()
            || to.is_some_and(|to| conservation.quarantined(Subject::Region(self.dimension, to)))
        {
            return false;
        }
        if !conservation.take_injected(transfer) {
            return true;
        }
        self.item_violation(
            None,
            ItemViolation {
                operation: transfer.operation(),
                player: None,
                pos,
                item: None,
                detail: "the transfer check was made to fail".to_owned(),
            },
        );
        false
    }

    /// Whether a `transfer` at `pos` between this region and `player` may
    /// run. An injected failure is a violation quarantining both. `owed`
    /// says the items already left the player (a hand return): then a
    /// refusal because either side is quarantined quarantines both too,
    /// since the player's state no longer holds them and must not be
    /// saved. Otherwise such a refusal just moves nothing.
    pub(in crate::world::region) fn may_transfer_with(
        &mut self,
        transfer: Transfer,
        pos: BlockPos,
        player: &PlayerHandle,
        owed: bool,
    ) -> bool {
        let conservation = &self.shared.conservation;
        let blocked = self.quarantined() || conservation.quarantined(Subject::Player(player.id));
        let detail = if blocked {
            if !owed {
                return false;
            }
            "a side of the transfer is quarantined"
        } else if conservation.take_injected(transfer) {
            "the transfer check was made to fail"
        } else {
            return true;
        };
        self.item_violation(
            Some(player),
            ItemViolation {
                operation: transfer.operation(),
                player: Some(player.id),
                pos,
                item: None,
                detail: detail.to_owned(),
            },
        );
        false
    }

    /// Hands `entity`, with everything it carries, to the region behind
    /// `to` once [`Self::may_transfer`] allows it. Refused or undelivered,
    /// the entity comes back.
    pub(in crate::world::region) fn hand_entity(
        &mut self,
        to: &RegionHandle,
        entity: Box<dyn RegionEntity>,
        seq: u64,
    ) -> Result<(), Box<dyn RegionEntity>> {
        let pos = BlockPos::containing(entity.base().position);
        if !self.may_transfer(Transfer::EntityHandOff, pos, Some(to.pos)) {
            return Err(entity);
        }
        to.try_entity(entity, seq)
    }

    /// The release check of one side of a `transfer` at `pos`: the items
    /// held `before` plus those `received` equal those held `after` plus
    /// those `sent` on. A mismatch is a violation; the moves already ran,
    /// so the region is quarantined and the answer is false.
    pub(in crate::world::region) fn transfer_balanced(
        &mut self,
        transfer: Transfer,
        pos: BlockPos,
        before: u64,
        received: u64,
        after: u64,
        sent: u64,
    ) -> bool {
        if before + received == after + sent || !tuff_item::item::conservation::checks() {
            return true;
        }
        self.item_violation(
            None,
            ItemViolation {
                operation: transfer.operation(),
                player: None,
                pos,
                item: None,
                detail: format!(
                    "held {before} and received {received}, but holds {after} and sent {sent}"
                ),
            },
        );
        false
    }

    /// Items in the container at `pos`, for [`Self::transfer_balanced`].
    pub(in crate::world::region) fn held_at(&self, pos: BlockPos) -> u64 {
        self.block_entity(pos).map_or(0, |entity| {
            entity
                .items
                .iter()
                .flatten()
                .map(|stack| u64::from(stack.count()))
                .sum()
        })
    }
}

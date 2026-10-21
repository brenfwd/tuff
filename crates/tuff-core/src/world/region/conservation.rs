//! A region's side of item conservation: refused operations quarantine it
//! and the player involved, and a quarantined region neither saves nor
//! trades items with players until it unloads.

use super::RegionActor;
use crate::world::conservation::{ItemViolation, Subject};
use crate::world::messages::{PlayerEvent, PlayerHandle};
use tuff_item::text::TextComponent;

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
}

use super::*;

/// Input that may move items in or out of the inventory. While a
/// container transaction is pending its refusal restores the slots the
/// click wrote, so anything else that touches those slots must wait.
pub enum Deferred {
    Packet(Serverbound),
    Event(Box<PlayerEvent>),
}

pub(crate) fn defers_packet(packet: &Serverbound) -> bool {
    matches!(
        packet,
        Serverbound::Attack(_)
            | Serverbound::Interact { .. }
            | Serverbound::ChatCommand(_)
            | Serverbound::SetCreativeModeSlot { .. }
            | Serverbound::ClientCommand(_)
            | Serverbound::ContainerButtonClick { .. }
            | Serverbound::SelectTrade(_)
            | Serverbound::SelectBundleItem { .. }
            | Serverbound::PlaceRecipe { .. }
            | Serverbound::PlayerAction { .. }
            | Serverbound::Punch
            | Serverbound::SetCarriedItem(_)
            | Serverbound::UseItemOn { .. }
            | Serverbound::UseItem { .. }
            | Serverbound::PickItemFromBlock { .. }
            | Serverbound::PickItemFromEntity { .. }
            | Serverbound::EditBook { .. }
            | Serverbound::RenameItem(_)
    )
}

pub(crate) fn defers_event(event: &PlayerEvent) -> bool {
    matches!(
        event,
        PlayerEvent::ItemGranted { .. }
            | PlayerEvent::PluginInventory(_)
            | PlayerEvent::Directive { .. }
            | PlayerEvent::Run(_)
            | PlayerEvent::DispenserEquip { .. }
            | PlayerEvent::OfferItem { .. }
            | PlayerEvent::ShoulderGranted { .. }
            | PlayerEvent::ItemUsedOnBlock { .. }
            | PlayerEvent::EntityInteracted { .. }
            | PlayerEvent::HandReturned { .. }
            | PlayerEvent::Hurt { .. }
            | PlayerEvent::EntityHurt(_)
            | PlayerEvent::Attacked(_)
            | PlayerEvent::Exploded { .. }
    )
}

impl PlayerSession {
    /// A container or trade click is waiting for its region's answer.
    pub fn inventory_busy(&self) -> bool {
        self.menus
            .menu
            .as_ref()
            .is_some_and(|m| m.pending.is_some())
    }

    pub(crate) fn defer(&mut self, input: Deferred) {
        self.menus
            .deferred
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(input);
    }

    /// The next held-back input once no transaction is pending.
    pub fn next_deferred(&mut self) -> Option<Deferred> {
        if self.inventory_busy() {
            return None;
        }
        self.menus
            .deferred
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Runs held-back input, as the session loop does between messages.
    pub async fn replay_deferred(&mut self) {
        while let Some(next) = self.next_deferred() {
            match next {
                Deferred::Packet(packet) => self.on_packet(packet).await,
                Deferred::Event(event) => self.on_event(*event).await,
            }
        }
    }
}

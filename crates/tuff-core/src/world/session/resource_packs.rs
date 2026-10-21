use std::collections::HashMap;

use super::PlayerSession;
use crate::protocol::pack::{PackStatus, PlayPop, PlayPush, Push};
use crate::resource_pack::{PackEvent, PackOffer, PackState};
use tuff_item::text::TextComponent;

pub const REQUIRED_DISCONNECT: &str = "multiplayer.requiredTexturePrompt.disconnect";

/// Packs offered to this player in play, with whether each is required.
#[derive(Default)]
struct Offered(HashMap<u128, bool>);

/// What this client reported per pack, from both phases.
#[derive(Default)]
struct PackStates(HashMap<u128, PackState>);

impl PlayerSession {
    /// What the client last reported for pack `id`, if it was offered.
    pub fn pack_state(&self, id: u128) -> Option<&PackState> {
        self.ext.get::<PackStates>()?.0.get(&id)
    }

    /// Takes over the configuration-phase statuses at join.
    pub fn joined_pack_states(&mut self, states: HashMap<u128, PackState>) {
        self.ext.entry::<PackStates>().0.extend(states);
    }

    pub fn offer_resource_pack(&mut self, offer: PackOffer) {
        self.ext
            .entry::<Offered>()
            .0
            .insert(offer.id, offer.required);
        self.ext
            .entry::<PackStates>()
            .0
            .entry(offer.id)
            .or_default()
            .offered(&offer.sha1);
        self.send(&PlayPush(Push {
            id: offer.id,
            url: &offer.url,
            hash: &offer.sha1,
            required: offer.required,
            prompt: offer.prompt.as_ref(),
        }));
    }

    pub fn pop_resource_pack(&mut self, id: Option<u128>) {
        let offered = self.ext.entry::<Offered>();
        match id {
            Some(id) => {
                offered.0.remove(&id);
            }
            None => offered.0.clear(),
        }
        let states = self.ext.entry::<PackStates>();
        match id {
            Some(id) => {
                states.0.remove(&id);
            }
            None => states.0.clear(),
        }
        self.send(&PlayPop(id));
    }

    pub fn resource_pack_status(&mut self, id: u128, status: Option<PackStatus>) {
        let Some(status) = status else { return };
        let Some(required) = self
            .ext
            .get::<Offered>()
            .and_then(|o| o.0.get(&id).copied())
        else {
            return;
        };
        if let Some(state) = self.ext.entry::<PackStates>().0.get_mut(&id) {
            state.apply(status);
        }
        self.shared.resource_packs.report(PackEvent {
            player: self.profile.id,
            pack: id,
            status,
        });
        if required && status.is_failure() {
            self.kick(TextComponent::translate(REQUIRED_DISCONNECT, []));
        }
    }
}

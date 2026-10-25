use super::*;

/// How long an unanswered shoulder offer holds the entity back.
const OFFER_TICKS: u64 = 20;

impl RegionActor {
    /// Entity `entity` touches player `player` and asks to ride its
    /// shoulder: the player is asked unless an offer is outstanding.
    pub(super) fn offer_shoulder(&mut self, entity: i32, player: i32) {
        let tick = self.sched.tick;
        if self
            .ents
            .shoulder_offers
            .get(&entity)
            .is_some_and(|r| tick.saturating_sub(r.at) < OFFER_TICKS)
        {
            return;
        }
        let Some(handle) = self
            .ents
            .presence
            .values()
            .find(|p| p.handle.id == player && p.is_physical())
            .map(|p| p.handle.clone())
        else {
            return;
        };
        let offer = crate::world::entity::next_offer_id();
        self.ents.shoulder_offers.insert(
            entity,
            Reservation {
                player: handle.id,
                at: tick,
                offer,
            },
        );
        handle.send(PlayerEvent::OfferShoulder {
            entity,
            offer,
            region: self.handle.clone(),
        });
    }

    /// A player answered shoulder offer `offer`: on acceptance the entity
    /// leaves the world, saved, for the player's shoulder. The player is
    /// always told how it settled so it can free the shoulder it held.
    pub(in crate::world::region) fn shoulder_taken(
        &mut self,
        entity: i32,
        offer: u64,
        player: &PlayerHandle,
        accepted: bool,
    ) {
        let outstanding = self
            .ents
            .shoulder_offers
            .get(&entity)
            .is_some_and(|r| r.offer == offer && r.player == player.id);
        if outstanding {
            self.ents.shoulder_offers.remove(&entity);
        }
        let mut saved = None;
        if outstanding
            && accepted
            && let Some(tracked) = self.ents.entities.get(&entity)
            && tracked.entity.living().is_some_and(|l| l.alive)
        {
            let nbt = crate::world::entity::save(&*tracked.entity);
            if let Some(mut tracked) = self.remove_entity_kept(entity, &[]) {
                saved = Some(crate::world::stored_entity::StoredEntity::taken(
                    nbt,
                    tracked.entity.base_mut().claim.take(),
                ));
            }
        }
        self.ents
            .shoulder_offers
            .retain(|id, _| self.ents.entities.contains_key(id));
        player.send(PlayerEvent::ShoulderGranted {
            offer,
            entity: saved,
        });
    }
}

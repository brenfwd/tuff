use super::*;
use crate::world::messages::RegionInternal;
use crate::world::stored_entity::StoredEntity;
use tuff_nbt::{Compound, Tag};

/// Ticks after an entity lands on a shoulder before anything can make it
/// get off again.
const SETTLE_TICKS: u64 = 20;
/// Ticks a held shoulder waits for the region to settle an accepted offer.
const PENDING_TICKS: u64 = 40;
/// How far above the player's feet an entity reappears when it gets off.
const RESPAWN_HEIGHT: f64 = 0.699_999_988_079_071;
const PARROT_ID: &str = "minecraft:parrot";

/// The entities riding the player's shoulders, kept in their saved form
/// while they are out of the world.
#[derive(Debug, Default)]
pub struct Shoulders {
    /// Left, then right.
    pub entities: [Option<StoredEntity>; 2],
    /// The tick the last one landed.
    pub sat_at: u64,
    /// Accepted offers the region has not settled yet, with the tick each
    /// was accepted: each holds a free shoulder.
    pending: Vec<(u64, u64)>,
    /// A Riptide spin started: drop the riders on the next tick.
    pub spun: bool,
    /// The parrot variants last published, left then right.
    published: [Option<i32>; 2],
}

impl Drop for Shoulders {
    /// The session is gone: what rides the shoulders stays in the player's
    /// saved data (the storage door).
    fn drop(&mut self) {
        for rider in self.entities.iter_mut().filter_map(Option::take) {
            drop(rider.into_storage());
        }
    }
}

impl Shoulders {
    fn free(&self) -> usize {
        self.entities.iter().filter(|e| e.is_none()).count()
    }

    /// The parrot variant riding each shoulder, as entity data shows it.
    pub fn variants(&self) -> [Option<i32>; 2] {
        self.entities
            .each_ref()
            .map(|e| e.as_ref().and_then(|e| parrot_variant(e.nbt())))
    }
}

/// The variant of a saved parrot; other entities show nothing.
fn parrot_variant(nbt: &Compound) -> Option<i32> {
    if nbt.get_str("id") != Some(PARROT_ID) {
        return None;
    }
    Some(nbt.get_int("Variant").unwrap_or(0).clamp(0, 4))
}

impl PlayerSession {
    /// Whether a tamed parrot may try to land on this player now.
    pub fn perchable(&self) -> bool {
        self.mode.game_mode != GameMode::Spectator
            && !self.mode.abilities.flying
            && !self.body.fall.in_water()
            && !self.surroundings.contact.freezing.powder_snow
    }

    /// Whether an entity may land on a shoulder right now.
    fn shoulder_open(&self) -> bool {
        !self.is_dead()
            && self.ride.riding.is_none()
            && self.pose.on_ground
            && self.perchable()
            && self.body.shoulders.free() > self.body.shoulders.pending.len()
    }

    /// A parrot asked to land on a shoulder: hold one for it if there is
    /// room.
    pub(super) fn offer_shoulder(&mut self, entity: i32, offer: u64, region: RegionHandle) {
        let accepted = region.dimension == self.dimension && self.shoulder_open();
        if accepted {
            self.body.shoulders.pending.push((offer, self.tick));
        }
        region.internal(RegionInternal::ShoulderTaken {
            entity,
            offer,
            player: self.conn.handle.clone(),
            accepted,
        });
    }

    /// The region settled shoulder offer `offer`; a parrot that left the
    /// world takes the first free shoulder, or gets straight off again
    /// when both filled up meanwhile.
    pub(super) async fn shoulder_granted(&mut self, offer: u64, entity: Option<StoredEntity>) {
        self.body.shoulders.pending.retain(|(o, _)| *o != offer);
        let Some(entity) = entity else { return };
        let dead = self.is_dead();
        let shoulders = &mut self.body.shoulders;
        match shoulders.entities.iter_mut().find(|e| e.is_none()) {
            Some(slot) if !dead => {
                *slot = Some(entity);
                shoulders.sat_at = self.tick;
            }
            _ => self.respawn_shoulder_entity(entity).await,
        }
    }

    /// Plays the riders' ambient sounds and drops them when the player
    /// falls, swims, flies, sleeps or wades into powder snow.
    pub(super) async fn tick_shoulders(&mut self) {
        let tick = self.tick;
        self.body
            .shoulders
            .pending
            .retain(|(_, at)| tick.saturating_sub(*at) < PENDING_TICKS);
        for side in 0..2 {
            let chirps = self.body.shoulders.entities[side]
                .as_ref()
                .map(StoredEntity::nbt)
                .is_some_and(|e| {
                    e.get_str("id") == Some(PARROT_ID) && e.get_bool("Silent") != Some(true)
                });
            if chirps && self.rng.gen_range(0..200) == 0 {
                let pitch = (self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>()) * 0.2 + 1.0;
                let packet = packets([framed(&play::Sound {
                    sound_event: SoundEvent::EntityParrotAmbient.id(),
                    category: play::sound_category::PLAYERS,
                    position: self.pose.position,
                    volume: 1.0,
                    pitch,
                    seed: self.rng.r#gen(),
                })]);
                self.broadcast_chunk(packet).await;
            }
        }
        if std::mem::take(&mut self.body.shoulders.spun)
            || self.body.fall.distance > 0.5
            || self.body.fall.in_water()
            || self.mode.abilities.flying
            || self.is_sleeping()
            || self.surroundings.contact.freezing.powder_snow
        {
            self.drop_shoulder_entities().await;
        }
    }

    /// Puts whatever rides the shoulders back into the world, unless it
    /// only just landed.
    pub async fn drop_shoulder_entities(&mut self) {
        if self.body.shoulders.sat_at + SETTLE_TICKS >= self.tick {
            return;
        }
        let riders = std::mem::take(&mut self.body.shoulders.entities);
        for rider in riders.into_iter().flatten() {
            self.respawn_shoulder_entity(rider).await;
        }
    }

    async fn respawn_shoulder_entity(&mut self, mut stored: StoredEntity) {
        stored.nbt_mut().insert("Owner", Tag::uuid(self.profile.id));
        let nbt = stored.into_spawn_data();
        let [x, y, z] = self.pose.position;
        let position = [x, y + RESPAWN_HEIGHT, z];
        let region = ChunkPos::of_position(position).region();
        if let Some(handle) = self.region(region).await {
            let (reply, _) = tokio::sync::oneshot::channel();
            handle
                .request(RegionRequest::Summon {
                    nbt,
                    position,
                    fresh: false,
                    reply,
                })
                .await;
        }
    }

    /// Entity data for shoulder riders that changed since last published.
    pub(super) fn shoulder_metadata(&mut self) -> Option<Vec<(u8, MetadataValue)>> {
        let variants = self.body.shoulders.variants();
        if variants == self.body.shoulders.published {
            return None;
        }
        self.body.shoulders.published = variants;
        Some(shoulder_values(variants))
    }

    /// Entity data for the shoulder riders a new viewer must be told of.
    pub(super) fn shoulder_spawn_metadata(&self) -> Vec<(u8, MetadataValue)> {
        let variants = self.body.shoulders.variants();
        if variants == [None, None] {
            return Vec::new();
        }
        shoulder_values(variants)
    }
}

fn shoulder_values(variants: [Option<i32>; 2]) -> Vec<(u8, MetadataValue)> {
    vec![
        (
            metadata::SHOULDER_LEFT,
            MetadataValue::OptionalVarInt(variants[0]),
        ),
        (
            metadata::SHOULDER_RIGHT,
            MetadataValue::OptionalVarInt(variants[1]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn only_parrots_show_a_variant() {
        let parrot = Compound::new().with("id", PARROT_ID).with("Variant", 3);
        assert_eq!(parrot_variant(&parrot), Some(3));
        let other = Compound::new()
            .with("id", "minecraft:cat")
            .with("Variant", 3);
        assert_eq!(parrot_variant(&other), None);
        let mut shoulders = Shoulders::default();
        shoulders.entities[1] = Some(StoredEntity::from_storage(parrot));
        assert_eq!(shoulders.variants(), [None, Some(3)]);
    }
}

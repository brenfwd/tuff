//! Debug feeds: the per-player `debug_subscription` sets the coordinator
//! publishes after the permission gate, read by the actors that produce
//! each feed.

use std::sync::Arc;

use tuff_vanilla_data::registry::DebugSubscription;

use crate::world::messages::PlayerId;

/// The bit of `feed` in a subscription mask.
pub const fn bit(feed: DebugSubscription) -> u32 {
    1 << feed as u32
}

/// The mask of the `debug_subscription` ids a client asked for; unknown
/// ids are ignored.
pub fn mask(ids: &[i32]) -> u32 {
    ids.iter()
        .filter_map(|&id| u32::try_from(id).ok())
        .filter(|&id| (id as usize) < DebugSubscription::COUNT)
        .fold(0, |mask, id| mask | 1 << id)
}

/// The players allowed and subscribed to world feeds, with their masks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DebugSubscribers {
    players: Vec<(PlayerId, u32)>,
    any: u32,
}

impl DebugSubscribers {
    /// `players` sorted by id, with empty masks left out.
    pub fn new(mut players: Vec<(PlayerId, u32)>) -> Self {
        players.retain(|(_, mask)| *mask != 0);
        players.sort_unstable();
        let any = players.iter().fold(0, |any, (_, mask)| any | mask);
        Self { players, any }
    }

    /// Whether anyone is subscribed to `feed`.
    pub fn any(&self, feed: DebugSubscription) -> bool {
        self.any & bit(feed) != 0
    }

    /// Whether anyone is subscribed to any feed in `mask`.
    pub fn any_of(&self, mask: u32) -> bool {
        self.any & mask != 0
    }

    pub fn wants(&self, player: PlayerId, feed: DebugSubscription) -> bool {
        self.players
            .binary_search_by_key(&player, |(id, _)| *id)
            .is_ok_and(|i| self.players[i].1 & bit(feed) != 0)
    }
}

pub type SharedSubscribers = Arc<DebugSubscribers>;

/// Feeds whose values belong to entities.
pub const ENTITY_FEEDS: [DebugSubscription; 6] = [
    DebugSubscription::Bees,
    DebugSubscription::Brains,
    DebugSubscription::Breezes,
    DebugSubscription::GoalSelectors,
    DebugSubscription::EntityPaths,
    DebugSubscription::GameEventListeners,
];

const FIRST_PARTS: [&str; 33] = [
    "Slim", "Far", "River", "Silly", "Fat", "Thin", "Fish", "Bat", "Dark", "Oak", "Sly", "Bush",
    "Zen", "Bark", "Cry", "Slack", "Soup", "Grim", "Hook", "Dirt", "Mud", "Sad", "Hard", "Crook",
    "Sneak", "Stink", "Weird", "Fire", "Soot", "Soft", "Rough", "Cling", "Scar",
];
const SECOND_PARTS: [&str; 30] = [
    "Fox", "Tail", "Jaw", "Whisper", "Twig", "Root", "Finder", "Nose", "Brow", "Blade", "Fry",
    "Seek", "Wart", "Tooth", "Foot", "Leaf", "Stone", "Fall", "Face", "Tongue", "Voice", "Lip",
    "Mouth", "Snail", "Toe", "Ear", "Hair", "Beard", "Shirt", "Fist",
];

/// The made-up name debug feeds give an unnamed entity, fixed by its UUID.
pub fn uuid_name(uuid: u128) -> String {
    use tuff_item::random::{LegacyRandom, RandomSource};
    let bits = (uuid >> 64) as u64 ^ uuid as u64;
    let hash = ((bits >> 32) as i32) ^ (bits as i32);
    let mut random = LegacyRandom::new(i64::from(hash >> 2));
    let first = FIRST_PARTS[random.next_int_bounded(FIRST_PARTS.len() as i32) as usize];
    let second = SECOND_PARTS[random.next_int_bounded(SECOND_PARTS.len() as i32) as usize];
    format!("{first}{second}")
}

/// The name debug feeds show for an entity: its custom name, else
/// [`uuid_name`].
pub fn entity_name(base: &crate::world::entity::EntityBase) -> String {
    base.custom_name
        .as_ref()
        .and_then(tuff_item::text::TextComponent::from_nbt)
        .map(|name| name.plain())
        .unwrap_or_else(|| uuid_name(base.uuid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn mask_keeps_known_ids_and_subscribers_answer_per_player() {
        let m = mask(&[8, 14, -1, 99]);
        assert_eq!(
            m,
            bit(DebugSubscription::Pois) | bit(DebugSubscription::NeighborUpdates)
        );
        let subs = DebugSubscribers::new(vec![
            (7, m),
            (3, 0),
            (2, bit(DebugSubscription::GameEvents)),
        ]);
        assert!(subs.any(DebugSubscription::Pois));
        assert!(!subs.any(DebugSubscription::Brains));
        assert!(subs.wants(7, DebugSubscription::NeighborUpdates));
        assert!(!subs.wants(2, DebugSubscription::Pois));
        assert!(!subs.wants(3, DebugSubscription::Pois));
    }

    #[tuff_core::watchdog]
    #[test]
    fn uuid_names_join_two_parts_picked_by_the_uuid_hash() {
        let name = uuid_name(0);
        assert!(FIRST_PARTS.iter().any(|p| name.starts_with(p)), "{name}");
        assert!(SECOND_PARTS.iter().any(|p| name.ends_with(p)), "{name}");
        assert_eq!(
            uuid_name(0x1234_5678_9abc_def0),
            uuid_name(0x1234_5678_9abc_def0)
        );
    }
}

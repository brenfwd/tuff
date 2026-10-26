//! Where the players are, for regions that need to see players who stand
//! outside them: mobs noticing, chasing and shooting players across a
//! region border, despawning and natural spawning near players.
//!
//! The coordinator already hears each player's state once a tick at most;
//! it publishes this read-only index on a `watch` whenever some state
//! changed. Each region reads only the players within [`REACH`] blocks of
//! its own square, once a tick, so players register their *presence* (for
//! touching, pickups and relaying) only with regions within a few blocks.

use crate::world::PlayerId;
use std::collections::HashMap;
use std::sync::Arc;

use crate::world::dimension::DimensionId;
use crate::world::messages::{PlayerHandle, PlayerLooks};
use tuff_world_model::pos::{RegionPos, regions_near};

/// How far outside its own square a region sees players: the monster
/// despawn distance, which is also the natural spawning distance.
pub const REACH: f64 = 128.0;

/// A living, non-spectating player as the index lists it.
#[derive(Debug, Clone)]
pub struct IndexedPlayer {
    pub handle: PlayerHandle,
    pub uuid: u128,
    pub name: Arc<str>,
    pub position: [f64; 3],
    pub looks: PlayerLooks,
    /// The `minecraft:time_since_rest` statistic, for phantom spawning.
    pub time_since_rest: i32,
}

#[derive(Debug, Default)]
pub struct PlayerIndex {
    near: HashMap<(DimensionId, RegionPos), Vec<IndexedPlayer>>,
    per_dimension: HashMap<DimensionId, usize>,
    spectators: HashMap<(DimensionId, RegionPos), Vec<[f64; 3]>>,
    spectator_positions: HashMap<PlayerId, (DimensionId, [f64; 3])>,
}

impl PlayerIndex {
    /// Indexes each player under every region within [`REACH`] of it.
    pub fn build(players: impl IntoIterator<Item = (DimensionId, IndexedPlayer)>) -> Self {
        let mut index = Self::default();
        for (dimension, player) in players {
            *index.per_dimension.entry(dimension).or_default() += 1;
            for region in regions_near(player.position, REACH) {
                index
                    .near
                    .entry((dimension, region))
                    .or_default()
                    .push(player.clone());
            }
        }
        for players in index.near.values_mut() {
            players.sort_unstable_by_key(|p| p.handle.id);
        }
        index
    }

    /// The living, non-spectating player with `uuid`, in any dimension.
    pub fn find_by_uuid(&self, uuid: u128) -> Option<(DimensionId, &IndexedPlayer)> {
        self.near.iter().find_map(|((dimension, _), players)| {
            players
                .iter()
                .find(|p| p.uuid == uuid)
                .map(|p| (*dimension, p))
        })
    }

    /// Also indexes living spectators by position. They keep chunks
    /// simulating but are otherwise invisible to mobs.
    pub fn with_spectators(
        mut self,
        spectators: impl IntoIterator<Item = (DimensionId, [f64; 3])>,
    ) -> Self {
        for (dimension, position) in spectators {
            for region in regions_near(position, REACH) {
                self.spectators
                    .entry((dimension, region))
                    .or_default()
                    .push(position);
            }
        }
        self
    }

    /// Also records where living spectators are, by id, wherever they are.
    pub fn with_spectator_positions(
        mut self,
        spectators: impl IntoIterator<Item = (PlayerId, DimensionId, [f64; 3])>,
    ) -> Self {
        for (id, dimension, position) in spectators {
            self.spectator_positions.insert(id, (dimension, position));
        }
        self
    }

    /// Where the living spectator `id` is, if it is in `dimension`.
    pub fn spectator_position(&self, dimension: DimensionId, id: PlayerId) -> Option<[f64; 3]> {
        self.spectator_positions
            .get(&id)
            .filter(|(at, _)| *at == dimension)
            .map(|(_, position)| *position)
    }

    /// Spectator positions within [`REACH`] of `region`.
    pub fn spectators_near(&self, dimension: DimensionId, region: RegionPos) -> &[[f64; 3]] {
        self.spectators
            .get(&(dimension, region))
            .map_or(&[], Vec::as_slice)
    }

    /// The players within [`REACH`] of `region`, by id.
    pub fn near(&self, dimension: DimensionId, region: RegionPos) -> &[IndexedPlayer] {
        self.near
            .get(&(dimension, region))
            .map_or(&[], Vec::as_slice)
    }

    /// How many living players are in `dimension`, wherever they are.
    pub fn in_dimension(&self, dimension: DimensionId) -> usize {
        self.per_dimension.get(&dimension).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_world_model::pos::ChunkPos;

    fn player(id: i32, position: [f64; 3]) -> (DimensionId, IndexedPlayer) {
        (
            DimensionId::for_tests(0),
            IndexedPlayer {
                handle: PlayerHandle::new(id).0,
                uuid: id as u128,
                name: Arc::from("p"),
                position,
                looks: PlayerLooks::default(),
                time_since_rest: 0,
            },
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_are_listed_for_every_region_within_reach() {
        let size = f64::from(tuff_world_model::pos::REGION_CHUNKS * 16);
        let index = PlayerIndex::build([
            player(1, [-100.0, 64.0, 200.0]),
            player(2, [size - 10.0, 64.0, 10.0]),
        ]);
        let home = ChunkPos::new(0, 0).region();
        let east = ChunkPos::new(tuff_world_model::pos::REGION_CHUNKS, 0).region();
        let north = ChunkPos::new(0, -1).region();
        let ids = |region| -> Vec<i32> {
            index
                .near(DimensionId::for_tests(0), region)
                .iter()
                .map(|p| p.handle.id)
                .collect()
        };
        assert_eq!(ids(home), [1, 2]);
        assert_eq!(ids(east), [2], "128 blocks reach across the border");
        assert_eq!(ids(north), [2]);
        assert!(index.near(DimensionId::for_tests(1), home).is_empty());
        assert_eq!(index.in_dimension(DimensionId::for_tests(0)), 2);
    }
}

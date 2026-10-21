//! Boss health bars a region entity publishes through
//! [`crate::world::entity::RegionEntity::boss_bar`], shown to nearby
//! players. This is deliberately a small, generic add-on rather than a
//! wither-specific mechanism, so any boss-tier mob (or a future feature's)
//! gets one for free just by implementing the trait method: no changes to
//! the chunk/entity tracking system, only this per-region broadcaster and
//! two call sites in `entities.rs`.
//!
//! The ender dragon has its own, older boss bar (`dragon/fight.rs`): it is
//! not a region-owned `RegionEntity` at all (a dedicated fight actor with
//! its own arena and player membership), so it predates and does not use
//! this mechanism. Retrofitting it is future work, not required for this
//! to be useful to the next boss mob.

use std::collections::HashSet;

use super::RegionActor;
use crate::protocol::play::{BossAction, BossEvent};
use crate::world::entity::BossBarState;
use crate::world::messages::{PlayerEvent, PlayerId};
use crate::world::{framed, packets};

/// Players within this many blocks of a boss entity are sent its bar. Not
/// a dumped value (boss bar visibility range is UX polish, not on the
/// wire in a way that ties it to a specific number); chosen to comfortably
/// cover a boss's own follow range.
const RANGE: f64 = 96.0;

#[derive(Default)]
pub(super) struct Shown {
    uuid: u128,
    viewers: HashSet<PlayerId>,
    progress: f32,
}

impl RegionActor {
    /// Drops any shown boss bar whose entity this region no longer has
    /// (it died, or was handed to a neighbour): entities are ticked and
    /// removed from `self.ents.entities` by many different paths, so this is
    /// swept once per tick instead of hooking each one.
    pub(super) fn sweep_boss_bars(&mut self) {
        let stale: Vec<i32> = self
            .ents
            .boss_bars
            .keys()
            .copied()
            .filter(|id| !self.ents.entities.contains_key(id))
            .collect();
        for id in stale {
            if let Some(entry) = self.ents.boss_bars.remove(&id) {
                self.send_boss_bar_removal(entry.uuid, &entry.viewers);
            }
        }
    }

    fn send_boss_bar_removal(&mut self, uuid: u128, viewers: &HashSet<PlayerId>) {
        if viewers.is_empty() {
            return;
        }
        let seen = self.seen_players();
        let remove = packets([framed(&BossEvent {
            uuid,
            action: BossAction::Remove,
        })]);
        for viewer in viewers {
            if let Some(player) = seen.iter().find(|p| p.handle.id == *viewer) {
                player.handle.send(PlayerEvent::Packets(remove.clone()));
            }
        }
    }

    /// Called once per tick for every entity still owned by this region,
    /// with its current position and what it wants shown (`None` shows
    /// nothing, removing a previously shown bar).
    pub(super) fn update_boss_bar(
        &mut self,
        id: i32,
        uuid: u128,
        position: [f64; 3],
        state: Option<BossBarState>,
    ) {
        let Some(state) = state else {
            if let Some(entry) = self.ents.boss_bars.remove(&id) {
                self.send_boss_bar_removal(entry.uuid, &entry.viewers);
            }
            return;
        };
        let seen = self.seen_players();
        let nearby: HashSet<PlayerId> = seen
            .iter()
            .filter(|p| crate::world::distance_sq(p.position, position) <= RANGE * RANGE)
            .map(|p| p.handle.id)
            .collect();
        let entry = self.ents.boss_bars.entry(id).or_insert_with(|| Shown {
            uuid,
            viewers: HashSet::new(),
            progress: -1.0,
        });
        let add = || {
            packets([framed(&BossEvent {
                uuid,
                action: BossAction::Add {
                    title: &state.name,
                    progress: state.progress,
                    color: state.color,
                    overlay: state.overlay,
                    flags: state.flags,
                },
            })])
        };
        for player in seen
            .iter()
            .filter(|p| nearby.contains(&p.handle.id) && !entry.viewers.contains(&p.handle.id))
        {
            player.handle.send(PlayerEvent::Packets(add()));
        }
        if entry.progress != state.progress {
            let update = packets([framed(&BossEvent {
                uuid,
                action: BossAction::Progress(state.progress),
            })]);
            for player in seen
                .iter()
                .filter(|p| nearby.contains(&p.handle.id) && entry.viewers.contains(&p.handle.id))
            {
                player.handle.send(PlayerEvent::Packets(update.clone()));
            }
        }
        let leaving: Vec<PlayerId> = entry
            .viewers
            .iter()
            .copied()
            .filter(|v| !nearby.contains(v))
            .collect();
        if !leaving.is_empty() {
            let remove = packets([framed(&BossEvent {
                uuid,
                action: BossAction::Remove,
            })]);
            for viewer in leaving {
                if let Some(player) = seen.iter().find(|p| p.handle.id == viewer) {
                    player.handle.send(PlayerEvent::Packets(remove.clone()));
                }
            }
        }
        entry.viewers = nearby;
        entry.progress = state.progress;
    }
}

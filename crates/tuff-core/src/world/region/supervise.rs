//! Recovery of a region whose actor loop panicked. The panicked instance's
//! state cannot be trusted, so only its channels and a few plain facts
//! (neighbours, forced chunks) carry over to a fresh instance that reloads
//! everything from storage. The handle and its channels stay the same, so
//! the manager's entry, its tickets and every holder of the handle keep
//! working; messages queued at the time of the panic reach the new
//! instance. Changes since the last save are lost. Players the dead
//! instance served (present, viewing chunks, using a container or sign)
//! are disconnected, since their subscriptions are gone; rejoining
//! resubscribes them to the new instance.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::RegionActor;
use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::messages::{
    PlayerEvent, PlayerHandle, PlayerId, RegionHandle, RegionInternal, RegionRequest,
};
use tuff_item::text::TextComponent;
use tuff_supervise::{CatchUnwind, panic_message};
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// Server ticks an instance must run for its panics to stop counting as
/// consecutive.
const STABLE_TICKS: u64 = 1200;

/// Server ticks to wait before restarting after the given number of
/// consecutive panics: none for the first few, then doubling up to about
/// five minutes, so a region that panics on load cannot spin.
fn restart_delay(panics: u32) -> u64 {
    match panics {
        0..=3 => 0,
        n => (20u64 << (n - 4).min(8)).min(6000),
    }
}

/// What a panicked instance leaves its successor.
struct Remains {
    dimension: DimensionId,
    pos: RegionPos,
    shared: Arc<Shared>,
    handle: RegionHandle,
    requests: mpsc::Receiver<RegionRequest>,
    internal: mpsc::UnboundedReceiver<RegionInternal>,
    requests_open: bool,
    internal_open: bool,
    neighbors: HashMap<RegionPos, RegionHandle>,
    forced: HashSet<ChunkPos>,
}

impl RegionActor {
    /// Runs the actor loop until it closes, replacing the instance with a
    /// fresh one whenever the loop panics.
    pub(super) async fn supervise(mut self) {
        let mut panics = 0u32;
        loop {
            let started = self.sched.server_tick;
            let Err(panic) = CatchUnwind::new(Box::pin(self.run())).await else {
                return;
            };
            if self.sched.server_tick.saturating_sub(started) >= STABLE_TICKS {
                panics = 0;
            }
            panics += 1;
            let Some(mut remains) = self.salvage(panic_message(&*panic)) else {
                return;
            };
            let delay = restart_delay(panics);
            if delay > 0 && !remains.wait_out(delay).await {
                return;
            }
            self = remains.restart();
        }
    }

    /// Reports the panic, disconnects the players this instance served and
    /// keeps what its successor needs; `None` if the region was closing.
    fn salvage(mut self, message: &str) -> Option<Remains> {
        self.shared.metrics.core().region_panics.inc();
        let name = self.dimension().key.clone();
        let (x, z) = (self.pos.x, self.pos.z);
        if let Some(done) = self.sched.closing.take() {
            log::error!(
                "region {name} ({x}, {z}) panicked while closing: {message}; \
                 changes since its last save are lost"
            );
            let _ = done.send(true);
            return None;
        }
        let players = self.served_players();
        log::error!(
            "region {name} ({x}, {z}) panicked: {message}; changes since its last save are \
             lost; restarting it from storage and disconnecting {} player(s)",
            players.len()
        );
        let reason = TextComponent::translate(
            "disconnect.genericReason",
            [TextComponent::new("The region you were in crashed")],
        );
        for player in players.values() {
            player.send(PlayerEvent::Kick(reason.clone()));
        }
        let RegionActor {
            dimension,
            pos,
            shared,
            handle,
            requests,
            internal,
            sched,
            border,
            chunk_io,
            ..
        } = self;
        Some(Remains {
            dimension,
            pos,
            shared,
            handle,
            requests,
            internal,
            requests_open: sched.requests_open,
            internal_open: sched.internal_open,
            neighbors: border.neighbors,
            forced: chunk_io.forced,
        })
    }

    /// Every player whose state this instance held.
    fn served_players(&self) -> HashMap<PlayerId, PlayerHandle> {
        let mut players = HashMap::new();
        let mut add = |handle: &PlayerHandle| {
            players.entry(handle.id).or_insert_with(|| handle.clone());
        };
        self.ents.presence.values().for_each(|p| add(&p.handle));
        self.columns
            .values()
            .flat_map(|c| c.viewers.values())
            .for_each(&mut add);
        self.chunk_io
            .loading
            .values()
            .flat_map(|l| l.viewers.values())
            .for_each(&mut add);
        self.menus.viewers.values().flatten().for_each(&mut add);
        self.menus
            .entity_viewers
            .values()
            .flatten()
            .for_each(&mut add);
        self.menus.sign_editors.values().for_each(&mut add);
        players
    }
}

impl Remains {
    /// Waits `ticks` server ticks before a restart, dropping what arrives
    /// meanwhile so no caller waits on an answer. False if the manager
    /// shut the region down in the meantime.
    async fn wait_out(&mut self, ticks: u64) -> bool {
        let mut clock = self.shared.server_ticks.clone();
        let until = clock.borrow_and_update().saturating_add(ticks);
        log::warn!(
            "region ({}, {}) keeps panicking; restarting it in {ticks} server ticks",
            self.pos.x,
            self.pos.z
        );
        loop {
            tokio::select! {
                message = self.internal.recv(), if self.internal_open => match message {
                    Some(RegionInternal::Shutdown { done }) => {
                        let _ = done.send(true);
                        return false;
                    }
                    Some(_) => {}
                    None => self.internal_open = false,
                },
                request = self.requests.recv(), if self.requests_open => {
                    if request.is_none() {
                        self.requests_open = false;
                    }
                }
                changed = clock.changed() => {
                    if changed.is_err() || *clock.borrow_and_update() >= until {
                        return true;
                    }
                }
            }
        }
    }

    /// A fresh instance on the same channels. It takes the forced chunks
    /// back, and its neighbours drop what they mirrored of the old one and
    /// send their border chunks again.
    fn restart(self) -> RegionActor {
        let mut actor = RegionActor::new(
            self.dimension,
            self.pos,
            self.shared,
            self.handle,
            self.requests,
            self.internal,
            self.neighbors,
        );
        actor.sched.requests_open = self.requests_open;
        actor.sched.internal_open = self.internal_open;
        if !self.forced.is_empty() {
            actor.handle.internal(RegionInternal::ForceChunks {
                chunks: self.forced.into_iter().collect(),
                on: true,
            });
        }
        for neighbor in actor.border.neighbors.values() {
            neighbor.internal(RegionInternal::NeighborDown(self.pos));
            neighbor.internal(RegionInternal::NeighborUp(actor.handle.clone()));
        }
        actor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn restarts_back_off_after_a_few_panics_and_cap() {
        assert_eq!(restart_delay(1), 0);
        assert_eq!(restart_delay(3), 0);
        assert_eq!(restart_delay(4), 20);
        assert_eq!(restart_delay(5), 40);
        assert_eq!(restart_delay(12), 5120);
        assert_eq!(restart_delay(40), 5120);
    }
}

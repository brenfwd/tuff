//! Lockstep ticking. The clock is a `watch`, so an actor still busy with
//! one tick sees only the newest value once it is free and skips every tick
//! in between. While the barrier is active the coordinator does not advance
//! the clock until every member has finished the current tick, so each
//! member runs every tick exactly once.
//!
//! Members are the actors that simulate the game on each tick (regions,
//! player sessions, dragon fights). A member reports each finished tick
//! through its [`TickMember`] and leaves by dropping it, so a closing or
//! panicking actor never stalls the clock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// When the coordinator waits for every member before the next tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TickLockstep {
    /// Only during `/tick sprint`; regular play ticks on schedule and a
    /// member that falls behind skips ticks.
    Sprint,
    /// Every tick: an overloaded server slows down instead of skipping.
    #[default]
    Always,
}

impl TickLockstep {
    pub const ALL: [Self; 2] = [Self::Sprint, Self::Always];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sprint => "sprint",
            Self::Always => "always",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

const STALL_WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default)]
struct Members {
    next_id: u64,
    /// Each member's last finished tick.
    finished: HashMap<u64, u64>,
}

pub struct TickBarrier {
    lockstep: TickLockstep,
    members: Mutex<Members>,
    progress: Notify,
}

impl TickBarrier {
    pub fn new(lockstep: TickLockstep) -> Arc<Self> {
        Arc::new(Self {
            lockstep,
            members: Mutex::default(),
            progress: Notify::new(),
        })
    }

    pub fn lockstep(&self) -> TickLockstep {
        self.lockstep
    }

    /// Joins as a member that counts as having finished `tick` already,
    /// since it was not around to run it.
    pub fn join(self: &Arc<Self>, tick: u64) -> TickMember {
        let mut members = self.lock();
        let id = members.next_id;
        members.next_id += 1;
        members.finished.insert(id, tick);
        TickMember {
            barrier: self.clone(),
            id,
        }
    }

    /// Whether every member has finished `tick`.
    pub fn all_finished(&self, tick: u64) -> bool {
        self.lock().finished.values().all(|&done| done >= tick)
    }

    /// Resolves after some member finished a tick or left since the last
    /// call; callers recheck [`Self::all_finished`].
    pub async fn progressed(&self) {
        self.progress.notified().await;
    }

    /// Members that have not finished `tick`, as `(id, last finished tick)`
    /// sorted by id.
    pub fn laggards(&self, tick: u64) -> Vec<(u64, u64)> {
        let mut behind: Vec<_> = self
            .lock()
            .finished
            .iter()
            .filter(|&(_, &done)| done < tick)
            .map(|(&id, &done)| (id, done))
            .collect();
        behind.sort_unstable();
        behind
    }

    /// Like [`Self::progressed`], but warns naming the members behind
    /// `tick` whenever `STALL_WARN_AFTER` passes without progress.
    pub async fn progressed_awaiting(&self, tick: u64) {
        self.progressed_awaiting_for(tick, STALL_WARN_AFTER).await;
    }

    async fn progressed_awaiting_for(&self, tick: u64, warn_after: std::time::Duration) {
        if tokio::time::timeout(warn_after, self.progressed())
            .await
            .is_err()
        {
            let behind = self.laggards(tick);
            if !behind.is_empty() {
                log::warn!(
                    "tick barrier stalled waiting for tick {tick}; members behind (id, last finished tick): {behind:?}"
                );
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Members> {
        self.members
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// One actor's place in the [`TickBarrier`]; dropping it leaves.
pub struct TickMember {
    barrier: Arc<TickBarrier>,
    id: u64,
}

impl TickMember {
    pub fn finished(&self, tick: u64) {
        if let Some(done) = self.barrier.lock().finished.get_mut(&self.id) {
            *done = (*done).max(tick);
        }
        self.barrier.progress.notify_one();
    }
}

impl Drop for TickMember {
    fn drop(&mut self) {
        self.barrier.lock().finished.remove(&self.id);
        self.barrier.progress.notify_one();
    }
}

impl std::fmt::Debug for TickMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickMember").field("id", &self.id).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn waits_for_every_member_and_forgets_leavers() {
        let barrier = TickBarrier::new(TickLockstep::Sprint);
        assert!(barrier.all_finished(5));
        let a = barrier.join(4);
        let b = barrier.join(4);
        assert!(barrier.all_finished(4));
        assert!(!barrier.all_finished(5));
        a.finished(5);
        assert!(!barrier.all_finished(5));
        b.finished(5);
        assert!(barrier.all_finished(5));
        a.finished(6);
        assert!(!barrier.all_finished(6));
        drop(b);
        assert!(barrier.all_finished(6));
    }

    #[tuff_core::watchdog]
    #[test]
    fn laggards_name_members_behind_the_tick() {
        let barrier = TickBarrier::new(TickLockstep::Sprint);
        let a = barrier.join(7);
        let b = barrier.join(7);
        a.finished(8);
        assert_eq!(barrier.laggards(8), vec![(b.id, 7)]);
        b.finished(8);
        assert!(barrier.laggards(8).is_empty());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn stalled_wait_returns_after_the_warn_interval() {
        let barrier = TickBarrier::new(TickLockstep::Sprint);
        let _held = barrier.join(1);
        barrier
            .progressed_awaiting_for(2, std::time::Duration::from_millis(20))
            .await;
        assert!(!barrier.all_finished(2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn lockstep_names_round_trip() {
        for mode in TickLockstep::ALL {
            assert_eq!(TickLockstep::from_name(mode.name()), Some(mode));
        }
        assert_eq!(TickLockstep::from_name("never"), None);
    }
}

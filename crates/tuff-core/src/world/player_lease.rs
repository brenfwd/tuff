//! One owner per player: a move-only lease on a profile's stored data.
//!
//! A session, or an offline writer, holds the lease of a profile to load
//! and to save its data. The lease changes hands in request order and
//! only when the holder returns it, so two owners never overlap. A holder
//! that ends without returning it (a panic) hands on a dirty lease: the
//! next owner loads whatever the last completed save left.
//!
//! The lease is touched at login, at saves and by offline edits only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

static DIRTY_RETURNS: AtomicU64 = AtomicU64::new(0);

/// How many leases were handed on without being returned, since start.
pub fn dirty_returns() -> u64 {
    DIRTY_RETURNS.load(Ordering::Relaxed)
}

#[derive(Default)]
struct GateState {
    generation: u64,
    written: u64,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    sequence: AtomicU64,
}

impl Gate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The right to read and write a profile's data, owned by one holder.
#[must_use = "a lease must be returned with release()"]
pub struct PlayerLease {
    uuid: u128,
    gate: Arc<Gate>,
    generation: u64,
    dirty: bool,
    next: Option<oneshot::Sender<PlayerLease>>,
}

impl PlayerLease {
    fn new(uuid: u128) -> Self {
        Self {
            uuid,
            gate: Arc::default(),
            generation: 0,
            dirty: false,
            next: None,
        }
    }

    /// A lease outside any table, for harness sessions that load and save
    /// without a coordinator; dropping it is silent.
    pub(crate) fn detached(uuid: u128) -> Self {
        let mut lease = Self::new(uuid);
        lease.generation = 1;
        lease.gate.lock().generation = 1;
        lease
    }

    fn arm(&mut self, next: oneshot::Sender<PlayerLease>) {
        self.generation += 1;
        self.gate.lock().generation = self.generation;
        self.next = Some(next);
    }

    /// Whether the previous holder ended without returning it.
    pub fn was_dirty(&self) -> bool {
        self.dirty
    }

    /// A handle to run storage reads and writes of this holder on any
    /// thread. Each call numbers one write, so an older snapshot landing
    /// after a newer one is dropped instead of rolling the data back.
    pub fn access(&self) -> LeaseAccess {
        LeaseAccess {
            gate: self.gate.clone(),
            generation: self.generation,
            sequence: self.gate.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }

    /// Returns the lease, leaving the profile's data as its holder saved it.
    pub fn release(mut self) {
        self.hand_on(false);
    }

    fn hand_on(&mut self, dirty: bool) {
        let Some(next) = self.next.take() else {
            return;
        };
        if dirty {
            DIRTY_RETURNS.fetch_add(1, Ordering::Relaxed);
            log::error!(
                "the lease of player {:032x} was dropped without being returned; \
                 the next session loads the last completed save",
                self.uuid
            );
        }
        let lease = PlayerLease {
            uuid: self.uuid,
            gate: self.gate.clone(),
            generation: self.generation,
            dirty,
            next: None,
        };
        let _ = next.send(lease);
        if dirty
            && cfg!(debug_assertions)
            && !std::thread::panicking()
            && !tuff_item::item::conservation::is_releasing()
        {
            panic!("player lease {:032x} dropped without release()", self.uuid);
        }
    }
}

impl Drop for PlayerLease {
    fn drop(&mut self) {
        self.hand_on(true);
    }
}

/// Serializes the reads and writes of one holder against the profile's
/// other holders and against its own older writes.
#[derive(Clone)]
pub struct LeaseAccess {
    gate: Arc<Gate>,
    generation: u64,
    sequence: u64,
}

impl LeaseAccess {
    /// Runs a read once every write already started has landed.
    pub fn read<R>(&self, f: impl FnOnce() -> R) -> R {
        let _gate = self.gate.lock();
        f()
    }

    /// Runs a write unless a later holder or a newer write of this holder
    /// owns the data now. Returns whether it ran.
    pub fn write(&self, f: impl FnOnce()) -> bool {
        let mut state = self.gate.lock();
        if state.generation != self.generation || state.written > self.sequence {
            return false;
        }
        state.written = self.sequence;
        f();
        true
    }
}

enum Tail {
    Free(PlayerLease),
    Pending(oneshot::Receiver<PlayerLease>),
}

/// The coordinator's table of leases, one per profile ever seen.
#[derive(Default)]
pub struct LeaseTable {
    tails: HashMap<u128, Tail>,
}

enum Source {
    Ready(PlayerLease),
    Waiting {
        previous: oneshot::Receiver<PlayerLease>,
        next: oneshot::Sender<PlayerLease>,
        uuid: u128,
    },
}

/// A place in a profile's queue; await [`LeaseRequest::granted`] for the
/// lease.
pub struct LeaseRequest(Source);

impl LeaseTable {
    /// Queues for the profile's lease. Requests are granted in call order.
    pub fn request(&mut self, uuid: u128) -> LeaseRequest {
        let (next, pending) = oneshot::channel();
        let tail = self
            .tails
            .insert(uuid, Tail::Pending(pending))
            .unwrap_or_else(|| Tail::Free(PlayerLease::new(uuid)));
        LeaseRequest(match tail {
            Tail::Free(mut lease) => {
                lease.arm(next);
                Source::Ready(lease)
            }
            Tail::Pending(previous) => Source::Waiting {
                previous,
                next,
                uuid,
            },
        })
    }
}

impl LeaseRequest {
    pub async fn granted(self) -> PlayerLease {
        match self.0 {
            Source::Ready(lease) => lease,
            Source::Waiting {
                previous,
                next,
                uuid,
            } => {
                let mut lease = previous.await.unwrap_or_else(|_| {
                    let mut lease = PlayerLease::new(uuid);
                    lease.dirty = true;
                    lease
                });
                lease.arm(next);
                lease
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn leases_pass_in_request_order_and_one_at_a_time() {
        let mut table = LeaseTable::default();
        let first = table.request(1).granted().await;
        let second = table.request(1);
        let third = table.request(1);
        let other = table.request(2).granted().await;
        let second = tokio::spawn(async move {
            let lease = second.granted().await;
            assert!(!lease.was_dirty());
            lease
        });
        let third = tokio::spawn(async move { third.granted().await });
        tokio::task::yield_now().await;
        assert!(!second.is_finished() && !third.is_finished());
        first.release();
        let second = second.await.unwrap();
        assert!(!third.is_finished());
        second.release();
        third.await.unwrap().release();
        other.release();
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_lease_dropped_while_panicking_reaches_the_next_holder_dirty() {
        let mut table = LeaseTable::default();
        let first = table.request(7).granted().await;
        let next = table.request(7);
        let before = dirty_returns();
        let crashed = std::thread::spawn(move || {
            let _lease = first;
            panic!("the session died");
        })
        .join();
        assert!(crashed.is_err());
        let lease = next.granted().await;
        assert!(lease.was_dirty());
        assert_eq!(dirty_returns(), before + 1);
        lease.release();
        let clean = table.request(7).granted().await;
        assert!(!clean.was_dirty());
        clean.release();
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_stale_write_cannot_land_after_a_newer_one_or_a_later_holder() {
        let mut table = LeaseTable::default();
        let lease = table.request(3).granted().await;
        let older = lease.access();
        let newer = lease.access();
        let mut landed = Vec::new();
        assert!(newer.write(|| landed.push("newer")));
        assert!(!older.write(|| landed.push("older")));
        assert_eq!(landed, ["newer"]);
        let late = lease.access();
        let next = table.request(3);
        lease.release();
        let lease = next.granted().await;
        assert!(!late.write(|| landed.push("late")));
        assert!(lease.access().write(|| landed.push("current")));
        assert_eq!(landed, ["newer", "current"]);
        lease.release();
    }
}

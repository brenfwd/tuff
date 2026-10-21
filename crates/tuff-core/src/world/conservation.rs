//! Fail-safe handling of item conservation violations in a running world.
//!
//! An item operation that would make or lose items is refused; the actors
//! it involved are quarantined so their inconsistent state is never saved:
//! a player until it reconnects (it is disconnected so the client reloads
//! from the last consistent save), a region until it unloads. A
//! quarantined region refuses item transfers with players so nothing taken
//! from its unsaved state can reach saved state elsewhere.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use tuff_item::item::ItemSnapshot;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

use crate::world::dimension::DimensionId;
use crate::world::messages::PlayerId;

/// An actor whose state may be quarantined. A player is its session: a
/// reconnected player loaded its last consistent save.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    Player(PlayerId),
    Region(DimensionId, RegionPos),
}

/// The quarantined actors of one world.
#[derive(Debug, Default)]
pub struct Conservation {
    quarantined: Mutex<HashSet<Subject>>,
    fail_next_check: AtomicBool,
}

impl Conservation {
    /// Failure injection for tests: the next cross-actor transfer check
    /// fails before anything moves.
    pub fn fail_next_check(&self) {
        self.fail_next_check.store(true, Ordering::SeqCst);
    }

    pub(crate) fn take_injected_failure(&self) -> bool {
        self.fail_next_check.swap(false, Ordering::SeqCst)
    }

    pub fn quarantine(&self, subject: Subject) {
        self.quarantined
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(subject);
    }

    pub fn quarantined(&self, subject: Subject) -> bool {
        self.quarantined
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&subject)
    }

    /// The subject reloaded from its last consistent save.
    pub fn release(&self, subject: Subject) {
        self.quarantined
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&subject);
    }
}

/// A refused item operation, for the log, the audit log and hooks.
#[derive(Clone, Debug)]
pub struct ItemViolation {
    /// What was refused, e.g. `container transaction`.
    pub operation: &'static str,
    pub player: Option<PlayerId>,
    pub pos: BlockPos,
    pub item: Option<ItemSnapshot>,
    pub detail: String,
}

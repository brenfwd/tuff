//! Fail-safe handling of item conservation violations in a running world.
//!
//! An item operation that would make or lose items is refused; the actors
//! it involved are quarantined so their inconsistent state is never saved:
//! a player until it reconnects (it is disconnected so the client reloads
//! from the last consistent save), a region until it unloads. A
//! quarantined region refuses item transfers with players so nothing taken
//! from its unsaved state can reach saved state elsewhere.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
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

/// A kind of cross-actor item transfer, for failure injection and the
/// log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Transfer {
    /// A container or entity container menu transaction.
    Settle = 1,
    /// A player dropping items into a region.
    Drop,
    /// A hopper, dropper or cart moving items through an item port.
    Port,
    /// A hopper or copper golem moving items to or from a container part
    /// another region owns.
    Hopper,
    /// Dropped items merging across a region border.
    BorderMerge,
    /// What a block or entity click offered and did not keep, handed
    /// back to the player.
    HandReturn,
    /// An item entity or ground pickup granted to a player.
    Pickup,
    /// An entity handed with everything it carries to another region or
    /// dimension.
    EntityHandOff,
    /// A plugin giving items to or taking items from a player.
    Plugin,
}

impl Transfer {
    pub fn operation(self) -> &'static str {
        match self {
            Self::Settle => "container transaction",
            Self::Drop => "drop into a region",
            Self::Port => "item port transfer",
            Self::Hopper => "cross-region hopper transfer",
            Self::BorderMerge => "cross-region item merge",
            Self::HandReturn => "hand return to a player",
            Self::Pickup => "item pickup",
            Self::EntityHandOff => "entity hand-off",
            Self::Plugin => "plugin inventory operation",
        }
    }
}

/// The quarantined actors of one world.
#[derive(Debug, Default)]
pub struct Conservation {
    quarantined: Mutex<HashSet<Subject>>,
    /// How many subjects are quarantined, so the usual case takes no lock.
    count: AtomicUsize,
    fail_next: AtomicU8,
}

impl Conservation {
    /// Failure injection for tests: the next menu transaction check fails
    /// before anything moves.
    pub fn fail_next_check(&self) {
        self.fail_next(Transfer::Settle);
    }

    /// Failure injection for tests: the next check of a `transfer` fails
    /// before anything moves.
    pub fn fail_next(&self, transfer: Transfer) {
        self.fail_next.store(transfer as u8, Ordering::SeqCst);
    }

    pub(crate) fn take_injected_failure(&self) -> bool {
        self.take_injected(Transfer::Settle)
    }

    pub(crate) fn take_injected(&self, transfer: Transfer) -> bool {
        let kind = transfer as u8;
        self.fail_next.load(Ordering::Relaxed) == kind
            && self
                .fail_next
                .compare_exchange(kind, 0, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
    }

    pub fn quarantine(&self, subject: Subject) {
        let mut held = self
            .quarantined
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if held.insert(subject) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn quarantined(&self, subject: Subject) -> bool {
        self.count.load(Ordering::Relaxed) != 0
            && self
                .quarantined
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&subject)
    }

    /// The subject reloaded from its last consistent save.
    pub fn release(&self, subject: Subject) {
        let mut held = self
            .quarantined
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if held.remove(&subject) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
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

//! An entity that is out of the world as data: in a hive, on a shoulder.
//!
//! It is move-only. It exists only because a live entity was taken out of
//! the world (`RegionWorld::take_entity`) or was read back from storage,
//! and it ends by being spawned again, burned with a reason, or written
//! to storage. Dropping one in debug builds is a bug (the entity would be
//! lost); in release it costs nothing beyond its data.

use std::sync::atomic::{AtomicU64, Ordering};

use tuff_item::item::conservation::OpCounter;
use tuff_nbt::Compound;

use crate::world::entity_claim::EntityClaim;

/// Why a stored entity is destroyed instead of spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityBurn {
    /// Its saved form no longer loads.
    Unloadable,
    /// Its holder was destroyed with it inside.
    Killed,
    /// Tests and fixtures.
    Test,
}

static TAKEN: OpCounter = OpCounter::new();
static LOADED: OpCounter = OpCounter::new();
static SPAWNED: OpCounter = OpCounter::new();
static STORED: OpCounter = OpCounter::new();
static BURNED: OpCounter = OpCounter::new();
static LOST: AtomicU64 = AtomicU64::new(0);

/// What entered and left the stored form, process wide. The per-operation
/// counts only run in debug builds; `lost` counts in every build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityLedgerTotals {
    pub taken: u64,
    pub loaded: u64,
    pub spawned: u64,
    pub stored: u64,
    pub burned: u64,
    pub lost: u64,
}

impl EntityLedgerTotals {
    pub fn since(self, before: Self) -> Self {
        Self {
            taken: self.taken - before.taken,
            loaded: self.loaded - before.loaded,
            spawned: self.spawned - before.spawned,
            stored: self.stored - before.stored,
            burned: self.burned - before.burned,
            lost: self.lost - before.lost,
        }
    }
}

pub fn totals() -> EntityLedgerTotals {
    EntityLedgerTotals {
        taken: TAKEN.get(),
        loaded: LOADED.get(),
        spawned: SPAWNED.get(),
        stored: STORED.get(),
        burned: BURNED.get(),
        lost: LOST.load(Ordering::Relaxed),
    }
}

#[derive(Debug)]
#[must_use = "a stored entity must be spawned, burned or stored"]
pub struct StoredEntity {
    nbt: Compound,
    /// The UUID claim of the entity it was taken from; released when the
    /// stored entity ends.
    claim: Option<EntityClaim>,
    spent: bool,
}

impl StoredEntity {
    /// Door: the saved form of an entity the region just removed, with the
    /// claim on its UUID.
    pub(crate) fn taken(nbt: Compound, claim: Option<EntityClaim>) -> Self {
        TAKEN.add(1);
        Self {
            nbt,
            claim,
            spent: false,
        }
    }

    /// Door: an entity read back from a save (it holds no claim until it
    /// is spawned).
    pub fn from_storage(nbt: Compound) -> Self {
        LOADED.add(1);
        Self {
            nbt,
            claim: None,
            spent: false,
        }
    }

    fn finish(&mut self, counter: &OpCounter) -> Compound {
        self.spent = true;
        self.claim = None;
        counter.add(1);
        std::mem::take(&mut self.nbt)
    }

    pub fn nbt(&self) -> &Compound {
        &self.nbt
    }

    /// Edits the saved form while it is held (aging, a new position).
    pub fn nbt_mut(&mut self) -> &mut Compound {
        &mut self.nbt
    }

    /// Door: the data of the entity to load it as a live one; this spends
    /// the stored entity (the caller spawns what it loads, which claims
    /// the UUID again).
    pub fn into_spawn_data(mut self) -> Compound {
        self.finish(&SPAWNED)
    }

    /// Door: the data to write into a save or a holder's own data.
    pub fn into_storage(mut self) -> Compound {
        self.finish(&STORED)
    }

    /// A copy of the data for a periodic save that leaves the entity held.
    pub fn snapshot(&self) -> Compound {
        self.nbt.clone()
    }

    pub fn burn(mut self, _reason: EntityBurn) {
        self.finish(&BURNED);
    }
}

impl Drop for StoredEntity {
    fn drop(&mut self) {
        if self.spent {
            return;
        }
        LOST.fetch_add(1, Ordering::Relaxed);
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!(
                "a stored entity was dropped unspent: {:?}",
                self.nbt.get_str("id")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_spent_stored_entity_drops_quietly() {
        let nbt = Compound::new().with("id", "minecraft:bee");
        let held = StoredEntity::from_storage(nbt.clone());
        assert_eq!(held.snapshot(), nbt);
        assert_eq!(held.into_storage(), nbt);
        StoredEntity::from_storage(nbt).burn(EntityBurn::Test);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_door_is_counted_and_a_claim_ends_with_the_stored_entity() {
        let uuids = std::sync::Arc::new(crate::world::entity_claim::EntityUuids::default());
        let before = totals();
        let claim = uuids.claim(5).unwrap();
        assert!(uuids.claim(5).is_none());
        let held = StoredEntity::taken(Compound::new(), Some(claim));
        assert!(uuids.contains(5));
        drop(held.into_storage());
        assert!(!uuids.contains(5));
        StoredEntity::from_storage(Compound::new()).burn(EntityBurn::Test);
        let diff = totals().since(before);
        if cfg!(debug_assertions) {
            assert!(diff.taken >= 1 && diff.loaded >= 1 && diff.stored >= 1 && diff.burned >= 1);
        }
        assert_eq!(diff.lost, 0);
    }

    #[cfg(not(debug_assertions))]
    #[tuff_core::watchdog]
    #[test]
    fn a_stored_entity_is_its_data_claim_and_spent_flag_in_release_builds() {
        use std::mem::size_of;
        assert_eq!(size_of::<OpCounter>(), 0);
        assert_eq!(
            size_of::<StoredEntity>(),
            size_of::<(Compound, Option<EntityClaim>, bool)>()
        );
    }

    #[cfg(debug_assertions)]
    #[tuff_core::watchdog]
    #[test]
    #[should_panic(expected = "dropped unspent")]
    fn a_lost_stored_entity_panics_in_debug() {
        drop(StoredEntity::from_storage(Compound::new()));
    }
}

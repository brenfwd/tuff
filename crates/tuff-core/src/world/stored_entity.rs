//! An entity that is out of the world as data: in a hive, on a shoulder.
//!
//! It is move-only. It exists only because a live entity was taken out of
//! the world (`RegionWorld::take_entity`) or was read back from storage,
//! and it ends by being spawned again, burned with a reason, or written
//! to storage. Dropping one in debug builds is a bug (the entity would be
//! lost); in release it costs nothing beyond its data.

use tuff_nbt::Compound;

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

#[derive(Debug)]
#[must_use = "a stored entity must be spawned, burned or stored"]
pub struct StoredEntity {
    nbt: Compound,
    #[cfg(debug_assertions)]
    spent: bool,
}

impl StoredEntity {
    fn new(nbt: Compound) -> Self {
        Self {
            nbt,
            #[cfg(debug_assertions)]
            spent: false,
        }
    }

    /// Door: the saved form of an entity the region just removed.
    pub(crate) fn taken(nbt: Compound) -> Self {
        Self::new(nbt)
    }

    /// Door: an entity read back from a save.
    pub fn from_storage(nbt: Compound) -> Self {
        Self::new(nbt)
    }

    #[cfg(debug_assertions)]
    fn spend(&mut self) {
        self.spent = true;
    }

    #[cfg(not(debug_assertions))]
    fn spend(&mut self) {}

    pub fn nbt(&self) -> &Compound {
        &self.nbt
    }

    /// Edits the saved form while it is held (aging, a new position).
    pub fn nbt_mut(&mut self) -> &mut Compound {
        &mut self.nbt
    }

    /// Door: the data of the entity to load it as a live one; this spends
    /// the stored entity (the caller spawns what it loads).
    pub fn into_spawn_data(mut self) -> Compound {
        self.spend();
        std::mem::take(&mut self.nbt)
    }

    /// Door: the data to write into a save or a holder's own data.
    pub fn into_storage(mut self) -> Compound {
        self.spend();
        std::mem::take(&mut self.nbt)
    }

    /// A copy of the data for a periodic save that leaves the entity held.
    pub fn snapshot(&self) -> Compound {
        self.nbt.clone()
    }

    pub fn burn(mut self, _reason: EntityBurn) {
        self.spend();
    }
}

#[cfg(debug_assertions)]
impl Drop for StoredEntity {
    fn drop(&mut self) {
        if !self.spent && !std::thread::panicking() {
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

    #[cfg(debug_assertions)]
    #[tuff_core::watchdog]
    #[test]
    #[should_panic(expected = "dropped unspent")]
    fn a_lost_stored_entity_panics_in_debug() {
        drop(StoredEntity::from_storage(Compound::new()));
    }
}

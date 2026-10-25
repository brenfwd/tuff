//! Which entity UUIDs are alive: one owned claim per UUID, level wide.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

const SHARDS: usize = 16;

/// The UUIDs of every living entity, sharded by UUID so regions claiming
/// at once rarely meet on one lock. Claims are taken when an entity joins
/// a region and live as long as the entity (or its stored form) does.
#[derive(Debug, Default)]
pub struct EntityUuids {
    shards: [Mutex<HashSet<u128>>; SHARDS],
}

impl EntityUuids {
    fn shard(&self, uuid: u128) -> std::sync::MutexGuard<'_, HashSet<u128>> {
        let index = (uuid ^ (uuid >> 64)) as usize % SHARDS;
        self.shards[index]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The claim on `uuid`, or `None` if some entity already holds it.
    pub(crate) fn claim(self: &Arc<Self>, uuid: u128) -> Option<EntityClaim> {
        self.shard(uuid).insert(uuid).then(|| EntityClaim {
            set: self.clone(),
            uuid,
        })
    }

    pub fn contains(&self, uuid: u128) -> bool {
        self.shard(uuid).contains(&uuid)
    }
}

/// The right to a UUID: move-only, released when dropped. Held by a live
/// entity, travelling with it across hand-offs, or by its stored form.
#[derive(Debug)]
#[must_use = "dropping a claim frees its UUID"]
pub struct EntityClaim {
    set: Arc<EntityUuids>,
    uuid: u128,
}

impl Drop for EntityClaim {
    fn drop(&mut self) {
        self.set.shard(self.uuid).remove(&self.uuid);
    }
}

/// Where an entity keeps its claim. A copy of the entity holds none and
/// slots always compare equal, so cloning or comparing entities ignores it.
#[derive(Debug, Default)]
pub struct ClaimSlot(Option<EntityClaim>);

impl Clone for ClaimSlot {
    fn clone(&self) -> Self {
        Self(None)
    }
}

impl PartialEq for ClaimSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ClaimSlot {
    pub fn is_held(&self) -> bool {
        self.0.is_some()
    }

    pub fn put(&mut self, claim: EntityClaim) {
        self.0 = Some(claim);
    }

    pub fn take(&mut self) -> Option<EntityClaim> {
        self.0.take()
    }
}

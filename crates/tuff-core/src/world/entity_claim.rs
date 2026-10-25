//! Which entity UUIDs are alive: one claim per UUID, level wide.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

const SHARDS: usize = 16;

/// The UUIDs of every living entity, sharded by UUID so regions claiming
/// at once rarely meet on one lock.
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

    /// False if some entity already holds `uuid`.
    pub fn claim(&self, uuid: u128) -> bool {
        self.shard(uuid).insert(uuid)
    }

    pub fn release(&self, uuid: u128) {
        self.shard(uuid).remove(&uuid);
    }

    pub fn contains(&self, uuid: u128) -> bool {
        self.shard(uuid).contains(&uuid)
    }
}

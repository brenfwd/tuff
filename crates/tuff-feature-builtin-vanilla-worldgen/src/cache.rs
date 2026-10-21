//! Bounded memo tables for pure functions of a chunk position, shared by
//! every job generating from one generator so neighbouring chunks do not
//! each recompute the same answer.

use std::sync::{Mutex, PoisonError};

use tuff_world_model::pos::{ChunkMap, ChunkPos};

pub struct ChunkCache<V> {
    entries: Mutex<ChunkMap<V>>,
    cap: usize,
    keep_radius: i32,
}

impl<V: Clone> ChunkCache<V> {
    /// Holds at most `cap` entries; when full, entries far from the chunk
    /// being inserted go first, since generation moves with the players.
    pub fn new(cap: usize) -> Self {
        Self {
            entries: Mutex::new(ChunkMap::default()),
            cap,
            keep_radius: (cap.isqrt() / 4) as i32,
        }
    }

    pub fn get(&self, pos: ChunkPos) -> Option<V> {
        self.lock().get(&pos).cloned()
    }

    pub fn insert(&self, pos: ChunkPos, value: V) {
        self.insert_locked(&mut self.lock(), pos, value);
    }

    fn insert_locked(&self, entries: &mut ChunkMap<V>, pos: ChunkPos, value: V) {
        if entries.len() >= self.cap && !entries.contains_key(&pos) {
            let radius = self.keep_radius;
            entries.retain(|at, _| (at.x - pos.x).abs().max((at.z - pos.z).abs()) <= radius);
            if entries.len() >= self.cap {
                entries.clear();
            }
        }
        entries.insert(pos, value);
    }

    /// The values at `positions` under one lock; `out` gets `None` for
    /// each miss.
    pub fn get_many(&self, positions: &[ChunkPos], out: &mut [Option<V>]) {
        let entries = self.lock();
        for (pos, slot) in positions.iter().zip(out.iter_mut()) {
            *slot = entries.get(pos).cloned();
        }
    }

    pub fn insert_many(&self, items: impl IntoIterator<Item = (ChunkPos, V)>) {
        let mut entries = self.lock();
        for (pos, value) in items {
            self.insert_locked(&mut entries, pos, value);
        }
    }

    /// The value at `pos`, computing and storing it on a miss. The
    /// computation runs unlocked, so two jobs may both compute the same
    /// entry, which is fine for a pure function.
    pub fn get_or_insert_with(&self, pos: ChunkPos, compute: impl FnOnce() -> V) -> V {
        if let Some(value) = self.get(pos) {
            return value;
        }
        let value = compute();
        self.insert(pos, value.clone());
        value
    }

    /// Drops every entry and the table's capacity.
    pub fn clear(&self) {
        let dropped = std::mem::take(&mut *self.lock());
        drop(dropped);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChunkMap<V>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn computes_once_per_position() {
        let cache = ChunkCache::new(16);
        let mut calls = 0;
        for _ in 0..3 {
            let value = cache.get_or_insert_with(ChunkPos::new(3, -4), || {
                calls += 1;
                calls
            });
            assert_eq!(value, 1);
        }
        assert_eq!(calls, 1);
        assert_eq!(cache.len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn full_cache_keeps_the_neighbourhood_of_the_new_entry() {
        let cache = ChunkCache::new(64);
        for x in 0..64 {
            cache.insert(ChunkPos::new(x, 0), x);
        }
        assert_eq!(cache.len(), 64);
        cache.insert(ChunkPos::new(63, 1), 100);
        assert_eq!(cache.get(ChunkPos::new(63, 1)), Some(100));
        assert_eq!(cache.get(ChunkPos::new(63, 0)), Some(63));
        assert_eq!(cache.get(ChunkPos::new(0, 0)), None);
        assert!(cache.len() < 64);
    }

    #[tuff_core::watchdog]
    #[test]
    fn clear_empties_the_cache() {
        let cache = ChunkCache::new(16);
        cache.insert(ChunkPos::new(1, 2), 3);
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.get_or_insert_with(ChunkPos::new(1, 2), || 4), 4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn full_cache_with_no_nearby_entries_starts_over() {
        let cache = ChunkCache::new(4);
        for x in 0..4 {
            cache.insert(ChunkPos::new(x, 0), x);
        }
        cache.insert(ChunkPos::new(1000, 1000), 9);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(ChunkPos::new(1000, 1000)), Some(9));
    }
}

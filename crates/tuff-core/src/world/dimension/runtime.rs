//! Dimensions made while the world runs, each a copy of an existing
//! dimension's type, height and generator under a new key, owned by whoever
//! made it (a feature). They live in slots after the world's own dimensions,
//! so their [`DimensionId`]s are stable while they exist.
//!
//! A discarded dimension leaves `by_key` at once but keeps its slot, and
//! with it its key, until its owner has dropped its regions and storage
//! and [`Runtime::forget`]s it. Freed slots are reused round robin, so a
//! stale id held somewhere resolves to a discarded dimension for as long
//! as possible. Every dimension ever made stays allocated (a few hundred
//! bytes; the generator is shared with the base) until the world is
//! dropped, because `&Dimension` borrows of it may still be held.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio::sync::watch;

use super::{Dimension, DimensionId};
use crate::storage::level::WorldBorder;
use crate::world::border::Border;

/// How many runtime dimensions may exist at once.
pub const MAX_RUNTIME: usize = 1024;

const DISCARDED: u32 = 1 << 31;
const INDEX: u32 = DISCARDED - 1;

/// Why a runtime dimension was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateError {
    /// A dimension with the key exists, or is still being discarded.
    Exists,
    /// [`MAX_RUNTIME`] runtime dimensions exist.
    Full,
    /// The key belongs to the world itself (see `Dimensions::is_reserved`).
    Reserved,
}

struct Entry {
    dimension: Dimension,
    owner: String,
    _border: watch::Sender<Border>,
}

const FIRST: usize = 16;
const CHUNKS: usize = 28;

/// Append-only storage whose entries never move, in chunks of doubling
/// size.
struct Arena {
    chunks: [OnceLock<Box<[OnceLock<Entry>]>>; CHUNKS],
}

impl Arena {
    fn new() -> Self {
        Self {
            chunks: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    fn locate(index: usize) -> (usize, usize) {
        let n = index / FIRST + 1;
        let chunk = (usize::BITS - 1 - n.leading_zeros()) as usize;
        (chunk, index - FIRST * ((1 << chunk) - 1))
    }

    fn get(&self, index: usize) -> Option<&Entry> {
        let (chunk, offset) = Self::locate(index);
        self.chunks.get(chunk)?.get()?.get(offset)?.get()
    }

    fn put(&self, index: usize, entry: Entry) -> bool {
        let (chunk, offset) = Self::locate(index);
        let Some(chunk_cell) = self.chunks.get(chunk) else {
            return false;
        };
        let slots =
            chunk_cell.get_or_init(|| (0..FIRST << chunk).map(|_| OnceLock::new()).collect());
        slots[offset].set(entry).is_ok()
    }
}

#[derive(Default)]
struct State {
    /// Every key in use, live or being discarded, by slot.
    keys: HashMap<String, usize>,
    used: Vec<bool>,
    next_entry: usize,
    cursor: usize,
}

pub(super) struct Runtime {
    arena: Arena,
    /// Per slot: 0 when never used, else the arena index plus one, with
    /// [`DISCARDED`] set once discarded.
    slots: Box<[AtomicU32]>,
    state: Mutex<State>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            arena: Arena::new(),
            slots: (0..MAX_RUNTIME).map(|_| AtomicU32::new(0)).collect(),
            state: Mutex::new(State {
                used: vec![false; MAX_RUNTIME],
                ..State::default()
            }),
        }
    }
}

impl Runtime {
    fn entry(&self, slot: usize) -> Option<&Entry> {
        let value = self.slots.get(slot)?.load(Ordering::Acquire) & INDEX;
        let index = value.checked_sub(1)?;
        self.arena.get(index as usize)
    }

    pub fn get(&self, slot: usize) -> Option<&Dimension> {
        self.entry(slot).map(|entry| &entry.dimension)
    }

    pub fn owner(&self, slot: usize) -> Option<&str> {
        self.entry(slot).map(|entry| entry.owner.as_str())
    }

    pub fn is_discarded(&self, slot: usize) -> bool {
        self.slots
            .get(slot)
            .is_some_and(|value| value.load(Ordering::Acquire) & DISCARDED != 0)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The live slot called `key`.
    pub fn by_key(&self, key: &str) -> Option<usize> {
        let slot = *self.state().keys.get(key)?;
        (!self.is_discarded(slot)).then_some(slot)
    }

    /// Whether `key` is taken, live or still being discarded.
    pub fn taken(&self, key: &str) -> bool {
        self.state().keys.contains_key(key)
    }

    /// Makes a dimension called `key` like `base` in a free slot; `first`
    /// is the id of slot 0.
    pub fn create(
        &self,
        first: usize,
        key: &str,
        base: &Dimension,
        owner: &str,
    ) -> Result<DimensionId, CreateError> {
        let mut state = self.state();
        if state.keys.contains_key(key) {
            return Err(CreateError::Exists);
        }
        let slot = (0..MAX_RUNTIME)
            .map(|step| (state.cursor + step) % MAX_RUNTIME)
            .find(|slot| !state.used[*slot])
            .ok_or(CreateError::Full)?;
        let id = u16::try_from(first + slot).map_err(|_| CreateError::Full)?;
        let index = state.next_entry;
        let stored = u32::try_from(index + 1)
            .ok()
            .filter(|stored| stored & DISCARDED == 0)
            .ok_or(CreateError::Full)?;
        let (writer, border) = watch::channel(Border::from_saved(&WorldBorder::default()));
        let dimension = Dimension {
            id: DimensionId(id),
            key: key.to_owned(),
            kind: base.kind,
            environment: base.environment,
            type_id: base.type_id,
            shape: base.shape,
            generator: base.generator.clone(),
            border,
            default_clock: base.default_clock,
            portal_link: None,
            natural_spawning: base.natural_spawning,
        };
        let entry = Entry {
            dimension,
            owner: owner.to_owned(),
            _border: writer,
        };
        if !self.arena.put(index, entry) {
            return Err(CreateError::Full);
        }
        state.next_entry += 1;
        state.used[slot] = true;
        state.cursor = (slot + 1) % MAX_RUNTIME;
        state.keys.insert(key.to_owned(), slot);
        self.slots[slot].store(stored, Ordering::Release);
        Ok(DimensionId(id))
    }

    /// Hides a live dimension from `by_key`; false when it is not live.
    pub fn discard(&self, slot: usize) -> bool {
        let Some(value) = self.slots.get(slot) else {
            return false;
        };
        let state = self.state();
        let old = value.load(Ordering::Acquire);
        if old & INDEX == 0 || old & DISCARDED != 0 || !state.used[slot] {
            return false;
        }
        value.store(old | DISCARDED, Ordering::Release);
        true
    }

    /// Frees a discarded dimension's key and slot.
    pub fn forget(&self, slot: usize) {
        if !self.is_discarded(slot) {
            return;
        }
        let mut state = self.state();
        if !state.used.get(slot).copied().unwrap_or(false) {
            return;
        }
        state.keys.retain(|_, used| *used != slot);
        state.used[slot] = false;
    }

    /// Slots in use (live or being discarded), in slot order.
    pub fn slots(&self) -> Vec<usize> {
        let state = self.state();
        (0..MAX_RUNTIME).filter(|slot| state.used[*slot]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn arena_chunks_cover_every_index_once() {
        let mut seen = std::collections::HashSet::new();
        for index in 0..5000 {
            let (chunk, offset) = Arena::locate(index);
            assert!(offset < FIRST << chunk, "{index} -> {chunk},{offset}");
            assert!(seen.insert((chunk, offset)));
        }
        assert_eq!(Arena::locate(0), (0, 0));
        assert_eq!(Arena::locate(15), (0, 15));
        assert_eq!(Arena::locate(16), (1, 0));
        assert_eq!(Arena::locate(48), (2, 0));
    }
}

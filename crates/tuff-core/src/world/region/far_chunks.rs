//! Read-only copies of loaded chunks beyond the halo, for entities whose
//! reach (path searches, line of sight) goes farther past the border than
//! the one-chunk halo. An entity block lookup that finds a chunk neither
//! here nor in the halo notes it; the next entity tick asks the owning
//! neighbour for a copy, which it answers with a task posted back. Copies
//! are refreshed while in use (an unchanged chunk is not resent) and
//! forgotten once idle, so entities see a neighbour's blocks as of a tick or
//! two ago, like the halo but on demand. Until a copy arrives the chunk reads
//! as unknown.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::RegionActor;
use crate::world::messages::{RegionHandle, RegionInternal};
use crate::world::region_api::{RegionTask, RegionWorld};
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::{ChunkMap, ChunkPos, RegionPos};

/// Ticks after which a copy still in use is asked for again.
const REFRESH_TICKS: u64 = 20;
/// Ticks a copy nobody reads is kept.
const FORGET_TICKS: u64 = 200;
/// Ticks a chunk found not loaded reads as unknown before it is asked for
/// again.
const MISSING_TICKS: u64 = 40;
/// Ticks after which a request nobody answered is sent again.
const PENDING_TICKS: u64 = 100;
/// Requests sent per tick, at most.
const MAX_REQUESTS: usize = 32;

struct Copy {
    /// `None` when the owner did not have it loaded.
    chunk: Option<Chunk>,
    fetched: u64,
    used: AtomicU64,
}

/// What an owner answers for one chunk.
#[derive(Debug)]
pub(super) enum Answer {
    Unchanged,
    Missing,
    Chunk(Chunk),
}

/// This region's copies of other regions' chunks, kept in its
/// `RegionState`.
#[derive(Default)]
pub(super) struct FarChunks {
    copies: ChunkMap<Copy>,
    pending: ChunkMap<u64>,
    missed: Mutex<BTreeSet<ChunkPos>>,
}

impl FarChunks {
    /// The copy of `chunk`, noting it as wanted when there is none yet.
    pub(super) fn chunk(&self, chunk: ChunkPos, now: u64) -> Option<&Chunk> {
        match self.copies.get(&chunk) {
            Some(copy) => {
                copy.used.fetch_max(now, Ordering::Relaxed);
                copy.chunk.as_ref()
            }
            None => {
                if !self.pending.contains_key(&chunk) {
                    self.missed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(chunk);
                }
                None
            }
        }
    }

    fn is_idle(&self) -> bool {
        self.copies.is_empty()
            && self.pending.is_empty()
            && self
                .missed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
    }

    /// Drops stale copies and requests, and picks the chunks to ask for
    /// now with the stamp of the copy already held. Chunks of `here` are
    /// never asked for: they read as unknown until the region loads them.
    fn plan(&mut self, now: u64, here: RegionPos) -> Vec<(ChunkPos, Option<u64>)> {
        let age = |at: u64| now.saturating_sub(at);
        self.pending.retain(|_, at| age(*at) < PENDING_TICKS);
        self.copies.retain(|_, copy| match copy.chunk {
            Some(_) => age(copy.used.load(Ordering::Relaxed)) <= FORGET_TICKS,
            None => age(copy.fetched) < MISSING_TICKS,
        });
        let mut wanted = BTreeSet::new();
        for chunk in std::mem::take(
            self.missed
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ) {
            if self.pending.contains_key(&chunk) || self.copies.contains_key(&chunk) {
                continue;
            }
            if here.contains(chunk) {
                self.receive(chunk, Answer::Missing, now);
            } else {
                wanted.insert((chunk, None));
            }
        }
        for (pos, copy) in &self.copies {
            if let Some(chunk) = &copy.chunk
                && age(copy.fetched) >= REFRESH_TICKS
                && age(copy.used.load(Ordering::Relaxed)) < REFRESH_TICKS
                && !self.pending.contains_key(pos)
            {
                wanted.insert((*pos, Some(chunk.stamp())));
            }
        }
        let mut requests: Vec<_> = wanted.into_iter().collect();
        if requests.len() > MAX_REQUESTS {
            let missed = self
                .missed
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (chunk, stamp) in requests.split_off(MAX_REQUESTS) {
                if stamp.is_none() {
                    missed.insert(chunk);
                }
            }
        }
        for (chunk, _) in &requests {
            self.pending.insert(*chunk, now);
        }
        requests
    }

    fn receive(&mut self, chunk: ChunkPos, answer: Answer, now: u64) {
        self.pending.remove(&chunk);
        let used = self
            .copies
            .get(&chunk)
            .map_or(now, |c| c.used.load(Ordering::Relaxed));
        match answer {
            Answer::Unchanged => {
                if let Some(copy) = self.copies.get_mut(&chunk) {
                    copy.fetched = now;
                }
            }
            Answer::Missing | Answer::Chunk(_) => {
                let chunk_copy = match answer {
                    Answer::Chunk(found) => Some(found),
                    _ => None,
                };
                self.copies.insert(
                    chunk,
                    Copy {
                        chunk: chunk_copy,
                        fetched: now,
                        used: AtomicU64::new(used),
                    },
                );
            }
        }
    }
}

/// The owner's side: copies `chunk` unless the asker's copy (`known`
/// stamp) is current, and posts the answer back.
fn answer(owner: &mut dyn RegionWorld, chunk: ChunkPos, known: Option<u64>, home: RegionHandle) {
    let answer = match owner.chunk(chunk) {
        None => Answer::Missing,
        Some(found) if Some(found.stamp()) == known => Answer::Unchanged,
        Some(found) => Answer::Chunk(found.clone()),
    };
    home.internal(RegionInternal::Run(RegionTask::new(move |home| {
        let now = home.tick();
        home.state().get::<FarChunks>().receive(chunk, answer, now);
    })));
}

impl RegionActor {
    /// Asks neighbours for the chunks entity lookups missed and for fresh
    /// copies of those still in use.
    pub(super) fn fetch_far_chunks(&mut self) {
        let now = self.sched.tick;
        let far = self.ents.feature_state.get::<FarChunks>();
        if far.is_idle() {
            return;
        }
        for (chunk, known) in far.plan(now, self.pos) {
            match self.border.neighbors.get(&chunk.region()) {
                Some(owner) => {
                    let home = self.handle.clone();
                    owner.internal(RegionInternal::Run(RegionTask::new(move |owner| {
                        answer(owner, chunk, known, home)
                    })));
                }
                None => far.receive(chunk, Answer::Missing, now),
            }
        }
    }

    /// Takes the copies out of the feature state for a tick context to read
    /// alongside it; [`Self::return_far_chunks`] puts them back.
    pub(super) fn take_far_chunks(&mut self) -> FarChunks {
        std::mem::take(self.ents.feature_state.get::<FarChunks>())
    }

    pub(super) fn return_far_chunks(&mut self, far: FarChunks) {
        *self.ents.feature_state.get::<FarChunks>() = far;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_world_model::chunk::ChunkShape;

    fn chunk() -> Chunk {
        Chunk::empty(ChunkShape::new(-64, 384))
    }

    const HERE: RegionPos = RegionPos { x: 0, z: 0 };

    #[tuff_core::watchdog]
    #[test]
    fn a_missed_chunk_is_asked_for_once_and_then_read() {
        let mut far = FarChunks::default();
        let east = ChunkPos::new(33, 0);
        assert!(far.chunk(east, 1).is_none());
        assert!(far.chunk(east, 1).is_none());
        assert_eq!(far.plan(1, HERE), vec![(east, None)]);
        assert!(far.chunk(east, 2).is_none());
        assert!(far.plan(2, HERE).is_empty(), "already pending");
        far.receive(east, Answer::Chunk(chunk()), 3);
        assert!(far.chunk(east, 4).is_some());
        assert!(far.plan(4, HERE).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn own_and_unloaded_chunks_read_as_unknown_for_a_while() {
        let mut far = FarChunks::default();
        let own = ChunkPos::new(3, 3);
        let east = ChunkPos::new(40, 0);
        far.chunk(own, 1);
        far.chunk(east, 1);
        assert_eq!(far.plan(1, HERE), vec![(east, None)]);
        far.receive(east, Answer::Missing, 2);
        far.chunk(own, 3);
        far.chunk(east, 3);
        assert!(far.plan(3, HERE).is_empty());
        far.chunk(east, 2 + MISSING_TICKS);
        far.plan(2 + MISSING_TICKS, HERE);
        far.chunk(east, 3 + MISSING_TICKS);
        assert_eq!(far.plan(3 + MISSING_TICKS, HERE), vec![(east, None)]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn copies_in_use_are_refreshed_by_stamp_and_idle_ones_forgotten() {
        let mut far = FarChunks::default();
        let east = ChunkPos::new(33, 0);
        let copy = chunk();
        let stamp = copy.stamp();
        far.receive(east, Answer::Chunk(copy), 0);
        far.chunk(east, REFRESH_TICKS);
        assert_eq!(far.plan(REFRESH_TICKS, HERE), vec![(east, Some(stamp))]);
        far.receive(east, Answer::Unchanged, REFRESH_TICKS + 1);
        assert!(far.chunk(east, REFRESH_TICKS + 1).is_some());
        assert!(
            far.plan(REFRESH_TICKS * 3, HERE).is_empty(),
            "not read lately"
        );
        far.plan(REFRESH_TICKS + 2 + FORGET_TICKS, HERE);
        assert!(far.is_idle());
    }

    #[tuff_core::watchdog]
    #[test]
    fn requests_per_tick_are_bounded_and_the_rest_wait() {
        let mut far = FarChunks::default();
        for x in 0..(MAX_REQUESTS as i32 + 5) {
            far.chunk(ChunkPos::new(100 + x, 0), 1);
        }
        assert_eq!(far.plan(1, HERE).len(), MAX_REQUESTS);
        assert_eq!(far.plan(2, HERE).len(), 5);
    }
}

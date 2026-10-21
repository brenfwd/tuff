//! Server-wide state of `/stopwatch` and `/random`: named stopwatches and
//! named random sequences. Both persist.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

use crate::storage::level::{inner, saved};
use tuff_item::random::{RandomSource, Seed128, XoroshiroRandom};

#[derive(Default)]
pub struct Stopwatches {
    state: Mutex<StopwatchState>,
}

#[derive(Default)]
struct StopwatchState {
    started: HashMap<String, (i64, Instant)>,
    backend: Option<Arc<dyn WorldBackend>>,
    dirty: bool,
}

const DOCUMENT: &str = "minecraft:stopwatches";

fn elapsed_millis(&(accumulated, since): &(i64, Instant)) -> i64 {
    accumulated.saturating_add(i64::try_from(since.elapsed().as_millis()).unwrap_or(i64::MAX))
}

impl Stopwatches {
    /// Stopwatches saved in `stopwatches.dat`, which keep running from the
    /// elapsed time they were saved with; later changes are written by
    /// [`Stopwatches::flush`].
    pub fn load(backend: Arc<dyn WorldBackend>) -> Self {
        let root = match backend.documents().read(Document::SavedData(DOCUMENT)) {
            Ok(root) => root,
            Err(err) => {
                log::warn!("failed to read stopwatches: {err}");
                None
            }
        };
        let now = Instant::now();
        let started = match inner(root).get("stopwatches") {
            Some(Tag::Compound(saved)) => saved
                .iter()
                .filter_map(|(id, elapsed)| Some((id.to_owned(), (elapsed.as_long()?, now))))
                .collect(),
            _ => HashMap::new(),
        };
        Self {
            state: Mutex::new(StopwatchState {
                started,
                backend: Some(backend),
                dirty: false,
            }),
        }
    }

    /// Starts `id`; false when it already exists.
    pub fn create(&self, id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.started.contains_key(id) {
            return false;
        }
        state.started.insert(id.to_owned(), (0, Instant::now()));
        state.dirty = true;
        true
    }

    pub fn elapsed_seconds(&self, id: &str) -> Option<f64> {
        let state = self.state.lock().unwrap();
        state
            .started
            .get(id)
            .map(|watch| elapsed_millis(watch) as f64 / 1000.0)
    }

    pub fn restart(&self, id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(watch) = state.started.get_mut(id) else {
            return false;
        };
        *watch = (0, Instant::now());
        state.dirty = true;
        true
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        let removed = state.started.remove(id).is_some();
        state.dirty |= removed;
        removed
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.state.lock().unwrap().started.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Writes the stopwatches with their current elapsed times. Running
    /// stopwatches are always saved, as their elapsed time keeps changing.
    pub async fn flush(&self) {
        let (backend, data) = {
            let mut state = self.state.lock().unwrap();
            let Some(backend) = state.backend.clone() else {
                return;
            };
            if state.started.is_empty() && !state.dirty {
                return;
            }
            state.dirty = false;
            let saved_times: Compound = state
                .started
                .iter()
                .map(|(id, watch)| (id.clone(), Tag::Long(elapsed_millis(watch))))
                .collect();
            (
                backend,
                saved(Compound::new().with("stopwatches", saved_times)),
            )
        };
        let written = tokio::task::spawn_blocking(move || {
            backend
                .documents()
                .write(Document::SavedData(DOCUMENT), data)
        })
        .await;
        if let Ok(Err(err)) = written {
            log::error!("failed to save stopwatches: {err}");
        }
    }
}

const SEQUENCES_DOCUMENT: &str = "minecraft:random_sequences";

pub struct RandomSequences {
    inner: Mutex<SequenceState>,
}

struct SequenceState {
    sequences: HashMap<String, XoroshiroRandom>,
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    backend: Option<Arc<dyn WorldBackend>>,
    dirty: bool,
}

impl Default for SequenceState {
    fn default() -> Self {
        Self {
            sequences: HashMap::new(),
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            backend: None,
            dirty: false,
        }
    }
}

impl Default for RandomSequences {
    fn default() -> Self {
        Self {
            inner: Mutex::new(SequenceState::default()),
        }
    }
}

fn new_sequence(
    world_seed: i64,
    id: &str,
    salt: i32,
    world: bool,
    with_id: bool,
) -> XoroshiroRandom {
    let seed = (if world { world_seed } else { 0 }) ^ i64::from(salt);
    XoroshiroRandom::from_seed(Seed128::for_sequence(seed, with_id.then_some(id)))
}

impl RandomSequences {
    /// The sequences saved in `random_sequences.dat`; later changes are
    /// written by [`RandomSequences::flush`].
    pub fn load(backend: Arc<dyn WorldBackend>) -> Self {
        let root = match backend
            .documents()
            .read(Document::SavedData(SEQUENCES_DOCUMENT))
        {
            Ok(root) => root,
            Err(err) => {
                log::warn!("failed to read random sequences: {err}");
                None
            }
        };
        let data = inner(root);
        let mut state = SequenceState {
            backend: Some(backend),
            ..SequenceState::default()
        };
        state.salt = data.get_int("salt").unwrap_or(0);
        state.include_world_seed = data.get_bool("include_world_seed").unwrap_or(true);
        state.include_sequence_id = data.get_bool("include_sequence_id").unwrap_or(true);
        if let Some(Tag::Compound(saved)) = data.get("sequences") {
            for (id, sequence) in saved.iter() {
                if let Some(Tag::LongArray(longs)) =
                    sequence.as_compound().and_then(|s| s.get("source"))
                    && let [lo, hi] = longs.as_slice()
                {
                    state
                        .sequences
                        .insert(id.to_owned(), XoroshiroRandom::from_parts(*lo, *hi));
                }
            }
        }
        Self {
            inner: Mutex::new(state),
        }
    }

    /// A value in `min..=max` from sequence `id`, creating it on first use.
    pub fn sample(&self, world_seed: i64, id: &str, min: i32, max: i32) -> i32 {
        let mut state = self.inner.lock().unwrap();
        let (salt, world, with_id) = (
            state.salt,
            state.include_world_seed,
            state.include_sequence_id,
        );
        state.dirty = true;
        let rng = state
            .sequences
            .entry(id.to_owned())
            .or_insert_with(|| new_sequence(world_seed, id, salt, world, with_id));
        random_between_inclusive(rng, min, max)
    }

    pub fn reset(&self, world_seed: i64, id: &str, salt: Option<(i32, bool, bool)>) {
        let mut state = self.inner.lock().unwrap();
        let (salt, world, with_id) = salt.unwrap_or((
            state.salt,
            state.include_world_seed,
            state.include_sequence_id,
        ));
        state.sequences.insert(
            id.to_owned(),
            new_sequence(world_seed, id, salt, world, with_id),
        );
        state.dirty = true;
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .sequences
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Forgets every sequence; the count that were cleared.
    pub fn clear(&self) -> usize {
        let mut state = self.inner.lock().unwrap();
        let count = state.sequences.len();
        state.sequences.clear();
        state.dirty = true;
        count
    }

    pub fn set_defaults(&self, salt: i32, include_world_seed: bool, include_sequence_id: bool) {
        let mut state = self.inner.lock().unwrap();
        state.salt = salt;
        state.include_world_seed = include_world_seed;
        state.include_sequence_id = include_sequence_id;
        state.dirty = true;
    }

    pub async fn flush(&self) {
        let (backend, data) = {
            let mut state = self.inner.lock().unwrap();
            let Some(backend) = state.backend.clone() else {
                return;
            };
            if !state.dirty {
                return;
            }
            state.dirty = false;
            let sequences: Compound = state
                .sequences
                .iter()
                .map(|(id, rng)| {
                    let (lo, hi) = rng.state();
                    (
                        id.clone(),
                        Tag::Compound(Compound::new().with("source", vec![lo, hi])),
                    )
                })
                .collect();
            let data = Compound::new()
                .with("salt", state.salt)
                .with("include_world_seed", state.include_world_seed)
                .with("include_sequence_id", state.include_sequence_id)
                .with("sequences", sequences);
            (backend, saved(data))
        };
        let written = tokio::task::spawn_blocking(move || {
            backend
                .documents()
                .write(Document::SavedData(SEQUENCES_DOCUMENT), data)
        })
        .await;
        if let Ok(Err(err)) = written {
            log::error!("failed to save random sequences: {err}");
        }
    }
}

fn random_between_inclusive(rng: &mut XoroshiroRandom, min: i32, max: i32) -> i32 {
    let span = (i64::from(max) - i64::from(min) + 1) as i32;
    rng.next_int_bounded(span).wrapping_add(min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::memory::MemoryBackend;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn stopwatches_survive_a_reload_with_their_elapsed_time() {
        let backend: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
        let first = Stopwatches::load(backend.clone());
        assert!(first.create("tuff:a"));
        assert!(first.create("tuff:b"));
        assert!(first.remove("tuff:b"));
        {
            let mut state = first.state.lock().unwrap();
            state.started.get_mut("tuff:a").unwrap().0 = 5_000;
        }
        first.flush().await;

        let second = Stopwatches::load(backend);
        assert_eq!(second.ids(), vec!["tuff:a".to_owned()]);
        assert!(second.elapsed_seconds("tuff:a").unwrap() >= 5.0);
        assert!(second.restart("tuff:a"));
        assert!(second.elapsed_seconds("tuff:a").unwrap() < 1.0);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn random_sequences_survive_a_reload_and_continue() {
        let backend: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
        let first = RandomSequences::load(backend.clone());
        first.set_defaults(7, true, false);
        let _ = first.sample(99, "tuff:a", 0, 1000);
        first.flush().await;
        let expected: Vec<i32> = (0..4)
            .map(|_| first.sample(99, "tuff:a", 0, 1000))
            .collect();

        let second = RandomSequences::load(backend);
        assert_eq!(second.ids(), vec!["tuff:a".to_owned()]);
        let resumed: Vec<i32> = (0..4)
            .map(|_| second.sample(99, "tuff:a", 0, 1000))
            .collect();
        assert_eq!(resumed, expected);
        assert_eq!(second.inner.lock().unwrap().salt, 7);
        assert!(!second.inner.lock().unwrap().include_sequence_id);
    }
}

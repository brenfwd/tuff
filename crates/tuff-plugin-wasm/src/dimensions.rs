//! Dimensions plugins make and discard while the server runs, such as one
//! arena per match. A runtime dimension copies an existing dimension's
//! type and generator under a key the owner granted with `cap.dimension`;
//! its chunks live in memory (the chunk storage actor's scratch stores),
//! never in the world's storage, so discarding it, unloading its plugin or
//! stopping the server leaves nothing behind. Clients need no new registry
//! entry: the dimension type is one they already know.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::chunk_io::{EntityWrite, IoOp, IoResult};
use tuff_core::world::dimension::{CreateError, DimensionId};
use tuff_core::world::messages::{self, CoordinatorMessage, ManagerMessage};
use tuff_world_model::pos::ChunkPos;

use crate::wit::bindings::types::{ChunkArea, DimensionError};

/// The most chunk columns one `create-dimension` copies.
pub const MAX_COPY_CHUNKS: i64 = 4096;

/// Chunks read and written per storage batch while copying.
const COPY_BATCH: usize = 256;

/// How long players get to leave a discarded dimension on their own.
const LEAVE_GRACE: Duration = Duration::from_secs(5);

/// How long players moved to the world spawn get to arrive there.
const MOVE_WAIT: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(100);

/// Whether `key` is a `namespace:path` resource location.
pub fn valid_key(key: &str) -> bool {
    let Some((namespace, path)) = key.split_once(':') else {
        return false;
    };
    let valid = |text: &str, extra: &str| {
        !text.is_empty()
            && text.len() <= 128
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(c))
    };
    valid(namespace, "_-.") && valid(path, "_-./")
}

/// The chunks of `area`, or `None` when it is inverted or too large.
pub fn chunks(area: &ChunkArea) -> Option<Vec<ChunkPos>> {
    let width = i64::from(area.max_x) - i64::from(area.min_x) + 1;
    let depth = i64::from(area.max_z) - i64::from(area.min_z) + 1;
    if width < 1 || depth < 1 || width * depth > MAX_COPY_CHUNKS {
        return None;
    }
    Some(
        (area.min_z..=area.max_z)
            .flat_map(|z| (area.min_x..=area.max_x).map(move |x| ChunkPos::new(x, z)))
            .collect(),
    )
}

/// Makes dimension `key` like `base` for plugin `owner`, after copying the
/// chunks `base` has saved among `copy` into its memory store.
pub async fn create(
    shared: &Arc<Shared>,
    owner: &str,
    key: &str,
    base: DimensionId,
    copy: &[ChunkPos],
) -> Result<DimensionId, DimensionError> {
    let dimensions = &shared.dimensions;
    if dimensions.is_reserved(key) {
        return Err(DimensionError::InvalidKey);
    }
    if dimensions.runtime_taken(key) {
        return Err(DimensionError::Exists);
    }
    let io = shared.chunk_io.get(&shared.backend);
    let kept = io
        .scratch(key, true)
        .await
        .map_err(|_| DimensionError::Unavailable)?;
    if !kept {
        return Err(DimensionError::Exists);
    }
    let made = async {
        let from = dimensions.get(base).key.clone();
        for batch in copy.chunks(COPY_BATCH) {
            let reads = batch.iter().map(|pos| IoOp::Read(*pos)).collect();
            let read = io.submit(&from, reads).await.ok()?;
            let writes: Vec<IoOp> = read
                .into_iter()
                .filter_map(|result| match result {
                    IoResult::Read {
                        pos,
                        blocks: Ok(Some(blocks)),
                        ..
                    } => Some(IoOp::Write {
                        pos,
                        blocks: Some(blocks),
                        entities: EntityWrite::Clear,
                        pois: EntityWrite::Clear,
                        ticket: None,
                    }),
                    _ => None,
                })
                .collect();
            io.submit(key, writes).await.ok()?;
        }
        Some(dimensions.create(key, base, owner))
    };
    let made = Unscratch {
        shared: Some(shared.clone()),
        key,
    }
    .run(made)
    .await;
    match made {
        Some(Ok(id)) => {
            log::info!("plugin {owner} made dimension {key}");
            Ok(id)
        }
        Some(Err(CreateError::Exists)) => Err(DimensionError::Exists),
        Some(Err(CreateError::Reserved)) => Err(DimensionError::InvalidKey),
        Some(Err(CreateError::Full)) => Err(DimensionError::OverBudget),
        None => Err(DimensionError::Unavailable),
    }
}

/// Drops a scratch store made for a dimension that never came to be,
/// also when the future making it is dropped (the guest's wait timed out).
struct Unscratch<'a> {
    shared: Option<Arc<Shared>>,
    key: &'a str,
}

impl Unscratch<'_> {
    async fn run(
        mut self,
        made: impl Future<Output = Option<Result<DimensionId, CreateError>>>,
    ) -> Option<Result<DimensionId, CreateError>> {
        let made = made.await;
        if matches!(made, Some(Ok(_))) {
            self.shared = None;
        }
        made
    }
}

impl Drop for Unscratch<'_> {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            drop(
                shared
                    .chunk_io
                    .get(&shared.backend)
                    .scratch(self.key, false),
            );
        }
    }
}

/// Who is in a dimension, and moving players out of one: the
/// coordinator, in the server.
pub trait Occupants: Sync {
    fn in_dimension(&self, dimension: DimensionId) -> impl Future<Output = Vec<u128>> + Send;
    fn to_spawn(&self, player: u128) -> impl Future<Output = ()> + Send;
}

struct Coordinator<'a>(&'a Arc<Shared>);

impl Occupants for Coordinator<'_> {
    async fn in_dimension(&self, dimension: DimensionId) -> Vec<u128> {
        crate::player_state::snapshots(self.0)
            .await
            .into_iter()
            .filter(|player| player.state.dimension == dimension)
            .map(|player| player.uuid)
            .collect()
    }

    async fn to_spawn(&self, player: u128) {
        let shared = self.0;
        let spawn = shared.spawn.borrow().clone();
        let target = Shared::resolve_spawn_dimension(&shared.dimensions, &spawn);
        let _ = shared
            .coordinator
            .send(CoordinatorMessage::TeleportPlayer {
                uuid: player,
                teleport: messages::Teleport {
                    dimension: target,
                    position: [
                        f64::from(spawn.pos.x) + 0.5,
                        f64::from(spawn.pos.y),
                        f64::from(spawn.pos.z) + 0.5,
                    ],
                    relative: [false; 3],
                    rotation: Some([spawn.yaw, spawn.pitch]),
                    relative_rotation: [false; 2],
                    look_at: None,
                },
            })
            .await;
    }
}

/// How long players get to leave, then to arrive at the spawn.
#[derive(Clone, Copy, Debug)]
pub struct Waits {
    pub grace: Duration,
    pub moving: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            grace: LEAVE_GRACE,
            moving: MOVE_WAIT,
        }
    }
}

/// Waits up to `limit` for `dimension` to be empty of players; returns
/// those still there.
async fn wait_empty(
    occupants: &impl Occupants,
    dimension: DimensionId,
    limit: Duration,
) -> Vec<u128> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let left = occupants.in_dimension(dimension).await;
        if left.is_empty() || tokio::time::Instant::now() >= deadline {
            return left;
        }
        tokio::time::sleep(POLL.min(limit)).await;
    }
}

/// Gives players `waits.grace` to leave `dimension`, then moves those
/// still there to the world spawn and waits for them to arrive; returns
/// any who never left.
pub async fn evacuate(
    occupants: &impl Occupants,
    dimension: DimensionId,
    waits: Waits,
) -> Vec<u128> {
    let stuck = wait_empty(occupants, dimension, waits.grace).await;
    if stuck.is_empty() {
        return stuck;
    }
    for player in stuck {
        occupants.to_spawn(player).await;
    }
    wait_empty(occupants, dimension, waits.moving).await
}

/// Finishes discarding runtime dimension `id` (already taken out of
/// `by_key`): moves players still there to the world spawn, drops its
/// regions and memory store, and frees its key.
pub async fn discard(shared: Arc<Shared>, id: DimensionId) {
    if !shared.dimensions.is_discarded(id) {
        return;
    }
    let key = shared.dimensions.get(id).key.clone();
    let left = evacuate(&Coordinator(&shared), id, Waits::default()).await;
    if !left.is_empty() {
        log::warn!(
            "{} players were still in discarded dimension {key}",
            left.len()
        );
    }
    let (done, dropped) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::DropDimension {
            dimension: id,
            done,
        })
        .await
        .is_ok()
    {
        let _ = dropped.await;
    }
    let _ = shared
        .chunk_io
        .get(&shared.backend)
        .scratch(&key, false)
        .await;
    shared.dimensions.forget(id);
    log::info!("dimension {key} was discarded");
}

/// Discards every dimension plugin `owner` made, for its unload.
pub fn discard_all(shared: &Arc<Shared>, owner: &str) {
    for dimension in shared.dimensions.runtime() {
        if shared.dimensions.owner(dimension.id) != Some(owner) {
            continue;
        }
        shared.dimensions.discard(dimension.id);
        tokio::spawn(discard(shared.clone(), dimension.id));
    }
}

/// The keys of the live dimensions plugin `owner` made.
pub fn owned(shared: &Shared, owner: &str) -> Vec<String> {
    shared
        .dimensions
        .runtime()
        .into_iter()
        .filter(|d| {
            shared.dimensions.owner(d.id) == Some(owner) && !shared.dimensions.is_discarded(d.id)
        })
        .map(|d| d.key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;

    const ARENA: DimensionId = DimensionId::for_tests(5);

    /// Players in the arena; `stubborn` ones ignore being moved.
    #[derive(Default)]
    struct Fake {
        inside: Mutex<HashSet<u128>>,
        stubborn: HashSet<u128>,
        moved: Mutex<Vec<u128>>,
    }

    impl Occupants for Fake {
        async fn in_dimension(&self, dimension: DimensionId) -> Vec<u128> {
            assert_eq!(dimension, ARENA);
            let mut inside: Vec<u128> = self.inside.lock().unwrap().iter().copied().collect();
            inside.sort();
            inside
        }

        async fn to_spawn(&self, player: u128) {
            self.moved.lock().unwrap().push(player);
            if !self.stubborn.contains(&player) {
                self.inside.lock().unwrap().remove(&player);
            }
        }
    }

    const WAITS: Waits = Waits {
        grace: Duration::from_millis(30),
        moving: Duration::from_millis(60),
    };

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn an_empty_dimension_is_left_at_once() {
        let fake = Fake::default();
        let started = tokio::time::Instant::now();
        assert!(evacuate(&fake, ARENA, Waits::default()).await.is_empty());
        assert!(started.elapsed() < LEAVE_GRACE);
        assert!(fake.moved.lock().unwrap().is_empty());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn players_who_stay_past_the_grace_are_moved_to_the_spawn() {
        let fake = Fake {
            inside: Mutex::new(HashSet::from([1, 2])),
            stubborn: HashSet::from([2]),
            ..Fake::default()
        };
        let started = tokio::time::Instant::now();
        assert_eq!(evacuate(&fake, ARENA, WAITS).await, [2]);
        assert!(started.elapsed() >= WAITS.grace + WAITS.moving);
        assert_eq!(*fake.moved.lock().unwrap(), [1, 2]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn copy_areas_are_bounded() {
        let area = |min_x, min_z, max_x, max_z| ChunkArea {
            min_x,
            min_z,
            max_x,
            max_z,
        };
        assert_eq!(chunks(&area(0, 0, 0, 0)), Some(vec![ChunkPos::new(0, 0)]));
        assert_eq!(chunks(&area(-1, 0, 0, 1)).map(|c| c.len()), Some(4));
        assert_eq!(chunks(&area(0, 0, 63, 63)).map(|c| c.len()), Some(4096));
        assert_eq!(chunks(&area(0, 0, 64, 63)), None);
        assert_eq!(chunks(&area(1, 0, 0, 0)), None);
        assert_eq!(chunks(&area(i32::MIN, 0, i32::MAX, 0)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn keys_are_namespaced_ids() {
        for good in ["game:arena_1", "a.b-c:d/e"] {
            assert!(valid_key(good), "{good}");
        }
        for bad in [
            "arena",
            "Game:arena",
            "game:",
            ":arena",
            "game:a b",
            "game:a*",
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
    }
}

//! Block reads and writes for plugins, each run on the region that owns
//! the blocks (regions in parallel), holding a ticket on the region only
//! while its part runs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rayon::iter::{IntoParallelIterator, ParallelIterator};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::generator::ChunkGenerator;
use tuff_core::world::messages::{
    BlockOp, BlockOpResult, CoordinatorMessage, ManagerMessage, RegionRequest,
};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// How long a guest waits on the world before its read fails.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// A block write a plugin queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockWrite {
    pub dimension: DimensionId,
    pub pos: BlockPos,
    pub state: BlockState,
    pub updates: bool,
}

/// A biome fill a plugin queued: every 4x4x4 cell holding a block of the
/// inclusive box from `min` to `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomeWrite {
    pub dimension: DimensionId,
    pub min: BlockPos,
    pub max: BlockPos,
    pub biome: u16,
}

impl BiomeWrite {
    /// How many cells the box covers.
    pub fn cells(&self) -> u64 {
        let span = |lo: i32, hi: i32| ((i64::from(hi) >> 2) - (i64::from(lo) >> 2) + 1) as u64;
        span(self.min.x, self.max.x)
            .saturating_mul(span(self.min.y, self.max.y))
            .saturating_mul(span(self.min.z, self.max.z))
    }
}

/// The server a sandbox reaches from guest code, which runs on a blocking
/// thread and waits here for the actors that answer.
#[derive(Clone)]
pub struct Env {
    pub shared: Arc<Shared>,
    pub runtime: tokio::runtime::Handle,
}

impl Env {
    /// Runs `future` to completion from guest code; `None` on timeout.
    pub fn wait<F: Future>(&self, future: F) -> Option<F::Output> {
        self.runtime
            .block_on(async { tokio::time::timeout(TIMEOUT, future).await.ok() })
    }

    /// The dimension called `key`, the `minecraft:` namespace optional.
    pub fn dimension(&self, key: &str) -> Option<DimensionId> {
        let dimensions = &self.shared.dimensions;
        match key.contains(':') {
            true => dimensions.by_key(key),
            false => dimensions.by_key(&format!("minecraft:{key}")),
        }
        .map(|dimension| dimension.id)
    }

    /// Whether height `y` is inside `dimension`'s build height.
    pub fn inside(&self, dimension: DimensionId, y: i32) -> bool {
        inside(&self.shared, dimension, y)
    }

    pub fn level_of(&self, uuid: u128) -> PermissionLevel {
        self.wait(async {
            let (reply, answer) = oneshot::channel();
            self.shared
                .coordinator
                .send(CoordinatorMessage::PermissionOf { uuid, reply })
                .await
                .ok()?;
            answer.await.ok()
        })
        .flatten()
        .unwrap_or(PermissionLevel::All)
    }
}

/// Parses a block state in command syntax, the namespace optional.
pub fn parse_state(text: &str) -> Option<BlockState> {
    let name_end = text.find('[').unwrap_or(text.len());
    if text[..name_end].contains(':') {
        text.parse().ok()
    } else {
        format!("minecraft:{text}").parse().ok()
    }
}

fn inside(shared: &Shared, dimension: DimensionId, y: i32) -> bool {
    let shape = shared.dimensions.get(dimension).shape;
    (shape.min_y..shape.min_y + shape.height).contains(&y)
}

fn region_of(pos: BlockPos) -> RegionPos {
    ChunkPos::of_block(pos).region()
}

async fn on_region(
    shared: &Shared,
    dimension: DimensionId,
    region: RegionPos,
    op: BlockOp,
) -> Option<BlockOpResult> {
    let (reply, handle) = oneshot::channel();
    shared
        .manager
        .send(ManagerMessage::Acquire {
            dimension,
            region,
            reply,
        })
        .await
        .ok()?;
    let handle = handle.await.ok()?;
    let (reply, answer) = oneshot::channel();
    let result = match handle.request(RegionRequest::Blocks { op, reply }).await {
        true => answer.await.ok(),
        false => None,
    };
    let _ = shared
        .manager
        .send(ManagerMessage::Release { dimension, region })
        .await;
    result
}

/// The states at `positions`, loading the chunks they are in; `None`
/// where a position is outside the world or its region did not answer.
pub async fn read(
    shared: Arc<Shared>,
    dimension: DimensionId,
    positions: Vec<BlockPos>,
) -> Vec<Option<BlockState>> {
    let mut groups: BTreeMap<RegionPos, Vec<usize>> = BTreeMap::new();
    for (index, pos) in positions.iter().enumerate() {
        if inside(&shared, dimension, pos.y) {
            groups.entry(region_of(*pos)).or_default().push(index);
        }
    }
    let mut out = vec![None; positions.len()];
    let mut tasks = JoinSet::new();
    for (region, indices) in groups {
        let shared = shared.clone();
        let op = BlockOp::Get {
            positions: indices.iter().map(|&i| positions[i]).collect(),
            load: true,
        };
        tasks.spawn(async move {
            let result = on_region(&shared, dimension, region, op).await;
            (indices, result)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        if let Ok((indices, Some(BlockOpResult::States(states)))) = joined {
            for (index, state) in indices.into_iter().zip(states) {
                out[index] = state;
            }
        }
    }
    out
}

/// The blocks the world generator makes at `positions`, whole chunks
/// generated in parallel on the rayon pool without loading or touching the
/// world. `None` where a position lies outside the dimension's height.
/// `generator` replaces the dimension's own when given.
pub async fn generate(
    shared: Arc<Shared>,
    dimension: DimensionId,
    positions: Vec<BlockPos>,
    generator: Option<Arc<dyn ChunkGenerator>>,
) -> Vec<Option<BlockState>> {
    let tags = tuff_datapack::tags::TagFeed::new(shared.datapacks.watch_contents()).current();
    let (reply, answer) = oneshot::channel();
    rayon::spawn(move || {
        let dim = shared.dimension(dimension);
        let generator = generator.unwrap_or_else(|| dim.generator.clone());
        let mut chunks: BTreeMap<ChunkPos, Vec<usize>> = BTreeMap::new();
        for (index, pos) in positions.iter().enumerate() {
            if inside(&shared, dimension, pos.y) {
                chunks
                    .entry(ChunkPos::of_block(*pos))
                    .or_default()
                    .push(index);
            }
        }
        let made: Vec<(Vec<usize>, Vec<BlockState>)> = chunks
            .into_par_iter()
            .map(|(chunk_pos, indices)| {
                let tags = tags.clone();
                tuff_datapack::tags::sync_scope(tags, || {
                    let chunk = generator.generate(chunk_pos, dim.shape);
                    let states = indices
                        .iter()
                        .map(|&i| chunk.get(positions[i].x, positions[i].y, positions[i].z))
                        .collect();
                    (indices, states)
                })
            })
            .collect();
        let mut out = vec![None; positions.len()];
        for (indices, states) in made {
            for (index, state) in indices.into_iter().zip(states) {
                out[index] = Some(state);
            }
        }
        let _ = reply.send(out);
    });
    answer.await.unwrap_or_default()
}

/// Applies `writes`: in order within each region, regions in parallel.
/// How many blocks changed.
pub async fn write(shared: Arc<Shared>, writes: Vec<BlockWrite>) -> usize {
    let mut groups: BTreeMap<(DimensionId, RegionPos), Vec<BlockWrite>> = BTreeMap::new();
    for write in writes {
        groups
            .entry((write.dimension, region_of(write.pos)))
            .or_default()
            .push(write);
    }
    let mut tasks = JoinSet::new();
    for ((dimension, region), writes) in groups {
        let shared = shared.clone();
        tasks.spawn(async move {
            let mut changed = 0;
            for run in writes.chunk_by(|a, b| a.updates == b.updates) {
                let op = BlockOp::Set {
                    changes: run.iter().map(|w| (w.pos, w.state)).collect(),
                    destroy: false,
                    keep: false,
                    strict: !run[0].updates,
                    quiet: false,
                    data: None,
                };
                if let Some(BlockOpResult::Set(n)) = on_region(&shared, dimension, region, op).await
                {
                    changed += n;
                }
            }
            changed
        });
    }
    let mut changed = 0;
    while let Some(joined) = tasks.join_next().await {
        changed += joined.unwrap_or(0);
    }
    changed
}

/// The biome names at `positions`, loading the chunks they are in; `None`
/// where a position is outside the world or its region did not answer.
pub async fn read_biomes(
    shared: Arc<Shared>,
    dimension: DimensionId,
    positions: Vec<BlockPos>,
) -> Vec<Option<&'static str>> {
    let mut groups: BTreeMap<RegionPos, Vec<usize>> = BTreeMap::new();
    for (index, pos) in positions.iter().enumerate() {
        if inside(&shared, dimension, pos.y) {
            groups.entry(region_of(*pos)).or_default().push(index);
        }
    }
    let mut out = vec![None; positions.len()];
    let mut tasks = JoinSet::new();
    for (region, indices) in groups {
        let shared = shared.clone();
        let op = BlockOp::Biomes {
            positions: indices.iter().map(|&i| positions[i]).collect(),
            load: true,
        };
        tasks.spawn(async move {
            let result = on_region(&shared, dimension, region, op).await;
            (indices, result)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        if let Ok((indices, Some(BlockOpResult::Biomes(names)))) = joined {
            for (index, name) in indices.into_iter().zip(names) {
                out[index] = name;
            }
        }
    }
    out
}

/// Applies biome fills in order, each across the regions its box touches
/// in parallel; chunks that are not loaded are skipped. How many cells
/// changed.
pub async fn write_biomes(shared: Arc<Shared>, writes: Vec<BiomeWrite>) -> usize {
    let mut changed = 0;
    for write in writes {
        let quantize = |pos: BlockPos| BlockPos::new(pos.x & !3, pos.y & !3, pos.z & !3);
        let (min, max) = (quantize(write.min), quantize(write.max));
        let mut regions = std::collections::BTreeSet::new();
        for z in (min.z >> 4)..=(max.z >> 4) {
            for x in (min.x >> 4)..=(max.x >> 4) {
                regions.insert(ChunkPos { x, z }.region());
            }
        }
        let mut tasks = JoinSet::new();
        for region in regions {
            let shared = shared.clone();
            let op = BlockOp::FillBiome {
                min,
                max,
                biome: write.biome,
                filter: None,
                apply: true,
            };
            let dimension = write.dimension;
            tasks.spawn(async move { on_region(&shared, dimension, region, op).await });
        }
        while let Some(joined) = tasks.join_next().await {
            if let Ok(Some(BlockOpResult::Set(n))) = joined {
                changed += n;
            }
        }
    }
    changed
}

/// An entity a plugin query found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub uuid: u128,
    pub kind: &'static str,
    pub at: [f64; 3],
}

/// The entities inside the box from `min` to `max` (inclusive) in the
/// loaded regions of `dimension`, each region asked on its own actor in
/// parallel and none loaded for it. Sorted by uuid and cut at `limit`.
pub async fn entities_in(
    shared: Arc<Shared>,
    dimension: DimensionId,
    min: [f64; 3],
    max: [f64; 3],
    limit: usize,
) -> Vec<Found> {
    let (reply, regions) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let low = region_of(BlockPos::new(
        min[0].floor() as i32,
        0,
        min[2].floor() as i32,
    ));
    let high = region_of(BlockPos::new(
        max[0].floor() as i32,
        0,
        max[2].floor() as i32,
    ));
    let mut tasks = JoinSet::new();
    for region in regions.await.unwrap_or_default() {
        let pos = region.pos;
        if region.dimension != dimension
            || !(low.x..=high.x).contains(&pos.x)
            || !(low.z..=high.z).contains(&pos.z)
        {
            continue;
        }
        tasks.spawn(async move {
            region
                .ask(move |world| {
                    let mut found = Vec::new();
                    for id in world.entity_ids() {
                        let Some(tracked) = world.entity(id) else {
                            continue;
                        };
                        let base = tracked.entity.base();
                        let at = base.position;
                        if (0..3).all(|axis| (min[axis]..=max[axis]).contains(&at[axis])) {
                            found.push(Found {
                                uuid: base.uuid,
                                kind: base.kind.name(),
                                at,
                            });
                        }
                    }
                    found
                })
                .await
                .unwrap_or_default()
        });
    }
    let mut out = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        out.extend(joined.unwrap_or_default());
    }
    out.sort_by_key(|found| found.uuid);
    out.truncate(limit);
    out
}

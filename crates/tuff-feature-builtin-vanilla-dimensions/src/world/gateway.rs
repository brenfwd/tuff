//! End gateways: where one leads, found or built on the outer islands the
//! first time something passes through, and where travellers land.
//!
//! Regions own each gateway's block entity (saved NBT) and cooldown. The
//! exit search reads and builds far from the gateway, so it runs in
//! whoever travels (a session, or a task a region spawns for its
//! entities) and talks to the regions it needs; the gateway's region
//! makes sure only one search per gateway runs at a time.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::Shared;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{
    BlockOp, BlockOpResult, ManagerMessage, RegionHandle, RegionRequest,
};
use tuff_item::random::LegacyRandom;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::end_spikes::island_blocks;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::{ChunkPos, RegionPos};

pub const ID: &str = "minecraft:end_gateway";
/// Ticks a gateway shows its beam and lets nothing through after a use.
pub const COOLDOWN_TICKS: u64 = 40;
/// A gateway flashes its beam this often on its own.
pub const ATTENTION_INTERVAL: i64 = 2400;
/// A new gateway's spawning beam lasts this long.
pub const SPAWN_TICKS: i64 = 200;
/// The block event that starts a gateway's cooldown beam on clients.
pub const BEAM_EVENT: u8 = 1;
/// Entities other than players wait this long before using a portal again.
pub const ENTITY_PORTAL_COOLDOWN: i32 = 300;
const EXIT_DISTANCE: f64 = 1024.0;
const CHUNK_STEPS: i32 = 16;
const ISLAND_Y: f64 = 75.0;
const EXIT_SEARCH_RADIUS: i32 = 16;
const EXIT_ABOVE_SURFACE: i32 = 10;
const ARRIVAL_RADIUS: i32 = 5;
const SPAWN_SEARCH_MIN_Y: i32 = 30;
const SPAWNABLE_XZ: i32 = 30_000_000;
const SPAWNABLE_Y: i32 = 20_000_000;

/// What a gateway's region answers about where it leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitLookup {
    Known {
        exit: BlockPos,
        exact: bool,
    },
    /// No exit yet: the asker builds one and reports it with `SetExit`.
    Build {
        exact: bool,
    },
    /// No exit and none is built here (outside the end, or not a gateway).
    Missing,
}

/// A region's part in gateway travel.
#[derive(Debug)]
pub enum GatewayRequest {
    Exit {
        pos: BlockPos,
        reply: oneshot::Sender<ExitLookup>,
    },
    /// Stores the exit of the gateway at `pos` (none when the search
    /// failed) and answers everyone waiting on that search.
    SetExit {
        pos: BlockPos,
        exit: Option<BlockPos>,
        exact: bool,
    },
    ChunkEmpty {
        chunk: ChunkPos,
        reply: oneshot::Sender<bool>,
    },
    /// The end stone block of a chunk closest to the world origin with
    /// room above it.
    SpawnInChunk {
        chunk: ChunkPos,
        reply: oneshot::Sender<Option<BlockPos>>,
    },
    /// The top full block of each column this region owns in the square
    /// of `radius` around `around`, as `(x, z, y)`.
    ColumnTops {
        around: BlockPos,
        radius: i32,
        allow_bedrock: bool,
        reply: oneshot::Sender<Vec<(i32, i32, i32)>>,
    },
}

impl GatewayRequest {
    /// The blocks whose chunks must be loaded to answer.
    pub fn area(&self) -> (BlockPos, BlockPos) {
        match self {
            Self::Exit { pos, .. } | Self::SetExit { pos, .. } => (*pos, *pos),
            Self::ChunkEmpty { chunk, .. } | Self::SpawnInChunk { chunk, .. } => {
                let min = BlockPos::new(chunk.x * 16, 0, chunk.z * 16);
                (min, min)
            }
            Self::ColumnTops { around, radius, .. } => (
                BlockPos::new(around.x - radius, 0, around.z - radius),
                BlockPos::new(around.x + radius, 0, around.z + radius),
            ),
        }
    }
}

fn int(tag: &Tag) -> Option<i64> {
    match tag {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        _ => None,
    }
}

fn spawnable(pos: BlockPos) -> bool {
    (-SPAWNABLE_XZ..SPAWNABLE_XZ).contains(&pos.x)
        && (-SPAWNABLE_XZ..SPAWNABLE_XZ).contains(&pos.z)
        && (-SPAWNABLE_Y..SPAWNABLE_Y).contains(&pos.y)
}

pub fn is_gateway(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(ID)
}

pub fn age(nbt: &Compound) -> i64 {
    nbt.get("Age").and_then(int).unwrap_or(0)
}

pub fn set_age(nbt: &mut Compound, age: i64) {
    nbt.insert("Age", Tag::Long(age));
}

pub fn exact(nbt: &Compound) -> bool {
    nbt.get("ExactTeleport")
        .and_then(int)
        .is_some_and(|v| v != 0)
}

/// Where the gateway leads, if it knows.
pub fn exit(nbt: &Compound) -> Option<BlockPos> {
    match nbt.get("exit_portal")? {
        Tag::IntArray(v) if v.len() == 3 => {
            Some(BlockPos::new(v[0], v[1], v[2])).filter(|p| spawnable(*p))
        }
        _ => None,
    }
}

pub fn set_exit(nbt: &mut Compound, exit: BlockPos, exact: bool) {
    nbt.insert("exit_portal", Tag::IntArray(vec![exit.x, exit.y, exit.z]));
    if exact {
        nbt.insert("ExactTeleport", true);
    } else {
        nbt.remove("ExactTeleport");
    }
}

/// A gateway with its bedrock caps, centred on `origin`.
pub fn gateway_blocks(origin: BlockPos) -> Vec<(BlockPos, BlockState)> {
    let air = Block::Air.default_state();
    let bedrock = Block::Bedrock.default_state();
    let mut blocks = Vec::new();
    for z in origin.z - 1..=origin.z + 1 {
        for y in origin.y - 2..=origin.y + 2 {
            for x in origin.x - 1..=origin.x + 1 {
                let (same_x, same_y, same_z) = (x == origin.x, y == origin.y, z == origin.z);
                let end = (y - origin.y).abs() == 2;
                let state = if same_x && same_y && same_z {
                    Block::EndGateway.default_state()
                } else if same_y {
                    air
                } else if end && same_x && same_z {
                    bedrock
                } else if (!same_x && !same_z) || end {
                    air
                } else {
                    bedrock
                };
                blocks.push((BlockPos::new(x, y, z), state));
            }
        }
    }
    blocks
}

fn highest_filled_section(chunk: &Chunk) -> Option<usize> {
    chunk
        .sections()
        .enumerate()
        .rev()
        .find(|(_, s)| !s.has_only_air())
        .map(|(i, _)| i)
}

/// Whether a chunk holds nothing but air.
pub fn chunk_is_empty(chunk: &Chunk) -> bool {
    highest_filled_section(chunk).is_none()
}

/// The end stone block of `chunk` closest to the world origin whose two
/// blocks above are not full, searching from y 30 up to the top of its
/// highest filled section.
pub fn spawn_in_chunk(chunk: &Chunk, pos: ChunkPos) -> Option<BlockPos> {
    let shape = chunk.shape();
    let top =
        highest_filled_section(chunk).map_or(shape.min_y - 16, |i| shape.section_base(i)) + 15;
    let full = |x: i32, y: i32, z: i32| chunk.get(x, y, z).is_collision_full_block();
    let mut closest: Option<(BlockPos, f64)> = None;
    for z in 0..16 {
        for y in SPAWN_SEARCH_MIN_Y..=top {
            for x in 0..16 {
                if chunk.get(x, y, z).block() != Block::EndStone
                    || full(x, y + 1, z)
                    || full(x, y + 2, z)
                {
                    continue;
                }
                let at = BlockPos::new(pos.x * 16 + x, y, pos.z * 16 + z);
                let center = at.center();
                let distance =
                    center[0] * center[0] + center[1] * center[1] + center[2] * center[2];
                if closest.is_none_or(|(_, best)| distance < best) {
                    closest = Some((at, distance));
                }
            }
        }
    }
    closest.map(|(at, _)| at)
}

/// The top full block of a column (bedrock only if allowed), above the
/// bottom of the world.
pub fn column_top(
    block: impl Fn(i32) -> Option<BlockState>,
    min_y: i32,
    max_y: i32,
    allow_bedrock: bool,
) -> Option<i32> {
    (min_y + 1..=max_y).rev().find(|y| {
        block(*y).is_some_and(|state| {
            state.is_collision_full_block() && (allow_bedrock || state.block() != Block::Bedrock)
        })
    })
}

/// The tallest full block around `around`: the first column, row by row
/// from the negative corner, reaching the greatest height. The centre
/// column only counts when bedrock does.
pub fn tallest(
    tops: &HashMap<(i32, i32), i32>,
    around: BlockPos,
    radius: i32,
    allow_bedrock: bool,
) -> BlockPos {
    let mut best: Option<BlockPos> = None;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            if dx == 0 && dz == 0 && !allow_bedrock {
                continue;
            }
            let (x, z) = (around.x + dx, around.z + dz);
            if let Some(&y) = tops.get(&(x, z))
                && best.is_none_or(|b| y > b.y)
            {
                best = Some(BlockPos::new(x, y, z));
            }
        }
    }
    best.unwrap_or(around)
}

/// The columns a square search around `around` reads.
pub fn in_square(around: BlockPos, radius: i32, allow_bedrock: bool, x: i32, z: i32) -> bool {
    let (dx, dz) = (x - around.x, z - around.z);
    dx.abs() <= radius && dz.abs() <= radius && (allow_bedrock || dx != 0 || dz != 0)
}

/// The regions a travelling search borrows, released together.
struct Regions<'a> {
    shared: &'a Arc<Shared>,
    dimension: DimensionId,
    held: HashMap<RegionPos, RegionHandle>,
}

impl<'a> Regions<'a> {
    fn new(shared: &'a Arc<Shared>, dimension: DimensionId) -> Self {
        Self {
            shared,
            dimension,
            held: HashMap::new(),
        }
    }

    async fn get(&mut self, region: RegionPos) -> Option<RegionHandle> {
        if let Some(handle) = self.held.get(&region) {
            return Some(handle.clone());
        }
        let (reply, handle) = oneshot::channel();
        self.shared
            .manager
            .send(ManagerMessage::Acquire {
                dimension: self.dimension,
                region,
                reply,
            })
            .await
            .ok()?;
        let handle = handle.await.ok()?;
        self.held.insert(region, handle.clone());
        Some(handle)
    }

    async fn ask<T>(
        &mut self,
        region: RegionPos,
        request: impl FnOnce(oneshot::Sender<T>) -> GatewayRequest,
    ) -> Option<T> {
        let handle = self.get(region).await?;
        let (reply, answer) = oneshot::channel();
        handle.request(when_loaded(request(reply))).await;
        answer.await.ok()
    }

    async fn tell(&mut self, region: RegionPos, request: GatewayRequest) {
        if let Some(handle) = self.get(region).await {
            handle.request(when_loaded(request)).await;
        }
    }

    async fn chunk_empty(&mut self, x: f64, z: f64) -> bool {
        let chunk = ChunkPos::new((x / 16.0).floor() as i32, (z / 16.0).floor() as i32);
        self.ask(chunk.region(), |reply| GatewayRequest::ChunkEmpty {
            chunk,
            reply,
        })
        .await
        .unwrap_or(false)
    }

    /// Sets blocks in whichever regions own them, loading their chunks.
    async fn set_blocks(&mut self, changes: Vec<(BlockPos, BlockState)>) {
        let mut by_region: HashMap<RegionPos, Vec<(BlockPos, BlockState)>> = HashMap::new();
        for (pos, state) in changes {
            by_region
                .entry(ChunkPos::of_block(pos).region())
                .or_default()
                .push((pos, state));
        }
        let mut regions: Vec<RegionPos> = by_region.keys().copied().collect();
        regions.sort_by_key(|r| (r.x, r.z));
        for region in regions {
            let Some(handle) = self.get(region).await else {
                continue;
            };
            let (reply, done) = oneshot::channel();
            handle
                .request(RegionRequest::Blocks {
                    op: BlockOp::Set {
                        changes: by_region.remove(&region).unwrap_or_default(),
                        destroy: false,
                        keep: false,
                        strict: false,
                        quiet: false,
                        data: None,
                    },
                    reply,
                })
                .await;
            let _: Result<BlockOpResult, _> = done.await;
        }
    }

    async fn tallest(&mut self, around: BlockPos, radius: i32, allow_bedrock: bool) -> BlockPos {
        let (min, max) = (
            ChunkPos::of_block(BlockPos::new(around.x - radius, 0, around.z - radius)).region(),
            ChunkPos::of_block(BlockPos::new(around.x + radius, 0, around.z + radius)).region(),
        );
        let mut tops = HashMap::new();
        for x in min.x..=max.x {
            for z in min.z..=max.z {
                let found = self
                    .ask(RegionPos::new(x, z), |reply| GatewayRequest::ColumnTops {
                        around,
                        radius,
                        allow_bedrock,
                        reply,
                    })
                    .await
                    .unwrap_or_default();
                tops.extend(found.into_iter().map(|(x, z, y)| ((x, z), y)));
            }
        }
        tallest(&tops, around, radius, allow_bedrock)
    }

    async fn release(self) {
        for region in self.held.into_keys() {
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release {
                    dimension: self.dimension,
                    region,
                })
                .await;
        }
    }
}

/// Where something entering the gateway at `gateway` comes out: the
/// bottom centre of the block above the tallest block near the exit (or
/// the exit itself for an exact gateway). Builds the exit first if the
/// gateway has none.
pub async fn destination(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    gateway: BlockPos,
) -> Option<[f64; 3]> {
    let mut regions = Regions::new(shared, dimension);
    let found = travel(&mut regions, gateway).await;
    regions.release().await;
    found
}

async fn travel(regions: &mut Regions<'_>, gateway: BlockPos) -> Option<[f64; 3]> {
    let home = ChunkPos::of_block(gateway).region();
    let lookup = regions
        .ask(home, |reply| GatewayRequest::Exit {
            pos: gateway,
            reply,
        })
        .await?;
    let (exit, exact) = match lookup {
        ExitLookup::Known { exit, exact } => (exit, exact),
        ExitLookup::Build { exact } => {
            let exit = build_exit(regions, gateway).await;
            regions
                .tell(
                    home,
                    GatewayRequest::SetExit {
                        pos: gateway,
                        exit,
                        exact,
                    },
                )
                .await;
            (exit?, exact)
        }
        ExitLookup::Missing => return None,
    };
    let at = if exact {
        exit
    } else {
        let above = BlockPos::new(exit.x, exit.y + 2, exit.z);
        let top = regions.tallest(above, ARRIVAL_RADIUS, false).await;
        BlockPos::new(top.x, top.y + 1, top.z)
    };
    Some([
        f64::from(at.x) + 0.5,
        f64::from(at.y),
        f64::from(at.z) + 0.5,
    ])
}

/// Walks from 1024 blocks out along the gateway's direction back past
/// solid chunks and then out past empty ones, up to 16 chunks each way.
async fn tentative_exit(regions: &mut Regions<'_>, entry: BlockPos) -> [f64; 2] {
    let (x, z) = (f64::from(entry.x), f64::from(entry.z));
    let length = (x * x + z * z).sqrt();
    let direction = if length < f64::from(1.0e-5f32) {
        [0.0, 0.0]
    } else {
        [x / length, z / length]
    };
    let mut at = [direction[0] * EXIT_DISTANCE, direction[1] * EXIT_DISTANCE];
    let mut limit = CHUNK_STEPS;
    while !regions.chunk_empty(at[0], at[1]).await {
        limit -= 1;
        if limit < 0 {
            break;
        }
        at = [at[0] + direction[0] * -16.0, at[1] + direction[1] * -16.0];
    }
    let mut limit = CHUNK_STEPS;
    while regions.chunk_empty(at[0], at[1]).await {
        limit -= 1;
        if limit < 0 {
            break;
        }
        at = [at[0] + direction[0] * 16.0, at[1] + direction[1] * 16.0];
    }
    at
}

/// Finds land for the exit (or raises an island), then builds the exit
/// gateway ten blocks above the tallest block near it, leading back.
async fn build_exit(regions: &mut Regions<'_>, entry: BlockPos) -> Option<BlockPos> {
    let [x, z] = tentative_exit(regions, entry).await;
    let chunk = ChunkPos::new((x / 16.0).floor() as i32, (z / 16.0).floor() as i32);
    let spawn = regions
        .ask(chunk.region(), |reply| GatewayRequest::SpawnInChunk {
            chunk,
            reply,
        })
        .await?;
    let base = match spawn {
        Some(pos) => pos,
        None => {
            let island = BlockPos::new(
                (x + 0.5).floor() as i32,
                ISLAND_Y as i32,
                (z + 0.5).floor() as i32,
            );
            let mut random = LegacyRandom::new(island.pack());
            let end_stone = Block::EndStone.default_state();
            let blocks = island_blocks(&mut random, island)
                .into_iter()
                .map(|pos| (pos, end_stone))
                .collect();
            regions.set_blocks(blocks).await;
            island
        }
    };
    let top = regions.tallest(base, EXIT_SEARCH_RADIUS, true).await;
    let exit = BlockPos::new(top.x, top.y + EXIT_ABOVE_SURFACE, top.z);
    regions.set_blocks(gateway_blocks(exit)).await;
    regions
        .tell(
            ChunkPos::of_block(exit).region(),
            GatewayRequest::SetExit {
                pos: exit,
                exit: Some(entry),
                exact: false,
            },
        )
        .await;
    Some(exit)
}

/// A gateway request for the region owning its area, answered once the
/// chunks it reads are loaded.
pub fn when_loaded(request: GatewayRequest) -> RegionRequest {
    let (min, max) = request.area();
    RegionRequest::WhenLoaded {
        min,
        max,
        run: tuff_core::world::region_api::RegionTask::new(move |world| {
            crate::world::region::gateways::answer(world, request)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_world_model::chunk::ChunkShape;

    #[tuff_core::watchdog]
    #[test]
    fn exits_round_trip_and_drop_unspawnable_positions() {
        let mut nbt = Compound::new().with("id", ID);
        assert_eq!(exit(&nbt), None);
        set_exit(&mut nbt, BlockPos::new(1000, 80, -20), true);
        assert_eq!(exit(&nbt), Some(BlockPos::new(1000, 80, -20)));
        assert!(exact(&nbt));
        set_exit(&mut nbt, BlockPos::new(1000, 80, -20), false);
        assert!(!exact(&nbt) && nbt.get("ExactTeleport").is_none());
        nbt.insert("exit_portal", Tag::IntArray(vec![40_000_000, 0, 0]));
        assert_eq!(exit(&nbt), None);
        assert_eq!(age(&nbt), 0);
        set_age(&mut nbt, 2400);
        assert_eq!(age(&nbt), 2400);
    }

    #[tuff_core::watchdog]
    #[test]
    fn gateways_are_bedrock_capped() {
        let blocks = gateway_blocks(BlockPos::new(10, 75, 10));
        let at = |x, y, z| {
            blocks
                .iter()
                .find(|(p, _)| *p == BlockPos::new(x, y, z))
                .map(|(_, s)| s.block())
        };
        assert_eq!(at(10, 75, 10), Some(Block::EndGateway));
        assert_eq!(at(10, 77, 10), Some(Block::Bedrock));
        assert_eq!(at(10, 76, 10), Some(Block::Bedrock));
        assert_eq!(at(11, 76, 10), Some(Block::Bedrock));
        assert_eq!(at(11, 76, 11), Some(Block::Air));
        assert_eq!(at(11, 77, 10), Some(Block::Air));
        assert_eq!(at(11, 75, 10), Some(Block::Air));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_spawn_is_the_open_end_stone_closest_to_the_origin() {
        let shape = ChunkShape::new(0, 256);
        let mut chunk = Chunk::empty(shape);
        assert!(chunk_is_empty(&chunk));
        assert_eq!(spawn_in_chunk(&chunk, ChunkPos::new(60, 0)), None);
        let stone = Block::EndStone.default_state();
        chunk.set(5, 60, 5, stone);
        chunk.set(2, 60, 5, stone);
        chunk.set(2, 61, 5, stone);
        chunk.set(9, 20, 9, stone);
        assert!(!chunk_is_empty(&chunk));
        assert_eq!(
            spawn_in_chunk(&chunk, ChunkPos::new(60, 0)),
            Some(BlockPos::new(962, 61, 5)),
            "covered stone and stone below y 30 do not count"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_tallest_block_is_the_first_highest_column() {
        let tops: HashMap<(i32, i32), i32> =
            [((0, 0), 90), ((-1, 2), 70), ((1, -1), 70), ((2, 2), 69)]
                .into_iter()
                .collect();
        let around = BlockPos::new(0, 0, 0);
        assert_eq!(tallest(&tops, around, 2, true), BlockPos::new(0, 90, 0));
        assert_eq!(tallest(&tops, around, 2, false), BlockPos::new(-1, 70, 2));
        assert_eq!(tallest(&HashMap::new(), around, 2, false), around);
        let top = column_top(
            |y| {
                Some(if y == 40 {
                    Block::Bedrock.default_state()
                } else if y < 30 {
                    Block::EndStone.default_state()
                } else {
                    Block::Air.default_state()
                })
            },
            0,
            255,
            false,
        );
        assert_eq!(top, Some(29));
    }
}

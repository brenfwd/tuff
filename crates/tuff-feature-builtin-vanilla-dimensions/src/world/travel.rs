//! Where a nether portal leads, for players and other entities alike.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::dimension::{DimensionId, THE_END, THE_NETHER};
use tuff_core::world::messages::{ManagerMessage, RegionHandle};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use crate::world::portal::{self, Axis, PortalLevel, PortalRectangle};
use crate::world::region::portals::PortalQuery;
use tuff_world_model::chunk::Chunk;

/// A source of region handles for a portal search.
#[expect(
    async_fn_in_trait,
    reason = "crate-internal trait whose futures are awaited in place"
)]
pub trait Regions {
    async fn region_in(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<RegionHandle>;
}

/// Regions acquired from the manager for one trip, released together.
pub struct Borrowed {
    shared: Arc<Shared>,
    held: HashMap<(DimensionId, RegionPos), RegionHandle>,
}

impl Borrowed {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            held: HashMap::new(),
        }
    }

    pub async fn release(self) {
        for (dimension, region) in self.held.into_keys() {
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release { dimension, region })
                .await;
        }
    }
}

impl Regions for Borrowed {
    async fn region_in(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<RegionHandle> {
        if let Some(handle) = self.held.get(&(dimension, region)) {
            return Some(handle.clone());
        }
        let (reply, handle) = oneshot::channel();
        self.shared
            .manager
            .send(ManagerMessage::Acquire {
                dimension,
                region,
                reply,
            })
            .await
            .ok()?;
        let handle = handle.await.ok()?;
        self.held.insert((dimension, region), handle.clone());
        Some(handle)
    }
}

/// The far side of a nether portal.
pub struct NetherExit {
    pub dimension: DimensionId,
    pub position: [f64; 3],
    /// Degrees to turn the traveller (and its motion) by.
    pub rotation: f32,
}

async fn shape(
    regions: &mut impl Regions,
    dimension: DimensionId,
    pos: BlockPos,
) -> Option<PortalRectangle> {
    let region = regions
        .region_in(dimension, ChunkPos::of_block(pos).region())
        .await?;
    let (reply, answer) = oneshot::channel();
    region
        .request(PortalQuery::Shape { pos, reply }.request())
        .await;
    answer.await.ok().flatten()
}

/// The other side of the nether portal at `entry` for a body of `size`
/// (width, height) at `position`: the closest portal near the scaled
/// position, or a new one built there when `create` allows it.
pub async fn nether_exit(
    regions: &mut impl Regions,
    shared: &Shared,
    from: DimensionId,
    entry: BlockPos,
    position: [f64; 3],
    size: [f64; 2],
    create: bool,
) -> Option<NetherExit> {
    let source = shared.dimension(from);
    let target = source.portal_link?;
    let to = shared.dimension(target);
    let scale = source.kind.coordinate_scale / to.kind.coordinate_scale;
    let exit = to.clamp_to_border(
        position[0] * scale,
        position[1].floor() as i32,
        position[2] * scale,
    );
    let radius = if to.key == THE_NETHER {
        portal::NETHER_PORTAL_RADIUS
    } else {
        portal::OVERWORLD_PORTAL_RADIUS
    };
    let (source_axis, offset) = match shape(regions, from, entry).await {
        Some(rectangle) => (
            rectangle.axis,
            portal::relative_position_of(&rectangle, position, size),
        ),
        None => (Axis::X, [0.5, 0.0, 0.0]),
    };
    let (min, max) = portal::search_bounds(exit, radius);
    let (min, max) = (
        ChunkPos::of_block(min).region(),
        ChunkPos::of_block(max).region(),
    );
    let mut candidates = Vec::new();
    for x in min.x..=max.x {
        for z in min.z..=max.z {
            let Some(region) = regions.region_in(target, RegionPos::new(x, z)).await else {
                continue;
            };
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    PortalQuery::Find {
                        origin: exit,
                        radius,
                        reply,
                    }
                    .request(),
                )
                .await;
            if let Ok(found) = answer.await {
                candidates.extend(found);
            }
        }
    }
    let rectangle = match portal::closest(candidates, exit) {
        Some(pos) => shape(regions, target, pos).await?,
        None if !create => return None,
        None => {
            let Some(rectangle) = create_exit(regions, target, to, exit, source_axis).await else {
                tuff_core::log::warn!(
                    "unable to create a portal, likely target out of worldborder"
                );
                return None;
            };
            rectangle
        }
    };
    let (position, rotation) = portal::arrival_of(&rectangle, source_axis, offset, size);
    let position = free_position(regions, target, position, size).await;
    Some(NetherExit {
        dimension: target,
        position,
        rotation,
    })
}

/// Builds an exit portal near `origin` from a copy of every region's
/// blocks around it, so the spot is the one the whole dimension offers,
/// then has each region write its part: frames first, then the portal
/// blocks once every frame stands.
async fn create_exit(
    regions: &mut impl Regions,
    dimension: DimensionId,
    info: &tuff_core::world::dimension::Dimension,
    origin: BlockPos,
    axis: Axis,
) -> Option<PortalRectangle> {
    let (min, max) = portal::create_bounds(origin);
    let mut level = Snapshot {
        chunks: HashMap::new(),
        dimension: info,
        writes: Vec::new(),
    };
    let covering = covering_regions(min, max);
    for region in &covering {
        let Some(handle) = regions.region_in(dimension, *region).await else {
            continue;
        };
        let (reply, answer) = oneshot::channel();
        handle
            .request(PortalQuery::Chunks { min, max, reply }.request())
            .await;
        if let Ok(chunks) = answer.await {
            level.chunks.extend(chunks);
        }
    }
    let rectangle = portal::create_portal(&mut level, origin, axis)?;
    let (portal_blocks, frame): (Vec<_>, Vec<_>) = level
        .writes
        .into_iter()
        .partition(|(_, state)| state.block() == tuff_vanilla_data::Block::NetherPortal);
    for writes in [frame, portal_blocks] {
        let mut by_region: Vec<(RegionPos, Vec<(BlockPos, tuff_vanilla_data::BlockState)>)> =
            Vec::new();
        for (pos, state) in writes {
            let region = ChunkPos::of_block(pos).region();
            match by_region.iter_mut().find(|(r, _)| *r == region) {
                Some((_, list)) => list.push((pos, state)),
                None => by_region.push((region, vec![(pos, state)])),
            }
        }
        let mut answers = Vec::new();
        for (region, writes) in by_region {
            let Some(handle) = regions.region_in(dimension, region).await else {
                continue;
            };
            let (reply, answer) = oneshot::channel();
            handle
                .request(PortalQuery::Build { writes, reply }.request())
                .await;
            answers.push(answer);
        }
        for answer in answers {
            let _ = answer.await;
        }
    }
    Some(rectangle)
}

fn covering_regions(min: BlockPos, max: BlockPos) -> Vec<RegionPos> {
    let (low, high) = (
        ChunkPos::of_block(min).region(),
        ChunkPos::of_block(max).region(),
    );
    (low.x..=high.x)
        .flat_map(|x| (low.z..=high.z).map(move |z| RegionPos::new(x, z)))
        .collect()
}

/// Moves an arrival out of the blocks around it, read by the region the
/// arrival stands in.
async fn free_position(
    regions: &mut impl Regions,
    dimension: DimensionId,
    bottom: [f64; 3],
    size: [f64; 2],
) -> [f64; 3] {
    let Some(region) = regions
        .region_in(dimension, ChunkPos::of_position(bottom).region())
        .await
    else {
        return bottom;
    };
    let (reply, answer) = oneshot::channel();
    region
        .request(
            PortalQuery::FreePosition {
                bottom,
                size,
                reply,
            }
            .request(),
        )
        .await;
    answer.await.unwrap_or(bottom)
}

/// Copies of a dimension's chunks as portal building reads them, with
/// the writes collected.
struct Snapshot<'a> {
    chunks: HashMap<ChunkPos, Chunk>,
    dimension: &'a tuff_core::world::dimension::Dimension,
    writes: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
}

impl PortalLevel for Snapshot<'_> {
    fn get(&self, pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        if let Some((_, state)) = self.writes.iter().rev().find(|(at, _)| *at == pos) {
            return Some(*state);
        }
        let chunk = self.chunks.get(&ChunkPos::of_block(pos))?;
        Some(chunk.get(pos.x, pos.y, pos.z))
    }

    fn set(&mut self, pos: BlockPos, state: tuff_vanilla_data::BlockState) {
        self.writes.push((pos, state));
    }

    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        let chunk = self.chunks.get(&ChunkPos::new(x >> 4, z >> 4))?;
        Some(chunk.motion_blocking_height(x, z))
    }

    fn min_y(&self) -> i32 {
        self.dimension.shape.min_y
    }

    fn logical_max_y(&self) -> i32 {
        self.dimension.logical_max_y()
    }

    fn inside_border(&self, pos: BlockPos) -> bool {
        self.dimension.contains_block(pos)
    }

    fn clamp_to_border(&self, pos: BlockPos) -> BlockPos {
        self.dimension
            .clamp_to_border(pos.x as f64, pos.y, pos.z as f64)
    }
}

/// Builds the end's obsidian platform under [`portal::END_SPAWN`],
/// returning the end's id.
pub async fn end_platform(regions: &mut impl Regions, shared: &Shared) -> Option<DimensionId> {
    let end = shared.dimensions.by_key(THE_END)?.id;
    let spawn = portal::END_SPAWN;
    let platform = BlockPos::new(spawn.x, spawn.y - 1, spawn.z);
    let corner = |dx: i32, dz: i32| {
        ChunkPos::of_block(BlockPos::new(platform.x + dx, platform.y, platform.z + dz)).region()
    };
    let (min, max) = (corner(-2, -2), corner(2, 2));
    for x in min.x..=max.x {
        for z in min.z..=max.z {
            let Some(region) = regions.region_in(end, RegionPos::new(x, z)).await else {
                continue;
            };
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    PortalQuery::EndPlatform {
                        origin: platform,
                        reply,
                    }
                    .request(),
                )
                .await;
            let _ = answer.await;
        }
    }
    Some(end)
}

//! Explorer maps: finding the structure, making the map around it and
//! sketching water outlines from the biomes it covers.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::chunk_io::{IoOp, IoResult};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::exploration::{ExplorationSpec, ExploredMap};
use tuff_vanilla_data::tag::worldgen::biome::key::WATER_ON_MAP_OUTLINES;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use crate::data::{MapData, PIXELS, SIZE};
use crate::service::MapService;

const ORANGE: u8 = 15;
const BROWN: u8 = 26;
const LOW: u8 = 0;
const NORMAL: u8 = 1;
const HIGH: u8 = 2;
const LOWEST: u8 = 3;

/// Where each pixel's biome is read, row by row.
pub fn preview_positions(data: &MapData, sea_level: i32) -> Vec<BlockPos> {
    let scale = 1 << data.scale;
    let start_x = data.center_x / scale - 64;
    let start_z = data.center_z / scale - 64;
    let mut positions = Vec::with_capacity(PIXELS);
    for row in 0..SIZE as i32 {
        for column in 0..SIZE as i32 {
            positions.push(BlockPos::new(
                (start_x + column) * scale,
                sea_level,
                (start_z + row) * scale,
            ));
        }
    }
    positions
}

fn table_sin(value: f32) -> f32 {
    let index = ((value * 10430.378) as i32) & 0xFFFF;
    (f64::from(index) * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32
}

/// The sketch drawn from which pixels are watery: orange shores with
/// ripples, brown land beside them.
pub fn preview_colors(watery: &[bool]) -> Vec<u8> {
    let wet = |x: usize, z: usize| watery[z * SIZE + x];
    let mut colors = vec![0; PIXELS];
    for mx in 1..SIZE - 1 {
        for mz in 1..SIZE - 1 {
            let mut count = 0;
            for dx in 0..3 {
                for dz in 0..3 {
                    if (dx, dz) != (1, 1) && wet(mx + dx - 1, mz + dz - 1) {
                        count += 1;
                    }
                }
            }
            let mut brightness = LOWEST;
            let mut color = 0;
            if wet(mx, mz) {
                color = ORANGE;
                if count > 7 && mz % 2 == 0 {
                    let wave = (mx as i32 + (table_sin(mz as f32) * 7.0) as i32) / 8;
                    match wave % 5 {
                        0 | 4 => brightness = LOW,
                        1 | 3 => brightness = NORMAL,
                        2 => brightness = HIGH,
                        _ => {}
                    }
                } else if count > 7 {
                    color = 0;
                } else if count > 5 {
                    brightness = NORMAL;
                } else if count > 1 {
                    brightness = LOW;
                }
            } else if count > 0 {
                color = BROWN;
                brightness = if count > 3 { NORMAL } else { LOWEST };
            }
            if color != 0 {
                colors[mz * SIZE + mx] = color << 2 | brightness;
            }
        }
    }
    colors
}

async fn off_actor<T: Send + 'static>(
    shared: &Shared,
    job: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (reply, answer) = oneshot::channel();
    let tags = shared.datapacks.contents().tags.clone();
    rayon::spawn(move || {
        if let Ok(result) = catch_unwind(AssertUnwindSafe(|| {
            tuff_datapack::tags::sync_scope(tags, job)
        })) {
            let _ = reply.send(result);
        }
    });
    answer.await.ok()
}

/// Whether the chunk was generated and saved before, so that a structure
/// starting in it is already known to players.
fn saved_chunk(
    shared: &Shared,
    dimension: DimensionId,
    skip_existing: bool,
) -> impl Fn(ChunkPos) -> bool + Sync + use<> {
    let io = skip_existing.then(|| shared.chunk_io.get(&shared.backend).clone());
    let key = shared.dimension(dimension).key.clone();
    move |pos| {
        let Some(io) = &io else {
            return false;
        };
        let Ok(results) = io.submit(&key, vec![IoOp::ReadBlocks(pos)]).blocking_recv() else {
            return false;
        };
        matches!(
            results.first(),
            Some(IoResult::Read {
                blocks: Ok(Some(_)),
                ..
            })
        )
    }
}

async fn make(
    shared: Arc<Shared>,
    dimension: DimensionId,
    origin: BlockPos,
    spec: ExplorationSpec,
) -> Option<ExploredMap> {
    let info = shared.dimension(dimension);
    let (generator, shape, key) = (info.generator.clone(), info.shape, info.key.clone());
    let sea_level = generator.sea_level();
    let structures =
        tuff_datapack::tags::sync_scope(shared.datapacks.contents().tags.clone(), || {
            spec.structures()
        });
    let radius = spec.search_radius;
    let locate = generator.clone();
    let saved = saved_chunk(&shared, dimension, spec.skip_existing);
    let (target, _) = off_actor(&shared, move || {
        locate.locate_structure_within(&structures, origin, radius, shape, Some(&saved))
    })
    .await
    .flatten()?;
    let service = shared.services.get::<MapService>()?;
    let id = service
        .create(
            &key,
            f64::from(target.x),
            f64::from(target.z),
            spec.zoom,
            true,
            true,
        )
        .await?;
    let data = service.data(id).await?;
    let positions = preview_positions(&data, sea_level);
    let biomes = off_actor(&shared, move || generator.biomes_at(&positions, shape)).await?;
    let tags = shared.datapacks.contents().tags.clone();
    let watery: Vec<bool> = biomes
        .iter()
        .map(|biome| biome.is_some_and(|name| tags.has(WATER_ON_MAP_OUTLINES, name)))
        .collect();
    service.paint(id, preview_colors(&watery));
    Some(ExploredMap {
        id,
        target,
        decoration: spec.decoration,
    })
}

async fn find(
    shared: Arc<Shared>,
    dimension: DimensionId,
    origin: BlockPos,
    spec: ExplorationSpec,
) -> Option<BlockPos> {
    let info = shared.dimension(dimension);
    let (generator, shape) = (info.generator.clone(), info.shape);
    let structures =
        tuff_datapack::tags::sync_scope(shared.datapacks.contents().tags.clone(), || {
            spec.structures()
        });
    let radius = spec.search_radius;
    let saved = saved_chunk(&shared, dimension, spec.skip_existing);
    off_actor(&shared, move || {
        generator.locate_structure_within(&structures, origin, radius, shape, Some(&saved))
    })
    .await
    .flatten()
    .map(|(pos, _)| pos)
}

pub fn locate(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    origin: BlockPos,
    spec: ExplorationSpec,
) -> oneshot::Receiver<Option<BlockPos>> {
    let (reply, answer) = oneshot::channel();
    let shared = shared.clone();
    tokio::spawn(async move {
        let _ = reply.send(find(shared, dimension, origin, spec).await);
    });
    answer
}

pub fn explore(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    origin: BlockPos,
    spec: ExplorationSpec,
) -> oneshot::Receiver<Option<ExploredMap>> {
    let (reply, answer) = oneshot::channel();
    let shared = shared.clone();
    tokio::spawn(async move {
        let _ = reply.send(make(shared, dimension, origin, spec).await);
    });
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn previews_sample_the_maps_area_at_sea_level() {
        let data = MapData::fresh(0.0, 0.0, 1, true, true, "minecraft:overworld");
        let positions = preview_positions(&data, 63);
        assert_eq!(positions.len(), PIXELS);
        assert_eq!(positions[0], BlockPos::new(-64 * 2 + 64, 63, -64 * 2 + 64));
        assert_eq!(positions[1].x - positions[0].x, 2);
        assert_eq!(positions[SIZE].z - positions[0].z, 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn shores_are_orange_and_land_beside_water_brown() {
        let mut watery = vec![false; PIXELS];
        for z in 0..SIZE {
            for x in 0..64 {
                watery[z * SIZE + x] = true;
            }
        }
        let colors = preview_colors(&watery);
        let at = |x: usize, z: usize| colors[z * SIZE + x];
        assert_eq!(at(63, 10), ORANGE << 2 | LOW, "the shore");
        assert_eq!(at(64, 10), BROWN << 2 | LOWEST, "land touching water");
        assert_eq!(at(65, 10), 0, "dry land");
        assert_eq!(at(30, 11), 0, "open water on odd rows");
        assert!(at(30, 10) >> 2 == ORANGE, "ripples on even rows");
        assert_eq!(at(0, 10), 0, "the border is left blank");
    }
}

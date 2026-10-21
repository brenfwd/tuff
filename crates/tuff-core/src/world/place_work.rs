//! Placing a planned structure or a feature request chunk by chunk, each
//! chunk by the region that owns it: what `/place` and the jigsaw block's
//! Generate button share.

use std::sync::Arc;

use rand::Rng;
use tokio::sync::oneshot;
use tuff_world_model::pos::ChunkPos;

use super::features::{FeatureSpan, PlaceRequest, PlannedStructure};
use super::messages::RegionHandle;
use super::messages::RegionRequest;
use super::region_api::{RegionTask, RegionWorld};
use tuff_item::random::WorldgenRandom;

pub enum Work {
    Request(PlaceRequest),
    Plan(Arc<dyn PlannedStructure>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Placed,
    Nothing,
    NoWindow,
}

fn apply(world: &mut dyn RegionWorld, center: ChunkPos, work: &Work, flags: u32) -> Outcome {
    let Some(host) = world.feature_host() else {
        return Outcome::Nothing;
    };
    let Some(window) = world.chunk_window(center) else {
        return Outcome::NoWindow;
    };
    let Some(shape) = window.first().map(|(_, chunk)| chunk.shape()) else {
        return Outcome::NoWindow;
    };
    let span = FeatureSpan {
        min_y: shape.min_y,
        height: shape.height,
        sea_level: world.sea_level(),
        seed: world.seed(),
    };
    let mut random = WorldgenRandom::legacy(world.rng().r#gen());
    let changes = match work {
        Work::Request(request) => host.place(request, center, &window, span, &mut random),
        Work::Plan(plan) => plan.place_in_window(center, &window, span, &mut random),
    };
    let Some(changes) = changes else {
        return Outcome::Nothing;
    };
    for (pos, state) in &changes.blocks {
        world.set(*pos, *state, flags);
    }
    for (pos, data) in &changes.block_entities {
        world.merge_block_entity(*pos, data.clone());
    }
    for entity in &changes.entities {
        world.spawn_saved_entity(entity.clone());
    }
    Outcome::Placed
}

const WINDOW_RETRIES: usize = 50;
const WINDOW_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// Places `work` in the window around each chunk on the region that owns
/// it, all regions at once. A window whose neighbours have not reached the
/// region yet (chunks that just loaded) is retried for a few seconds.
/// Returns how many chunks placed, or `None` if a window never formed.
pub async fn place_chunks(
    targets: &[(ChunkPos, RegionHandle)],
    work: &Arc<Work>,
    flags: u32,
) -> Option<usize> {
    let mut remaining: Vec<&(ChunkPos, RegionHandle)> = targets.iter().collect();
    let mut placed = 0;
    for attempt in 0..WINDOW_RETRIES {
        let mut pending = Vec::new();
        for target in std::mem::take(&mut remaining) {
            let (chunk, region) = target;
            let chunk = *chunk;
            let (reply, answer) = oneshot::channel();
            let work = work.clone();
            let task = RegionTask::new(move |world| {
                let _ = reply.send(apply(world, chunk, &work, flags));
            });
            if region.request(RegionRequest::Run(task)).await {
                pending.push((target, answer));
            }
        }
        for (target, answer) in pending {
            match answer.await {
                Ok(Outcome::Placed) => placed += 1,
                Ok(Outcome::NoWindow) | Err(_) => remaining.push(target),
                Ok(Outcome::Nothing) => {}
            }
        }
        if remaining.is_empty() {
            return Some(placed);
        }
        if attempt + 1 < WINDOW_RETRIES {
            tokio::time::sleep(WINDOW_RETRY_WAIT).await;
        }
    }
    None
}

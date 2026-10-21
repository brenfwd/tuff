//! Getting chunks to draw without disturbing the world: clones of loaded
//! chunks from the region actors that own them, and saved chunks through
//! the chunk I/O actor. Nothing here loads, generates or keeps a region.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tuff_core::storage::codec::WorldBackendExt as _;

use tokio::sync::oneshot;
use tuff_core::world::Shared;
use tuff_core::world::chunk_io::{IoOp, IoResult};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{ManagerMessage, RegionHandle, RegionRequest};
use tuff_core::world::region_api::RegionTask;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::chunk_meta::Decoded;
use tuff_world_model::pos::{ChunkPos, FileRegionPos, RegionPos};

/// Saved chunks are read this many at a time, so a player's chunk loads
/// in the same file queue between the map's batches.
pub const READ_BATCH: usize = 64;

/// A chunk to draw: a clone from its region (with its stamp), or a saved
/// blob still to decode.
pub enum Source {
    Live(u64, Box<Chunk>),
    Saved(Vec<u8>),
}

/// The running regions of `dimension`, without taking tickets.
pub async fn running(shared: &Shared, dimension: DimensionId) -> Vec<RegionHandle> {
    let (reply, answer) = oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    answer
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|handle| handle.dimension == dimension)
        .collect()
}

/// Chunks to ask a region about, with the stamp each was last drawn at.
pub type Wanted = Vec<(ChunkPos, Option<u64>)>;

/// What a region said about the chunks it was asked about.
#[derive(Default)]
pub struct Answer {
    /// Loaded chunks whose stamp differs from the one given.
    pub changed: Vec<(ChunkPos, u64, Box<Chunk>)>,
    /// Asked-about chunks it does not have loaded.
    pub absent: Vec<ChunkPos>,
}

/// Runs a task on `region` at once (an empty area loads nothing) that
/// clones each loaded chunk of `wanted` whose stamp is not the known one.
/// `None` if the region has closed.
pub async fn ask_region(region: &RegionHandle, wanted: Wanted) -> Option<Answer> {
    let (reply, answer) = oneshot::channel();
    let task = RegionTask::new(move |world| {
        let mut out = Answer::default();
        for (pos, known) in wanted {
            match world.chunk(pos) {
                Some(chunk) if Some(chunk.stamp()) != known => {
                    out.changed
                        .push((pos, chunk.stamp(), Box::new(chunk.clone())))
                }
                Some(_) => {}
                None => out.absent.push(pos),
            }
        }
        let _ = reply.send(out);
    });
    let sent = region
        .request(RegionRequest::WhenLoaded {
            min: BlockPos::new(16, 0, 16),
            max: BlockPos::new(0, 0, 0),
            run: task,
        })
        .await;
    if !sent {
        return None;
    }
    answer.await.ok()
}

/// The file regions `dimension` has saved chunks in.
pub async fn stored_regions(shared: &Arc<Shared>, key: &str) -> HashSet<FileRegionPos> {
    let backend = shared.backend.clone();
    let key = key.to_owned();
    tokio::task::spawn_blocking(move || backend.list_regions(&key))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Saved blobs of `chunks` through chunk I/O, in [`READ_BATCH`]es; a chunk
/// with no saved data (or an unreadable one) is left out.
pub async fn read_saved(
    shared: &Shared,
    key: &str,
    chunks: Vec<ChunkPos>,
) -> HashMap<ChunkPos, Vec<u8>> {
    let mut out = HashMap::new();
    let io = shared.chunk_io.get(&shared.backend);
    for batch in chunks.chunks(READ_BATCH) {
        let ops = batch.iter().map(|pos| IoOp::ReadBlocks(*pos)).collect();
        let Ok(results) = io.submit(key, ops).await else {
            break;
        };
        for result in results {
            if let IoResult::Read {
                pos,
                blocks: Ok(Some(blob)),
                ..
            } = result
            {
                out.insert(pos, blob);
            }
        }
    }
    out
}

/// Every chunk of `chunks` that exists: live clones from the running
/// regions, the rest read from storage when their file region is saved.
pub async fn collect(
    shared: &Arc<Shared>,
    dimension: DimensionId,
    key: &str,
    chunks: &[ChunkPos],
    stored: &HashSet<FileRegionPos>,
) -> HashMap<ChunkPos, Source> {
    let mut by_region: HashMap<RegionPos, Wanted> = HashMap::new();
    for pos in chunks {
        by_region
            .entry(pos.region())
            .or_default()
            .push((*pos, None));
    }
    let mut out = HashMap::new();
    let mut absent: Vec<ChunkPos> = Vec::new();
    let handles = running(shared, dimension).await;
    for (region, wanted) in by_region {
        let answer = match handles.iter().find(|h| h.pos == region) {
            Some(handle) => ask_region(handle, wanted.clone()).await,
            None => None,
        };
        match answer {
            Some(answer) => {
                absent.extend(answer.absent);
                for (pos, stamp, chunk) in answer.changed {
                    out.insert(pos, Source::Live(stamp, chunk));
                }
            }
            None => absent.extend(wanted.into_iter().map(|(pos, _)| pos)),
        }
    }
    absent.retain(|pos| stored.contains(&pos.file_region()));
    for (pos, blob) in read_saved(shared, key, absent).await {
        out.insert(pos, Source::Saved(blob));
    }
    out
}

/// The chunk a source holds, decoding saved data; `None` for a chunk not
/// generated yet or unreadable.
pub fn decode(shared: &Shared, pos: ChunkPos, shape: ChunkShape, source: Source) -> Option<Chunk> {
    match source {
        Source::Live(_, chunk) => Some(*chunk),
        Source::Saved(blob) => match shared.backend.chunk_codec().decode_chunk_in(
            &blob,
            pos,
            shape,
            shared.dimensions.biomes(),
        ) {
            Ok(Decoded::Chunk(chunk, _)) => Some(*chunk),
            _ => None,
        },
    }
}

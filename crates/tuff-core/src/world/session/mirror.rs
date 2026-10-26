use std::collections::VecDeque;
use std::sync::Arc;

use tuff_vanilla_data::BlockState;

use crate::world::physics::{self, Aabb};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::{ChunkMap, ChunkPos};

/// Read-only copy of the chunks a client has been sent, patched by the
/// block changes delivered to it, so it holds what the client believes.
/// Sections are shared with the owning region, and a delivered block
/// change copies only the section it touches. Unsent chunks read as
/// unknown (`None`), which callers must treat as "do not flag".
#[derive(Default)]
pub struct ChunkMirror {
    chunks: ChunkMap<Arc<Chunk>>,
    clock: u64,
    recent: VecDeque<(u64, BlockPos)>,
}

/// Ticks a delivered block change stays in the recent window.
pub const RECENT_TICKS: u64 = 20;
const RECENT_CAP: usize = 256;

impl ChunkMirror {
    /// Records `chunk` as sent to the client.
    pub fn insert(&mut self, pos: ChunkPos, chunk: Arc<Chunk>) {
        self.chunks.insert(pos, chunk);
    }

    pub(super) fn remove(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    pub(super) fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn apply(&mut self, pos: ChunkPos, changes: &[(BlockPos, BlockState)]) {
        if let Some(chunk) = self.chunks.get_mut(&pos) {
            apply_changes(chunk, changes);
            for (at, _) in changes {
                if self.recent.len() >= RECENT_CAP {
                    self.recent.pop_front();
                }
                self.recent.push_back((self.clock, *at));
            }
        }
    }

    pub fn set_clock(&mut self, tick: u64) {
        self.clock = tick;
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| tick.saturating_sub(*at) > RECENT_TICKS)
        {
            self.recent.pop_front();
        }
    }

    /// Whether a block meeting `area` (grown by one block) was changed
    /// within the last [`RECENT_TICKS`] ticks, so the client may still have
    /// moved against the older shape.
    pub fn changed_recently(&self, area: Aabb) -> bool {
        let lo = |v: f64| v.floor() as i32 - 1;
        let hi = |v: f64| v.floor() as i32 + 1;
        self.recent.iter().any(|(at, pos)| {
            self.clock.saturating_sub(*at) <= RECENT_TICKS
                && (lo(area[0])..=hi(area[3])).contains(&pos.x)
                && (lo(area[1])..=hi(area[4])).contains(&pos.y)
                && (lo(area[2])..=hi(area[5])).contains(&pos.z)
        })
    }

    /// Whether a block meeting `area` (grown by one block) was changed
    /// within the last `ticks` ticks.
    pub fn changed_within(&self, area: Aabb, ticks: u64) -> bool {
        let lo = |v: f64| v.floor() as i32 - 1;
        let hi = |v: f64| v.floor() as i32 + 1;
        self.recent.iter().any(|(at, pos)| {
            self.clock.saturating_sub(*at) <= ticks
                && (lo(area[0])..=hi(area[3])).contains(&pos.x)
                && (lo(area[1])..=hi(area[4])).contains(&pos.y)
                && (lo(area[2])..=hi(area[5])).contains(&pos.z)
        })
    }

    /// Records a block the client placed ahead of the server's answer; the
    /// answer, when it comes, replaces it either way.
    pub fn predict(&mut self, pos: BlockPos, state: BlockState) {
        if let Some(chunk) = self.chunks.get_mut(&ChunkPos::of_block(pos)) {
            apply_changes(chunk, &[(pos, state)]);
        }
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos).map(|chunk| &**chunk)
    }

    pub fn in_loaded_chunk(&self, pos: BlockPos) -> bool {
        self.chunks.contains_key(&ChunkPos::of_block(pos))
    }

    /// The block the client sees at `pos`, or `None` when its chunk was
    /// not sent (or `pos` is outside the world's height).
    pub fn block_at(&self, pos: BlockPos) -> Option<BlockState> {
        let chunk = self.chunks.get(&ChunkPos::of_block(pos))?;
        chunk
            .shape()
            .contains_y(pos.y)
            .then(|| chunk.get(pos.x, pos.y, pos.z))
    }

    /// Collision boxes meeting `area`; blocks in unsent chunks have none.
    pub fn colliders(&self, area: Aabb) -> Vec<Aabb> {
        physics::colliders_shaped(area, &|pos| match self.block_at(pos) {
            Some(state) => physics::Shape::collision(Some(state), pos),
            None => physics::Shape::from(&[][..]),
        })
    }

    /// Whether any part of `area` lies in a chunk that was not sent.
    pub fn touches_unknown(&self, area: Aabb) -> bool {
        let chunk = |v: f64| (v.floor() as i32) >> 4;
        (chunk(area[0])..=chunk(area[3])).any(|x| {
            (chunk(area[2])..=chunk(area[5]))
                .any(|z| !self.chunks.contains_key(&ChunkPos::new(x, z)))
        })
    }
}

pub(super) fn apply_changes(chunk: &mut Arc<Chunk>, changes: &[(BlockPos, BlockState)]) {
    let chunk = Arc::make_mut(chunk);
    for (pos, state) in changes {
        chunk.set(pos.x, pos.y, pos.z, *state);
    }
}

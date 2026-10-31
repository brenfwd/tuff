//! Unloading chunks on request, for the SQL console to edit their stored
//! rows: each chunk nothing keeps loaded is saved, then dropped.

use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tuff_storage::sqlite::console::ChunkKey;
use tuff_world_model::pos::ChunkPos;

use super::RegionActor;

const EVICT_WAIT: Duration = Duration::from_secs(5);

pub(super) struct EvictWaiter {
    pending: Vec<(ChunkKey, ChunkPos)>,
    pinned: Vec<ChunkKey>,
    deadline: Instant,
    reply: oneshot::Sender<Vec<ChunkKey>>,
}

impl RegionActor {
    pub(super) fn evict(&mut self, chunks: &[ChunkKey], reply: oneshot::Sender<Vec<ChunkKey>>) {
        let key = self.dimension().key.clone();
        let pending = chunks
            .iter()
            .filter(|c| c.dimension == key)
            .map(|c| (c.clone(), ChunkPos::new(c.x, c.z)))
            .filter(|(_, pos)| self.columns.contains_key(pos))
            .collect();
        self.sched.evict_waiters.push(EvictWaiter {
            pending,
            pinned: Vec::new(),
            deadline: Instant::now() + EVICT_WAIT,
            reply,
        });
        self.answer_evictions();
    }

    fn pinned(&self, pos: ChunkPos) -> bool {
        self.quarantined()
            || self.chunk_io.forced.contains(&pos)
            || self.ents.presence.values().any(|p| p.chunk == pos)
            || self
                .columns
                .get(&pos)
                .is_some_and(|c| !c.viewers.is_empty() || !c.held.is_empty())
    }

    pub(super) fn answer_evictions(&mut self) {
        if self.sched.evict_waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut waiters = std::mem::take(&mut self.sched.evict_waiters);
        for waiter in &mut waiters {
            let mut still = Vec::new();
            for (key, pos) in std::mem::take(&mut waiter.pending) {
                if !self.columns.contains_key(&pos) {
                    continue;
                }
                if self.pinned(pos) || now >= waiter.deadline {
                    waiter.pinned.push(key);
                    continue;
                }
                let column = &self.columns[&pos];
                if column.saving > 0 {
                    still.push((key, pos));
                } else if column.needs_save() {
                    self.start_save(pos);
                    still.push((key, pos));
                } else {
                    self.drop_column(pos);
                }
            }
            for (_, pos) in &still {
                if let Some(column) = self.columns.get_mut(pos)
                    && !column.unloading
                {
                    column.unloading = true;
                    self.block_entity_generation += 1;
                }
            }
            waiter.pending = still;
        }
        for waiter in waiters {
            if waiter.pending.is_empty() {
                let _ = waiter.reply.send(waiter.pinned);
            } else {
                self.sched.evict_waiters.push(waiter);
            }
        }
    }
}

//! A region's part of pre-generation jobs (`world::pregen`): each chunk
//! asked for is loaded or generated, lit, saved and dropped again, or for
//! a regenerating job dropped with everything in it and deleted from
//! storage. Requests wait here until the chunk's own loading, saving and
//! I/O allow the next step; [`RegionActor::poll_pregen`] runs after every
//! message the region handles.
//!
//! A removed chunk something still needs (viewers, tickets, queries) is
//! parked: its load waits until the job has had the generation pipeline
//! forget the whole area and sends [`OpKind::Release`], so it comes back
//! decorated by fresh neighbours rather than by what the pipeline still
//! remembers of the old ones.

use std::collections::{HashMap, HashSet};

use super::RegionActor;
use crate::world::chunk_io::IoOp;
use crate::world::pregen::{OpKind, Outcome, RegionOp, WaitingOn};
use tuff_world_model::pos::ChunkPos;

/// Saves of one chunk a job waits for before it drops the chunk anyway.
const MAX_SAVES: u8 = 3;
/// Loads of one chunk a job tries before giving up on it.
const MAX_LOADS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Start,
    Loading,
    Lit,
    Saving,
    /// The delete is out with storage.
    Removing,
}

struct Waiting {
    op: RegionOp,
    stage: Stage,
    loads: u8,
    saves: u8,
}

#[derive(Default)]
pub(super) struct Pending {
    waiting: Vec<Waiting>,
    /// Chunks whose delete came back from storage, and whether it worked.
    removed: HashMap<ChunkPos, bool>,
    /// Chunks removed by the running job whose loads wait for its release.
    pub(super) parked: HashSet<ChunkPos>,
}

impl Pending {
    /// Whether loading `pos` waits for the job to release it.
    pub(super) fn parked(&self, pos: ChunkPos) -> bool {
        self.parked.contains(&pos)
    }

    /// Whether a job waits for `pos` to be generated.
    pub(super) fn wants(&self, pos: ChunkPos) -> bool {
        self.waiting
            .iter()
            .any(|w| w.op.pos == pos && w.op.kind == OpKind::Generate)
    }
}

enum Step {
    Wait,
    Next(Stage),
    Finish(Outcome),
}

impl RegionActor {
    pub(super) fn pregen_request(&mut self, op: RegionOp) {
        if op.kind == OpKind::Release {
            self.chunk_io.release_parked();
            let _ = op.reply.send(Outcome::Done);
            return;
        }
        if self.sched.closing.is_some() || !self.pos.contains(op.pos) {
            let _ = op.reply.send(Outcome::Failed);
            return;
        }
        self.chunk_io.pregen.waiting.push(Waiting {
            op,
            stage: Stage::Start,
            loads: 0,
            saves: 0,
        });
        self.poll_pregen();
    }

    /// Storage answered the delete of `pos`.
    pub(super) fn pregen_removed(&mut self, pos: ChunkPos, ok: bool) {
        self.chunk_io.pregen.removed.insert(pos, ok);
    }

    /// A batch of storage work was lost: deletes in it count as failed.
    pub(super) fn pregen_io_lost(&mut self, positions: &[ChunkPos]) {
        for pos in positions {
            if self
                .chunk_io
                .pregen
                .waiting
                .iter()
                .any(|w| w.op.pos == *pos && w.stage == Stage::Removing)
            {
                self.chunk_io.pregen.removed.insert(*pos, false);
            }
        }
    }

    pub(super) fn poll_pregen(&mut self) {
        if self.sched.closing.is_some() && !self.chunk_io.pregen.parked.is_empty() {
            self.chunk_io.release_parked();
        }
        if self.chunk_io.pregen.waiting.is_empty() {
            return;
        }
        if self.sched.closing.is_some() {
            for waiting in self.chunk_io.pregen.waiting.drain(..) {
                let _ = waiting.op.reply.send(Outcome::Failed);
            }
            self.chunk_io.pregen.removed.clear();
            return;
        }
        let mut waiting = std::mem::take(&mut self.chunk_io.pregen.waiting);
        let mut index = 0;
        while index < waiting.len() {
            let step = loop {
                let entry = &mut waiting[index];
                let step = match entry.op.kind {
                    OpKind::Generate => self.generate_step(entry),
                    OpKind::Remove => self.remove_step(entry),
                    OpKind::Release => Step::Finish(Outcome::Done),
                };
                match step {
                    Step::Next(stage) => entry.stage = stage,
                    other => break other,
                }
            };
            match step {
                Step::Finish(outcome) => {
                    let done = waiting.swap_remove(index);
                    let _ = done.op.reply.send(outcome);
                }
                _ => {
                    let entry = &waiting[index];
                    entry.op.waiting.set(self.waiting_on(entry));
                    index += 1;
                }
            }
        }
        waiting.append(&mut self.chunk_io.pregen.waiting);
        self.chunk_io.pregen.waiting = waiting;
    }

    fn waiting_on(&self, entry: &Waiting) -> WaitingOn {
        let pos = entry.op.pos;
        let saving = self.columns.get(&pos).is_some_and(|c| c.saving > 0);
        match (entry.op.kind, entry.stage) {
            (_, Stage::Removing) => WaitingOn::Delete,
            (OpKind::Remove, _) if saving => WaitingOn::Save,
            (OpKind::Remove, _) => WaitingOn::Load,
            (_, Stage::Loading) if self.chunk_io.generating.contains_key(&pos) => {
                WaitingOn::Generation
            }
            (_, Stage::Loading) => WaitingOn::Read,
            (_, Stage::Lit)
                if self
                    .columns
                    .get(&pos)
                    .is_some_and(|c| c.lit && self.light.light_idle()) =>
            {
                WaitingOn::SaveHold
            }
            (_, Stage::Lit) => WaitingOn::Light,
            (_, Stage::Saving) => WaitingOn::Save,
            (_, Stage::Start) => WaitingOn::Region,
        }
    }

    fn generate_step(&mut self, entry: &mut Waiting) -> Step {
        let pos = entry.op.pos;
        match entry.stage {
            Stage::Start => {
                if self.chunk_io.pregen.parked.remove(&pos)
                    && self.chunk_io.loading.contains_key(&pos)
                {
                    self.chunk_io.io_queue.push(IoOp::Read(pos));
                }
                if self.columns.contains_key(&pos) {
                    return Step::Next(Stage::Lit);
                }
                if entry.loads >= MAX_LOADS {
                    return Step::Finish(Outcome::Failed);
                }
                entry.loads += 1;
                self.load(pos);
                Step::Next(Stage::Loading)
            }
            Stage::Loading => {
                if self.columns.contains_key(&pos) {
                    Step::Next(Stage::Lit)
                } else if self.chunk_io.loading.contains_key(&pos) {
                    Step::Wait
                } else {
                    Step::Next(Stage::Start)
                }
            }
            Stage::Lit => {
                let light_idle = self.light.light_idle();
                let Some(column) = self.columns.get(&pos) else {
                    return Step::Next(Stage::Start);
                };
                if !column.lit || !light_idle {
                    return Step::Wait;
                }
                if !column.persist {
                    self.drop_if_unused(pos);
                    return Step::Finish(Outcome::Failed);
                }
                if column.saving > 0 {
                    return Step::Next(Stage::Saving);
                }
                if !column.needs_save() {
                    self.drop_if_unused(pos);
                    return Step::Finish(Outcome::Done);
                }
                if !self.shared.saves().saves() {
                    return Step::Wait;
                }
                entry.saves += 1;
                self.start_save(pos);
                Step::Next(Stage::Saving)
            }
            Stage::Saving => {
                let Some(column) = self.columns.get(&pos) else {
                    return Step::Finish(Outcome::Done);
                };
                if column.saving > 0 {
                    return Step::Wait;
                }
                if column.needs_save() && entry.saves < MAX_SAVES {
                    return Step::Next(Stage::Lit);
                }
                self.drop_if_unused(pos);
                Step::Finish(Outcome::Done)
            }
            Stage::Removing => Step::Finish(Outcome::Failed),
        }
    }

    fn remove_step(&mut self, entry: &mut Waiting) -> Step {
        let pos = entry.op.pos;
        match entry.stage {
            Stage::Removing => match self.chunk_io.pregen.removed.remove(&pos) {
                Some(true) => Step::Finish(Outcome::Done),
                Some(false) => Step::Finish(Outcome::Failed),
                None => Step::Wait,
            },
            _ if self.chunk_io.pregen.parked.contains(&pos) => {
                self.chunk_io.pregen.removed.remove(&pos);
                self.chunk_io.io_queue.push(IoOp::Remove(pos));
                Step::Next(Stage::Removing)
            }
            _ => {
                if self.chunk_io.loading.contains_key(&pos)
                    || self.chunk_io.generating.contains_key(&pos)
                {
                    return Step::Wait;
                }
                let viewers = match self.columns.get_mut(&pos) {
                    Some(column) if column.saving > 0 => return Step::Wait,
                    Some(column) => Some((
                        std::mem::take(&mut column.viewers),
                        std::mem::take(&mut column.held),
                    )),
                    None => None,
                };
                if viewers.is_some() {
                    let ids: Vec<i32> = self
                        .ents
                        .entities
                        .values()
                        .filter(|tracked| tracked.chunk() == pos)
                        .map(|tracked| tracked.id())
                        .collect();
                    for id in ids {
                        self.remove_entity(id, &[]);
                    }
                    if let Some(column) = self.columns.get_mut(&pos) {
                        let stacks = column.meta.block_entities.values_mut();
                        for stack in stacks
                            .flat_map(|e| e.items.iter_mut())
                            .flat_map(Option::take)
                        {
                            stack.burn(tuff_item::item::BurnReason::Discarded);
                        }
                    }
                    self.drop_column(pos);
                }
                self.chunk_io.pregen.removed.remove(&pos);
                self.chunk_io.io_queue.push(IoOp::Remove(pos));
                self.chunk_io.pregen.parked.insert(pos);
                let (viewers, held) = viewers.unwrap_or_default();
                if !viewers.is_empty() || !held.is_empty() || self.needed(pos) {
                    let loading = self.load(pos);
                    loading.viewers = viewers;
                    loading.held = held;
                }
                Step::Next(Stage::Removing)
            }
        }
    }

    /// Whether tickets, players standing in it or queries need `pos`.
    fn needed(&self, pos: ChunkPos) -> bool {
        self.chunk_io.forced.contains(&pos)
            || self.ents.presence.values().any(|p| p.chunk == pos)
            || self
                .chunk_io
                .chunk_queries
                .iter()
                .any(|q| q.chunks.contains(&pos))
    }

    /// Drops `pos` unless players, tickets, queries or another job keep it.
    fn drop_if_unused(&mut self, pos: ChunkPos) {
        let Some(column) = self.columns.get(&pos) else {
            return;
        };
        let used = !column.viewers.is_empty()
            || !column.held.is_empty()
            || column.saving > 0
            || column.needs_save()
            || self.needed(pos)
            || self.chunk_io.pregen.wants(pos);
        if !used {
            self.drop_column(pos);
        }
    }
}

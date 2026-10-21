//! Chunk lifecycle: reading and writing region files, decoding or
//! generating chunks on the rayon pool, installing them once lit, and
//! the unload/autosave sweep. Called from region.rs's job/tick dispatch.

use crate::storage::chunk_nbt;
use crate::storage::codec::WorldBackendExt as _;
use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use tuff_vanilla_data::Block;

use super::fluids;
use super::{
    Blob, Column, EntityWrite, IoOp, IoResult, JobResult, Loading, PACKET_KEEP_TICKS, Prepared,
    RegionActor, Stage, UNLOAD_DELAY_TICKS,
};
use crate::protocol::play;
use crate::storage::normalise::SaveView;
use crate::world::block_entity::{self, SavePicture as _};
use crate::world::dimension::DimensionId;
use crate::world::lighting::{LightChange, LightOp, Lighting};
use crate::world::messages::{PlayerEvent, PlayerId, RegionInternal};
use crate::world::ticks::{self};
use crate::world::{Shared, framed, packets};
use tuff_timings::Tag;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{self as chunk_data, Chunk};
use tuff_world_model::chunk_meta::{ChunkMeta, Decoded};
use tuff_world_model::light::{ChunkLight, SectionLight};
use tuff_world_model::pos::{ChunkPos, REGION_CHUNKS};

impl RegionActor {
    pub(super) fn spawn_job(
        &mut self,
        stage: Stage,
        pos: ChunkPos,
        job: impl FnOnce() -> JobResult + Send + 'static,
    ) {
        self.sched.jobs_in_flight += 1;
        let tx = self.sched.jobs_tx.clone();
        let tag = self.timings_tag();
        let tags = tuff_datapack::tags::current();
        rayon::spawn(move || {
            let _region = tuff_timings::span!("region.job", tag);
            let _stage = tuff_timings::span!("job", Tag::Name(stage.label()));
            let job = std::panic::AssertUnwindSafe(|| tuff_datapack::tags::sync_scope(tags, job));
            let result = std::panic::catch_unwind(job).unwrap_or(JobResult::Panicked {
                stage,
                positions: vec![pos],
            });
            let _ = tx.send(result);
        });
    }

    pub(super) fn pump_io(&mut self) {
        if self.chunk_io.io_queue.is_empty() || self.chunk_io.io_busy {
            return;
        }
        self.chunk_io.io_busy = true;
        let ops = std::mem::take(&mut self.chunk_io.io_queue);
        let positions: Vec<ChunkPos> = ops.iter().map(IoOp::pos).collect();
        let answer = self
            .shared
            .chunk_io
            .get(&self.shared.backend)
            .submit(&self.dimension().key, ops);
        let tx = self.sched.jobs_tx.clone();
        self.sched.jobs_in_flight += 1;
        tokio::spawn(async move {
            let _ = tx.send(match answer.await {
                Ok(results) => JobResult::Io(results),
                Err(_) => JobResult::Panicked {
                    stage: Stage::Io,
                    positions,
                },
            });
        });
    }

    pub(super) fn pump_light(&mut self) {
        if self.light.light_ops.is_empty() {
            return;
        }
        let Some(mut lighting) = self.light.lighting.take() else {
            return;
        };
        let ops = std::mem::take(&mut self.light.light_ops);
        self.light.halo_in_job = ops
            .iter()
            .filter_map(|op| match op {
                LightOp::Add {
                    pos, owned: false, ..
                } => Some(*pos),
                _ => None,
            })
            .collect();
        let pos = ChunkPos::new(self.pos.x * REGION_CHUNKS, self.pos.z * REGION_CHUNKS);
        self.spawn_job(Stage::Light, pos, move || {
            let changes = lighting.apply(ops);
            JobResult::Lit { lighting, changes }
        });
    }

    /// Rebuilds the light state from scratch after a lighting job was lost.
    pub(super) fn resync_lighting(&mut self) {
        let dimension = self.dimension();
        self.light.lighting = Some(Box::new(Lighting::new(
            dimension.shape,
            dimension.has_sky_light(),
        )));
        self.light.light_ops.clear();
        self.light.halo_in_job.clear();
        for (pos, column) in &self.columns {
            self.light.light_ops.push(LightOp::Add {
                pos: *pos,
                chunk: column.chunk.clone(),
                owned: true,
            });
        }
        for (pos, chunk) in &self.border.halo {
            self.border
                .halo_pending
                .entry(*pos)
                .or_insert(self.sched.server_tick);
            self.light.light_ops.push(LightOp::Add {
                pos: *pos,
                chunk: chunk.clone(),
                owned: false,
            });
        }
    }

    pub(super) fn lit(&mut self, changes: Vec<LightChange>) {
        let _span = tuff_timings::span!("region.light_apply");
        let mut creatures = Vec::new();
        let mut settled = Vec::new();
        for change in changes {
            let Some(column) = self.columns.get_mut(&change.pos) else {
                continue;
            };
            if column.lit {
                let (sky, block) = changed_sections(
                    column.chunk.light(),
                    &change.light,
                    change.sky_changed,
                    change.block_changed,
                );
                if sky | block == 0 {
                    continue;
                }
                if column.meta.relight {
                    column.blocks_dirty = true;
                }
                let holders = column
                    .viewers
                    .keys()
                    .filter(|id| !column.awaiting_packet.contains(id));
                let mut holders = holders.peekable();
                if holders.peek().is_some() {
                    let dirty = self.light.light_dirty.entry(change.pos).or_default();
                    dirty.sky |= sky;
                    dirty.block |= block;
                    dirty.holders.extend(holders);
                }
            } else {
                column.lit = true;
                column
                    .awaiting_packet
                    .extend(column.viewers.keys().copied());
                if std::mem::take(&mut column.creatures_pending) {
                    creatures.push(change.pos);
                }
                settled.push(change.pos);
            }
            column.version += 1;
            column.packet = None;
            column.chunk.set_light(Some(change.light));
            if !column.awaiting_packet.is_empty() {
                self.request_packet(change.pos);
            }
        }
        for pos in settled {
            self.request_neighbour_packets(pos);
        }
        for pos in creatures {
            self.chunk_generated(pos);
        }
    }

    /// Sends the light sections that changed in a chunk since the last tick
    /// to the viewers that held it when they changed. Viewers waiting for
    /// the chunk get its light with the chunk itself.
    pub(super) fn flush_light(&mut self) {
        if self.light.light_dirty.is_empty() {
            return;
        }
        let dimension = self.dimension;
        for (pos, dirty) in std::mem::take(&mut self.light.light_dirty) {
            let Some(column) = self.columns.get(&pos) else {
                continue;
            };
            let Some(light) = column.chunk.light() else {
                continue;
            };
            let mut holders = dirty
                .holders
                .iter()
                .filter(|id| !column.awaiting_packet.contains(id))
                .filter_map(|id| column.viewers.get(id))
                .peekable();
            if holders.peek().is_none() {
                continue;
            }
            let body = chunk_data::light_update_body(pos.x, pos.z, light, dirty.sky, dirty.block);
            let update = packets([framed(&play::LightUpdate(body))]);
            for viewer in holders {
                viewer.send(PlayerEvent::ChunkPackets {
                    dimension,
                    pos,
                    packets: update.clone(),
                });
            }
        }
    }

    /// Whether every neighbour of `pos` that this region is loading has
    /// been lit, so the light sent with `pos` already includes theirs.
    fn neighbours_lit(&self, pos: ChunkPos) -> bool {
        neighbours(pos).all(|at| {
            !self.chunk_io.loading.contains_key(&at)
                && self.columns.get(&at).is_none_or(|c| c.lit)
                && !self.halo_gates(at)
        })
    }

    fn halo_gates(&self, at: ChunkPos) -> bool {
        self.border
            .halo_pending
            .get(&at)
            .is_some_and(|since| self.sched.server_tick - since < HALO_WAIT_TICKS)
    }

    /// Called once the light job that added `settled` halo chunks is done.
    pub(super) fn halo_settled(&mut self, settled: Vec<ChunkPos>) {
        for pos in &settled {
            self.border.halo_pending.remove(pos);
        }
        for pos in settled {
            self.request_neighbour_packets(pos);
        }
    }

    /// Retries the packets of `pos`'s neighbours that were held back
    /// waiting for it.
    pub(super) fn request_neighbour_packets(&mut self, pos: ChunkPos) {
        for at in neighbours(pos) {
            if self
                .columns
                .get(&at)
                .is_some_and(|c| !c.awaiting_packet.is_empty())
            {
                self.request_packet(at);
            }
        }
    }

    pub(super) fn on_job(&mut self, result: JobResult) {
        let _span = tuff_timings::span!("region.job_result");
        self.sched.jobs_in_flight -= 1;
        match result {
            JobResult::Panicked { stage, positions } => {
                log::error!(
                    "region {:?}: {stage:?} job panicked for {positions:?}",
                    self.pos
                );
                if matches!(stage, Stage::Io) {
                    self.chunk_io.io_busy = false;
                    self.pregen_io_lost(&positions);
                }
                if matches!(stage, Stage::Light) {
                    self.resync_lighting();
                    return;
                }
                for pos in positions {
                    if self.chunk_io.loading.remove(&pos).is_some() {
                        self.request_neighbour_packets(pos);
                    }
                    if let Some(column) = self.columns.get_mut(&pos) {
                        match stage {
                            Stage::Encode => column.encoding = false,
                            Stage::Save | Stage::Io => {
                                column.saving = column.saving.saturating_sub(1)
                            }
                            Stage::Prepare | Stage::Light => {}
                        }
                    }
                }
            }
            JobResult::Io(results) => {
                self.chunk_io.io_busy = false;
                for result in results {
                    match result {
                        IoResult::Read {
                            pos,
                            blocks,
                            entities,
                            pois,
                        } => self.prepare(pos, blocks, entities, pois),
                        IoResult::Written(pos) => {
                            if let Some(column) = self.columns.get_mut(&pos) {
                                column.saving -= 1;
                            }
                        }
                        IoResult::Removed { pos, ok } => self.pregen_removed(pos, ok),
                    }
                }
            }
            JobResult::Prepared(prepared) if prepared.absent => self.absent(prepared.pos),
            JobResult::Prepared(prepared) if prepared.pending => self.await_generation(*prepared),
            JobResult::Prepared(prepared) => self.install(*prepared),
            JobResult::Lit { lighting, changes } => {
                self.light.lighting = Some(lighting);
                self.lit(changes);
                let settled = std::mem::take(&mut self.light.halo_in_job);
                self.halo_settled(settled);
            }
            JobResult::Encoded {
                pos,
                version,
                packet,
                obfuscated,
            } => self.encoded(pos, version, packet, obfuscated),
            JobResult::Serialized {
                pos,
                blocks,
                entities,
                pois,
            } => self.chunk_io.io_queue.push(IoOp::Write {
                pos,
                blocks,
                entities,
                pois,
            }),
        }
    }

    fn peek_only(&self, pos: ChunkPos) -> bool {
        self.chunk_io.loading.get(&pos).is_some_and(|l| {
            !l.viewers.is_empty()
                && l.held.is_empty()
                && l.entities.is_empty()
                && l.viewers.keys().all(|v| l.peeking.contains(v))
        }) && !self.chunk_io.forced.contains(&pos)
            && !self.chunk_io.pregen.wants(pos)
            && !self
                .chunk_io
                .chunk_queries
                .iter()
                .any(|q| q.chunks.contains(&pos))
    }

    /// A chunk only peeked at turned out not to exist: nothing is made, and
    /// its viewers wait for someone else to make it.
    fn absent(&mut self, pos: ChunkPos) {
        if !self.peek_only(pos) {
            if self.chunk_io.loading.contains_key(&pos) {
                if let Some(loading) = self.chunk_io.loading.get_mut(&pos) {
                    loading.peeking.clear();
                }
                self.chunk_io.io_queue.push(IoOp::Read(pos));
            }
            return;
        }
        if let Some(loading) = self.chunk_io.loading.remove(&pos) {
            self.chunk_io
                .peekers
                .entry(pos)
                .or_default()
                .extend(loading.viewers);
        }
    }

    pub(super) fn prepare(
        &mut self,
        pos: ChunkPos,
        blocks: Result<Option<Blob>, String>,
        entities: Result<Option<Blob>, String>,
        pois: Result<Option<Blob>, String>,
    ) {
        let shared = self.shared.clone();
        let dimension = self.dimension;
        let peek = self.peek_only(pos);
        self.spawn_job(Stage::Prepare, pos, move || {
            let mut prepared = prepare_chunk(&shared, dimension, pos, blocks, entities, peek);
            prepared.saved_pois = saved_pois(&shared, pos, pois);
            JobResult::Prepared(Box::new(prepared))
        });
    }

    /// Asks the generation pipeline for a chunk that has never been
    /// generated; it is installed when [`RegionInternal::Generated`]
    /// arrives.
    pub(super) fn await_generation(&mut self, prepared: Prepared) {
        let pos = prepared.pos;
        if self.sched.closing.is_some() {
            self.chunk_io.loading.remove(&pos);
            return;
        }
        if !self.chunk_io.loading.contains_key(&pos) {
            return;
        }
        let priority = self.generation_priority(pos);
        let asked = self
            .shared
            .generation(self.dimension)
            .is_some_and(|generation| generation.want(pos, self.handle.clone(), priority));
        if asked {
            self.chunk_io.generating.insert(pos, prepared);
            return;
        }
        let shared = self.shared.clone();
        let dimension = self.dimension;
        self.spawn_job(Stage::Prepare, pos, move || {
            let dimension = shared.dimension(dimension);
            let chunk = dimension.generator.generate(pos, dimension.shape);
            JobResult::Prepared(Box::new(Prepared {
                pois: Some(crate::world::poi::scan(pos, &chunk)),
                chunk,
                pending: false,
                ..prepared
            }))
        });
    }

    /// Withdraws the generation requests of chunks nothing waits for any
    /// more (their viewers moved on), so the pipeline's backlog follows
    /// what players can see instead of everything they ever saw. Only
    /// chunks whose prepared state is the default for a missing chunk are
    /// dropped, so one that arrives anyway can be installed as if asked.
    pub(super) fn cancel_unwanted_generation(&mut self, occupied: &HashSet<ChunkPos>) {
        if self.chunk_io.generating.is_empty() {
            return;
        }
        let queried: HashSet<ChunkPos> = self
            .chunk_io
            .chunk_queries
            .iter()
            .flat_map(|query| query.chunks.iter().copied())
            .collect();
        let unwanted: Vec<ChunkPos> = self
            .chunk_io
            .generating
            .iter()
            .filter(|(pos, prepared)| {
                self.chunk_io.loading.get(pos).is_some_and(|l| {
                    l.viewers.is_empty() && l.held.is_empty() && l.entities.is_empty()
                }) && !occupied.contains(pos)
                    && !self.chunk_io.forced.contains(pos)
                    && !queried.contains(pos)
                    && !self.chunk_io.pregen.wants(**pos)
                    && prepared.is_blank()
            })
            .map(|(pos, _)| *pos)
            .collect();
        if unwanted.is_empty() {
            return;
        }
        let pipeline = self.shared.generation(self.dimension).cloned();
        for pos in unwanted {
            self.chunk_io.generating.remove(&pos);
            self.chunk_io.loading.remove(&pos);
            self.chunk_io.withdrawn.insert(pos);
            if let Some(pipeline) = &pipeline {
                pipeline.unwant(pos, self.pos);
            }
        }
    }

    fn generation_priority(&self, pos: ChunkPos) -> u32 {
        use crate::world::generator::{BACKGROUND, URGENT, VIEWED};
        let waited_on = self.chunk_io.forced.contains(&pos)
            || self
                .chunk_io
                .chunk_queries
                .iter()
                .any(|q| q.chunks.contains(&pos));
        let viewed = self
            .chunk_io
            .loading
            .get(&pos)
            .is_some_and(|l| !l.viewers.is_empty() || !l.held.is_empty());
        if waited_on {
            URGENT
        } else if !viewed && self.chunk_io.pregen.wants(pos) {
            BACKGROUND
        } else {
            VIEWED
        }
    }

    /// Installs a finished chunk. One whose request was withdrawn but that
    /// was already on its way is installed all the same (unloading once
    /// idle, or taking the viewers of a read of it started since), so the
    /// pipeline never has to make a delivered chunk twice.
    pub(super) fn generated(&mut self, pos: ChunkPos, finished: crate::world::generator::Finished) {
        let withdrawn = self.chunk_io.withdrawn.remove(&pos);
        let prepared = match self.chunk_io.generating.remove(&pos) {
            Some(prepared) => Some(prepared),
            None if self.sched.closing.is_some() || self.columns.contains_key(&pos) => None,
            None if withdrawn => Some(Prepared::blank(pos, self.dimension().shape)),
            None => None,
        };
        let Some(mut prepared) = prepared else {
            return;
        };
        let crate::world::generator::Finished {
            chunk,
            block_entities,
            structures,
            post_processing,
            fluid_ticks,
            block_ticks,
            entities,
        } = finished;
        let shared = self.shared.clone();
        let mut fresh = Vec::new();
        for mut nbt in entities {
            let finalize = nbt.remove(crate::world::entity::FINALIZE_SPAWN).is_some();
            let (live, raw) = shared
                .entity_types
                .load_all(vec![nbt], || shared.next_entity_id());
            prepared.raw_entities.extend(raw);
            if finalize {
                fresh.extend(live);
            } else {
                prepared.entities.extend(live);
            }
        }
        if !fresh.is_empty() {
            let mut budget = 0;
            self.with_context(&[], &mut budget, |cx| {
                cx.state.get::<crate::world::entity::SettingUp>().0 =
                    Some(crate::world::entity::SpawnReason::Structure);
                for entity in &mut fresh {
                    entity.finalize_spawn(cx, 0);
                }
                cx.state.get::<crate::world::entity::SettingUp>().0 = None;
            });
            prepared.entities.extend(fresh);
        }
        let (typed, raw) = block_entity::split(block_entities);
        let meta = &mut prepared.meta;
        if let Some(structures) = structures {
            meta.extra.insert("structures", structures);
        }
        fluids::store_post_processing(&mut meta.extra, &post_processing, chunk.shape());
        for (name, ticks) in [("fluid_ticks", fluid_ticks), ("block_ticks", block_ticks)] {
            if !ticks.is_empty() {
                let list = ticks
                    .into_iter()
                    .map(|t| tuff_nbt::Tag::Compound(t.to_nbt()))
                    .collect();
                meta.extra.insert(name, tuff_nbt::Tag::List(list));
            }
        }
        meta.extra.remove("block_entities");
        meta.block_entities.extend(typed);
        meta.raw_block_entities.extend(raw);
        block_entity::fill_missing(
            &chunk,
            pos,
            &mut meta.block_entities,
            &mut meta.raw_block_entities,
        );
        prepared.pois = Some(crate::world::poi::scan(pos, &chunk));
        prepared.chunk = chunk;
        prepared.pending = false;
        self.install(prepared);
    }

    pub(super) fn install(&mut self, prepared: Prepared) {
        let pos = prepared.pos;
        if self.columns.contains_key(&pos) {
            return;
        }
        let mut loading = self.chunk_io.loading.remove(&pos).unwrap_or_default();
        if let Some(waiting) = self.chunk_io.peekers.remove(&pos) {
            for (id, handle) in waiting {
                loading.viewers.entry(id).or_insert(handle);
            }
        }
        let mut meta = prepared.meta;
        let behaviours = self.shared.growth_behaviors.clone();
        let redstone = self.shared.redstone_behaviors.clone();
        let rules = self.shared.block_rules.clone();
        let block_ticks = ticks::take(&mut meta.extra, "block_ticks", self.sched.tick, |name| {
            Block::from_name(name).filter(|block| {
                block.component().is_some()
                    || rules.ticks_when_covered(*block)
                    || behaviours.has_scheduled(*block)
                    || redstone.get(*block).is_active()
            })
        });
        let fluid_ticks = ticks::take(&mut meta.extra, "fluid_ticks", self.sched.tick, |name| {
            tuff_vanilla_data::fluid::Fluid::from_name(name).filter(|fluid| fluid.rules().is_some())
        });
        let post_processing =
            fluids::take_post_processing(&mut meta.extra, pos, self.dimension().shape);
        let inhabited_time = match meta.extra.remove("InhabitedTime") {
            Some(tuff_nbt::Tag::Long(v)) => v.max(0),
            _ => 0,
        };
        let mut column = Column {
            chunk: prepared.chunk,
            meta,
            inhabited_time,
            persist: prepared.persist,
            entities_persist: prepared.entities_persist,
            blocks_dirty: prepared.generated,
            pois_dirty: false,
            entities_dirty: !loading.entities.is_empty()
                || (prepared.generated
                    && !(prepared.raw_entities.is_empty() && prepared.entities.is_empty())),
            other_entities: prepared.raw_entities,
            version: 0,
            lit: false,
            packet: None,
            packet_used: 0,
            snapshot: None,
            xray_cache: None,
            encoding: false,
            awaiting_packet: loading.viewers.keys().copied().collect(),
            resending: HashSet::new(),
            viewers: loading.viewers,
            held: loading.held,
            last_used: self.sched.server_tick,
            saving: 0,
            unloading: false,
            block_ticks,
            fluid_ticks,
            post_processing,
            creatures_pending: prepared.generated
                && self
                    .shared
                    .dimension(self.dimension)
                    .generator
                    .spawns_creatures(),
        };
        column.last_used = self.sched.server_tick;
        let found = prepared
            .pois
            .unwrap_or_else(|| crate::world::poi::scan(pos, &column.chunk));
        let (records, changed) = crate::world::poi::merge(found, prepared.saved_pois.as_deref());
        column.pois_dirty = changed;
        if prepared.saved_pois.is_none() {
            self.chunk_io.pois.mark_uncounted(pos);
        }
        self.chunk_io.pois.install(pos, records);
        self.light.light_ops.push(LightOp::Add {
            pos,
            chunk: column.chunk.clone(),
            owned: true,
        });
        for (at, entity) in &column.meta.block_entities {
            if entity.kind.ticks() {
                self.menus.ticking.insert(*at);
            }
        }
        self.border.neighbourhood_cache = None;
        self.columns.insert(pos, column);
        self.block_entity_generation += 1;
        self.load_moving_blocks(pos);
        let shared = self.shared.clone();
        let arrived = prepared
            .entities
            .into_iter()
            .filter(|entity| {
                let fresh = shared.claim_entity_uuid(entity.base().uuid);
                if !fresh {
                    log::warn!(
                        "skipping {} at {pos:?}: its UUID already exists",
                        entity.saved_id()
                    );
                }
                fresh
            })
            .map(|entity| (entity, 0));
        for (entity, seq) in arrived.chain(loading.entities) {
            if entity.base().chunk() == pos {
                self.add_entity(entity, seq, false);
            } else {
                self.receive_entity(entity, seq);
            }
        }
        if self.pos.is_border(pos) {
            self.send_halo(pos);
        }
        self.poll_chunk_queries();
    }

    pub(super) fn encoded(
        &mut self,
        pos: ChunkPos,
        version: u64,
        packet: Bytes,
        obfuscated: Option<(Arc<Chunk>, Box<crate::world::anti_xray::Cache>)>,
    ) {
        let Some(column) = self.columns.get_mut(&pos) else {
            return;
        };
        column.encoding = false;
        let sent = obfuscated.map(|(sent, cache)| {
            column.xray_cache = Some(cache);
            sent
        });
        if column.version != version {
            self.request_packet(pos);
            return;
        }
        column.packet = Some(packet.clone());
        column.packet_used = self.sched.server_tick;
        let snapshot = sent.unwrap_or_else(|| Arc::new(column.chunk.clone()));
        column.snapshot = Some(snapshot.clone());
        let mut delivered = Vec::new();
        let dimension = self.dimension;
        let resending = std::mem::take(&mut column.resending);
        for viewer in std::mem::take(&mut column.awaiting_packet) {
            if let Some(handle) = column.viewers.get(&viewer) {
                handle.send(PlayerEvent::Chunk {
                    dimension,
                    pos,
                    packet: packet.clone(),
                    snapshot: Some(snapshot.clone()),
                });
                if !resending.contains(&viewer) {
                    delivered.push(handle.clone());
                }
            }
        }
        for viewer in &delivered {
            self.send_entities(pos, viewer);
        }
    }

    pub(super) fn request_packet(&mut self, pos: ChunkPos) {
        let Some(column) = self.columns.get_mut(&pos) else {
            return;
        };
        if column.encoding || column.awaiting_packet.is_empty() || !column.lit {
            return;
        }
        if !self.neighbours_lit(pos) {
            return;
        }
        let Some(column) = self.columns.get_mut(&pos) else {
            return;
        };
        let only_mirrors = column
            .awaiting_packet
            .iter()
            .all(|id| column.viewers.get(id).is_some_and(|h| h.mirror_only));
        if only_mirrors {
            let snapshot = column
                .packet
                .as_ref()
                .and(column.snapshot.clone())
                .unwrap_or_else(|| Arc::new(column.chunk.clone()));
            let resending = std::mem::take(&mut column.resending);
            let mut delivered = Vec::new();
            for viewer in std::mem::take(&mut column.awaiting_packet) {
                if let Some(handle) = column.viewers.get(&viewer) {
                    handle.send(PlayerEvent::Chunk {
                        dimension: self.dimension,
                        pos,
                        packet: Bytes::new(),
                        snapshot: Some(snapshot.clone()),
                    });
                    if !resending.contains(&viewer) {
                        delivered.push(handle.clone());
                    }
                }
            }
            for viewer in &delivered {
                self.send_entities(pos, viewer);
            }
            return;
        }
        column.encoding = true;
        let chunk = column.chunk.clone();
        let version = column.version;
        let mut block_entities = chunk_nbt::client_block_entities(&column.meta);
        let xray = self.anti_xray_job(pos);
        self.spawn_job(Stage::Encode, pos, move || {
            let obfuscated = xray.map(|job| job.run(&chunk, pos, &mut block_entities));
            let sent = obfuscated.as_ref().map_or(&chunk, |(sent, _)| &**sent);
            JobResult::Encoded {
                pos,
                version,
                packet: framed(&play::LevelChunkWithLight(sent.encode(
                    pos.x,
                    pos.z,
                    &block_entities,
                ))),
                obfuscated,
            }
        });
    }

    pub(super) fn load(&mut self, pos: ChunkPos) -> &mut Loading {
        if !self.chunk_io.loading.contains_key(&pos) && !self.chunk_io.pregen.parked(pos) {
            self.chunk_io.io_queue.push(IoOp::Read(pos));
        }
        if self.pos.is_border(pos) && !self.chunk_io.loading.contains_key(&pos) {
            for neighbor in self.border.neighbors.values() {
                if neighbor.pos.is_adjacent(pos) {
                    neighbor.internal(RegionInternal::HaloLoading { pos });
                }
            }
        }
        self.chunk_io.loading.entry(pos).or_default()
    }

    pub(super) fn unload_idle(&mut self) {
        let _span = tuff_timings::span!("region.unload");
        self.sync_pois();
        let occupied: HashSet<ChunkPos> = self.ents.presence.values().map(|p| p.chunk).collect();
        self.cancel_unwanted_generation(&occupied);
        let tick = self.sched.server_tick;
        self.border
            .halo_pending
            .retain(|_, since| tick - *since < HALO_WAIT_TICKS);
        let mut remove = Vec::new();
        let mut save = Vec::new();
        let autosaves = self.shared.saves().autosaves();
        for (pos, column) in &mut self.columns {
            if column.packet.is_some()
                && self.sched.server_tick - column.packet_used >= PACKET_KEEP_TICKS
            {
                column.packet = None;
                column.snapshot = None;
            }
            if !column.viewers.is_empty()
                || !column.held.is_empty()
                || occupied.contains(pos)
                || self.chunk_io.forced.contains(pos)
            {
                column.last_used = self.sched.server_tick;
                if column.unloading {
                    column.unloading = false;
                    self.block_entity_generation += 1;
                }
                continue;
            }
            if self.sched.server_tick - column.last_used < UNLOAD_DELAY_TICKS {
                continue;
            }
            if column.needs_save() {
                if autosaves {
                    save.push(*pos);
                }
            } else if column.saving == 0 {
                remove.push(*pos);
            }
            if !column.unloading {
                column.unloading = true;
                self.block_entity_generation += 1;
            }
        }
        for pos in save {
            self.start_save(pos);
        }
        for pos in remove {
            self.drop_column(pos);
        }
        let held: Vec<ChunkPos> = self
            .columns
            .iter()
            .filter(|(_, c)| c.lit && !c.encoding && !c.awaiting_packet.is_empty())
            .map(|(pos, _)| *pos)
            .collect();
        for pos in held {
            self.request_packet(pos);
        }
    }

    /// Unloads `pos` as it is, without saving it.
    pub(super) fn drop_column(&mut self, pos: ChunkPos) {
        let Some(column) = self.columns.remove(&pos) else {
            return;
        };
        self.block_entity_generation += 1;
        self.border.neighbourhood_cache = None;
        self.chunk_io.pois.unload(pos);
        for at in column.meta.block_entities.keys() {
            self.menus.ticking.remove(at);
            self.menus.hopper_ticks.remove(at);
        }
        let viewed: Vec<BlockPos> = self
            .menus
            .viewers
            .keys()
            .filter(|at| ChunkPos::of_block(**at) == pos)
            .copied()
            .collect();
        for at in viewed {
            self.close_viewers(at);
        }
        self.light.light_ops.push(LightOp::Remove(pos));
        self.forget_entities(pos);
        for neighbor in self.border.neighbors.values() {
            if neighbor.pos.is_adjacent(pos) {
                neighbor.internal(RegionInternal::Halo { pos, chunk: None });
            }
        }
    }

    pub(super) fn start_save(&mut self, pos: ChunkPos) {
        let entities = self
            .columns
            .get(&pos)
            .filter(|column| column.entities_dirty && column.entities_persist)
            .map(|_| self.saved_entities(pos));
        let tick = self.sched.tick as i64;
        let light_idle = self.light.light_idle();
        let shape = self.dimension().shape;
        let unsaved_pois = self.chunk_io.pois.is_unsaved(pos);
        let pois = self
            .columns
            .get(&pos)
            .filter(|column| (column.pois_dirty || unsaved_pois) && column.persist)
            .map(|_| self.chunk_io.pois.records(pos).to_vec());
        if pois.is_some() {
            self.chunk_io.pois.saved(pos);
        }
        let Some(column) = self.columns.get_mut(&pos) else {
            return;
        };
        let blocks = (column.blocks_dirty && column.persist).then(|| {
            let mut chunk = column.chunk.clone();
            if !(column.lit && light_idle) {
                chunk.set_light(None);
            }
            let mut meta = column.meta.save_picture();
            ticks::store(
                &mut meta.extra,
                "block_ticks",
                &column.block_ticks,
                tick as u64,
                Block::name,
            );
            ticks::store(
                &mut meta.extra,
                "fluid_ticks",
                &column.fluid_ticks,
                tick as u64,
                tuff_vanilla_data::fluid::Fluid::name,
            );
            fluids::store_post_processing(&mut meta.extra, &column.post_processing, shape);
            if column.inhabited_time > 0 {
                meta.extra.insert("InhabitedTime", column.inhabited_time);
            }
            (chunk, meta)
        });
        column.blocks_dirty = false;
        column.entities_dirty = false;
        column.pois_dirty = false;
        column.saving += 1;
        let (blocks, entities) = if self.shared.save_normalisers.is_empty() {
            (blocks, entities)
        } else {
            let view = self.shared.save_normalisers.apply(SaveView {
                dimension: self.dimension().key.clone(),
                pos,
                blocks,
                entities,
            });
            (view.blocks, view.entities)
        };
        let codec = self.shared.backend.chunk_codec();
        let biomes = self.shared.dimensions.biomes().clone();
        self.spawn_job(Stage::Save, pos, move || {
            let blocks = blocks.and_then(|(chunk, meta)| {
                codec
                    .encode_chunk_in(&chunk, &meta, pos, tick, &biomes)
                    .map_err(|err| log::error!("failed to serialize chunk {pos:?}: {err}"))
                    .ok()
            });
            let entities = match entities {
                None => EntityWrite::Keep,
                Some(entities) => match codec.encode_entities(pos, &entities) {
                    Ok(Some(blob)) => EntityWrite::Write(blob),
                    Ok(None) => EntityWrite::Clear,
                    Err(err) => {
                        log::error!("failed to serialize entities {pos:?}: {err}");
                        EntityWrite::Keep
                    }
                },
            };
            let pois = match pois {
                None => EntityWrite::Keep,
                Some(records) => match codec.encode_pois(pos, &records) {
                    Ok(Some(blob)) => EntityWrite::Write(blob),
                    Ok(None) => EntityWrite::Clear,
                    Err(err) => {
                        log::error!("failed to serialize points of interest {pos:?}: {err}");
                        EntityWrite::Keep
                    }
                },
            };
            JobResult::Serialized {
                pos,
                blocks,
                entities,
                pois,
            }
        });
    }

    pub(super) fn flush_halo(&mut self) {
        let _span = tuff_timings::span!("region.halo");
        self.sync_pois();
        for pos in std::mem::take(&mut self.border.halo_dirty) {
            self.send_halo(pos);
        }
    }

    pub(super) fn send_halo(&self, pos: ChunkPos) {
        let Some(column) = self.columns.get(&pos) else {
            return;
        };
        for neighbor in self.border.neighbors.values() {
            if neighbor.pos.is_adjacent(pos) {
                neighbor.internal(RegionInternal::Halo {
                    pos,
                    chunk: Some(column.chunk.clone()),
                });
            }
        }
    }
}

/// Light sections changed in a chunk this tick, and the viewers that held
/// the chunk when they changed.
#[derive(Default)]
pub(super) struct LightDirty {
    sky: u64,
    block: u64,
    holders: HashSet<PlayerId>,
}

/// How long a packet waits for a neighbour region's chunk before sending
/// anyway (and relighting the border later).
const HALO_WAIT_TICKS: u64 = 100;

fn neighbours(pos: ChunkPos) -> impl Iterator<Item = ChunkPos> {
    (-1..=1)
        .flat_map(move |dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| dx != 0 || dz != 0)
        .map(move |(dx, dz)| ChunkPos::new(pos.x + dx, pos.z + dz))
}

/// The sky and block sections, among those flagged, whose light differs
/// between `old` and `new`.
fn changed_sections(
    old: Option<&ChunkLight>,
    new: &ChunkLight,
    sky: u64,
    block: u64,
) -> (u64, u64) {
    let differs = |old: Option<&[SectionLight]>, new: &[SectionLight], mask: u64| {
        new.iter().enumerate().fold(0u64, |acc, (i, section)| {
            if mask >> i & 1 == 1 && old.and_then(|old| old.get(i)) != Some(section) {
                acc | 1 << i
            } else {
                acc
            }
        })
    };
    (
        differs(old.map(|l| l.sky.as_slice()), &new.sky, sky),
        differs(old.map(|l| l.block.as_slice()), &new.block, block),
    )
}

/// The points of interest saved for a chunk; unreadable ones are rebuilt
/// from its blocks.
fn saved_pois(
    shared: &Shared,
    pos: ChunkPos,
    blob: Result<Option<Blob>, String>,
) -> Option<Vec<tuff_world_model::poi::Record>> {
    let decoded = blob.and_then(|blob| match blob {
        None => Ok(None),
        Some(blob) => shared.backend.chunk_codec().decode_pois(&blob, pos),
    });
    decoded.unwrap_or_else(|err| {
        log::error!(
            "points of interest in chunk {pos:?} are unreadable and will be rebuilt: {err}"
        );
        None
    })
}

/// Decodes stored data or generates terrain. Stored light is discarded: the
/// region relights every chunk it loads.
fn prepare_chunk(
    shared: &Shared,
    dimension: DimensionId,
    pos: ChunkPos,
    blocks: Result<Option<Blob>, String>,
    entities: Result<Option<Blob>, String>,
    peek: bool,
) -> Prepared {
    let dimension = shared.dimension(dimension);
    let codec = shared.backend.chunk_codec();
    let decoded = blocks.and_then(|blob| match blob {
        None => Ok(None),
        Some(blob) => codec
            .decode_chunk_in(&blob, pos, dimension.shape, shared.dimensions.biomes())
            .map(Some),
    });
    if peek && matches!(decoded, Ok(None) | Ok(Some(Decoded::Ungenerated(_)))) {
        return Prepared {
            absent: true,
            ..Prepared::blank(pos, dimension.shape)
        };
    }
    let staged = shared.generation(dimension.id).is_some();
    let generate = || {
        if staged {
            Chunk::empty(dimension.shape)
        } else {
            dimension.generator.generate(pos, dimension.shape)
        }
    };
    let (chunk, meta, persist, generated) = match decoded {
        Ok(Some(Decoded::Chunk(chunk, meta))) => {
            let mut chunk = *chunk;
            if meta.below_zero_retrogen {
                dimension
                    .generator
                    .retrogen_below_zero(pos, dimension.shape, &mut chunk);
            }
            (chunk, meta, true, false)
        }
        Ok(Some(Decoded::Ungenerated(extra))) => {
            let meta = ChunkMeta {
                extra,
                ..ChunkMeta::generated()
            };
            (generate(), meta, true, true)
        }
        Ok(None) => (generate(), ChunkMeta::generated(), true, true),
        Err(err) => {
            log::error!("chunk {pos:?} is unreadable and will not be saved: {err}");
            (generate(), ChunkMeta::generated(), false, false)
        }
    };
    let pending = staged && (generated || !persist);
    let entities = entities.and_then(|blob| match blob {
        None => Ok(Vec::new()),
        Some(blob) => codec.decode_entities(&blob, pos),
    });
    let (saved, entities_persist) = match entities {
        Ok(entities) => (entities, true),
        Err(err) => {
            log::error!("entities in chunk {pos:?} are unreadable and will not be saved: {err}");
            (Vec::new(), false)
        }
    };
    let (entities, raw_entities) = shared
        .entity_types
        .load_all(saved, || shared.next_entity_id());
    let pois = (!pending).then(|| crate::world::poi::scan(pos, &chunk));
    Prepared {
        saved_pois: None,
        pos,
        chunk,
        meta,
        persist,
        generated: generated || pending,
        pending,
        entities,
        raw_entities,
        entities_persist,
        pois,
        absent: false,
    }
}

//! The region's side of anti-xray (see `world::anti_xray`): its
//! dimension's plan, kept current with the settings, the inputs of each
//! chunk encode, and resending the true blocks around changes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use tokio::sync::watch;
use tuff_vanilla_data::BlockState;

use super::RegionActor;
use super::blocks::section_index;
use crate::config::Change;
use crate::world::Shared;
use crate::world::anti_xray::{self, AROUND, Cache, Plan};
use crate::world::dimension::DimensionId;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

pub(super) struct AntiXray {
    plan: Option<Arc<Plan>>,
    changes: watch::Receiver<Option<Change>>,
}

impl AntiXray {
    pub(super) fn new(shared: &Shared, dimension: DimensionId) -> Self {
        Self {
            changes: shared.config.subscribe(),
            plan: build(shared, dimension),
        }
    }
}

/// Fill mode's random blocks must not repeat across server runs, or two
/// runs' chunks could be compared to tell them apart; seeded test worlds
/// stay reproducible.
static RUN_SALT: LazyLock<u64> = LazyLock::new(rand::random);

fn build(shared: &Shared, dimension: DimensionId) -> Option<Arc<Plan>> {
    let seed = shared.random_seed.unwrap_or(*RUN_SALT) ^ shared.seed as u64;
    shared
        .anti_xray
        .plan(&shared.config, shared.dimension(dimension), seed)
        .map(Arc::new)
}

/// What a chunk encode needs to obfuscate the chunk off the actor.
pub(super) struct EncodeJob {
    plan: Arc<Plan>,
    around: [Option<Chunk>; 4],
    cache: Box<Cache>,
}

impl EncodeJob {
    /// The chunk as sent, with the block entities clients may know about.
    pub(super) fn run(
        mut self,
        chunk: &Chunk,
        pos: ChunkPos,
        block_entities: &mut Vec<(BlockPos, u32, Compound)>,
    ) -> (Arc<Chunk>, Box<Cache>) {
        let _span = tuff_timings::span!("anti_xray.obfuscate");
        let (sent, _) = anti_xray::obfuscate(chunk, pos, &self.around, &self.plan, &mut self.cache);
        block_entities
            .retain(|(at, _, _)| sent.get(at.x, at.y, at.z) == chunk.get(at.x, at.y, at.z));
        (Arc::new(sent), self.cache)
    }
}

impl RegionActor {
    /// Picks up changed settings: every chunk is encoded again with the new
    /// plan and sent again to the viewers that already had it.
    pub(super) fn refresh_anti_xray(&mut self) {
        if !self
            .chunk_io
            .anti_xray
            .changes
            .has_changed()
            .unwrap_or(false)
        {
            return;
        }
        self.chunk_io.anti_xray.changes.borrow_and_update();
        let plan = build(&self.shared, self.dimension);
        if plan.as_deref() == self.chunk_io.anti_xray.plan.as_deref() {
            return;
        }
        self.chunk_io.anti_xray.plan = plan;
        let mut waiting = Vec::new();
        for (pos, column) in &mut self.columns {
            column.version += 1;
            column.packet = None;
            column.xray_cache = None;
            for viewer in column.viewers.keys() {
                if column.awaiting_packet.insert(*viewer) {
                    column.resending.insert(*viewer);
                }
            }
            if !column.awaiting_packet.is_empty() {
                waiting.push(*pos);
            }
        }
        for pos in waiting {
            self.request_packet(pos);
        }
    }

    pub(super) fn anti_xray_job(&mut self, pos: ChunkPos) -> Option<EncodeJob> {
        let plan = self.chunk_io.anti_xray.plan.clone()?;
        let around = AROUND.map(|(dx, dz)| {
            let at = ChunkPos::new(pos.x + dx, pos.z + dz);
            self.columns
                .get(&at)
                .map(|column| &column.chunk)
                .or_else(|| self.border.halo.get(&at))
                .cloned()
        });
        let cache = self
            .columns
            .get_mut(&pos)
            .and_then(|column| column.xray_cache.take())
            .unwrap_or_default();
        Some(EncodeJob {
            plan,
            around,
            cache,
        })
    }

    /// A block changed here: if that opened up the blocks around it, they
    /// are sent as they are.
    pub(super) fn anti_xray_block_changed(
        &mut self,
        pos: BlockPos,
        old: BlockState,
        new: BlockState,
    ) {
        if self.chunk_io.anti_xray.plan.is_some()
            && anti_xray::occludes(old)
            && !anti_xray::occludes(new)
        {
            self.reveal(pos, true);
        }
    }

    /// A player started mining at `pos`: what is next to it is sent as it
    /// is, before the block breaks.
    pub(super) fn anti_xray_mining(&mut self, pos: BlockPos) {
        if self.chunk_io.anti_xray.plan.is_some() {
            self.reveal(pos, false);
        }
    }

    /// A neighbouring region's border chunk changed: blocks it opened up
    /// next to ours are sent as they are.
    pub(super) fn anti_xray_halo_changed(&mut self, pos: ChunkPos, old: &Chunk, new: &Chunk) {
        let Some(radius) = self
            .chunk_io
            .anti_xray
            .plan
            .as_ref()
            .map(|plan| plan.radius)
        else {
            return;
        };
        for (x, y, z, state) in old.changed_blocks(new) {
            let at = BlockPos::new(pos.x * 16 + x, y, pos.z * 16 + z);
            let reaches_us = [(-1, -1), (-1, 1), (1, -1), (1, 1)].iter().any(|(sx, sz)| {
                self.columns.contains_key(&ChunkPos::of_block(BlockPos::new(
                    at.x + sx * radius.max(1),
                    y,
                    at.z + sz * radius.max(1),
                )))
            });
            if reaches_us && anti_xray::occludes(old.get(x, y, z)) && !anti_xray::occludes(state) {
                self.reveal(at, true);
            }
        }
    }

    /// Sends the true state of every block within the update radius of
    /// `center` that may have been sent as something else. `opened` means
    /// `center` stopped hiding its neighbours, so cached packets of other
    /// chunks with such a neighbour are stale.
    fn reveal(&mut self, center: BlockPos, opened: bool) {
        let Some(plan) = self.chunk_io.anti_xray.plan.clone() else {
            return;
        };
        if center.y - plan.radius.max(1) > plan.max_y {
            return;
        }
        let home = ChunkPos::of_block(center);
        let mut found: HashMap<ChunkPos, BTreeMap<i32, BTreeSet<u16>>> = HashMap::new();
        let mut stale = HashSet::new();
        let mut entities = Vec::new();
        for (dx, dy, dz) in anti_xray::offsets(plan.radius.max(1)) {
            let distance = dx.abs() + dy.abs() + dz.abs();
            let at = BlockPos::new(center.x + dx, center.y + dy, center.z + dz);
            if at.y > plan.max_y || (distance > plan.radius && distance > 1) {
                continue;
            }
            let chunk = ChunkPos::of_block(at);
            let Some(column) = self.columns.get(&chunk) else {
                continue;
            };
            if !column.chunk.shape().contains_y(at.y) {
                continue;
            }
            let state = column.chunk.get(at.x, at.y, at.z);
            if !plan.may_alter(state) {
                continue;
            }
            if opened && distance == 1 && chunk != home {
                stale.insert(chunk);
            }
            if distance > plan.radius {
                continue;
            }
            found
                .entry(chunk)
                .or_default()
                .entry(at.y >> 4)
                .or_default()
                .insert(section_index(at));
            if state.block().creates_block_entity() {
                entities.push(at);
            }
        }
        for chunk in stale {
            if let Some(column) = self.columns.get_mut(&chunk) {
                column.version += 1;
                column.packet = None;
            }
        }
        for (chunk, sections) in found {
            match &mut self.batched_changes {
                Some(batch) => {
                    let pending = batch.entry(chunk).or_default();
                    for (section, indices) in sections {
                        pending.entry(section).or_default().extend(indices);
                    }
                }
                None => self.send_section_changes(chunk, sections),
            }
        }
        for at in entities {
            if !self.block_entity_updates.contains(&at) {
                self.block_entity_updates.push(at);
            }
        }
    }
}

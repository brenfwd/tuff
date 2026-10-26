//! Answers to `RegionRequest`s that read or search world state rather than
//! mutate it: chunk-position queries, portal search, spawn search, the end
//! fight's chunk-bound requests, and random teleport target search. Called
//! from region.rs's `on_request`/`on_internal` dispatch.

use std::collections::HashMap;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::{Block, BlockState};

use super::{ChunkQuery, ChunkQueryKind, RegionActor, block_at};
use crate::protocol::play;
use crate::world::framed;
use crate::world::messages::{BlockOp, BlockOpResult, BlockUpdate, Packets};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{self as chunk_data, is_air};
use tuff_world_model::pos::ChunkPos;

impl RegionActor {
    /// Positions of `block` in this region's loaded chunks within the box.
    pub(super) fn find_blocks(&self, block: Block, min: BlockPos, max: BlockPos) -> Vec<BlockPos> {
        let mut found = Vec::new();
        for (pos, column) in &self.columns {
            if pos.x * 16 + 15 < min.x
                || pos.x * 16 > max.x
                || pos.z * 16 + 15 < min.z
                || pos.z * 16 > max.z
            {
                continue;
            }
            let shape = column.chunk.shape();
            for (index, section) in column.chunk.sections().enumerate() {
                if !section.palette().iter().any(|s| s.block() == block) {
                    continue;
                }
                let base = shape.section_base(index);
                for i in 0..chunk_data::SECTION_VOLUME {
                    if section.get(i).block() != block {
                        continue;
                    }
                    let at = BlockPos::new(
                        pos.x * 16 + (i & 15) as i32,
                        base + (i >> 8) as i32,
                        pos.z * 16 + ((i >> 4) & 15) as i32,
                    );
                    if (min.x..=max.x).contains(&at.x)
                        && (min.y..=max.y).contains(&at.y)
                        && (min.z..=max.z).contains(&at.z)
                    {
                        found.push(at);
                    }
                }
            }
        }
        found.sort_by_key(|p| (p.y, p.z, p.x));
        found
    }

    /// Queues a portal query over the chunks of this region between `min`
    /// and `max`, loading the ones that are not in yet.
    pub(super) fn chunk_query(&mut self, min: BlockPos, max: BlockPos, kind: ChunkQueryKind) {
        let (min, max) = (ChunkPos::of_block(min), ChunkPos::of_block(max));
        let mut chunks = Vec::new();
        for x in min.x..=max.x {
            for z in min.z..=max.z {
                let pos = ChunkPos::new(x, z);
                if self.pos.contains(pos) {
                    chunks.push(pos);
                }
            }
        }
        if self.sched.closing.is_none() {
            for pos in &chunks {
                if !self.columns.contains_key(pos) {
                    self.load(*pos);
                }
            }
        }
        self.chunk_io
            .chunk_queries
            .push(ChunkQuery { chunks, kind });
        self.poll_chunk_queries();
    }

    pub(super) fn poll_chunk_queries(&mut self) {
        let ready: Vec<ChunkQuery> = {
            let closing = self.sched.closing.is_some();
            let columns = &self.columns;
            let loading = &self.chunk_io.loading;
            let (ready, waiting): (Vec<_>, Vec<_>) =
                self.chunk_io.chunk_queries.drain(..).partition(|query| {
                    query.chunks.iter().all(|pos| {
                        columns.contains_key(pos) || (closing && !loading.contains_key(pos))
                    })
                });
            self.chunk_io.chunk_queries = waiting;
            ready
        };
        for query in ready {
            self.answer_chunk_query(query.kind);
        }
    }

    pub(super) fn answer_chunk_query(&mut self, kind: ChunkQueryKind) {
        match kind {
            ChunkQueryKind::Blocks { op, reply } => {
                let result = self.batch_block_changes(|region| region.apply_block_op(op));
                let _ = reply.send(result);
            }
            ChunkQueryKind::Respawn {
                pos,
                yaw,
                pitch,
                forced,
                consume,
                reply,
            } => {
                let _ = reply.send(self.respawn_point(pos, yaw, pitch, forced, consume));
            }
            ChunkQueryKind::Spawn {
                center,
                radius,
                offset,
                reply,
            } => {
                let _ = reply.send(self.find_spawn(center, radius, offset));
            }
            ChunkQueryKind::Run(run) => run.run(self),
            ChunkQueryKind::AwaitChunk { reply } => {
                let _ = reply.send(());
            }
        }
    }

    /// Where a player without a spawn block appears around the world
    /// spawn `center`: a feature's answer, else the spawn itself.
    pub(super) fn find_spawn(&mut self, center: BlockPos, radius: i32, offset: u32) -> [f64; 3] {
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .find_map(|hook| hook.spawn_position(self, center, radius, offset))
            .unwrap_or([
                f64::from(center.x) + 0.5,
                f64::from(center.y),
                f64::from(center.z) + 0.5,
            ])
    }

    /// Sets a block in this region without neighbour reactions, or hands
    /// it to the owning neighbour.
    pub(super) fn set_block_anywhere(&mut self, pos: BlockPos, state: BlockState) {
        let chunk = ChunkPos::of_block(pos);
        if self.pos.contains(chunk) {
            self.set_block(pos, state);
        } else {
            let update = BlockUpdate::Place {
                pos,
                state,
                cause: self.sim.cause,
            };
            self.send_block_updates(HashMap::from([(chunk.region(), vec![update])]));
        }
    }

    /// One above the highest motion-blocking block of a loaded column.
    pub(super) fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        let chunk_pos = ChunkPos::new(x >> 4, z >> 4);
        let chunk = self
            .columns
            .get(&chunk_pos)
            .map(|c| &c.chunk)
            .or_else(|| self.border.halo.get(&chunk_pos))?;
        Some(chunk.motion_blocking_height(x, z))
    }

    /// One above the highest non-air block of a loaded column.
    pub(super) fn world_surface_height(&self, x: i32, z: i32) -> Option<i32> {
        let chunk_pos = ChunkPos::new(x >> 4, z >> 4);
        let chunk = self
            .columns
            .get(&chunk_pos)
            .map(|c| &c.chunk)
            .or_else(|| self.border.halo.get(&chunk_pos))?;
        Some(chunk.world_surface_height(x, z))
    }

    pub(super) fn block(&self, pos: BlockPos) -> Option<BlockState> {
        block_at(&self.columns, &self.border.halo, pos)
    }

    /// A vanilla-style random teleport: up to 16 attempts at an offset
    /// within `diameter` blocks of `origin`, landing on the first block
    /// below that entities can stand on with nothing solid or liquid in
    /// the `width` x `height` box above it. `None` if every attempt lands
    /// outside a loaded chunk, in the void, or somewhere blocked.
    pub(super) fn random_teleport_target(
        &mut self,
        origin: [f64; 3],
        diameter: f32,
        width: f64,
        height: f64,
    ) -> Option<[f64; 3]> {
        let min_y = self.dimension().shape.min_y;
        let max_y = f64::from(self.dimension().logical_max_y() - 1);
        for _ in 0..16 {
            let offset = |rng: &mut StdRng| (rng.r#gen::<f64>() - 0.5) * f64::from(diameter);
            let x = origin[0] + offset(&mut self.rng);
            let y = (origin[1] + offset(&mut self.rng)).clamp(f64::from(min_y), max_y);
            let z = origin[2] + offset(&mut self.rng);
            if let Some(target) = self.random_teleport_landing(x, y, z, width, height, min_y) {
                return Some(target);
            }
        }
        None
    }

    /// Scans down from `(x, y, z)` for the first block entities can stand
    /// on, then checks the box above it is free of collisions and liquid.
    pub(super) fn random_teleport_landing(
        &self,
        x: f64,
        y: f64,
        z: f64,
        width: f64,
        height: f64,
        min_y: i32,
    ) -> Option<[f64; 3]> {
        let [x, z] = self.dimension().border().clamp(x, z);
        let mut pos = crate::world::physics::block_pos([x, y, z]);
        self.block(pos)?;
        let avoided = |state: BlockState| {
            tuff_datapack::tags::with(|tags| {
                tags.contains(
                    "minecraft:block",
                    "minecraft:consumable_does_not_teleport_to",
                    state.block().name(),
                )
            })
        };
        let mut y = y;
        while pos.y > min_y {
            pos.y -= 1;
            let state = self.block(pos)?;
            if !tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::ENTITIES_CAN_TELEPORT_TO,
                state.block(),
            ) {
                y -= 1.0;
                continue;
            }
            if avoided(state) {
                return None;
            }
            let half = width / 2.0;
            let area = [x - half, y, z - half, x + half, y + height, z + half];
            let lookup: crate::world::physics::BlockLookup = &|p| self.block(p);
            if !crate::world::physics::no_collision(area, lookup) {
                return None;
            }
            let min = crate::world::physics::block_pos([area[0], area[1], area[2]]);
            let max = crate::world::physics::block_pos([area[3], area[4], area[5]]);
            for bx in min.x..=max.x {
                for by in min.y..=max.y {
                    for bz in min.z..=max.z {
                        let state = self.block(BlockPos::new(bx, by, bz))?;
                        if !state.fluid_state().is_empty() || avoided(state) {
                            return None;
                        }
                    }
                }
            }
            return Some([x, y, z]);
        }
        None
    }

    pub(super) fn corrections(&self, positions: &[BlockPos]) -> Packets {
        positions
            .iter()
            .filter_map(|pos| {
                let state = self.block(*pos)?;
                if self.anti_xray_conceals(*pos) {
                    return None;
                }
                Some(framed(&play::BlockUpdate {
                    position: *pos,
                    state: state.id(),
                }))
            })
            .collect()
    }

    /// Fills biome cells in this region's loaded chunks of the box; see
    /// [`BlockOp::FillBiome`].
    fn fill_biome(
        &mut self,
        min: BlockPos,
        max: BlockPos,
        biome: u16,
        filter: Option<&[u16]>,
        apply: bool,
    ) -> usize {
        let mut result = 0;
        for chunk_z in (min.z >> 4)..=(max.z >> 4) {
            for chunk_x in (min.x >> 4)..=(max.x >> 4) {
                let chunk_pos = ChunkPos {
                    x: chunk_x,
                    z: chunk_z,
                };
                if !self.pos.contains(chunk_pos) {
                    continue;
                }
                let Some(column) = self.columns.get_mut(&chunk_pos) else {
                    result += usize::from(!apply);
                    continue;
                };
                if !apply {
                    continue;
                }
                let (base_x, base_z) = (chunk_x * 16, chunk_z * 16);
                let changed = column.chunk.fill_biomes(|x, y, z, current| {
                    let (x, z) = (base_x + x, base_z + z);
                    let inside = (min.x..=max.x).contains(&x)
                        && (min.y..=max.y).contains(&y)
                        && (min.z..=max.z).contains(&z);
                    (inside && filter.is_none_or(|ids| ids.contains(&current))).then_some(biome)
                });
                if changed == 0 {
                    continue;
                }
                result += changed;
                column.blocks_dirty = true;
                column.version += 1;
                column.packet = None;
                let update = Packets::from_iter([framed(&play::ChunksBiomes {
                    x: chunk_x,
                    z: chunk_z,
                    data: column.chunk.encode_biomes(),
                })]);
                let dimension = self.dimension;
                for viewer in column.viewers.values() {
                    viewer.send(crate::world::messages::PlayerEvent::ChunkPackets {
                        dimension,
                        pos: chunk_pos,
                        packets: update.clone(),
                    });
                }
                if !column.awaiting_packet.is_empty() {
                    self.request_packet(chunk_pos);
                }
            }
        }
        result
    }

    /// Runs a command's block operation over the positions this region
    /// owns; the chunks were loaded first.
    pub(super) fn apply_block_op(&mut self, op: BlockOp) -> BlockOpResult {
        match op {
            BlockOp::Biomes { positions, .. } => BlockOpResult::Biomes(
                positions
                    .iter()
                    .map(|pos| self.biome_at(*pos).filter(|_| self.owns(*pos)))
                    .collect(),
            ),
            BlockOp::Get { positions, .. } => BlockOpResult::States(
                positions
                    .iter()
                    .map(|pos| self.block(*pos).filter(|_| self.owns(*pos)))
                    .collect(),
            ),
            BlockOp::Set {
                changes,
                destroy,
                keep,
                strict,
                quiet,
                data,
            } => {
                let mut count = 0;
                let mut changed = Vec::new();
                for (pos, state) in changes {
                    if !self.owns(pos) {
                        continue;
                    }
                    let Some(old) = self.block(pos) else {
                        continue;
                    };
                    if keep && !is_air(old) {
                        continue;
                    }
                    if destroy && !is_air(old) {
                        if let Some(broken) = self.claim(
                            pos,
                            crate::world::block_ledger::Remains::Air,
                            crate::world::block_ledger::ClaimReason::Command,
                        ) {
                            let drops = self.shared.block_loot().destroyed(broken, &mut self.rng);
                            self.drop_items(pos, drops);
                        } else {
                            self.put_block(pos, Block::Air.default_state());
                        }
                        self.break_event(pos, old, None);
                        count += usize::from(is_air(state));
                    }
                    if quiet
                        && old.block() != state.block()
                        && self.block_entity_kind(pos).is_some()
                    {
                        self.remove_block_entity(pos, None);
                    }
                    let flags = if strict {
                        crate::world::redstone::UPDATE_CLIENTS
                            | crate::world::redstone::UPDATE_KNOWN_SHAPE
                            | crate::world::redstone::UPDATE_SKIP_ON_PLACE
                    } else {
                        crate::world::redstone::UPDATE_CLIENTS
                    };
                    let previous = self.set_block_flags(
                        pos,
                        state,
                        flags,
                        crate::world::redstone::UPDATE_LIMIT,
                    );
                    let mut affected = false;
                    if let Some(previous) = previous
                        && previous != state
                    {
                        changed.push((pos, previous));
                        affected = true;
                    }
                    if let Some(nbt) = &data {
                        affected |= self.load_block_entity_data(pos, state, nbt);
                    }
                    count += usize::from(affected);
                }
                if !strict {
                    for (pos, previous) in changed {
                        self.notify_block_set(pos, previous);
                    }
                }
                BlockOpResult::Set(count)
            }
            BlockOp::Copy { positions } => BlockOpResult::Copied(
                positions
                    .iter()
                    .map(|pos| {
                        let state = self.block(*pos).filter(|_| self.owns(*pos))?;
                        Some((state, self.saved_block_entity(*pos)))
                    })
                    .collect(),
            ),
            BlockOp::Paste { changes, strict } => self.paste_blocks(changes, strict),
            BlockOp::ReplacePortItem { pos, stack } => {
                BlockOpResult::Set(usize::from(self.replace_port_item(pos, stack)))
            }
            BlockOp::TestInstances { center, radius } => {
                BlockOpResult::Positions(self.chunk_io.pois.in_square(
                    center,
                    radius,
                    tuff_vanilla_data::registry::PointOfInterestType::TestInstance,
                ))
            }
            BlockOp::TestBlocks {
                positions,
                trigger,
                reset,
            } => BlockOpResult::Triggered(self.test_blocks_op(&positions, trigger, reset)),
            BlockOp::FillBiome {
                min,
                max,
                biome,
                filter,
                apply,
            } => BlockOpResult::Set(self.fill_biome(min, max, biome, filter.as_deref(), apply)),
            BlockOp::CopyTicks { min, max } => BlockOpResult::Ticks(self.copy_ticks(min, max)),
            BlockOp::PasteTicks { ticks } => {
                let mut count = 0;
                for tick in ticks {
                    if self.owns(tick.pos) && self.block(tick.pos).is_some() {
                        self.schedule_tick_with(tick.pos, tick.block, tick.delay, tick.priority);
                        count += 1;
                    }
                }
                BlockOpResult::Set(count)
            }
        }
    }

    fn paste_blocks(
        &mut self,
        changes: Vec<(BlockPos, BlockState, Option<Compound>)>,
        strict: bool,
    ) -> BlockOpResult {
        use crate::world::redstone::{
            UPDATE_CLIENTS, UPDATE_KNOWN_SHAPE, UPDATE_LIMIT, UPDATE_SKIP_ON_PLACE,
            UPDATE_SUPPRESS_DROPS,
        };
        let flags = if strict {
            UPDATE_CLIENTS | UPDATE_KNOWN_SHAPE | UPDATE_SKIP_ON_PLACE
        } else {
            UPDATE_CLIENTS
        };
        let silent =
            UPDATE_CLIENTS | UPDATE_KNOWN_SHAPE | UPDATE_SUPPRESS_DROPS | UPDATE_SKIP_ON_PLACE;
        let mut originals = std::collections::HashMap::new();
        for (pos, state, data) in &changes {
            if !self.owns(*pos) {
                continue;
            }
            let Some(old) = self.block(*pos) else {
                continue;
            };
            originals.insert(*pos, old);
            let replaces_entity = data.is_some() || old.block() != state.block();
            if replaces_entity && self.block_entity_kind(*pos).is_some() {
                self.remove_block_entity(*pos, None);
            }
            self.set_block_flags(*pos, Block::Barrier.default_state(), silent, 0);
        }
        let mut count = 0;
        let mut changed = Vec::new();
        for (pos, state, data) in changes {
            let Some(old) = originals.get(&pos).copied() else {
                continue;
            };
            let replaces_entity = data.is_some() || old.block() != state.block();
            if replaces_entity && self.block_entity_kind(pos).is_some() {
                self.remove_block_entity(pos, None);
            }
            let previous = self.set_block_flags(pos, state, flags, UPDATE_LIMIT);
            let mut pasted = previous.is_some() && old != state;
            if pasted {
                changed.push((pos, old));
            }
            if let Some(nbt) = data
                && self.store_pasted(pos, state, nbt)
            {
                pasted = true;
            }
            count += usize::from(pasted);
        }
        if !strict {
            for (pos, previous) in changed {
                self.notify_block_set(pos, previous);
            }
        }
        BlockOpResult::Set(count)
    }

    fn store_pasted(&mut self, pos: BlockPos, state: BlockState, mut nbt: Compound) -> bool {
        let kind = self
            .block_entity_kind(pos)
            .or_else(|| state.block().block_entity());
        if !kind.is_some_and(|kind| crate::world::block_entity::raw::kind(&nbt) == Some(kind)) {
            return false;
        }
        nbt.insert("x", Tag::Int(pos.x));
        nbt.insert("y", Tag::Int(pos.y));
        nbt.insert("z", Tag::Int(pos.z));
        let kind = crate::world::block_entity::raw::kind(&nbt);
        let entity = match crate::world::block_entity::BlockEntity::decode(&nbt) {
            Some((_, entity)) => Ok(entity),
            None => Err(nbt),
        };
        self.store_block_entity(pos, entity);
        if kind.is_some_and(|kind| kind.sends_updates())
            && !self.block_entity_updates.contains(&pos)
        {
            self.block_entity_updates.push(pos);
        }
        true
    }

    /// Reloads the block entity at `pos` from a command's block entity
    /// data; whether anything changed.
    pub(super) fn load_block_entity_data(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        data: &Compound,
    ) -> bool {
        let Some(before) = self.saved_block_entity(pos) else {
            return false;
        };
        let Some(after) = crate::world::block_entity::raw::reload_block_entity_data(&before, data)
        else {
            return false;
        };
        self.store_pasted(pos, state, after) && self.saved_block_entity(pos) != Some(before)
    }

    fn copy_ticks(&self, min: BlockPos, max: BlockPos) -> Vec<crate::world::messages::CopiedTick> {
        let inside = |pos: BlockPos| {
            (min.x..=max.x).contains(&pos.x)
                && (min.y..=max.y).contains(&pos.y)
                && (min.z..=max.z).contains(&pos.z)
        };
        let mut ticks: Vec<_> = self
            .columns
            .iter()
            .filter(|(chunk, _)| {
                let (x, z) = (chunk.x * 16, chunk.z * 16);
                x <= max.x && x + 15 >= min.x && z <= max.z && z + 15 >= min.z
            })
            .flat_map(|(_, column)| column.block_ticks.in_order())
            .filter(|tick| inside(tick.pos))
            .map(|tick| crate::world::messages::CopiedTick {
                pos: tick.pos,
                block: tick.kind,
                delay: u32::try_from(tick.due.saturating_sub(self.sched.tick)).unwrap_or(u32::MAX),
                priority: tick.priority,
                order: tick.order,
            })
            .collect();
        ticks.sort_by_key(|tick| tick.order);
        ticks
    }
}

//! Block placement, breaking and other world edits: the shared set/break/
//! place pipeline, batched section updates, and the right-click dispatch
//! into item-use and interaction. Called from region.rs's dispatch.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{BlockEntityType, EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockKind, BlockState, Item};

use super::{AIR, RegionActor, UNBREAKABLE, menu_stat};
use crate::protocol::play;
use crate::world::block_entity::{self, ContainerKind};
use crate::world::items::{self};
use crate::world::lighting::LightOp;
use crate::world::menu::MenuKind;
use crate::world::messages::{
    ActionOutcome, BlockUpdate, BreakMode, DestroyTick, Igniter, MenuOpen, MenuSource, Packets,
    Place, Placement, PlayerEvent, PlayerHandle, PlayerId, RegionInternal,
};
use crate::world::placement;
use crate::world::region_api::{BlockAction, BlockCause, BlockChange, RegionHooks};
use crate::world::{
    PLAYER_HALF_WIDTH, PLAYER_HEIGHT, REACH, block_contact, distance_sq, framed, packets, player,
};
use tuff_item::item::{Components, ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// The share of a block a stop-mining packet must have mined.
const MIN_FINISHED_PROGRESS: f32 = 0.7;

pub(super) use crate::world::region_api::{LEVEL_EVENT_RANGE, PARTICLE_RANGE, SOUND_RANGE};

const ANVIL_BROKEN: i32 = 1029;
const ANVIL_USED: i32 = 1030;
const GRINDSTONE_USED: i32 = 1042;

impl RegionActor {
    /// Sets a block with no neighbour or shape updates, as the game's
    /// flags 2 and 16 do (the new block's placement reaction still runs).
    pub(super) fn set_block(&mut self, pos: BlockPos, state: BlockState) -> Option<BlockState> {
        self.set_block_flags(
            pos,
            state,
            crate::world::redstone::UPDATE_CLIENTS | crate::world::redstone::UPDATE_KNOWN_SHAPE,
            crate::world::redstone::UPDATE_LIMIT,
        )
    }

    /// Changes the stored block and everything tied to it (block entity,
    /// clients, light), with no reactions.
    pub(super) fn raw_set_block(&mut self, pos: BlockPos, state: BlockState) -> Option<BlockState> {
        let chunk_pos = ChunkPos::of_block(pos);
        if !self.block_watchers().is_empty() {
            self.report_block_change(pos, state);
        }
        let column = self.columns.get_mut(&chunk_pos)?;
        let old = column.chunk.set(pos.x, pos.y, pos.z, state)?;
        if old == state {
            return Some(old);
        }
        if tuff_vanilla_data::registry::PointOfInterestType::of_state(old).is_some()
            || tuff_vanilla_data::registry::PointOfInterestType::of_state(state).is_some()
        {
            self.chunk_io.pois.block_changed(pos, state);
        }
        let collision_changed = old.collision_boxes() != state.collision_boxes()
            || old.path_type() != state.path_type();
        if collision_changed {
            for tracked in self.ents.entities.values_mut() {
                tracked.entity.block_changed(pos);
            }
        }
        let (old_block, block) = (old.block(), state.block());
        if old_block.block_entity().is_some() || block.block_entity().is_some() {
            self.block_entity_generation += 1;
        }
        if old_block != block {
            if old_block.block_entity().is_some() && !block.keeps_block_entity_of(old_block) {
                self.remove_block_entity(pos, Some(old));
            } else if self.menus.viewers.contains_key(&pos) {
                self.close_viewers(pos);
            }
        }
        let kind = block
            .block_entity()
            .filter(|_| block.creates_block_entity());
        if let Some(kind) = kind {
            match self.block_entity_kind(pos) {
                Some(existing) if existing == kind => {}
                existing => {
                    if existing.is_some() {
                        self.remove_block_entity(pos, None);
                    }
                    self.create_block_entity(pos, kind, &Default::default());
                }
            }
            if kind.sends_updates() && !self.block_entity_updates.contains(&pos) {
                self.block_entity_updates.push(pos);
            }
        }
        let column = self.columns.get_mut(&chunk_pos)?;
        column.meta.block_changed();
        column.blocks_dirty = true;
        column.version += 1;
        column.packet = None;
        self.light.light_ops.push(LightOp::SetBlock {
            chunk: chunk_pos,
            x: pos.x & 15,
            y: pos.y,
            z: pos.z & 15,
            state,
        });
        if let Some(batch) = &mut self.batched_changes {
            batch
                .entry(chunk_pos)
                .or_default()
                .entry(pos.y >> 4)
                .or_default()
                .insert(section_index(pos));
        } else {
            let update = packets([framed(&play::BlockUpdate {
                position: pos,
                state: state.id(),
            })]);
            let dimension = self.dimension;
            for viewer in column.viewers.values() {
                viewer.send(PlayerEvent::ChunkPackets {
                    dimension,
                    pos: chunk_pos,
                    packets: update.clone(),
                });
                viewer.send(PlayerEvent::BlockChanges {
                    dimension,
                    pos: chunk_pos,
                    changes: Arc::from([(pos, state)]),
                });
            }
        }
        if !column.awaiting_packet.is_empty() {
            self.request_packet(chunk_pos);
        }
        if self.pos.is_border(chunk_pos) {
            self.border.halo_dirty.insert(chunk_pos);
        }
        if state.is_liquid_block() {
            self.liquid_placed(pos, state);
        }
        if state.is_liquid_block()
            || old.is_liquid_block()
            || state.block().fire_damage().is_some()
            || old.block().fire_damage().is_some()
        {
            self.notify_fluid_contact(pos);
        }
        if state.is_suffocating() {
            self.notify_suffocation(pos);
        }
        self.anti_xray_block_changed(pos, old, state);
        Some(old)
    }

    /// The features that observe block changes.
    fn block_watchers(&self) -> &[Arc<dyn RegionHooks>] {
        self.sim.block_watchers.get_or_init(|| {
            self.shared
                .region_hooks
                .iter()
                .filter(|hook| hook.observes_block_changes())
                .cloned()
                .collect()
        })
    }

    /// Tells the observing features the block at `pos` is about to become
    /// `state`, while the old block and its block entity are still there.
    fn report_block_change(&mut self, pos: BlockPos, state: BlockState) {
        let Some(column) = self.columns.get(&ChunkPos::of_block(pos)) else {
            return;
        };
        if !column.chunk.shape().contains_y(pos.y) {
            return;
        }
        let old = column.chunk.get(pos.x, pos.y, pos.z);
        if old == state {
            return;
        }
        let change = BlockChange {
            pos,
            old,
            new: state,
            cause: self.sim.cause,
        };
        let watchers = self.sim.block_watchers.get().cloned().unwrap_or_default();
        for hook in watchers.iter() {
            hook.block_changed(self, &change);
        }
    }

    /// Runs `f` with the block changes it makes attributed to `cause`.
    pub(super) fn with_cause<R>(&mut self, cause: BlockCause, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = std::mem::replace(&mut self.sim.cause, cause);
        let result = f(self);
        self.sim.cause = previous;
        result
    }

    /// A block that could suffocate something just appeared at `pos`: tell
    /// any player already trapped in it now, rather than waiting for its
    /// next movement report (a falling block landing on a stationary
    /// player, or a piston pushing one into it, must not wait for the
    /// player to move before it starts taking damage).
    pub(super) fn notify_suffocation(&self, pos: BlockPos) {
        for presence in self.ents.presence.values() {
            if !presence.is_physical() {
                continue;
            }
            let [x, y, z] = presence.position;
            if (pos.x as f64 - x).abs() > 2.0
                || (pos.y as f64 - y).abs() > 3.0
                || (pos.z as f64 - z).abs() > 2.0
            {
                continue;
            }
            let eye = [x, y + presence.looks.eye_height, z];
            let width = (PLAYER_HALF_WIDTH * 2.0) as f32;
            if block_contact::suffocates(eye, width, &|p| self.block(p)) {
                presence.handle.send(PlayerEvent::Suffocating {
                    dimension: self.dimension,
                });
            }
        }
    }

    /// A liquid or fire block appeared or vanished at `pos`: recheck any
    /// nearby present player's water, lava and fire contact now, rather
    /// than waiting for its next movement report. Without this a motionless
    /// player that water or lava flows over (or fire spreads onto) is never
    /// marked as touching it until it moves and republishes on its own.
    pub(super) fn notify_fluid_contact(&self, pos: BlockPos) {
        for presence in self.ents.presence.values() {
            if !presence.is_physical() {
                continue;
            }
            let [x, y, z] = presence.position;
            if (pos.x as f64 - x).abs() > 2.0
                || (pos.y as f64 - y).abs() > 3.0
                || (pos.z as f64 - z).abs() > 2.0
            {
                continue;
            }
            let block = |p: BlockPos| self.block(p);
            presence.handle.send(PlayerEvent::FluidContact {
                dimension: self.dimension,
                in_water: player::touches_water(presence.position, block),
                submersion: player::submersion(presence.position, presence.looks.eye_height, block),
                in_lava: player::touches_lava(presence.position, block),
                in_fire: player::fire_contact(presence.position, block),
            });
        }
    }

    /// Runs `f` with block changes held back and sends them afterwards as
    /// one packet per changed section, as the game does once per tick, so a
    /// bulk edit cannot flood the viewers' queues. Nested calls join the
    /// outermost batch.
    pub(super) fn batch_block_changes<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.batched_changes.is_some() {
            return f(self);
        }
        self.batched_changes = Some(HashMap::new());
        let result = f(self);
        for (chunk, sections) in self.batched_changes.take().unwrap_or_default() {
            self.send_section_changes(chunk, sections);
        }
        result
    }

    /// Sends a batch's pending changes in `chunk` now, so packets about
    /// those blocks cannot overtake them.
    pub(super) fn flush_batched_chunk(&mut self, chunk: ChunkPos) {
        if let Some(sections) = self
            .batched_changes
            .as_mut()
            .and_then(|batch| batch.remove(&chunk))
        {
            self.send_section_changes(chunk, sections);
        }
    }

    pub(super) fn send_section_changes(
        &self,
        chunk: ChunkPos,
        sections: BTreeMap<i32, BTreeSet<u16>>,
    ) {
        let Some(column) = self.columns.get(&chunk) else {
            return;
        };
        if column.viewers.is_empty() {
            return;
        }
        let mut blocks = Vec::new();
        let mut changes = Vec::new();
        let update = packets(sections.into_iter().map(|(section_y, indices)| {
            blocks.clear();
            for index in indices {
                let pos = section_block(chunk, section_y, index);
                let state = column.chunk.get(pos.x, pos.y, pos.z);
                blocks.push((index, state.id()));
                changes.push((pos, state));
            }
            match blocks[..] {
                [(index, state)] => framed(&play::BlockUpdate {
                    position: section_block(chunk, section_y, index),
                    state,
                }),
                _ => framed(&play::SectionBlocksUpdate {
                    section: [chunk.x, section_y, chunk.z],
                    blocks: &blocks,
                }),
            }
        }));
        let changes: Arc<[(BlockPos, BlockState)]> = changes.into();
        for viewer in column.viewers.values() {
            viewer.send(PlayerEvent::ChunkPackets {
                dimension: self.dimension,
                pos: chunk,
                packets: update.clone(),
            });
            viewer.send(PlayerEvent::BlockChanges {
                dimension: self.dimension,
                pos: chunk,
                changes: changes.clone(),
            });
        }
    }

    pub(super) fn destroy_progress(&mut self, tick: DestroyTick) {
        let DestroyTick {
            player,
            pos,
            event,
            data,
            ticks,
            tool,
            miner,
            eye,
        } = tick;
        let Some(state) = self.block(pos).filter(|state| !is_air(*state)) else {
            self.clear_crack(player.id, pos);
            player.send(PlayerEvent::DestroyEnded(pos));
            return;
        };
        let submerged = player::eye_in_water(eye, |pos| self.block(pos));
        let hardness = state.destroy_speed();
        let progress =
            player::destroy_progress_at(state, hardness, tool.as_deref(), &miner, submerged);
        let stage = player::crack_stage(progress, ticks);
        if self.ents.crack_stages.get(&player.id) != Some(&(pos, stage)) {
            self.show_crack(player.id, pos, stage);
        }
        let event = packets([framed(&play::LevelEvent {
            event,
            position: pos,
            data,
            global: false,
        })]);
        self.send_near(pos.center(), LEVEL_EVENT_RANGE, None, event);
    }

    pub(super) fn abort_destroying(&mut self, player: PlayerId, pos: BlockPos) {
        if let Some((old, _)) = self.ents.crack_stages.get(&player).copied()
            && old != pos
        {
            self.clear_crack(player, old);
        }
        self.clear_crack(player, pos);
    }

    fn clear_crack(&mut self, player: PlayerId, pos: BlockPos) {
        self.show_crack(player, pos, -1);
        self.ents.crack_stages.remove(&player);
    }

    /// Shows `player`'s crack `stage` at `pos` to every other player in this
    /// dimension within 32 blocks (-1 clears it).
    fn show_crack(&mut self, player: PlayerId, pos: BlockPos, stage: i32) {
        if stage >= 0 {
            self.ents.crack_stages.insert(player, (pos, stage));
        }
        let packet = packets([framed(&play::BlockDestruction {
            id: player,
            position: pos,
            stage: stage as i8,
        })]);
        let mut sent = HashSet::from([player]);
        let present = self.ents.presence.values().map(|p| (&p.handle, p.position));
        let index = self.shared.players.borrow().clone();
        let near = index
            .near(self.dimension, self.pos)
            .iter()
            .map(|p| (&p.handle, p.position));
        for (handle, position) in present.chain(near) {
            if player::within_crack_range(pos, position) && sent.insert(handle.id) {
                handle.send(PlayerEvent::Packets(packet.clone()));
            }
        }
    }

    /// Packets about `chunk`'s contents for everyone viewing it.
    pub(super) fn send_chunk_packets(
        &self,
        chunk: ChunkPos,
        except: Option<PlayerId>,
        packets: Packets,
    ) {
        let dimension = self.dimension;
        self.fanout_chunk(chunk, except, |_| PlayerEvent::ChunkPackets {
            dimension,
            pos: chunk,
            packets: packets.clone(),
        });
    }

    /// Like `send_chunk_packets`, but skips viewers whose known position is
    /// farther than `range` from `center`.
    pub(super) fn send_near(
        &self,
        center: [f64; 3],
        range: f64,
        except: Option<PlayerId>,
        packets: Packets,
    ) {
        let chunk = ChunkPos::of_position(center);
        let dimension = self.dimension;
        let limit = range * range;
        let in_range = |id: &PlayerId| {
            self.ents.presence.get(id).is_none_or(|p| {
                let d = [
                    p.position[0] - center[0],
                    p.position[1] - center[1],
                    p.position[2] - center[2],
                ];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= limit
            })
        };
        if let Some(column) = self.columns.get(&chunk) {
            for (id, viewer) in &column.viewers {
                if Some(*id) != except && in_range(id) {
                    viewer.send(PlayerEvent::ChunkPackets {
                        dimension,
                        pos: chunk,
                        packets: packets.clone(),
                    });
                }
            }
        }
    }

    pub(super) fn send_to_players_near(
        &self,
        center: [f64; 3],
        range: f64,
        skip: &[PlayerId],
        packets: Packets,
    ) {
        let limit = range * range;
        for presence in self.ents.presence.values() {
            if skip.contains(&presence.handle.id) {
                continue;
            }
            let d = [
                presence.position[0] - center[0],
                presence.position[1] - center[1],
                presence.position[2] - center[2],
            ];
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= limit {
                presence.handle.send(PlayerEvent::Packets(packets.clone()));
            }
        }
    }

    pub(super) fn send_sound_near(
        &self,
        center: [f64; 3],
        volume: f32,
        except: Option<PlayerId>,
        packets: Packets,
    ) {
        let range = SOUND_RANGE * f64::from(volume.max(1.0));
        self.send_near(center, range, except, packets);
    }

    /// Whether a building-blocking entity (mobs, boats, minecarts, armor
    /// stands, crystals, falling blocks, primed TNT) not carrying `placer`
    /// overlaps `state` placed at `at`.
    fn entities_obstruct(&self, state: BlockState, at: BlockPos, placer: Option<PlayerId>) -> bool {
        self.ents.entities.values().any(|tracked| {
            let entity = &tracked.entity;
            let base = entity.base();
            let blocks_building = entity.living().is_some()
                || Item::of_boat(base.kind).is_some()
                || matches!(
                    base.kind,
                    EntityType::Minecart
                        | EntityType::ChestMinecart
                        | EntityType::FurnaceMinecart
                        | EntityType::TntMinecart
                        | EntityType::HopperMinecart
                        | EntityType::CommandBlockMinecart
                        | EntityType::SpawnerMinecart
                        | EntityType::EndCrystal
                        | EntityType::FallingBlock
                        | EntityType::Tnt
                );
            blocks_building
                && !placer.is_some_and(|id| base.riders.iter().any(|r| r.handle.id == id))
                && placement::obstructs(state, at, entity.bounds())
        })
    }

    fn adventure_allows(
        &self,
        components: &tuff_item::item::Components,
        component: &str,
        pos: BlockPos,
    ) -> bool {
        let Some(state) = self.block(pos) else {
            return false;
        };
        components.other.get(component).is_some_and(|predicates| {
            crate::world::predicate::adventure_allows(predicates, state, self.raw_block_entity(pos))
        })
    }

    pub(super) fn break_restricted(
        &mut self,
        player: PlayerHandle,
        pos: BlockPos,
        tool: Option<tuff_item::item::ItemSnapshot>,
        mode: BreakMode,
        sequence: i32,
    ) {
        if tool
            .as_ref()
            .is_some_and(|tool| self.adventure_allows(&tool.components, "minecraft:can_break", pos))
        {
            self.break_block(player, pos, tool, mode, sequence);
        } else {
            player.send(PlayerEvent::ActionResult {
                sequence,
                outcome: ActionOutcome::Rejected(self.corrections(&[pos])),
            });
        }
    }

    pub(super) fn break_block(
        &mut self,
        player: PlayerHandle,
        pos: BlockPos,
        tool: Option<tuff_item::item::ItemSnapshot>,
        mode: BreakMode,
        sequence: i32,
    ) {
        let tool_stack = tool;
        let tool = tool_stack.as_deref();
        let reject = |region: &Self| {
            player.send(PlayerEvent::ActionResult {
                sequence,
                outcome: ActionOutcome::Rejected(region.corrections(&[pos])),
            });
        };
        let Some(state) = self.block(pos) else {
            return reject(self);
        };
        if !self.owns(pos) || is_air(state) || UNBREAKABLE.contains(&state.block()) {
            return reject(self);
        }
        if !self.allows_block_action(player.id, pos, state, BlockAction::Broke) {
            self.clear_crack(player.id, pos);
            return reject(self);
        }
        if let BreakMode::Start { eye, miner } = mode {
            self.anti_xray_mining(pos);
            if state.block().kind() == BlockKind::NoteBlock {
                player.send(PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::PlayNoteblock,
                    1,
                ));
                self.play_note(pos);
            }
            self.attack_block(pos, state);
            let submerged = player::eye_in_water(eye, |pos| self.block(pos));
            let hardness = state.destroy_speed();
            let progress = player::destroy_progress_at(state, hardness, tool, &miner, submerged);
            if progress < 1.0 {
                self.show_crack(player.id, pos, player::crack_stage(progress, 0));
                player.send(PlayerEvent::ActionResult {
                    sequence,
                    outcome: ActionOutcome::Mining,
                });
                return;
            }
        }
        if let BreakMode::Verified { ticks, eye, miner } = mode {
            let submerged = player::eye_in_water(eye, |pos| self.block(pos));
            let hardness = state.destroy_speed();
            let progress = player::destroy_progress_at(state, hardness, tool, &miner, submerged);
            if progress * (ticks as f32 + 1.0) < MIN_FINISHED_PROGRESS {
                self.clear_crack(player.id, pos);
                return reject(self);
            }
        }
        if matches!(mode, BreakMode::Finish | BreakMode::Verified { .. }) {
            self.clear_crack(player.id, pos);
        }
        let block_drops = self.shared.game_rules.borrow().bool("block_drops");
        let drops = block_drops
            && match mode {
                BreakMode::Creative => false,
                BreakMode::Start { .. } | BreakMode::Finish | BreakMode::Verified { .. } => {
                    self.shared.block_loot().can_harvest(state, tool)
                }
            };
        let unstable_tnt = state.block() == Block::Tnt
            && !matches!(mode, BreakMode::Creative)
            && state.get("unstable") == Some("true");
        if unstable_tnt && self.shared.rule_bool("tnt_explodes") {
            let igniter = self.ents.presence.get(&player.id).map(|p| Igniter {
                id: player.id,
                uuid: p.uuid,
                name: p.name.clone(),
            });
            self.spawn_primed_tnt(pos, None, igniter);
        }
        let drops = drops && !unstable_tnt;
        let partner_drops = drops || state.block().kind() != BlockKind::Door;
        if let Some(partner) = self.shared.block_rules.partner(state, pos)
            && !(partner_drops
                && matches!(
                    mode,
                    BreakMode::Start { .. } | BreakMode::Finish | BreakMode::Verified { .. }
                ))
            && let Some(partner_state) = self.block(partner)
            && partner_state.block() == state.block()
        {
            self.break_with(partner, partner_state, player.id);
        }
        if matches!(mode, BreakMode::Creative) {
            self.block_behavior(state.block())
                .creative_break(self, pos, state);
        }
        if state.block() == Block::Tripwire
            && tool.is_some_and(|tool| tool.item() == Item::Shears)
            && let Some(disarmed) = state.with("disarmed", "true")
        {
            self.raw_set_block(pos, disarmed);
        }
        let saved = self.saved_block_entity(pos);
        if !matches!(mode, BreakMode::Creative)
            && matches!(state.block(), Block::BeeNest | Block::Beehive)
            && let Some(nbt) = &saved
        {
            let bees = nbt.get_list("bees").map_or(0, |bees| bees.len() as i32);
            player.send(PlayerEvent::Advancement(Box::new(
                crate::world::messages::AdvancementEvent::BeeNestDestroyed {
                    block: state,
                    item: tool_stack.as_ref().map(tuff_item::item::ItemSnapshot::of),
                    bees,
                },
            )));
        }
        self.player_breaking_block(player.id, pos, state, tool_stack.as_deref());
        let creative_drop = self
            .block_entity(pos)
            .filter(|entity| entity.kind == ContainerKind::ShulkerBox && !entity.is_empty())
            .and(saved.as_ref())
            .map(|nbt| {
                Components::from_nbt(&block_entity::raw::collect_components(
                    BlockEntityType::ShulkerBox,
                    nbt,
                ))
            });
        self.put_block(pos, placement::remains(state));
        self.break_event(pos, state, Some(player.id));
        if !matches!(mode, BreakMode::Creative)
            && state.block() == Block::TurtleEgg
            && let Some(fewer) = state
                .eggs()
                .filter(|&n| n > 1)
                .and_then(|n| state.with_eggs(n - 1))
        {
            self.put_block(pos, fewer);
        }
        self.player_block_action(player.id, pos, state, BlockAction::Broke);
        if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::FIRE, state.block()) {
            self.level_event(play::level_event::SOUND_EXTINGUISH_FIRE, pos, 0, false);
        }
        if drops {
            let at = self.location_facts(pos);
            let seed = self.rng.r#gen();
            let mut roll =
                <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(self.rng.r#gen());
            let stacks = crate::world::commands::predicates::with_region_view(
                &*self,
                &self.shared,
                crate::world::region_api::RegionWorld::dimension_id(&*self),
                seed,
                |world| {
                    self.shared.block_loot().drops_at(
                        state,
                        tool,
                        saved.as_ref(),
                        &at,
                        Some(world),
                        &mut roll,
                    )
                },
            );
            self.drop_items(pos, stacks);
            self.release_infestation(pos, state, tool_stack.as_deref());
            if let Some(experience) = state.block().experience() {
                let amount = experience.sample(|bound| self.rng.gen_range(0..bound.max(1)));
                let amount = block_experience_effects(tool_stack.as_deref(), amount);
                if amount > 0 {
                    self.spawn_experience(pos.center(), amount);
                }
            }
        } else if let Some(components) = creative_drop
            && let Some(item) = state.block().item()
        {
            let stack = ItemStack::mint(item, 1, MintReason::Creative).with_components(components);
            self.drop_items(pos, vec![stack]);
        }
        if !matches!(mode, BreakMode::Creative) && state.is_ice() {
            self.melt_broken_ice(pos, tool);
        }
        let tool_damage = match mode {
            BreakMode::Creative => 0,
            BreakMode::Start { .. } | BreakMode::Finish | BreakMode::Verified { .. } => tool_stack
                .as_deref()
                .and_then(ItemStack::tool)
                .filter(|_| {
                    state
                        .destroy_speed()
                        .is_some_and(|hardness| hardness != 0.0)
                })
                .map_or(0, |tool| tool.damage_per_block()),
        };
        player.send(PlayerEvent::ActionResult {
            sequence,
            outcome: ActionOutcome::Broken { state, tool_damage },
        });
    }

    /// Ice a player broke leaves water behind over a liquid or a block that
    /// melts it, unless the tool keeps it frozen (Silk Touch) or water
    /// evaporates here.
    fn melt_broken_ice(&mut self, pos: BlockPos, tool: Option<&ItemStack>) {
        let kept = tool.is_some_and(|tool| {
            tuff_datapack::tags::with(|tags| {
                tags.get(
                    "minecraft:enchantment",
                    tuff_vanilla_data::tag::enchantment::key::PREVENTS_ICE_MELTING.name,
                )
                .unwrap_or_default()
                .iter()
                .any(|id| tool.enchantment_level(id) > 0)
            })
        });
        if kept || self.dimension().environment().water_evaporates {
            return;
        }
        let melts = self
            .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
            .is_some_and(|below| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::ICE_MELTS_WHEN_DESTROYED_ABOVE,
                    below.block(),
                ) || below.is_liquid_block()
            });
        if melts {
            self.set_block_flags(
                pos,
                Block::Water.default_state(),
                crate::world::redstone::UPDATE_ALL,
                crate::world::redstone::UPDATE_LIMIT,
            );
        }
    }

    /// Removes `state` at `pos` without drops along with a block a player
    /// broke, telling its region when another region owns it.
    fn break_with(&mut self, pos: BlockPos, state: BlockState, player: PlayerId) {
        if self.owns(pos) {
            self.put_block(pos, placement::remains(state));
            self.break_event(pos, state, Some(player));
        } else {
            self.break_remote(pos, state, Some(player), false);
        }
    }

    /// Tells the region owning `pos` to break `state` there along with a
    /// block this one removed.
    pub(super) fn break_remote(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        player: Option<PlayerId>,
        drops: bool,
    ) {
        let chunk = ChunkPos::of_block(pos);
        if !self.pos.contains(chunk) {
            let update = BlockUpdate::BrokenWith {
                pos,
                expected: state,
                player,
                drops,
            };
            self.send_block_updates(HashMap::from([(chunk.region(), vec![update])]));
        }
    }

    /// A neighbour's break took the block at `pos` with it.
    fn broken_with(
        &mut self,
        pos: BlockPos,
        expected: BlockState,
        player: Option<PlayerId>,
        drops: bool,
    ) {
        if !self.owns(pos) || self.block(pos) != Some(expected) {
            return;
        }
        if drops {
            self.destroy_by_update(pos, expected, true, crate::world::redstone::UPDATE_LIMIT);
        } else {
            self.put_block(pos, placement::remains(expected));
            self.break_event(pos, expected, player);
        }
    }

    pub(super) fn owns(&self, pos: BlockPos) -> bool {
        self.columns.contains_key(&ChunkPos::of_block(pos))
    }

    /// Shows `state` breaking at `pos`: particles and sound.
    pub(super) fn break_event(&self, pos: BlockPos, state: BlockState, except: Option<PlayerId>) {
        if except.is_none() && matches!(state.block(), Block::Fire | Block::SoulFire) {
            return;
        }
        let event = packets([framed(&play::LevelEvent {
            event: play::level_event::BLOCK_BREAK,
            position: pos,
            data: state.id() as i32,
            global: false,
        })]);
        self.send_near(pos.center(), LEVEL_EVENT_RANGE, except, event);
    }

    pub(super) fn drop_items(&mut self, pos: BlockPos, stacks: Vec<ItemStack>) {
        for stack in stacks {
            let center = pos.center();
            let jitter = |rng: &mut StdRng| rng.gen_range(-0.25..0.25);
            let position = [
                center[0] + jitter(&mut self.rng),
                center[1] + jitter(&mut self.rng) - 0.125,
                center[2] + jitter(&mut self.rng),
            ];
            let velocity = [
                self.rng.gen_range(-0.1..0.1),
                0.2,
                self.rng.gen_range(-0.1..0.1),
            ];
            self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
        }
    }

    pub(super) fn send_block_updates(&mut self, updates: HashMap<RegionPos, Vec<BlockUpdate>>) {
        if updates.is_empty() {
            return;
        }
        self.flush_halo();
        for (region, updates) in updates {
            if let Some(neighbor) = self.border.neighbors.get(&region) {
                neighbor.internal(RegionInternal::BlockUpdates {
                    tick: Some(self.sched.tick),
                    updates,
                });
            }
        }
    }

    /// Applies the neighbours' updates made before this tick, as of the
    /// tick each was made on (at most one tick back).
    pub(super) fn apply_block_updates(&mut self) {
        let _span = tuff_timings::span!("region.block_updates");
        let now = self.sched.tick;
        let (current, earlier): (Vec<_>, Vec<_>) = std::mem::take(&mut self.border.pending_updates)
            .into_iter()
            .partition(|(tick, _)| tick.is_some_and(|tick| tick >= now));
        self.border.pending_updates = current;
        for (tick, updates) in earlier {
            self.sim.redstone.as_of = tick.map(|tick| tick.max(now.saturating_sub(1)));
            self.apply_updates(updates);
            self.sim.redstone.as_of = None;
        }
    }

    /// While frozen, neighbours' updates made by players or commands land
    /// at once, as within a region; the block events they queue wait.
    pub(super) fn apply_frozen_block_updates(&mut self) {
        let _span = tuff_timings::span!("region.block_updates");
        for (_, updates) in std::mem::take(&mut self.border.pending_updates) {
            self.apply_updates(updates);
        }
    }

    /// Applies the neighbours' updates made on this very tick (they ran it
    /// first), where the block events of the tick run, and the block events
    /// they queue.
    pub(super) fn apply_current_block_updates(&mut self) {
        let _span = tuff_timings::span!("region.block_updates");
        if self.border.pending_updates.is_empty() {
            return;
        }
        for (_, updates) in std::mem::take(&mut self.border.pending_updates) {
            self.apply_updates(updates);
        }
        self.run_block_events();
    }

    fn apply_updates(&mut self, updates: Vec<BlockUpdate>) {
        for update in updates {
            match update {
                BlockUpdate::Shape {
                    pos,
                    direction,
                    neighbour,
                    flags,
                    limit,
                    cause,
                } => self.with_cause(cause, |region| {
                    region.receive_shape(pos, direction, neighbour, flags, limit)
                }),
                BlockUpdate::Neighbour { pos, source, cause } => {
                    self.with_cause(cause, |region| region.receive_neighbour(pos, source))
                }
                BlockUpdate::Fluid {
                    pos,
                    direction,
                    fluid,
                } => {
                    let cause = super::fluids::fluid_cause(fluid.fluid());
                    self.with_cause(cause, |region| region.receive_spread(pos, direction, fluid))
                }
                BlockUpdate::Place { pos, state, cause } => {
                    self.with_cause(cause, |region| region.put_block(pos, state))
                }
                BlockUpdate::PrimeTnt { pos } => {
                    if self.owns(pos) && self.shared.rule_bool("tnt_explodes") {
                        self.spawn_primed_tnt(pos, None, None);
                    }
                }
                BlockUpdate::Piston {
                    pos,
                    state,
                    flags,
                    drops,
                    moving,
                    tick,
                    expected,
                    carry,
                } => {
                    self.receive_piston_set(pos, state, flags, drops, moving, tick, expected, carry)
                }
                BlockUpdate::Carried { pos, moved } => self.receive_carried(pos, moved),
                BlockUpdate::CarriedEntity { pos, nbt } => self.receive_carried_entity(pos, nbt),
                BlockUpdate::FinishMove { pos } => {
                    let cause = BlockCause::Block(Block::MovingPiston);
                    self.with_cause(cause, |region| region.finish_move(pos))
                }
                BlockUpdate::BrokenWith {
                    pos,
                    expected,
                    player,
                    drops,
                } => {
                    let cause = player.map_or(self.sim.cause, BlockCause::Player);
                    self.with_cause(cause, |region| {
                        region.broken_with(pos, expected, player, drops)
                    })
                }
                BlockUpdate::Exploded { pos, decay, cause } => {
                    let drops = self.explode_block(pos, decay, &cause);
                    self.drop_items(pos, drops);
                }
            }
        }
    }

    /// Sets a block and updates its neighbours, handing it to the owning
    /// region if it lies in another.
    pub(super) fn put_block(&mut self, pos: BlockPos, state: BlockState) {
        let chunk = ChunkPos::of_block(pos);
        if !self.pos.contains(chunk) {
            let update = BlockUpdate::Place {
                pos,
                state,
                cause: self.sim.cause,
            };
            self.send_block_updates(HashMap::from([(chunk.region(), vec![update])]));
        } else if self.owns(pos) {
            self.set_block_flags(
                pos,
                state,
                crate::world::redstone::UPDATE_ALL,
                crate::world::redstone::UPDATE_LIMIT,
            );
        }
    }

    /// Places a lily pad or frogspawn above the block (or water source)
    /// the player's view ray hits first.
    pub(super) fn place_on_water(&mut self, mut place: Place) {
        let look = crate::world::raycast::view_vector(place.yaw, place.pitch);
        let reach = crate::world::session::BLOCK_INTERACTION_RANGE;
        let to = [0, 1, 2].map(|i| place.eye[i] + look[i] * reach);
        let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
        let Some(hit) = crate::world::raycast::cast(
            place.eye,
            to,
            crate::world::raycast::FluidMode::SourceOnly,
            &level,
        ) else {
            let outcome = ActionOutcome::Rejected(self.corrections(&[]));
            self.answer_place(&place, outcome);
            return;
        };
        place.target = BlockPos::new(hit.pos.x, hit.pos.y + 1, hit.pos.z);
        place.face = tuff_world_model::Face::from_id(hit.face.id() as i32)
            .unwrap_or(tuff_world_model::Face::Up);
        place.cursor = [0, 1, 2].map(|i| {
            let base = [hit.pos.x, hit.pos.y, hit.pos.z][i] as f64;
            (hit.location[i] - base) as f32
        });
        place.interact = false;
        self.place(place);
    }

    fn report_item_used_on_block(&self, place: &Place, outcome: &ActionOutcome) {
        if let ActionOutcome::Used { off_hand, item, .. }
        | ActionOutcome::StartUsing { off_hand, item, .. } = outcome
            && place.held == Some(*item)
        {
            place.player.send(PlayerEvent::ItemUsedOnBlock {
                off_hand: *off_hand,
                item: *item,
                used_on: Box::new(self.location_facts(place.target)),
            });
        }
    }

    pub(super) fn place(&mut self, mut place: Place) {
        let reject = |region: &mut Self, place: &Place, at: &[BlockPos]| {
            let outcome = ActionOutcome::Rejected(region.corrections(at));
            region.answer_place(place, outcome);
        };
        let context = match place.context.clone() {
            Some(context) => context,
            None => {
                let content = self.shared.custom.clone();
                let item_def = crate::custom::items::prepare_place(&content, &mut place);
                let Some(target_state) = self.block(place.target) else {
                    return reject(self, &place, &[place.target]);
                };
                if place.interact
                    && self.owns(place.target)
                    && let Some(outcome) = self.interact_block(&place, target_state)
                {
                    let used = !matches!(outcome, ActionOutcome::Rejected(_));
                    self.report_item_used_on_block(&place, &outcome);
                    self.answer_place(&place, outcome);
                    if used {
                        self.player_used_block(place.player.id, place.target, target_state);
                        self.report_block_use(&place, !place.off_hand);
                    }
                    return;
                }
                if !place.may_build
                    && place.item.is_some()
                    && !self.adventure_allows(
                        &place.components,
                        "minecraft:can_place_on",
                        place.target,
                    )
                {
                    return reject(self, &place, &[place.target]);
                }
                if self.owns(place.target)
                    && let Some(behavior) = item_def.as_ref().and_then(|def| def.behavior.clone())
                    && let Some(outcome) = behavior.0.use_on_block(self, &place, target_state)
                {
                    let used = !matches!(outcome, ActionOutcome::Rejected(_));
                    self.answer_place(&place, outcome);
                    if used {
                        self.report_block_use(&place, false);
                    }
                    return;
                }
                if self.owns(place.target)
                    && let Some(mut outcome) = self.use_item_on(&place, target_state)
                {
                    if let ActionOutcome::Placed { placement, .. } = &mut outcome {
                        *placement = Some(Box::new(Placement {
                            used_on: self.location_facts(place.target),
                            placed: None,
                        }));
                    }
                    let used = !matches!(outcome, ActionOutcome::Rejected(_));
                    self.report_item_used_on_block(&place, &outcome);
                    self.answer_place(&place, outcome);
                    if used {
                        self.report_block_use(&place, false);
                    }
                    return;
                }
                let Some(item) = place.item else {
                    return reject(self, &place, &[place.target]);
                };
                if !place.game_master
                    && matches!(
                        item,
                        Item::CommandBlock
                            | Item::RepeatingCommandBlock
                            | Item::ChainCommandBlock
                            | Item::StructureBlock
                            | Item::Jigsaw
                            | Item::TestBlock
                            | Item::TestInstanceBlock
                    )
                {
                    return reject(self, &place, &[place.target]);
                }
                let context = {
                    let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
                    self.shared.block_rules.context(
                        place.target,
                        placement::facing(place.face),
                        place.cursor,
                        place.yaw,
                        place.pitch,
                        place.sneaking,
                        item,
                        self.dimension().shape,
                        &level,
                    )
                };
                let region = ChunkPos::of_block(context.pos).region();
                if region != self.pos {
                    let at = context.pos;
                    place.context = Some(context);
                    let refused = match self.border.neighbors.get(&region) {
                        Some(neighbor) => match neighbor.try_internal(RegionInternal::Place(place))
                        {
                            Ok(()) => return,
                            Err(refused) => match *refused {
                                RegionInternal::Place(place) => place,
                                _ => return,
                            },
                        },
                        None => place,
                    };
                    return reject(self, &refused, &[refused.target, at]);
                }
                context
            }
        };
        let at = context.pos;
        let corrected = [place.target, at];
        let center = at.center();
        let in_world = self.dimension().shape.contains_y(at.y);
        let reached = if context.item == Item::Scaffolding {
            context.clicked.center()
        } else {
            center
        };
        let reachable = distance_sq(place.eye, reached) <= REACH * REACH;
        if !(in_world && reachable && self.owns(at)) {
            return reject(self, &place, &corrected);
        }
        let state = {
            let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
            let rules = &self.shared.block_rules;
            if !rules.can_place(&context, &level) {
                None
            } else {
                rules.placement_state(&context, &level)
            }
        };
        let Some(state) = state else {
            return reject(self, &place, &corrected);
        };
        if !self
            .shared
            .growth_behaviors
            .clone()
            .light_allows(self, state, at)
        {
            return reject(self, &place, &corrected);
        }
        let state = place.components.apply_block_state(state);
        let state = self.redstone_placement(at, state);
        let blocked = self.ents.presence.values().any(|p| {
            let [x, y, z] = p.position;
            p.is_physical()
                && placement::obstructs(
                    state,
                    at,
                    [
                        x - PLAYER_HALF_WIDTH,
                        y,
                        z - PLAYER_HALF_WIDTH,
                        x + PLAYER_HALF_WIDTH,
                        y + PLAYER_HEIGHT,
                        z + PLAYER_HALF_WIDTH,
                    ],
                )
        });
        let blocked = blocked || self.entities_obstruct(state, at, Some(place.player.id));
        if blocked || !self.allows_block_action(place.player.id, at, state, BlockAction::Placed) {
            return reject(self, &place, &corrected);
        }
        self.put_block(at, state);
        self.apply_placed_item(at, &place);
        self.edit_placed_sign(at, &place.player);
        if let Some((pos, companion)) = self.shared.block_rules.companions(state, at) {
            self.put_block(pos, companion);
        }
        self.redstone_placed(at, state);
        let (mut sound, volume, pitch) = state.place_sound();
        let solid_bucket = match context.item.bucket() {
            Some(tuff_vanilla_data::item::Bucket::Solid { place_sound, .. }) => {
                sound = place_sound;
                true
            }
            _ => false,
        };
        let seed = self.rng.r#gen();
        let sound = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::BLOCKS,
            position: center,
            volume: (volume + 1.0) / 2.0,
            pitch: pitch * 0.8,
            seed,
        })]);
        self.send_sound_near(center, (volume + 1.0) / 2.0, Some(place.player.id), sound);
        let outcome = if solid_bucket {
            ActionOutcome::Used {
                off_hand: place.off_hand,
                item: context.item,
                effect: crate::world::messages::HandEffect::Emptied(Item::Bucket),
            }
        } else {
            ActionOutcome::Placed {
                off_hand: place.off_hand,
                item: context.item,
                placement: Some(Box::new(Placement {
                    used_on: self.location_facts(place.target),
                    placed: Some(self.location_facts(at)),
                })),
            }
        };
        self.answer_place(&place, outcome);
        self.report_block_use(&place, false);
        self.player_block_action(place.player.id, at, state, BlockAction::Placed);
    }

    /// Answers a block click, handing the stacks it offered and did not
    /// keep back to the player first.
    pub(crate) fn answer_place(&mut self, place: &Place, outcome: ActionOutcome) {
        self.return_offer(place);
        place.player.send(PlayerEvent::ActionResult {
            sequence: place.sequence,
            outcome,
        });
    }

    /// Hands what is left of a block click's offer back to the player, or
    /// drops it where the player stood if the player is gone.
    pub(crate) fn return_offer(&mut self, place: &Place) {
        for (slot, stack) in place.offer.drain() {
            if stack.is_empty() {
                continue;
            }
            if let Err(refused) = place.player.try_send(PlayerEvent::HandReturned {
                slot,
                stack,
                creative: false,
            }) && let PlayerEvent::HandReturned { stack, .. } = *refused
            {
                self.spawn_item_at(stack, place.position, [0.0; 3], 0);
            }
        }
    }

    /// Tells the player its right click on `place.target` did something,
    /// for `any_block_use` (and `default_block_use` when the block reacted
    /// on its own), with the block as it is after the use.
    fn report_block_use(&self, place: &Place, default: bool) {
        place.player.send(PlayerEvent::Advancement(Box::new(
            crate::world::messages::AdvancementEvent::UsedBlock {
                location: self.location_facts(place.target),
                off_hand: place.off_hand,
                default,
            },
        )));
    }

    /// A block as advancement location conditions see it.
    pub(super) fn location_facts(&self, pos: BlockPos) -> crate::world::predicate::LocationFacts {
        crate::world::predicate::LocationFacts {
            position: pos.center(),
            dimension: self.dimension().key.clone(),
            biome: self.biome_at(pos).map(str::to_owned),
            block: self.block(pos),
            ..Default::default()
        }
    }
}

/// A block's `x << 8 | z << 4 | y` index within its chunk section.
pub(super) fn section_index(pos: BlockPos) -> u16 {
    (((pos.x & 15) << 8) | ((pos.z & 15) << 4) | (pos.y & 15)) as u16
}

mod extra;
use extra::*;

/// The candle cake a candle item turns an uncut cake into.
fn candle_cake(item: Item) -> Option<Block> {
    let candle = item.block()?;
    if !tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::CANDLES, candle) {
        return None;
    }
    Block::from_name(&format!("{}_cake", item.name()))
}
mod interaction;

//! The region as feature behaviour sees it ([`RegionWorld`]) and the hooks
//! core calls it through.

use std::sync::Arc;

use super::{ChunkPos, RegionActor, RegionPos};
use crate::feature::feature_span;
use crate::world::Shared;
use crate::world::contact::PortalContact;
use crate::world::dimension::{Dimension, DimensionId};
use crate::world::entity::{RegionEntity, TrackedEntity};
use crate::world::explosion::Explosion;
use crate::world::messages::{
    ActionOutcome, Igniter, Packets, Place, PlayerEvent, PlayerId, RegionHandle,
};
use crate::world::messages::{BoatPlace, BucketUse, PlayerHandle, PlayerUpdate, RespawnSpot};
use crate::world::region_api::{
    BlockAction, PlayerBlockAction, PresentPlayer, RegionState, RegionWorld, RespawnQuery,
    TickPhase,
};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_timings::Tag;
use tuff_vanilla_data::fluid::LiquidPlacement;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{BlockState, Item};
use tuff_world_model::BlockPos;

impl RegionWorld for RegionActor {
    fn as_dyn(&mut self) -> &mut dyn RegionWorld {
        self
    }

    fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    fn tick(&self) -> u64 {
        self.sched.tick
    }

    fn inhabited_time(&self, chunk: ChunkPos) -> u64 {
        self.columns
            .get(&chunk)
            .map_or(0, |c| c.inhabited_time.max(0) as u64)
    }

    fn area_run(
        &mut self,
        area: crate::world::area::Area,
        local: bool,
        task: crate::world::area::AreaTask,
    ) -> u64 {
        RegionActor::area_run(self, area, local, task)
    }

    fn alert_entities(
        &mut self,
        kinds: &'static [tuff_vanilla_data::registry::EntityType],
        area: [f64; 6],
        attacker: i32,
    ) {
        RegionActor::alert_entities(self, kinds, area, attacker)
    }

    fn kill_entity(&mut self, id: i32) -> bool {
        RegionActor::kill_entity(self, id)
    }

    fn strike_entity(&mut self, id: i32, victim: u128) -> bool {
        RegionActor::strike_entity(self, id, victim)
    }

    fn hurt_entity(
        &mut self,
        id: i32,
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        amount: f32,
        culprit: Option<crate::world::entity::Culprit>,
    ) -> bool {
        RegionActor::hurt_entity(self, id, kind, amount, culprit)
    }

    fn replace_entity(&mut self, id: i32, nbt: tuff_nbt::Compound) -> bool {
        RegionActor::replace_entity(self, id, nbt)
    }

    fn edit_entity_tags(&mut self, id: i32, edit: &crate::world::messages::TagEdit) -> bool {
        RegionActor::edit_entity_tags(self, id, edit)
    }

    fn effect_entity(&mut self, id: i32, command: &crate::world::messages::EffectCommand) -> bool {
        RegionActor::effect_entity(self, id, command)
    }

    fn enchant_entity(
        &mut self,
        id: i32,
        enchantment: &str,
        level: i32,
    ) -> Option<crate::world::messages::EnchantAnswer> {
        RegionActor::enchant_entity(self, id, enchantment, level)
    }

    fn swing_entity(
        &mut self,
        id: i32,
        hand: i32,
        kind: tuff_item::item::SwingKind,
        duration: i32,
    ) -> Option<bool> {
        RegionActor::swing_entity(self, id, hand, kind, duration)
    }

    fn debug_path_entity(
        &mut self,
        id: i32,
        target: tuff_world_model::BlockPos,
    ) -> Option<crate::world::entity::DebugPath> {
        RegionActor::debug_path_entity(self, id, target)
    }

    fn attribute_entity(
        &mut self,
        id: i32,
        command: &crate::world::messages::AttributeCommand,
    ) -> Option<crate::world::messages::AttributeAnswer> {
        RegionActor::attribute_entity(self, id, command)
    }

    fn teleport_entity(&mut self, id: i32, teleport: &crate::world::messages::Teleport) -> bool {
        RegionActor::teleport_entity(self, id, teleport)
    }

    fn glide_entity(&mut self, id: i32, position: [f64; 3], rotation: Option<[f32; 2]>) -> bool {
        RegionActor::glide_entity(self, id, position, rotation)
    }

    fn take_teleported_entity(
        &mut self,
        id: i32,
        teleport: &crate::world::messages::Teleport,
    ) -> Option<tuff_nbt::Compound> {
        RegionActor::take_teleported_entity(self, id, teleport)
    }

    fn ride(&mut self, op: crate::world::messages::RideOp) -> crate::world::messages::RideAnswer {
        RegionActor::ride(self, op)
    }

    fn region_pos(&self) -> RegionPos {
        self.pos
    }

    fn dimension_id(&self) -> DimensionId {
        self.dimension
    }

    fn dimension_info(&self) -> &Dimension {
        RegionActor::dimension(self)
    }

    fn lit_ready_chunks(&self) -> Vec<ChunkPos> {
        self.ready_chunks(|column| column.lit)
    }

    fn loaded_chunks(&self) -> Vec<ChunkPos> {
        let mut chunks: Vec<ChunkPos> = self.columns.keys().copied().collect();
        chunks.sort_unstable_by_key(|c| (c.x, c.z));
        chunks
    }

    fn chunk_lit(&self, chunk: ChunkPos) -> bool {
        self.pos.contains(chunk)
            && self
                .columns
                .get(&chunk)
                .is_some_and(|column| column.lit && !column.unloading)
    }

    fn chunk_extra(&self, chunk: ChunkPos) -> Option<&Compound> {
        Some(&self.columns.get(&chunk)?.meta.extra)
    }

    fn biome_name(&self, pos: BlockPos) -> Option<&'static str> {
        self.biome_at(pos)
    }

    fn world_surface(&self, x: i32, z: i32) -> Option<i32> {
        let chunk = &self.columns.get(&ChunkPos::new(x >> 4, z >> 4))?.chunk;
        let shape = chunk.shape();
        for (index, section) in chunk.sections().enumerate().rev() {
            if section.is_empty() {
                continue;
            }
            let base = shape.section_base(index);
            for y in (base..base + 16).rev() {
                if !tuff_world_model::chunk::is_air(chunk.get(x, y, z)) {
                    return Some(y + 1);
                }
            }
        }
        Some(shape.min_y)
    }

    fn engine_light(&self, pos: BlockPos) -> Option<(u8, u8)> {
        self.light.lighting.as_ref()?.light_at(pos.x, pos.y, pos.z)
    }

    fn lighting_available(&self) -> bool {
        self.light.lighting.is_some()
    }

    fn block_event(&self, pos: BlockPos, action: u8, parameter: u8) {
        RegionActor::block_event(self, pos, action, parameter);
    }

    fn random_block_pos(&mut self, x: i32, y: i32, z: i32, y_mask: i32) -> BlockPos {
        RegionActor::random_block_pos(self, x, y, z, y_mask)
    }

    fn present_players(&self) -> Vec<PresentPlayer> {
        self.ents
            .presence
            .iter()
            .filter(|(_, presence)| presence.is_physical())
            .map(|(id, presence)| PresentPlayer {
                id: *id,
                position: presence.position,
                height: presence.looks.height,
                eye_height: presence.looks.eye_height,
                handle: presence.handle.clone(),
                armor: presence.looks.armor,
            })
            .collect()
    }

    fn seen_player_positions(&mut self) -> Vec<[f64; 3]> {
        self.seen_players().iter().map(|p| p.position).collect()
    }

    fn seen_players(&mut self) -> Vec<(PlayerHandle, [f64; 3])> {
        RegionActor::seen_players(self)
            .iter()
            .map(|p| (p.handle.clone(), p.position))
            .collect()
    }

    fn seen_player_details(&mut self) -> Vec<crate::world::region_api::SeenPlayer> {
        RegionActor::seen_players(self)
            .iter()
            .map(|p| crate::world::region_api::SeenPlayer {
                handle: p.handle.clone(),
                uuid: p.uuid,
                name: p.name.to_string(),
                position: p.position,
                looks: p.looks,
            })
            .collect()
    }

    fn entity_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.ents.entities.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    fn entity_ids_of(&self, kinds: &[tuff_vanilla_data::registry::EntityType]) -> Vec<i32> {
        let mut ids: Vec<i32> = kinds
            .iter()
            .flat_map(|kind| self.ents.entities.ids_of(*kind))
            .collect();
        if kinds.len() > 1 {
            ids.sort_unstable();
        }
        ids
    }

    fn entity(&self, id: i32) -> Option<&TrackedEntity> {
        self.ents.entities.get(&id)
    }

    fn entity_mut(&mut self, id: i32) -> Option<&mut TrackedEntity> {
        self.ents.rider_vehicles = None;
        self.ents.entities.get_mut(&id)
    }

    fn insert_item(
        &mut self,
        target: BlockPos,
        face: Option<tuff_vanilla_data::block::Facing>,
        stack: ItemStack,
        by: crate::world::item_port::Porter,
    ) -> crate::world::region_api::Insert {
        RegionActor::insert_item(self, target, face, stack, by)
    }

    fn shear_entity_in(&mut self, pos: BlockPos) -> bool {
        RegionActor::shear_entity_in(self, pos)
    }

    fn dispense_on_entity_in(
        &mut self,
        pos: BlockPos,
        stack: &crate::world::region_api::Fired,
    ) -> Option<crate::world::region_api::DispenseOutcome> {
        self.act_on_entities_in(pos, |entity, cx| entity.dispensed_on(stack, cx))
    }

    fn add_entity(&mut self, entity: Box<dyn RegionEntity>) {
        self.receive_entity(entity, 0);
    }

    fn rained_on(&self, position: [f64; 3], top: f64) -> bool {
        RegionActor::rained_on(self, position, top)
    }

    fn hit_entity(&mut self, target: i32, hit: crate::world::entity::EntityHit) {
        RegionActor::hit(self, target, hit);
    }

    fn finalize_spawn(&mut self, entity: &mut Box<dyn RegionEntity>, index: u32) {
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let ((), effects) = self.with_context(&summaries, &mut budget, |cx| {
            entity.finalize_spawn(cx, index);
        });
        let id = entity.base().id;
        let chunk = entity.base().chunk();
        self.apply_effects(id, chunk, effects);
    }

    fn best_neighbour_signal(&self, pos: BlockPos) -> u8 {
        let behaviors = self.shared.redstone_behaviors.clone();
        behaviors.best_neighbour_signal(self, pos)
    }

    fn raw_block_entity(&self, pos: BlockPos) -> Option<&Compound> {
        RegionActor::raw_block_entity(self, pos)
    }

    fn raw_block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound> {
        RegionActor::raw_block_entity_mut(self, pos)
    }

    fn raw_block_entities_where(&self, matches: &dyn Fn(&Compound) -> bool) -> Vec<BlockPos> {
        self.columns
            .values()
            .filter(|column| !column.unloading)
            .flat_map(|column| {
                column
                    .meta
                    .raw_block_entities
                    .iter()
                    .filter(|(_, nbt)| matches(nbt))
                    .map(|(pos, _)| *pos)
            })
            .collect()
    }

    fn raw_block_entities_near(
        &self,
        center: BlockPos,
        radius: i32,
        matches: &dyn Fn(&Compound) -> bool,
    ) -> Vec<BlockPos> {
        let near = |pos: &BlockPos| {
            (pos.x - center.x).abs() <= radius
                && (pos.y - center.y).abs() <= radius
                && (pos.z - center.z).abs() <= radius
        };
        let lo = ChunkPos::of_block(BlockPos::new(center.x - radius, 0, center.z - radius));
        let hi = ChunkPos::of_block(BlockPos::new(center.x + radius, 0, center.z + radius));
        let mut found = Vec::new();
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                let Some(column) = self.columns.get(&ChunkPos::new(x, z)) else {
                    continue;
                };
                if column.unloading {
                    continue;
                }
                found.extend(
                    column
                        .meta
                        .raw_block_entities
                        .iter()
                        .filter(|(pos, nbt)| near(pos) && matches(nbt))
                        .map(|(pos, _)| *pos),
                );
            }
        }
        found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        found
    }

    fn block_entity_generation(&self) -> u64 {
        self.block_entity_generation
    }

    fn mark_blocks_dirty(&mut self, pos: BlockPos) {
        if let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) {
            column.blocks_dirty = true;
        }
    }

    fn block_entity_changed(&mut self, pos: BlockPos) {
        self.mark_blocks_dirty(pos);
        self.send_block_entity_data(pos);
    }

    fn chunk(&self, pos: ChunkPos) -> Option<&tuff_world_model::chunk::Chunk> {
        Some(&self.columns.get(&pos)?.chunk)
    }

    fn handle(&self) -> RegionHandle {
        self.handle.clone()
    }

    fn projectile_hit_block(
        &mut self,
        hit: crate::world::raycast::Hit,
        impact: &crate::world::entity::ProjectileImpact,
    ) {
        RegionActor::projectile_hit_block(self, hit, impact);
    }

    fn relocate_entity(&mut self, id: i32, position: [f64; 3], target: RegionHandle, still: bool) {
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return;
        };
        let from = tracked.chunk();
        let base = tracked.entity.base_mut();
        let (old_position, old_velocity) = (base.position, base.velocity);
        base.position = position;
        if still {
            base.velocity = [0.0; 3];
            base.yaw = 0.0;
            base.pitch = 0.0;
        }
        let to = tracked.chunk();
        if let Some(column) = self.columns.get_mut(&from) {
            column.entities_dirty = true;
        }
        let track = tracked.broadcast(self.dimension, to);
        self.send_to_viewers(&[from], || PlayerEvent::Track(track.clone()));
        let seq = tracked.seq;
        if self.pos.contains(to) {
            self.receive_entity(tracked.entity, seq);
            return;
        }
        let handed = target.try_entity(tracked.entity, seq);
        if let Err(mut entity) = handed {
            let base = entity.base_mut();
            base.position = old_position;
            base.velocity = old_velocity;
            self.receive_entity(entity, seq);
        }
    }

    fn owns(&self, pos: BlockPos) -> bool {
        RegionActor::owns(self, pos)
    }

    fn find_blocks(
        &self,
        block: tuff_vanilla_data::Block,
        min: BlockPos,
        max: BlockPos,
    ) -> Vec<BlockPos> {
        RegionActor::find_blocks(self, block, min, max)
    }

    fn pois_near(
        &self,
        center: BlockPos,
        radius: i32,
        kind: tuff_vanilla_data::registry::PointOfInterestType,
    ) -> Vec<BlockPos> {
        self.chunk_io.pois.in_square(center, radius, kind)
    }

    fn pois(&self) -> &crate::world::poi::PoiStore {
        &self.chunk_io.pois
    }

    fn remove_entity(&mut self, id: i32) -> Option<TrackedEntity> {
        RegionActor::remove_entity(self, id, &[])
    }

    fn remove_entity_intact(&mut self, id: i32) -> Option<TrackedEntity> {
        RegionActor::remove_entity_intact(self, id, &[])
    }

    fn remove_entity_kept(&mut self, id: i32) -> Option<TrackedEntity> {
        RegionActor::remove_entity_kept(self, id, &[])
    }

    fn broadcast_level_event(&mut self, event: i32, pos: BlockPos, data: i32) {
        RegionActor::level_event(self, event, pos, data, false);
    }

    fn broadcast_global_level_event(&mut self, event: i32, pos: BlockPos, data: i32) {
        let _ = self
            .shared
            .coordinator
            .try_send(crate::world::CoordinatorMessage::GlobalLevelEvent { event, pos, data });
    }

    fn set_block_anywhere(&mut self, pos: BlockPos, state: BlockState) {
        RegionActor::set_block_anywhere(self, pos, state);
    }

    fn put_block(&mut self, pos: BlockPos, state: BlockState) {
        RegionActor::put_block(self, pos, state);
    }

    fn place_item_block(&mut self, pos: BlockPos, state: BlockState, stack: &ItemStack) -> bool {
        RegionActor::place_item_block(self, pos, state, stack)
    }

    fn set_block_flags(&mut self, pos: BlockPos, state: BlockState, flags: u32) {
        RegionActor::set_block_flags(
            self,
            pos,
            state,
            flags,
            crate::world::redstone::UPDATE_LIMIT,
        );
    }

    fn break_by_update(&mut self, pos: BlockPos, state: BlockState, drops: bool) {
        self.destroy_by_update(pos, state, drops, crate::world::redstone::UPDATE_LIMIT);
    }

    fn play_sound(
        &self,
        sound: SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
        except: Option<PlayerId>,
    ) {
        RegionActor::play_sound(self, sound, pos, volume, pitch, except);
    }

    fn send_to_players_near(
        &self,
        center: [f64; 3],
        range: f64,
        skip: &[PlayerId],
        packets: Packets,
    ) {
        RegionActor::send_to_players_near(self, center, range, skip, packets);
    }

    fn send_near(&self, center: [f64; 3], range: f64, except: Option<PlayerId>, packets: Packets) {
        RegionActor::send_near(self, center, range, except, packets);
    }

    fn send_to_chunk(&self, chunk: ChunkPos, except: Option<PlayerId>, packets: Packets) {
        self.send_chunk_packets(chunk, except, packets);
    }

    fn break_event(&self, pos: BlockPos, state: BlockState, except: Option<PlayerId>) {
        RegionActor::break_event(self, pos, state, except);
    }

    fn corrections(&self, positions: &[BlockPos]) -> Packets {
        RegionActor::corrections(self, positions)
    }

    fn target_hit(
        &mut self,
        hit: crate::world::raycast::Hit,
        impact: &crate::world::entity::ProjectileImpact,
    ) {
        RegionActor::target_hit(self, hit, impact);
    }

    fn award_stat(&mut self, id: i32, stat: tuff_vanilla_data::registry::CustomStat, amount: i32) {
        if let Some(presence) = self.ents.presence.get(&id) {
            presence
                .handle
                .send(crate::world::messages::PlayerEvent::AwardStat(stat, amount));
        }
    }

    fn ring_bell(&mut self, pos: BlockPos, face: Option<tuff_vanilla_data::block::Facing>) {
        RegionActor::ring_bell(self, pos, face);
    }

    fn wind_reached(&mut self, pos: BlockPos) {
        RegionActor::wind_reached(self, pos);
    }

    fn neighbor(&self, pos: RegionPos) -> Option<RegionHandle> {
        self.border.neighbors.get(&pos).cloned()
    }

    fn drop_items(&mut self, pos: BlockPos, stacks: Vec<ItemStack>) {
        RegionActor::drop_items(self, pos, stacks);
    }

    fn spawn_item_at(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    ) {
        RegionActor::spawn_item_at(self, stack, position, velocity, pickup_delay);
    }

    fn saved_block_entity(&self, pos: BlockPos) -> Option<Compound> {
        RegionActor::saved_block_entity(self, pos)
    }

    fn release_infestation(&mut self, pos: BlockPos, state: BlockState, tool: Option<&ItemStack>) {
        RegionActor::release_infestation(self, pos, state, tool);
    }

    fn explode(&mut self, explosion: Explosion) {
        RegionActor::explode(self, explosion);
    }

    fn explode_shielded(&mut self, explosion: Explosion, shield: BlockPos) {
        RegionActor::explode_shielded(self, explosion, shield);
    }

    fn spawn_primed_tnt(&mut self, pos: BlockPos, fuse: Option<i32>, igniter: Option<Igniter>) {
        RegionActor::spawn_primed_tnt(self, pos, fuse, igniter);
    }

    fn fire_sound(&mut self, item: Item) -> (SoundEvent, f32) {
        RegionActor::fire_sound(self, item)
    }

    fn allows_block_action(
        &mut self,
        player: PlayerId,
        pos: BlockPos,
        state: BlockState,
        action: BlockAction,
    ) -> bool {
        RegionActor::allows_block_action(self, player, pos, state, action)
    }

    fn apply_liquid_placement(&mut self, pos: BlockPos, placement: &'static LiquidPlacement) {
        RegionActor::apply_liquid_placement(self, pos, placement);
    }

    fn add_menu_viewer(&mut self, pos: BlockPos, player: PlayerHandle) {
        self.add_viewer(pos, player);
    }

    fn menu_viewers(&self, pos: BlockPos) -> Vec<PlayerHandle> {
        self.menus.viewers.get(&pos).cloned().unwrap_or_default()
    }

    fn close_menu_viewers(&mut self, pos: BlockPos) {
        self.close_viewers(pos);
    }

    fn is_spectator(&self, id: PlayerId) -> bool {
        self.ents
            .presence
            .get(&id)
            .is_some_and(|presence| presence.spectator)
    }

    fn use_redstone(&mut self, pos: BlockPos, state: BlockState) -> bool {
        RegionActor::use_redstone(self, pos, state)
    }

    fn use_sign(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        RegionActor::use_sign(self, place, state)
    }

    fn wake_bed_occupant(&mut self, pos: BlockPos, foot: BlockPos) -> bool {
        RegionActor::wake_bed_occupant(self, pos, foot)
    }

    fn state(&mut self) -> &mut RegionState {
        &mut self.ents.feature_state
    }

    fn with_tick_context(
        &mut self,
        position: [f64; 3],
        f: &mut dyn FnMut(&mut crate::world::entity::TickContext),
    ) {
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let ((), effects) = self.with_context(&summaries, &mut budget, |cx| f(cx));
        self.apply_effects(-1, ChunkPos::of_position(position), effects);
    }

    fn player_effect(&mut self, event: crate::world::region_api::PlayerEffectEvent) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.player_effect(self, &event);
        }
    }

    fn game_event(
        &mut self,
        event: crate::world::game_event::GameEvent,
        pos: [f64; 3],
        context: crate::world::game_event::Context,
    ) {
        let emission = crate::world::game_event::Emission {
            event,
            pos,
            context,
        };
        let block = BlockPos::new(
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        );
        self.debug_event(
            tuff_vanilla_data::registry::DebugSubscription::GameEvents,
            block,
            crate::protocol::play::DebugValue::GameEvent {
                event: event.id() as i32,
                pos,
            },
        );
        self.hear_game_event(&emission);
        let radius = f64::from(crate::world::game_event::radius(event));
        for region in tuff_world_model::pos::regions_near(pos, radius) {
            if region == self.pos {
                continue;
            }
            if let Some(neighbor) = self.border.neighbors.get(&region) {
                neighbor.internal(crate::world::messages::RegionInternal::Run(
                    crate::world::region_api::RegionTask::new(move |world| {
                        world.hear_game_event(&emission)
                    }),
                ));
            }
        }
    }

    fn hear_game_event(&mut self, emission: &crate::world::game_event::Emission) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.game_event(self, emission);
        }
    }

    fn comparator_output(&self, pos: BlockPos) -> u8 {
        crate::world::redstone::Reader::comparator_output(self, pos)
    }

    fn update_neighbors_at(&mut self, pos: BlockPos, block: tuff_vanilla_data::Block) {
        crate::world::redstone::RedstoneLevel::update_neighbors_at(self, pos, block);
    }

    fn update_output_signal(&mut self, pos: BlockPos, block: tuff_vanilla_data::Block) {
        crate::world::redstone::update_neighbour_for_output_signal(self, pos, block);
    }

    fn block_cause(&self) -> crate::world::region_api::BlockCause {
        self.sim.cause
    }

    fn set_block_cause(
        &mut self,
        cause: crate::world::region_api::BlockCause,
    ) -> crate::world::region_api::BlockCause {
        std::mem::replace(&mut self.sim.cause, cause)
    }

    fn report_container_edit(
        &mut self,
        holder: crate::world::region_api::ContainerHolder,
        slots: &[(usize, Option<&ItemStack>, Option<&ItemStack>)],
    ) {
        if !self.observes_containers() {
            return;
        }
        let slots = slots
            .iter()
            .map(|(slot, old, new)| crate::world::region_api::SlotEdit::of(*slot, *old, *new))
            .collect();
        RegionActor::report_container_edit(self, holder, slots);
    }

    fn container_kind(&self, pos: BlockPos) -> Option<crate::world::block_entity::ContainerKind> {
        Some(self.block_entity(pos)?.kind)
    }

    fn block_entity_state_mut(
        &mut self,
        pos: BlockPos,
    ) -> Option<&mut crate::world::block_entity::State> {
        Some(&mut self.block_entity_mut(pos)?.state)
    }

    fn container_items(&self, pos: BlockPos) -> Option<&[Option<ItemStack>]> {
        Some(&self.block_entity(pos)?.items)
    }

    fn set_container_items(
        &mut self,
        pos: BlockPos,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        self.set_container_items_for_plugin(pos, slots)
    }

    fn edit_container(
        &mut self,
        pos: BlockPos,
        edit: &mut dyn FnMut(&mut [Option<ItemStack>]),
    ) -> bool {
        self.edit_container_for_feature(pos, edit)
    }

    fn edit_container_slot(
        &mut self,
        pos: BlockPos,
        slot: usize,
        edit: &mut dyn FnMut(&mut Option<ItemStack>),
    ) -> bool {
        self.edit_container_slot_for_feature(pos, slot, edit)
    }

    fn entity_container_items(
        &self,
        uuid: u128,
    ) -> Option<crate::world::entity_slots::ContainerSlots<'_>> {
        self.entity_container_items_by_uuid(uuid)
    }

    fn entity_held_items(&self, uuid: u128) -> Option<Vec<Option<tuff_item::item::ItemView<'_>>>> {
        self.entity_held_items_by_uuid(uuid)
    }

    fn set_entity_held_items(
        &mut self,
        uuid: u128,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        self.set_entity_held_items_for_plugin(uuid, slots)
    }

    fn set_entity_container_items(
        &mut self,
        uuid: u128,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        self.set_entity_container_items_for_plugin(uuid, slots)
    }
}

impl RegionActor {
    /// Runs every feature's region hooks for this tick phase.
    pub(super) fn run_tick_hooks(&mut self, phase: TickPhase) {
        self.flush_destroyed();
        let _span = tuff_timings::span!("region.hooks", Tag::Name(phase.label()));
        let hooks = self.shared.region_hooks.clone();
        let owners = self.shared.hook_owners.clone();
        for (i, hook) in hooks.iter().enumerate() {
            let _feature = feature_span(&owners.region_hooks, i);
            hook.tick(self, phase);
        }
    }

    /// The biome name at `pos`, here or in the halo.
    pub(super) fn biome_at(&self, pos: BlockPos) -> Option<&'static str> {
        let chunk = ChunkPos::of_block(pos);
        self.columns
            .get(&chunk)
            .map(|c| &c.chunk)
            .or_else(|| self.border.halo.get(&chunk))
            .map(|c| {
                self.shared
                    .dimensions
                    .biomes()
                    .name(c.biome(pos.x, pos.y, pos.z))
            })
    }

    /// Lets every feature add the creatures a fresh chunk starts with.
    pub(super) fn chunk_generated(&mut self, chunk: ChunkPos) {
        let _span = tuff_timings::span!("region.chunk_generated_hooks");
        let hooks = self.shared.region_hooks.clone();
        let owners = self.shared.hook_owners.clone();
        for (i, hook) in hooks.iter().enumerate() {
            let _feature = feature_span(&owners.region_hooks, i);
            hook.chunk_generated(self, chunk);
        }
    }

    /// Lets every feature react to an entity that moved from `before`.
    pub(super) fn entity_moved(&mut self, tracked: &mut TrackedEntity, before: [f64; 3]) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.entity_moved(self, tracked, before);
        }
    }

    pub(super) fn entity_landed(&mut self, landing: &crate::world::region_api::Landing) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.entity_landed(self, landing);
        }
    }

    pub(super) fn player_moved(&mut self, update: &PlayerUpdate) {
        self.footsteps(update);
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.player_moved(self, update);
        }
    }

    /// Whether a feature lit a portal where fire was just placed.
    pub(super) fn light_portal(&mut self, pos: BlockPos) -> bool {
        let hooks = self.shared.region_hooks.clone();
        hooks.iter().any(|hook| hook.light_portal(self, pos, None))
    }

    /// The portal block the player in `update` is inside.
    pub(super) fn portal_contact(&mut self, update: &PlayerUpdate) -> Option<PortalContact> {
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .find_map(|hook| hook.portal_contact(self, update))
    }

    /// Hands a block menu button press to the first feature that takes it.
    pub(super) fn menu_button(
        &mut self,
        player: PlayerHandle,
        pos: BlockPos,
        button: i32,
        may_build: bool,
    ) {
        let viewing = self
            .menus
            .viewers
            .get(&pos)
            .is_some_and(|viewers| viewers.iter().any(|v| v.id == player.id));
        if !viewing || !self.owns(pos) {
            return;
        }
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .any(|hook| hook.menu_button(self, &player, pos, button, may_build));
    }

    /// The first feature answer to a block click.
    pub(super) fn use_block(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .find_map(|hook| hook.use_block(self, place, state))
    }

    /// The first feature answer to an item used on a block.
    pub(super) fn use_item_hooks(
        &mut self,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .find_map(|hook| hook.use_item_on(self, place, state))
    }

    pub(super) fn attack_block(&mut self, pos: BlockPos, state: BlockState) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.attack_block(self, pos, state);
        }
    }

    pub(super) fn play_note(&mut self, pos: BlockPos) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.play_note(self, pos);
        }
        RegionWorld::game_event(
            self,
            crate::world::game_event::GameEvent::NoteBlockPlay,
            pos.center(),
            crate::world::game_event::Context::default(),
        );
    }

    pub(super) fn use_door(&mut self, pos: BlockPos, open: bool) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.use_door(self, pos, open);
        }
    }

    pub(super) fn occupy_bed(&mut self, pos: BlockPos, occupied: bool) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.occupy_bed(self, pos, occupied);
        }
    }

    pub(super) fn leave_bed(&mut self, player: PlayerHandle, pos: BlockPos, yaw: f32) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.leave_bed(self, &player, pos, yaw);
        }
    }

    pub(super) fn respawn_point(
        &mut self,
        pos: BlockPos,
        yaw: f32,
        pitch: f32,
        forced: bool,
        consume: bool,
    ) -> Option<RespawnSpot> {
        let query = RespawnQuery {
            pos,
            yaw,
            pitch,
            forced,
            consume,
        };
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .find_map(|hook| hook.respawn_point(self, &query))
            .flatten()
    }

    pub(super) fn use_bucket(&mut self, request: BucketUse) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.use_bucket(self, &request);
        }
    }

    pub(super) fn place_boat(&mut self, request: BoatPlace) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.place_boat(self, &request);
        }
    }

    /// A projectile landed: the `projectile_land` game event, then features.
    pub(super) fn projectile_landed(&mut self, landing: &crate::world::entity::ProjectileLanding) {
        use crate::world::game_event::{Context, GameEvent, Source};
        let (position, affected) = match landing.block {
            Some(pos) => (pos.center(), self.block(pos)),
            None => (landing.position, None),
        };
        RegionWorld::game_event(
            self,
            GameEvent::ProjectileLand,
            position,
            Context {
                source: Some(Source::entity(landing.id)),
                affected,
            },
        );
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.projectile_landed(self, landing);
        }
    }

    /// Whether every feature lets `player` break (or place) `state` at `pos`.
    pub(super) fn allows_block_action(
        &mut self,
        player: PlayerId,
        pos: BlockPos,
        state: BlockState,
        action: BlockAction,
    ) -> bool {
        let action = PlayerBlockAction {
            player,
            pos,
            state,
            action,
        };
        let hooks = self.shared.region_hooks.clone();
        hooks
            .iter()
            .all(|hook| hook.allows_block_action(self, &action))
    }

    /// Tells every feature a player is about to break a block.
    pub(super) fn player_breaking_block(
        &mut self,
        player: PlayerId,
        pos: BlockPos,
        state: BlockState,
        tool: Option<&tuff_item::item::ItemStack>,
    ) {
        let action = PlayerBlockAction {
            player,
            pos,
            state,
            action: BlockAction::Broke,
        };
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.player_breaking_block(self, &action, tool);
        }
    }

    pub(super) fn player_used_block(&mut self, player: PlayerId, pos: BlockPos, old: BlockState) {
        let used = crate::world::region_api::PlayerBlockUse {
            player,
            pos,
            old,
            new: self.block(pos),
        };
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.player_used_block(self, &used);
        }
    }

    pub(super) fn sign_edited(&mut self, edit: crate::world::region_api::SignEdit) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.sign_edited(self, &edit);
        }
    }

    /// Tells every feature what a player did to a block.
    pub(super) fn player_block_action(
        &mut self,
        player: PlayerId,
        pos: BlockPos,
        state: BlockState,
        action: BlockAction,
    ) {
        let action = PlayerBlockAction {
            player,
            pos,
            state,
            action,
        };
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.player_block_action(self, &action);
        }
        use crate::world::game_event::{Context, GameEvent, Source};
        let event = match action.action {
            BlockAction::Broke => GameEvent::BlockDestroy,
            BlockAction::Placed => GameEvent::BlockPlace,
            BlockAction::Opened => GameEvent::ContainerOpen,
        };
        RegionWorld::game_event(
            self,
            event,
            pos.center(),
            Context::from(Source::player(player)).with_block(state),
        );
    }

    /// Tells every feature a player opened or broke something piglins
    /// guard that is not a block (a chest boat or cart).
    pub(super) fn disturb_guarded_container(&mut self, player: PlayerId) {
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.guarded_container_disturbed(self, player);
        }
    }
}

impl RegionActor {
    /// Turns a player's movement into the step and landing events the
    /// game posts: one step per stretch walked on the ground, one landing
    /// per fall.
    fn footsteps(&mut self, update: &PlayerUpdate) {
        use crate::world::game_event::{Context, GameEvent, Source, Steps};
        let id = update.handle.id;
        let steps = self.ents.feature_state.get::<Steps>();
        if !update.alive {
            steps.forget(id);
            return;
        }
        let walked = steps.walk(id, update.position, update.grounded);
        let source = Source::player(id)
            .sneaking(update.looks.sneaking)
            .spectating(update.spectator);
        let at = update.position;
        if update.landed.is_some_and(|fall| fall > 0.0) {
            RegionWorld::game_event(self, GameEvent::HitGround, at, Context::from(source));
        }
        if walked {
            RegionWorld::game_event(self, GameEvent::Step, at, Context::from(source));
        }
    }
}

impl RegionActor {
    /// Posts the game event of a door, gate, trapdoor, lever, button or
    /// plate switching.
    pub(super) fn announce_toggle(&mut self, pos: BlockPos, old: BlockState, state: BlockState) {
        use crate::world::game_event::{Context, GameEvent};
        use tuff_vanilla_data::{Block, tag};
        let block = state.block();
        let event = if tuff_datapack::tags::is(tag::block::key::DOORS, block)
            || tuff_datapack::tags::is(tag::block::key::TRAPDOORS, block)
            || tuff_datapack::tags::is(tag::block::key::FENCE_GATES, block)
        {
            match (old.get("open"), state.get("open")) {
                (Some(before), Some(after)) if before != after => Some(if after == "true" {
                    GameEvent::BlockOpen
                } else {
                    GameEvent::BlockClose
                }),
                _ => None,
            }
        } else if block == Block::Lever
            || tuff_datapack::tags::is(tag::block::key::BUTTONS, block)
            || tuff_datapack::tags::is(tag::block::key::PRESSURE_PLATES, block)
        {
            match (old.get("powered"), state.get("powered")) {
                (Some(before), Some(after)) if before != after => Some(if after == "true" {
                    GameEvent::BlockActivate
                } else {
                    GameEvent::BlockDeactivate
                }),
                _ => None,
            }
        } else {
            None
        };
        if let Some(event) = event {
            RegionWorld::game_event(self, event, pos.center(), Context::block(state));
        }
    }
}

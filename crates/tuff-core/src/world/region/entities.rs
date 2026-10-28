//! The region's entities (players aside): ownership across chunks and
//! regions, ticking, reporting them to viewers, saving them with their
//! chunk, and what items and experience orbs do with players nearby.
//!
//! Everything here works on [`RegionEntity`] trait objects; only the item
//! and orb interactions with players and hoppers look at concrete types.

use std::collections::{HashMap, HashSet};

use rand::Rng;

use super::{PICKUP_RESERVATION_TICKS, RegionActor, framed, packets};
use crate::protocol::play::{self, MetadataValue, metadata};
use crate::world::entity::{
    self, Effect, EntitySummary, NearbyPlayer, RegionEntity, Tick, TickContext, TrackedEntity,
};
use crate::world::entity::{EntityAttack, EntityGrid, EntityHit, EntityInteraction, Reservation};
use crate::world::experience;
use crate::world::items::ItemEntity;
use crate::world::messages::AdvancementEvent;
use crate::world::messages::{EffectCommand, Sweep};
use crate::world::messages::{EntitySnapshot, PlayerEvent, PlayerHandle, PlayerId, Teleport};
use crate::world::predicate::{DamageFacts, EntityFacts};
use crate::world::region_api::{BlockCause, RegionStateLock};
use compute::Frame;
use compute::{Computed, PATH_CREDIT_TICKS, PathCredit};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Path search nodes a region may visit per tick, across all its mobs.
mod admin;
mod compute;
mod effects;
mod interact;
mod pickup;
mod shoulder;

const PATH_BUDGET: u32 = 16_384;

/// Starts the tick order at a different entity each tick so the same
/// entity does not always apply its tick first.
fn rotate_for_fairness(ids: &mut [i32], tick: u64) {
    if !ids.is_empty() {
        let by = (tick % ids.len() as u64) as usize;
        ids.rotate_left(by);
    }
}

#[cfg(test)]
mod fairness_tests {
    use super::rotate_for_fairness;

    #[tuff_core::watchdog]
    #[test]
    fn every_entity_leads_in_turn() {
        let mut leaders = Vec::new();
        for tick in 0..4 {
            let mut ids = [1, 2, 3, 4];
            rotate_for_fairness(&mut ids, tick);
            leaders.push(ids[0]);
        }
        assert_eq!(leaders, [1, 2, 3, 4]);
    }
}

/// A living, non-spectating player the region's entities can see.
#[derive(Debug, Clone)]
pub(super) struct Seen {
    pub handle: PlayerHandle,
    pub uuid: u128,
    pub name: std::sync::Arc<str>,
    pub position: [f64; 3],
    pub looks: crate::world::messages::PlayerLooks,
}

fn summary(entity: &dyn RegionEntity) -> EntitySummary {
    let base = entity.base();
    EntitySummary {
        id: base.id,
        uuid: base.uuid,
        kind: base.kind,
        position: base.position,
        velocity: base.velocity,
        yaw: base.yaw,
        bounds: entity.bounds(),
        living: entity.living(),
        pickable: entity.pickable(),
        item: entity.downcast_ref::<ItemEntity>().map(|item| {
            let takeable = item.pickup_delay == 0 && item.reserved.is_none();
            (item.stack.item(), item.stack.count(), takeable)
        }),
        vehicle: base.vehicle.as_ref().map(|mount| mount.id),
        carries: !base.riders.is_empty() || !base.entity_riders.is_empty(),
        blue_sheep: entity.blue_sheep(),
        ominous_banner: entity.downcast_ref::<ItemEntity>().is_some_and(|item| {
            item.stack.item() == tuff_vanilla_data::Item::WhiteBanner
                && item
                    .stack
                    .components
                    .banner_patterns
                    .as_ref()
                    .is_some_and(|p| p.len() == 8)
        }),
    }
}

impl RegionActor {
    fn alert_neighbours(
        &mut self,
        kinds: &'static [tuff_vanilla_data::registry::EntityType],
        area: [f64; 6],
        attacker: i32,
    ) {
        crate::world::area::broadcast(self, area, move |world| {
            world.alert_entities(kinds, area, attacker);
        });
    }

    /// Tells this region's idle mobs of `kinds` within `area` to attack
    /// `attacker`.
    pub(super) fn alert_entities(
        &mut self,
        kinds: &'static [tuff_vanilla_data::registry::EntityType],
        area: [f64; 6],
        attacker: i32,
    ) {
        for tracked in self.ents.entities.values_mut() {
            let helps = kinds.contains(&tracked.entity.base().kind)
                && tracked
                    .entity
                    .living()
                    .is_some_and(|l| l.alive && l.target.is_none())
                && crate::world::physics::intersects(tracked.entity.bounds(), area);
            if helps {
                tracked
                    .entity
                    .notice(crate::world::entity::Notice::Alert { attacker });
            }
        }
    }

    /// Asks the manager to open the region owning `entity`'s chunk and
    /// hands the entity to it, so it is never dropped at a border.
    fn hand_to_unloaded(&mut self, entity: Box<dyn RegionEntity>, seq: u64) {
        let (dimension, region) = (self.dimension, entity.base().chunk().region());
        let manager = self.shared.manager.clone();
        tokio::spawn(async move {
            let (reply, handle) = tokio::sync::oneshot::channel();
            let asked = manager
                .send(crate::world::messages::ManagerMessage::Acquire {
                    dimension,
                    region,
                    reply,
                })
                .await
                .is_ok();
            let Some(handle) = (if asked { handle.await.ok() } else { None }) else {
                log::warn!("dropping {} outside of any region", entity.saved_id());
                burn_held(entity);
                return;
            };
            if let Err(entity) = handle.try_entity(entity, seq) {
                log::warn!("dropping {} outside of any region", entity.saved_id());
                burn_held(entity);
            }
            let _ = manager
                .send(crate::world::messages::ManagerMessage::Release { dimension, region })
                .await;
        });
    }

    /// Takes ownership of an entity in one of this region's chunks, loading
    /// the chunk first if needed, or hands it to the neighbour owning it.
    pub(super) fn receive_entity(&mut self, entity: Box<dyn RegionEntity>, seq: u64) {
        self.ents.rider_vehicles = None;
        let chunk = entity.base().chunk();
        if !self.pos.contains(chunk) {
            let entity = match self.border.neighbors.get(&chunk.region()) {
                Some(neighbor) => match neighbor.try_entity(entity, seq) {
                    Ok(()) => return,
                    Err(entity) => entity,
                },
                None => entity,
            };
            self.hand_to_unloaded(entity, seq);
            return;
        }
        if seq == 0 {
            let hooks = self.shared.region_hooks.clone();
            for hook in hooks.iter() {
                hook.entity_spawned(self, &*entity);
            }
        }
        if self.columns.contains_key(&chunk) {
            self.add_entity(entity, seq, true);
        } else {
            self.load(chunk).entities.push((entity, seq));
        }
    }

    /// A broken infested block lets its silverfish out, unless block drops
    /// are off or the tool keeps it in (Silk Touch).
    pub(super) fn release_infestation(
        &mut self,
        pos: BlockPos,
        state: tuff_vanilla_data::BlockState,
        tool: Option<tuff_item::item::AnyItem<'_>>,
    ) {
        if state.block().infested_host().is_none() || !self.shared.rule_bool("block_drops") {
            return;
        }
        let kept_in = tool.is_some_and(|tool| {
            tuff_datapack::tags::with(|tags| {
                tags.get(
                    "minecraft:enchantment",
                    tuff_vanilla_data::tag::enchantment::key::PREVENTS_INFESTED_SPAWNS.name,
                )
                .unwrap_or_default()
                .iter()
                .any(|id| tuff_item::with_item!(tool, |tool| tool.enchantment_level(id)) > 0)
            })
        });
        if kept_in {
            return;
        }
        let id = self.shared.next_entity_id();
        let position = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
        let Some(silverfish) = self.shared.entity_types.create(
            tuff_vanilla_data::registry::EntityType::Silverfish,
            id,
            position,
            0.0,
        ) else {
            return;
        };
        self.receive_entity(silverfish, 0);
        let packet = framed(&play::EntityEvent {
            entity_id: id,
            status: play::entity_event::SILVERFISH_MERGE_WITH_STONE,
        });
        self.send_to_viewers(&[ChunkPos::of_block(pos)], || PlayerEvent::EntityPackets {
            entity: id,
            packets: packets([packet.clone()]),
        });
    }

    pub(super) fn spawn_entity(&mut self, entity: impl RegionEntity) {
        self.receive_entity(Box::new(entity), 0);
    }

    /// Throws away an entity whose UUID is already alive, burning what it
    /// carries.
    fn refuse_duplicate(mut entity: Box<dyn RegionEntity>) {
        log::warn!("dropping {}: its UUID already exists", entity.saved_id());
        for stack in entity.take_items() {
            stack.burn(tuff_item::item::BurnReason::Discarded);
        }
        if let Some(block) = entity.take_held() {
            block.burn(crate::world::block_ledger::BlockBurn::Killed);
        }
    }

    /// Starts tracking an entity in a loaded chunk; `announce` reports it to
    /// the chunk's viewers at once.
    pub(super) fn add_entity(
        &mut self,
        mut entity: Box<dyn RegionEntity>,
        seq: u64,
        announce: bool,
    ) {
        if !entity.base().claim.is_held() {
            match self.shared.claim_entity_uuid(entity.base().uuid) {
                Some(claim) => entity.base_mut().claim.put(claim),
                None => {
                    Self::refuse_duplicate(entity);
                    return;
                }
            }
        }
        let span = entity.take_id_span();
        if span > 0 {
            entity.base_mut().id = self.shared.next_entity_ids(span + 1);
        }
        if !entity.parts().is_empty() {
            self.ents.part_owners.insert(entity.base().id);
        }
        let tracked = TrackedEntity::new(entity, seq);
        let chunk = tracked.chunk();
        if let Some(column) = self.columns.get_mut(&chunk) {
            column.entities_dirty = true;
        }
        if announce {
            let track = tracked.track(self.dimension, chunk);
            self.fanout_chunk(chunk, None, |_| PlayerEvent::Track(track.clone()));
        }
        let id = tracked.id();
        self.ents.entities.insert(id, tracked);
        self.board_saved_passengers(id, announce, 0);
    }

    /// Removes an entity for good, telling the viewers of `chunks` (its
    /// chunk when empty); the items still in it are burned.
    pub(super) fn remove_entity(&mut self, id: i32, chunks: &[ChunkPos]) -> Option<TrackedEntity> {
        let mut tracked = self.remove_entity_intact(id, chunks)?;
        for stack in tracked.entity.take_items() {
            stack.burn(tuff_item::item::BurnReason::Discarded);
        }
        if let Some(block) = tracked.entity.take_held() {
            block.burn(crate::world::block_ledger::BlockBurn::Killed);
        }
        Some(tracked)
    }

    /// Like `remove_entity` for an entity whose items live on (saved into
    /// a block or sent elsewhere).
    pub(super) fn remove_entity_intact(
        &mut self,
        id: i32,
        chunks: &[ChunkPos],
    ) -> Option<TrackedEntity> {
        let parts = self.custom_entity_parts(id);
        let tracked = self.remove_entity_kept(id, chunks)?;
        self.ents.destroyed.push(tracked.entity.base().uuid);
        for part in parts {
            self.remove_entity(part, &[]);
        }
        Some(tracked)
    }

    /// Hands the scoreboard the entities destroyed so far.
    pub(super) fn flush_destroyed(&mut self) {
        if self.ents.destroyed.is_empty() {
            return;
        }
        let gone = std::mem::take(&mut self.ents.destroyed);
        let board = self.shared.scoreboard.clone();
        tokio::spawn(async move { board.forget_entities(gone).await });
    }

    /// Like `remove_entity` for an entity that lives on elsewhere.
    pub(super) fn remove_entity_kept(
        &mut self,
        id: i32,
        chunks: &[ChunkPos],
    ) -> Option<TrackedEntity> {
        let mut tracked = self.ents.entities.remove(&id)?;
        self.chunk_io.pois.release_holder(id);
        self.eject_riders(&mut tracked);
        self.unlink_removed(&mut tracked);
        self.ents.part_owners.remove(&id);
        self.ents
            .feature_state
            .get::<crate::world::game_event::Steps>()
            .forget(id);
        let own = [tracked.chunk()];
        let chunks = if chunks.is_empty() { &own[..] } else { chunks };
        for chunk in chunks {
            if let Some(column) = self.columns.get_mut(chunk) {
                column.entities_dirty = true;
            }
        }
        let seq = tracked.seq + 1;
        self.send_to_viewers(chunks, || PlayerEvent::Untrack { entity: id, seq });
        Some(tracked)
    }

    /// Reports every entity in the chunk to a new viewer.
    pub(super) fn send_owned_entities(
        &self,
        pos: ChunkPos,
        viewer: &crate::world::messages::PlayerHandle,
    ) {
        for tracked in self.ents.entities.values() {
            if tracked.chunk() == pos {
                viewer.send(PlayerEvent::Track(tracked.track(self.dimension, pos)));
            }
        }
    }

    /// Sends a report to the viewers of any of `chunks`, once each.
    pub(super) fn send_to_viewers(&self, chunks: &[ChunkPos], event: impl Fn() -> PlayerEvent) {
        for (index, chunk) in chunks.iter().enumerate() {
            if chunks[..index].contains(chunk) {
                continue;
            }
            let Some(column) = self.columns.get(chunk) else {
                continue;
            };
            for (viewer, handle) in &column.viewers {
                let seen = chunks[..index].iter().any(|earlier| {
                    self.columns
                        .get(earlier)
                        .is_some_and(|c| c.viewers.contains_key(viewer))
                });
                if !seen {
                    handle.send(event());
                }
            }
        }
    }

    pub(super) fn despawn_settings(&self) -> crate::world::despawn_settings::DespawnSettings {
        crate::world::despawn_settings::DespawnSettings::from_config(&self.shared.config)
    }

    /// Removes the free-standing entities of a chunk beyond the per-type
    /// caps, oldest kept, before the chunk is saved; their items burn.
    pub(super) fn enforce_entity_caps(&mut self, pos: ChunkPos) {
        let caps = crate::world::entity_caps::EntityCaps::from_config(&self.shared.config);
        let mut free: Vec<(i32, &str)> = self
            .ents
            .entities
            .values()
            .filter(|tracked| {
                tracked.chunk() == pos
                    && tracked.entity.persists()
                    && caps.limit(tracked.entity.saved_id()).is_some()
                    && tracked.entity.base().entity_riders.is_empty()
                    && tracked.entity.base().custom_name.is_none()
                    && tracked.entity.leash().is_none()
                    && !self.saved_nested(tracked.id())
            })
            .map(|tracked| (tracked.id(), tracked.entity.saved_id()))
            .collect();
        free.sort_unstable_by_key(|(id, _)| *id);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut excess: Vec<i32> = Vec::new();
        let mut dropped: HashMap<&str, (usize, usize)> = HashMap::new();
        for (id, name) in free {
            let cap = caps.limit(name).unwrap_or(usize::MAX);
            let seen = counts.entry(name).or_default();
            *seen += 1;
            if *seen > cap {
                excess.push(id);
                dropped.entry(name).or_insert((cap, 0)).1 += 1;
            }
        }
        for (name, (kept, lost)) in dropped {
            log::warn!("chunk {pos:?} holds too many {name} entities: kept {kept}, dropped {lost}");
        }
        for id in excess {
            let Some(mut tracked) = self.remove_entity_intact(id, &[]) else {
                continue;
            };
            for stack in tracked.entity.take_items() {
                stack.burn(tuff_item::item::BurnReason::CapDropped);
            }
            if let Some(block) = tracked.entity.take_held() {
                block.burn(crate::world::block_ledger::BlockBurn::Killed);
            }
        }
    }

    /// The saved form of every entity in the chunk, raw ones included.
    pub(super) fn saved_entities(&self, pos: ChunkPos) -> Vec<Compound> {
        let mut saved = self
            .columns
            .get(&pos)
            .map(|column| column.other_entities.clone())
            .unwrap_or_default();
        let mut owned: Vec<&TrackedEntity> = self
            .ents
            .entities
            .values()
            .filter(|tracked| {
                tracked.chunk() == pos
                    && tracked.entity.persists()
                    && !self.saved_nested(tracked.id())
            })
            .collect();
        owned.sort_unstable_by_key(|tracked| tracked.id());
        saved.extend(
            owned
                .into_iter()
                .map(|tracked| self.save_with_passengers(tracked, 0)),
        );
        saved
    }

    /// Drops the entities of an unloading chunk; they were saved with it.
    pub(super) fn forget_entities(&mut self, pos: ChunkPos) {
        let roots: Vec<i32> = self
            .ents
            .entities
            .values()
            .filter(|tracked| tracked.chunk() == pos && !self.saved_nested(tracked.id()))
            .map(|tracked| tracked.id())
            .collect();
        let mut gone: HashSet<i32> = self.stacks_on(&roots).into_iter().collect();
        gone.extend(roots);
        let entities = &mut self.ents.entities;
        tuff_item::item::conservation::releasing(|| entities.retain(|id, _| !gone.contains(id)));
    }

    /// What other entities see of each entity, for this tick.
    pub(super) fn entity_summaries(&self) -> Vec<EntitySummary> {
        let mut summaries: Vec<EntitySummary> = self
            .ents
            .entities
            .values()
            .map(|tracked| summary(&*tracked.entity))
            .collect();
        summaries.sort_unstable_by_key(|s| s.id);
        summaries
    }

    /// The players its entities see this tick: those present here, then
    /// those the player index lists within reach of this region.
    /// The burn and extinguish sounds [`entity::base_tick`] asked for.
    fn base_tick_sounds(&mut self, entity: &dyn RegionEntity, outcome: entity::BaseTick) {
        let base = entity.base();
        if base.silent || !(outcome.burned || outcome.extinguished) {
            return;
        }
        let category = if base.kind.category() == tuff_vanilla_data::entity::MobCategory::Monster {
            play::sound_category::HOSTILE
        } else {
            play::sound_category::NEUTRAL
        };
        let mut sounds = Vec::new();
        if outcome.burned {
            sounds.push((
                SoundEvent::EntityGenericBurn,
                0.4,
                2.0 + self.rng.r#gen::<f32>() * 0.4,
            ));
        }
        if outcome.extinguished {
            let spread = self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>();
            sounds.push((
                SoundEvent::EntityGenericExtinguishFire,
                0.7,
                1.6 + spread * 0.4,
            ));
        }
        let packet = packets(sounds.into_iter().map(|(sound, volume, pitch)| {
            framed(&play::Sound {
                sound_event: sound.id(),
                category,
                position: base.position,
                volume,
                pitch,
                seed: self.rng.r#gen(),
            })
        }));
        self.send_sound_near(base.position, 1.0, None, packet);
    }

    pub(super) fn seen_players(&mut self) -> std::sync::Arc<[Seen]> {
        if let Some((tick, seen)) = &self.ents.seen
            && *tick == self.sched.tick
        {
            return seen.clone();
        }
        let mut seen: Vec<Seen> = self
            .ents
            .presence
            .values()
            .filter(|p| p.is_physical())
            .map(|p| Seen {
                handle: p.handle.clone(),
                uuid: p.uuid,
                name: std::sync::Arc::from(p.name.as_str()),
                position: p.position,
                looks: p.looks,
            })
            .collect();
        let index = self.shared.players.borrow().clone();
        for player in index.near(self.dimension, self.pos) {
            if !self.ents.presence.contains_key(&player.handle.id) {
                seen.push(Seen {
                    handle: player.handle.clone(),
                    uuid: player.uuid,
                    name: player.name.clone(),
                    position: player.position,
                    looks: player.looks,
                });
            }
        }
        seen.sort_unstable_by_key(|p| p.handle.id);
        let seen: std::sync::Arc<[Seen]> = seen.into();
        self.ents.seen = Some((self.sched.tick, seen.clone()));
        seen
    }

    /// A seen player's session.
    pub(super) fn seen_player(&mut self, id: i32) -> Option<PlayerHandle> {
        self.seen_players()
            .iter()
            .find(|p| p.handle.id == id)
            .map(|p| p.handle.clone())
    }

    /// Runs `f` with a context over this region's world, returning what it
    /// returned and the effects it queued.
    pub(super) fn with_context<R>(
        &mut self,
        summaries: &[EntitySummary],
        budget: &mut u32,
        f: impl FnOnce(&mut TickContext) -> R,
    ) -> (R, Vec<Effect>) {
        self.with_indexed_context(summaries, None, None, budget, None, f)
    }

    /// The players entities here see, as the tick context lists them.
    fn nearby_players<'s>(&mut self, seen: &'s [Seen]) -> Vec<NearbyPlayer<'s>> {
        let entities = &self.ents.entities;
        let vehicles = self
            .ents
            .rider_vehicles
            .get_or_insert_with(|| rider_vehicles(entities));
        seen.iter()
            .map(|p| NearbyPlayer {
                vehicle: player_vehicle(vehicles, entities, p.handle.id),
                id: p.handle.id,
                uuid: p.uuid,
                name: &p.name,
                position: p.position,
                eye_height: p.looks.eye_height,
                yaw: p.looks.yaw,
                pitch: p.looks.pitch,
                held: p.looks.held,
                creative: p.looks.creative,
                height: p.looks.height,
                sneaking: p.looks.sneaking,
                invisible: p.looks.invisible,
                invulnerable: p.looks.invulnerable,
                armor: p.looks.armor,
                effects: p.looks.effects,
                movement: p.looks.movement,
                sleeping: p.looks.sleeping,
                last_hurt_by: p.looks.last_hurt_by,
                last_hurt_mob: p.looks.last_hurt_mob,
                perchable: p.looks.perchable,
                sprinting: p.looks.sprinting,
            })
            .collect()
    }

    /// Runs `f` with a frame over this region's world as it is now.
    /// `parts` stands in for the part boxes when given.
    pub(super) fn with_frame<R>(
        &mut self,
        parts: Option<Vec<crate::world::entity::PartBox>>,
        f: impl FnOnce(&Frame) -> R,
    ) -> R {
        let far = self.take_far_chunks();
        let state = RegionStateLock::new(std::mem::take(&mut self.ents.feature_state));
        let result = f(&Frame::new(self, &far, &state, parts));
        self.ents.feature_state = state.into_inner();
        self.return_far_chunks(far);
        result
    }

    pub(super) fn with_indexed_context<R>(
        &mut self,
        summaries: &[EntitySummary],
        grid: Option<&EntityGrid>,
        players: Option<&[NearbyPlayer]>,
        budget: &mut u32,
        rng: Option<&mut rand::rngs::StdRng>,
        f: impl FnOnce(&mut TickContext) -> R,
    ) -> (R, Vec<Effect>) {
        let seen = self.seen_players();
        let built = match players {
            Some(_) => Vec::new(),
            None => self.nearby_players(&seen),
        };
        let nearby = players.unwrap_or(&built);
        let mut own_rng = match rng {
            Some(_) => None,
            None => Some(std::mem::replace(
                &mut self.rng,
                rand::SeedableRng::seed_from_u64(0),
            )),
        };
        let result = self.with_frame(None, |frame| {
            let rng = match rng {
                Some(rng) => rng,
                None => own_rng.as_mut().expect("taken above"),
            };
            frame.run(summaries, grid, nearby, budget, rng, None, f)
        });
        if let Some(rng) = own_rng {
            self.rng = rng;
        }
        result
    }

    /// Breaks a block as the world does, with its particles and sound.
    /// A burning thing stood in the water or powder snow cauldron at
    /// `pos`: it loses a level (powder snow melts to water first).
    pub(super) fn lower_cauldron(&mut self, pos: BlockPos) {
        use crate::world::game_event::{Context, GameEvent};
        use crate::world::redstone::{UPDATE_ALL, UPDATE_LIMIT};
        use crate::world::region_api::RegionWorld;
        use tuff_vanilla_data::Block;
        let Some(state) = self.block(pos).filter(|_| self.owns(pos)) else {
            return;
        };
        if !matches!(
            state.block(),
            Block::WaterCauldron | Block::PowderSnowCauldron
        ) {
            return;
        }
        let level = state.level().unwrap_or(1).saturating_sub(1);
        let next = if level == 0 {
            Block::Cauldron.default_state()
        } else {
            let water = if state.block() == Block::PowderSnowCauldron {
                Block::WaterCauldron.default_state()
            } else {
                state
            };
            water.with_level(level).unwrap_or(water)
        };
        self.set_block_flags(pos, next, UPDATE_ALL, UPDATE_LIMIT);
        let context = Context {
            source: None,
            affected: Some(next),
        };
        RegionWorld::game_event(self, GameEvent::BlockChange, pos.center(), context);
    }

    pub(super) fn destroy_block(&mut self, pos: BlockPos, drop: bool) {
        let Some(state) = self.block(pos).filter(|_| self.owns(pos)) else {
            return;
        };
        let broken = self.claim(
            pos,
            crate::world::block_ledger::Remains::Air,
            crate::world::block_ledger::ClaimReason::Popped,
        );
        self.break_event(pos, state, None);
        if drop && let Some(broken) = broken {
            let stacks = self.shared.block_loot().destroyed(broken, &mut self.rng);
            self.drop_items(pos, stacks);
        }
        if drop {
            self.release_infestation(pos, state, None);
        }
    }

    /// Moves and ticks every entity, handing those that leave the region to
    /// their new owner.
    pub(super) fn tick_entities(&mut self) {
        let _span = tuff_timings::span!("region.entities");
        self.fetch_far_chunks();
        self.close_orphaned_mob_openers();
        if !self.ents.boss_bars.is_empty() {
            self.sweep_boss_bars();
        }
        if self.ents.entities.is_empty() {
            return;
        }
        if !self.ents.mount_requests.is_empty() {
            self.handle_mount_requests();
        }
        self.prune_mounts();
        self.ents.item_grid = None;
        let mut ids: Vec<i32> = self.ents.entities.keys().copied().collect();
        ids.sort_unstable();
        rotate_for_fairness(&mut ids, self.sched.tick);
        let ids = self.ride_order(ids);
        let mut seats = HashMap::new();
        let void_y = self.dimension().void_y();
        let mut summaries = self.entity_summaries();
        let mut grid = EntityGrid::build(&summaries);
        let mut slots = summary_slots(&summaries);
        self.ents.rider_vehicles = Some(rider_vehicles(&self.ents.entities));
        let seen = self.seen_players();
        let mut nearby = self.nearby_players(&seen);
        let entity_seed: u64 = self.rng.r#gen();
        let share = PATH_BUDGET / u32::try_from(ids.len()).unwrap_or(u32::MAX).max(1);
        let credit = PathCredit {
            share,
            cap: share.saturating_mul(PATH_CREDIT_TICKS).min(PATH_BUDGET),
        };
        for tracked in self.ents.entities.values() {
            self.chunk_io
                .pois
                .arrive(tracked.id(), tracked.entity.poi_claims());
        }
        let present = &self.ents.entities;
        self.chunk_io
            .pois
            .forget_absent_holders(|id| present.contains_key(&id));
        let ready = self.prepare_entities(&ids);
        self.refresh_vehicles(&mut nearby);
        let mut order: Vec<i32> = ready
            .iter()
            .copied()
            .filter(|id| {
                self.ents
                    .entities
                    .get(id)
                    .is_some_and(|t| t.entity.base().vehicle.is_none())
            })
            .collect();
        let shuffle = self
            .shared
            .entity_compute_shuffle
            .load(std::sync::atomic::Ordering::Relaxed);
        if shuffle != 0 {
            use rand::SeedableRng;
            use rand::seq::SliceRandom;
            order.shuffle(&mut rand::rngs::StdRng::seed_from_u64(
                shuffle ^ self.sched.tick,
            ));
        }
        let mut computed =
            self.compute_entities(&order, &summaries, &grid, &nearby, entity_seed, credit);
        for id in ready {
            let result = match computed.remove(&id) {
                Some(result) => Some(result),
                None => {
                    if self.ents.rider_vehicles.is_none() {
                        nearby = self.nearby_players(&seen);
                    }
                    self.refresh_vehicles(&mut nearby);
                    self.compute_entity(id, &summaries, &grid, &nearby, entity_seed, credit)
                }
            };
            let Some(Computed {
                from,
                before,
                data_changed,
                riders_before,
                holder_before,
                base,
                outcome,
                effects,
            }) = result
            else {
                continue;
            };
            let (crushes, effects): (Vec<Effect>, Vec<Effect>) = effects
                .into_iter()
                .partition(|e| matches!(e, Effect::Crammed { .. }));
            for crush in crushes {
                let Effect::Crammed {
                    counted,
                    players,
                    max,
                } = crush
                else {
                    continue;
                };
                let left = counted
                    .iter()
                    .filter(|other| {
                        slots
                            .get(other)
                            .is_some_and(|&i| summaries[i].living.is_none_or(|l| l.alive))
                    })
                    .count();
                if left as i32 + players > max - 1 {
                    let cramming = &tuff_vanilla_data::damage_type::CRAMMING;
                    self.hit(id, EntityHit::new(6.0, cramming));
                }
            }
            let Some(mut tracked) = self.ents.entities.remove(&id) else {
                continue;
            };
            self.base_tick_sounds(&*tracked.entity, base);
            if let Some(pos) = base.lower_cauldron {
                self.lower_cauldron(pos);
            }
            self.chunk_io.pois.hold(id, tracked.entity.poi_claims());
            if tracked.entity.take_impulse() {
                tracked.sync.impulse();
            }
            if tracked.entity.base().riders.len() != riders_before
                || tracked.entity.leash().and_then(|l| l.holder) != holder_before
            {
                tracked.refresh_spawn();
            }
            if let Some(&index) = slots.get(&id) {
                summaries[index] = summary(&*tracked.entity);
                grid.replace(index, &summaries[index]);
            }
            let mut changed = tracked.entity.take_changed_data();
            if data_changed {
                let flags = tracked.entity.base().flags();
                changed.insert(0, (metadata::ENTITY_FLAGS, MetadataValue::Byte(flags)));
            }
            if let Some(data) = tracked.delta_packets(&changed) {
                self.send_to_viewers(&[from], || PlayerEvent::EntityPackets {
                    entity: id,
                    packets: data.clone(),
                });
            }
            self.send_equipment(&mut tracked);
            if tracked.ticks % 20 == 0 {
                self.send_attributes(&mut tracked);
            }
            let partners: Vec<i32> = effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Breed { partner } => Some(*partner),
                    _ => None,
                })
                .collect();
            let taken: Vec<i32> = effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::TakeItem { item, .. } => Some(*item),
                    _ => None,
                })
                .collect();
            self.ents.lifts = Some(Vec::new());
            let cause = BlockCause::entity(&*tracked.entity);
            self.with_cause(cause, |region| region.apply_effects(id, from, effects));
            for (collector, stack) in std::mem::take(&mut self.ents.taken_stacks) {
                if collector == id {
                    tracked.entity.took_item(stack);
                }
            }
            for (holder, block) in std::mem::take(&mut self.ents.taken_blocks) {
                if holder == id {
                    tracked.entity.receive_held(block);
                } else {
                    block.burn(crate::world::block_ledger::BlockBurn::Orphaned);
                }
            }
            for (gap, top) in self.ents.lifts.take().unwrap_or_default() {
                super::redstone::lift(tracked.entity.base_mut(), gap, top);
            }
            for item in taken {
                match self.ents.entities.get(&item) {
                    Some(left) => {
                        let left = summary(&*left.entity);
                        if let Some(&index) = slots.get(&item) {
                            summaries[index] = left;
                            grid.replace(index, &summaries[index]);
                        }
                    }
                    None => {
                        summaries.retain(|s| s.id != item);
                        grid = EntityGrid::build(&summaries);
                        slots = summary_slots(&summaries);
                    }
                }
            }
            for partner in partners {
                let mut budget = tracked.path_credit;
                self.breed(&mut tracked, partner, &summaries, &mut budget);
                tracked.path_credit = budget;
            }
            if let Some(seat) = seats.remove(&id)
                && tracked.entity.base().vehicle.is_some()
            {
                tracked.entity.base_mut().position = seat;
            }
            let base = tracked.entity.base_mut();
            let mut to = base.chunk();
            if to != from && !self.columns.contains_key(&to) && !self.border.halo.contains_key(&to)
            {
                base.position = before;
                base.velocity = [0.0; 3];
                to = from;
            }
            match outcome {
                Tick::Alive if base.position[1] >= void_y => {}
                outcome => {
                    self.ents.entities.insert(id, tracked);
                    self.remove_entity(id, &[from, to]);
                    if let Tick::Explode(explosion) = outcome {
                        self.explode(explosion);
                    }
                    continue;
                }
            }
            self.entity_moved(&mut tracked, before);
            if tracked.entity.base().position != before {
                self.follow_vehicle(&tracked);
            }
            self.note_seats(&tracked, &mut seats);
            self.entity_footsteps(&tracked);
            let boss = tracked.entity.boss_bar();
            if boss.is_some() || self.ents.boss_bars.contains_key(&id) {
                let uuid = tracked.entity.base().uuid;
                let position = tracked.entity.base().position;
                self.update_boss_bar(id, uuid, position, boss);
            }
            if let Some(column) = self.columns.get_mut(&from) {
                column.entities_dirty = true;
            }
            let position = tracked.entity.base().position;
            self.forward_impulse(&tracked);
            let movement = tracked.tick_movement();
            if movement.is_some() || to != from {
                let track = tracked.moved(self.dimension, to, movement.unwrap_or_default());
                self.send_to_viewers(&[from, to], || PlayerEvent::Track(track.clone()));
            } else if tracked.sync.was_due()
                && let Some(head) = tracked.head_packets()
            {
                self.send_to_viewers(&[from], || PlayerEvent::EntityPackets {
                    entity: id,
                    packets: head.clone(),
                });
            }
            let seated_here = tracked
                .entity
                .base()
                .vehicle
                .as_ref()
                .is_some_and(|mount| self.ents.entities.contains_key(&mount.id));
            if !self.pos.contains(to) && !seated_here {
                let seq = tracked.seq;
                let handed = match self.border.neighbors.get(&to.region()).cloned() {
                    Some(neighbor) => self
                        .hand_entity(&neighbor, tracked.entity, seq)
                        .map(|()| neighbor),
                    None => Err(tracked.entity),
                };
                let mut entity = match handed {
                    Ok(neighbor) => {
                        self.hand_off_viewers(id, &neighbor);
                        summaries.retain(|s| s.id != id);
                        grid = EntityGrid::build(&summaries);
                        slots = summary_slots(&summaries);
                        continue;
                    }
                    Err(entity) => entity,
                };
                let base = entity.base_mut();
                base.position = before;
                base.velocity = [0.0; 3];
                tracked.entity = entity;
                self.follow_vehicle(&tracked);
            } else if to != from
                && let Some(column) = self.columns.get_mut(&to)
            {
                column.entities_dirty = true;
            }
            let moved = (0..3).any(|axis| before[axis].floor() != position[axis].floor());
            self.ents.entities.insert(id, tracked);
            for touched in self.merge_items(id, moved) {
                match self.ents.entities.get(&touched) {
                    Some(left) => {
                        let left = summary(&*left.entity);
                        if let Some(&index) = slots.get(&touched) {
                            summaries[index] = left;
                            grid.replace(index, &summaries[index]);
                        }
                    }
                    None => {
                        summaries.retain(|s| s.id != touched);
                        grid = EntityGrid::build(&summaries);
                        slots = summary_slots(&summaries);
                    }
                }
            }
            self.collect_orb(id);
            if !self.ents.mount_requests.is_empty() {
                self.handle_mount_requests();
            }
        }
        self.ents.rider_vehicles = None;
    }

    /// Lets a dropped item that is due merge with the items around it the
    /// way the game does, and returns every item it changed or removed.
    /// Items in a neighbour region are asked for through it.
    fn merge_items(&mut self, id: i32, moved: bool) -> Vec<i32> {
        let settings = self.despawn_settings();
        let Some(reach) = self
            .ents
            .entities
            .get(&id)
            .and_then(|tracked| tracked.entity.downcast_ref::<ItemEntity>())
            .filter(|item| item.merge_due(moved, &settings))
            .map(|item| item.merge_reach(&settings))
        else {
            return Vec::new();
        };
        if self.ents.item_grid.is_none() {
            let mut grid: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
            for other in self
                .ents
                .entities
                .values()
                .filter_map(|other| other.entity.downcast_ref::<ItemEntity>())
                .filter(|other| other.mergeable(&settings))
            {
                let [x, _, z] = other.base.position;
                grid.entry((x.floor() as i32, z.floor() as i32))
                    .or_default()
                    .push(other.base.id);
            }
            self.ents.item_grid = Some(grid);
        }
        let grid = self.ents.item_grid.as_ref().expect("built above");
        let (x0, x1) = (reach[0].floor() as i32 - 1, reach[3].floor() as i32 + 1);
        let (z0, z1) = (reach[2].floor() as i32 - 1, reach[5].floor() as i32 + 1);
        let mut nearby: Vec<i32> = (x0..=x1)
            .flat_map(|x| (z0..=z1).map(move |z| (x, z)))
            .filter_map(|cell| grid.get(&cell))
            .flatten()
            .copied()
            .filter(|&other_id| {
                other_id != id
                    && self
                        .ents
                        .entities
                        .get(&other_id)
                        .and_then(|other| other.entity.downcast_ref::<ItemEntity>())
                        .is_some_and(|other| {
                            other.mergeable(&settings)
                                && experience::intersects(other.bounds(), reach)
                        })
            })
            .collect();
        nearby.sort_unstable();
        let mut touched = Vec::new();
        for other_id in nearby {
            let Some(mut own) = self.ents.entities.remove(&id) else {
                break;
            };
            let result = self
                .ents
                .entities
                .get_mut(&other_id)
                .and_then(|other| other.entity.downcast_mut::<ItemEntity>())
                .zip(own.entity.downcast_mut::<ItemEntity>())
                .filter(|(other, item)| other.mergeable(&settings) && item.merges_with(other))
                .map(|(other, item)| {
                    if item.absorbs(other) {
                        item.absorb(other);
                    } else {
                        other.absorb(item);
                    }
                    (item.stack.count(), other.stack.count())
                });
            self.ents.entities.insert(id, own);
            let Some((own_count, other_count)) = result else {
                continue;
            };
            touched.extend([id, other_id]);
            for (entity, count) in [(other_id, other_count), (id, own_count)] {
                if count == 0 {
                    self.remove_entity(entity, &[]);
                } else {
                    self.refresh_item_stack(entity);
                }
            }
            if !self.ents.entities.contains_key(&id) {
                break;
            }
        }
        if self.ents.entities.contains_key(&id) {
            self.probe_border_merges(id);
        }
        touched.sort_unstable();
        touched.dedup();
        touched
    }

    /// Tells viewers what the entity wears and holds, if that changed.
    fn send_attributes(&self, tracked: &mut TrackedEntity) {
        let Some(packet) = tracked.attribute_packet() else {
            return;
        };
        let packet = packets([packet]);
        let (id, chunk) = (tracked.id(), tracked.chunk());
        self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
            entity: id,
            packets: packet.clone(),
        });
    }

    fn send_equipment(&self, tracked: &mut TrackedEntity) {
        if !tracked.entity.take_equipment_changed() {
            return;
        }
        tracked.refresh_spawn();
        let items = tracked.entity.equipment();
        let packet = packets([framed(&play::SetEquipment {
            entity_id: tracked.id(),
            items: &items,
        })]);
        let (id, chunk) = (tracked.id(), tracked.chunk());
        self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
            entity: id,
            packets: packet.clone(),
        });
    }

    fn send_changed_data(&mut self, tracked: &mut TrackedEntity) {
        self.send_equipment(tracked);
        let changed = tracked.entity.take_changed_data();
        if changed.is_empty() {
            return;
        }
        let (id, chunk) = (tracked.id(), tracked.chunk());
        let Some(data) = tracked.delta_packets(&changed) else {
            return;
        };
        self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
            entity: id,
            packets: data.clone(),
        });
    }
}

/// Sets a teleported entity's position (keeping axes given as non-finite)
/// and rotation, turns it to face `teleport.look_at`, zeroes its vertical
/// velocity and puts it on the ground.
fn place_teleported(entity: &mut dyn RegionEntity, teleport: &Teleport) {
    let eye_height = crate::world::entity::eye_height(entity.base().kind, entity.bounds());
    let base = entity.base_mut();
    for axis in 0..3 {
        if teleport.position[axis].is_finite() {
            base.position[axis] = teleport.position[axis];
        }
    }
    base.velocity[1] = 0.0;
    base.on_ground = true;
    if let Some([yaw, pitch]) = teleport.rotation {
        base.yaw = if teleport.relative_rotation[0] {
            yaw + base.yaw
        } else {
            yaw
        };
        base.pitch = if teleport.relative_rotation[1] {
            pitch + base.pitch
        } else {
            pitch
        };
    }
    if let Some(look) = teleport.look_at {
        let mut from = base.position;
        if look.from == crate::command::args::Anchor::Eyes {
            from[1] += eye_height;
        }
        [base.yaw, base.pitch] = crate::command::coords::look_angles(from, look.point);
    }
}

impl RegionActor {
    /// The step events of a living entity walking on the ground.
    fn entity_footsteps(&mut self, tracked: &TrackedEntity) {
        use crate::world::game_event::{Context, GameEvent, Source, Steps};
        if tracked.entity.living().is_none() {
            return;
        }
        let base = tracked.entity.base();
        if base.kind == EntityType::Player {
            return;
        }
        let (id, position, grounded) = (base.id, base.position, base.on_ground);
        if self
            .ents
            .feature_state
            .get::<Steps>()
            .walk(id, position, grounded)
        {
            crate::world::region_api::RegionWorld::game_event(
                self,
                GameEvent::Step,
                position,
                Context::from(Source::entity(id)),
            );
        }
    }
}

fn summary_slots(summaries: &[EntitySummary]) -> HashMap<i32, usize> {
    summaries
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect()
}

fn rider_vehicles(entities: &HashMap<i32, TrackedEntity>) -> HashMap<PlayerId, i32> {
    entities
        .iter()
        .flat_map(|(id, t)| t.entity.base().riders.iter().map(move |r| (r.id, *id)))
        .collect()
}

fn player_vehicle(
    vehicles: &HashMap<PlayerId, i32>,
    entities: &HashMap<i32, TrackedEntity>,
    player: PlayerId,
) -> Option<i32> {
    vehicles.get(&player).copied().filter(|vehicle| {
        entities
            .get(vehicle)
            .is_some_and(|t| t.entity.base().riders.iter().any(|r| r.id == player))
    })
}

#[cfg(test)]
mod rider_lookup_tests {
    use super::*;
    use crate::world::messages::PlayerHandle;
    use tuff_vanilla_data::registry::EntityType;

    fn cow(id: i32) -> TrackedEntity {
        let nbt = Compound::new().with("Pos", Tag::List([0.0f64; 3].map(Tag::Double).to_vec()));
        let inert = crate::world::inert_entity::Inert::load(EntityType::Cow, id, &nbt).unwrap();
        TrackedEntity::new(Box::new(inert), 0)
    }

    #[tuff_core::watchdog]
    #[test]
    fn riders_resolve_after_mounting_and_dismounting_mid_tick() {
        let (handle, _events) = PlayerHandle::new(9);
        let rider = crate::world::entity::Rider {
            id: handle.id,
            uuid: 9,
            handle,
        };
        let mut entities: HashMap<i32, TrackedEntity> = [(1, cow(1)), (2, cow(2))].into();
        let mut vehicles = rider_vehicles(&entities);
        assert_eq!(player_vehicle(&vehicles, &entities, 9), None);
        entities
            .get_mut(&1)
            .unwrap()
            .entity
            .base_mut()
            .riders
            .push(rider.clone());
        vehicles = rider_vehicles(&entities);
        assert_eq!(player_vehicle(&vehicles, &entities, 9), Some(1));
        let ticking = entities.remove(&1).unwrap();
        assert_eq!(player_vehicle(&vehicles, &entities, 9), None);
        entities.insert(1, ticking);
        assert_eq!(player_vehicle(&vehicles, &entities, 9), Some(1));
        entities
            .get_mut(&1)
            .unwrap()
            .entity
            .base_mut()
            .riders
            .clear();
        assert_eq!(player_vehicle(&vehicles, &entities, 9), None);
        entities
            .get_mut(&2)
            .unwrap()
            .entity
            .base_mut()
            .riders
            .push(rider);
        vehicles = rider_vehicles(&entities);
        assert_eq!(player_vehicle(&vehicles, &entities, 9), Some(2));
    }
}

/// Burns the block an entity that is dropped outside of any region held.
fn burn_held(mut entity: Box<dyn RegionEntity>) {
    if let Some(block) = entity.take_held() {
        block.burn(crate::world::block_ledger::BlockBurn::Orphaned);
    }
}

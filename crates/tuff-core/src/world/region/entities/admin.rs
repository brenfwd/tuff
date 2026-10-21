use super::*;

impl RegionActor {
    pub(in crate::world::region) fn entity_snapshots(&self, nbt: bool) -> Vec<EntitySnapshot> {
        self.ents
            .entities
            .values()
            .map(|tracked| self.entity_snapshot(tracked, nbt))
            .collect()
    }

    pub(in crate::world::region) fn entity_snapshot(
        &self,
        tracked: &TrackedEntity,
        nbt: bool,
    ) -> EntitySnapshot {
        let entity = &*tracked.entity;
        let base = entity.base();
        EntitySnapshot {
            id: base.id,
            uuid: base.uuid,
            kind: base.kind,
            name: base
                .custom_name
                .as_ref()
                .and_then(TextComponent::from_nbt)
                .map_or_else(|| entity.display_name(), |name| name.plain()),
            dimension: self.dimension,
            position: base.position,
            bounds: entity.bounds(),
            experience: entity.experience(),
            owner_name: entity
                .living()
                .and_then(|l| l.owner)
                .and_then(|owner| self.ents.presence.values().find(|p| p.uuid == owner))
                .map(|p| p.name.to_string()),
            tags: base.tags.clone(),
            registered: (entity.saved_id() != base.kind.name())
                .then(|| entity.saved_id().to_owned()),
            nbt: nbt.then(|| {
                let mut saved = crate::world::entity::save(entity);
                saved.remove("id");
                saved
            }),
        }
    }

    /// Loads an entity from its saved form at `position`, as `/summon`
    /// does; a `fresh` one is set up like a newly spawned mob.
    pub(in crate::world::region) fn summon(
        &mut self,
        mut nbt: Compound,
        position: [f64; 3],
        fresh: bool,
    ) -> bool {
        nbt.insert("Pos", Tag::List(position.map(Tag::Double).to_vec()));
        let id = self.shared.next_entity_id();
        let Some(mut entity) = self.shared.entity_types.load_or_inert(&nbt, id) else {
            return false;
        };
        let uuid = entity.base().uuid;
        if self
            .ents
            .entities
            .values()
            .any(|t| t.entity.base().uuid == uuid)
        {
            return false;
        }
        if fresh {
            let summaries = self.entity_summaries();
            let mut budget = 0;
            let ((), effects) = self.with_context(&summaries, &mut budget, |cx| {
                entity.finalize_spawn(cx, 0);
            });
            let cause = BlockCause::entity(&*entity);
            let chunk = entity.base().chunk();
            self.with_cause(cause, |region| region.apply_effects(id, chunk, effects));
        }
        self.receive_entity(entity, 0);
        true
    }

    /// `/kill` on one entity: gone at once, or dying its own way. False if
    /// there is no such entity here.
    /// `/effect` on one entity other than a player. False if there is no
    /// such entity here or nothing changed.
    pub(in crate::world::region) fn effect_entity(
        &mut self,
        id: i32,
        command: &EffectCommand,
    ) -> bool {
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return false;
        };
        match command {
            EffectCommand::Give(effect) => tracked.entity.give_effect(effect.clone()),
            EffectCommand::Clear(effect) => tracked.entity.clear_effects(*effect),
        }
    }

    /// `/enchant` on one entity; `None` if there is no such entity here.
    pub(in crate::world::region) fn enchant_entity(
        &mut self,
        id: i32,
        enchantment: &str,
        level: i32,
    ) -> Option<crate::world::messages::EnchantAnswer> {
        use crate::world::messages::{EnchantAnswer, enchant_stack};
        let tracked = self.ents.entities.get_mut(&id)?;
        let Some(held) = tracked.entity.main_hand_item() else {
            return Some(EnchantAnswer::NotLiving);
        };
        let Some(stack) = held else {
            return Some(EnchantAnswer::Itemless);
        };
        Some(match enchant_stack(&stack, enchantment, level) {
            Ok(updated) => {
                tracked.entity.set_main_hand_item(Some(updated));
                EnchantAnswer::Done
            }
            Err(answer) => answer,
        })
    }

    /// `/swing` on one entity; `None` if there is no such entity here.
    pub(in crate::world::region) fn swing_entity(
        &mut self,
        id: i32,
        hand: i32,
        kind: tuff_item::item::SwingKind,
        duration: i32,
    ) -> Option<bool> {
        let tracked = self.ents.entities.get(&id)?;
        if tracked.entity.main_hand_item().is_none() {
            return Some(false);
        }
        let chunk = tracked.chunk();
        let swing = framed(&play::SwingAnimation {
            entity_id: id,
            hand,
            kind,
            duration_ticks: duration,
        });
        self.apply_effects(id, chunk, vec![Effect::EntityPackets(packets([swing]))]);
        Some(true)
    }

    /// `/debugpath` on one entity; `None` if there is no such entity here.
    pub(in crate::world::region) fn debug_path_entity(
        &mut self,
        id: i32,
        target: tuff_world_model::BlockPos,
    ) -> Option<crate::world::entity::DebugPath> {
        let mut tracked = self.ents.entities.remove(&id)?;
        let summaries = self.entity_summaries();
        let mut budget = u32::MAX;
        let (answer, _) = self.with_context(&summaries, &mut budget, |cx| {
            tracked.entity.debug_path(target, cx)
        });
        self.ents.entities.insert(id, tracked);
        Some(answer)
    }

    /// `/attribute` on one entity; `None` if there is no such entity here.
    pub(in crate::world::region) fn attribute_entity(
        &mut self,
        id: i32,
        command: &crate::world::messages::AttributeCommand,
    ) -> Option<crate::world::messages::AttributeAnswer> {
        let mut tracked = self.ents.entities.remove(&id)?;
        let answer = tracked.entity.edit_attribute(command);
        self.send_attributes(&mut tracked);
        self.ents.entities.insert(id, tracked);
        Some(answer)
    }

    /// `/tag` on one entity; false if there is no such entity here or
    /// nothing changed.
    pub(in crate::world::region) fn edit_entity_tags(
        &mut self,
        id: i32,
        edit: &crate::world::messages::TagEdit,
    ) -> bool {
        use crate::world::messages::TagEdit;
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return false;
        };
        let tags = &mut tracked.entity.base_mut().tags;
        match edit {
            TagEdit::Add(name) => {
                if tags.len() >= crate::storage::player_data::MAX_TAGS || tags.contains(name) {
                    return false;
                }
                tags.push(name.clone());
                true
            }
            TagEdit::Remove(name) => {
                let before = tags.len();
                tags.retain(|tag| tag != name);
                tags.len() != before
            }
        }
    }

    /// `/data` on one entity: replaces it with one loaded from `nbt`, with
    /// the same network id (and, through `nbt`, the same UUID). If `nbt`
    /// does not load, the entity stays as it was.
    pub(in crate::world::region) fn replace_entity(&mut self, id: i32, nbt: Compound) -> bool {
        let Some(entity) = self.shared.entity_types.load(&nbt, id) else {
            return false;
        };
        self.chunk_io.pois.forget_holder(id);
        if self.remove_entity_kept(id, &[]).is_none() {
            return false;
        }
        self.receive_entity(entity, 0);
        true
    }

    pub(in crate::world::region) fn hurt_entity(
        &mut self,
        id: i32,
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        amount: f32,
        culprit: Option<crate::world::entity::Culprit>,
    ) -> bool {
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        let bypasses = crate::world::damage_type_in_tag(kind, "minecraft:bypasses_invulnerability");
        let blocked =
            (tracked.entity.base().invulnerable && !bypasses) || tracked.entity.resists(kind);
        let gone = !blocked && tracked.entity.hurt_blamed(amount, kind, culprit.as_ref());
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(id, tracked);
        if gone {
            self.remove_entity(id, &[]);
        }
        !blocked
    }

    pub(in crate::world::region) fn strike_entity(&mut self, id: i32, victim: u128) -> bool {
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (struck, effects) = self.with_context(&summaries, &mut budget, |cx| {
            tracked.entity.melee_strike(victim, cx)
        });
        let chunk = tracked.chunk();
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(id, tracked);
        self.apply_effects(id, chunk, effects);
        struck
    }

    pub(in crate::world::region) fn kill_entity(&mut self, id: i32) -> bool {
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (gone, effects) =
            self.with_context(&summaries, &mut budget, |cx| tracked.entity.kill(cx));
        let chunk = tracked.chunk();
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(id, tracked);
        self.apply_effects(id, chunk, effects);
        if gone {
            self.remove_entity(id, &[]);
        }
        true
    }

    /// `/tp` of one entity to another dimension: removes it, placed and
    /// turned as `teleport` says, and gives its saved form. `None` if there
    /// is no such entity here.
    pub(in crate::world::region) fn take_teleported_entity(
        &mut self,
        id: i32,
        teleport: &Teleport,
    ) -> Option<Compound> {
        self.chunk_io.pois.forget_holder(id);
        let mut tracked = self.remove_entity_kept(id, &[])?;
        place_teleported(&mut *tracked.entity, teleport);
        Some(crate::world::entity::save(&*tracked.entity))
    }

    /// Places one entity at `position` (and `rotation`) as its own step
    /// would: its motion is cleared and the next movement report sends the
    /// change as relative moves rather than a teleport. A move into another
    /// chunk is sent as a full report; one out of the region is refused.
    pub(in crate::world::region) fn glide_entity(
        &mut self,
        id: i32,
        position: [f64; 3],
        rotation: Option<[f32; 2]>,
    ) -> bool {
        if position.iter().any(|v| !v.is_finite()) {
            return false;
        }
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        let from = tracked.chunk();
        let before = tracked.entity.base().position;
        let base = tracked.entity.base_mut();
        base.position = position;
        base.velocity = [0.0; 3];
        if let Some([yaw, pitch]) = rotation {
            base.yaw = yaw;
            base.pitch = pitch;
        }
        let to = tracked.chunk();
        if to != from {
            let refused = !self.pos.contains(to)
                || (!self.columns.contains_key(&to) && !self.border.halo.contains_key(&to));
            if refused {
                let base = tracked.entity.base_mut();
                base.position = before;
                self.ents.entities.insert(id, tracked);
                return false;
            }
            if let Some(column) = self.columns.get_mut(&from) {
                column.entities_dirty = true;
            }
            if let Some(column) = self.columns.get_mut(&to) {
                column.entities_dirty = true;
            }
            let track = tracked.broadcast(self.dimension, to);
            self.send_to_viewers(&[from, to], || PlayerEvent::Track(track.clone()));
        }
        self.ents.entities.insert(id, tracked);
        true
    }

    /// `/tp` on one entity other than a player: sets its position and, if
    /// given, rotation, resets its vertical velocity and puts it on the
    /// ground, and hands it off across a region border like a normal move.
    /// False if there is no such entity here, `teleport.dimension` is a
    /// different dimension, or a cross-region handoff could not land (the
    /// entity stays where it was).
    pub(in crate::world::region) fn teleport_entity(
        &mut self,
        id: i32,
        teleport: &Teleport,
    ) -> bool {
        if teleport.dimension != self.dimension {
            return false;
        }
        let Some(mut tracked) = self.ents.entities.remove(&id) else {
            return false;
        };
        let from = tracked.chunk();
        let before = tracked.entity.base().position;
        place_teleported(&mut *tracked.entity, teleport);
        let mut to = tracked.chunk();
        if to != from && !self.columns.contains_key(&to) && !self.border.halo.contains_key(&to) {
            let base = tracked.entity.base_mut();
            base.position = before;
            base.velocity = [0.0; 3];
            to = from;
        }
        if let Some(column) = self.columns.get_mut(&from) {
            column.entities_dirty = true;
        }
        let track = tracked.broadcast(self.dimension, to);
        self.send_to_viewers(&[from, to], || PlayerEvent::Track(track.clone()));
        if !self.pos.contains(to) {
            let seq = tracked.seq;
            let handed = match self.border.neighbors.get(&to.region()) {
                Some(neighbor) => neighbor.try_entity(tracked.entity, seq),
                None => Err(tracked.entity),
            };
            let Err(mut entity) = handed else {
                if let Some(neighbor) = self.border.neighbors.get(&to.region()).cloned() {
                    self.hand_off_viewers(id, &neighbor);
                }
                return true;
            };
            let base = entity.base_mut();
            base.position = before;
            base.velocity = [0.0; 3];
            tracked.entity = entity;
            self.ents.entities.insert(id, tracked);
            return false;
        } else if to != from
            && let Some(column) = self.columns.get_mut(&to)
        {
            column.entities_dirty = true;
        }
        self.ents.entities.insert(id, tracked);
        true
    }
}

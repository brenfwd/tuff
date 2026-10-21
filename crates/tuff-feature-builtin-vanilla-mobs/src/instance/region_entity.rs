use super::*;
use tuff_core::world::entity_slots::{ContainerSlots, ContainerSlotsMut};

impl<S: Species> RegionEntity for Mob<S> {
    fn base(&self) -> &EntityBase {
        &self.core.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.core.base
    }

    fn boss_bar(&self) -> Option<tuff_core::world::entity::BossBarState> {
        self.species.boss_bar(&self.core)
    }

    fn track_shape(&self) -> Option<tuff_core::world::messages::Shape> {
        self.species.track_shape(&self.core)
    }

    fn facts(&self) -> tuff_core::world::predicate::EntityFacts {
        self.core.facts()
    }

    fn sits_on_chests(&self) -> bool {
        self.core.base.kind == EntityType::Cat && self.core.tame.as_ref().is_some_and(|t| t.pose)
    }

    fn attack_target(&self) -> Option<i32> {
        self.core.target
    }

    fn debug_value(
        &self,
        feed: tuff_vanilla_data::registry::DebugSubscription,
        tick: u64,
    ) -> Option<tuff_core::protocol::play::DebugValue> {
        use tuff_core::protocol::play::{DebugPath, DebugValue};
        use tuff_vanilla_data::registry::DebugSubscription as Feed;
        if self.core.not_a_mob {
            return None;
        }
        match feed {
            Feed::EntityPaths => {
                let path = self.core.navigation.path.as_ref()?;
                let debug = path.debug.as_ref()?;
                if debug.targets.is_empty() {
                    return None;
                }
                Some(DebugValue::Path(Box::new(DebugPath {
                    reached: path.reached,
                    next_node: path.next as i32,
                    target: path.target,
                    nodes: debug.nodes.clone(),
                    open_set: debug.open_set.clone(),
                    closed_set: debug.closed_set.clone(),
                    targets: debug.targets.clone(),
                    max_node_distance: self.core.navigation.max_distance_to_waypoint(),
                })))
            }
            Feed::GoalSelectors => Some(DebugValue::Goals(self.core.goals.debug_goals())),
            Feed::GameEventListeners
                if matches!(self.core.base.kind, EntityType::Warden | EntityType::Allay) =>
            {
                Some(DebugValue::GameEventListener { radius: 16 })
            }
            Feed::Brains if crate::brain_debug::has_brain(self.core.base.kind) => self
                .species
                .debug_value(&self.core, feed, tick)
                .or_else(|| {
                    let behaviors = self
                        .core
                        .goals
                        .debug_goals()
                        .into_iter()
                        .filter(|g| g.running)
                        .map(|g| g.name)
                        .collect();
                    Some(crate::brain_debug::dump(
                        &self.core,
                        self.species.brain_state(&self.core),
                        behaviors,
                        tick,
                    ))
                }),
            _ => self.species.debug_value(&self.core, feed, tick),
        }
    }

    fn save(&self, out: &mut Compound) {
        let core = &self.core;
        out.insert("Health", core.health);
        out.insert("HurtTime", core.hurt_time);
        out.insert("DeathTime", core.death_time);
        out.insert("AbsorptionAmount", core.absorption);
        if !core.effects.is_empty() {
            out.insert("active_effects", core.effects.to_nbt());
        }
        let attributes = save_attributes(core);
        if !attributes.is_empty() {
            out.insert("attributes", Tag::List(attributes));
        }
        if let Some(anger) = core.anger {
            out.insert(
                "anger_end_time",
                core.world_tick as i64 + i64::from(anger.ticks),
            );
            out.insert("angry_at", Tag::uuid(anger.target));
        } else if let Some((end, target)) = core.anger_loaded {
            out.insert("anger_end_time", end);
            out.insert("angry_at", Tag::uuid(target));
        }
        if let Some(uuid) = core.last_hurt_by_player {
            out.insert("last_hurt_by_player", Tag::uuid(uuid));
            out.insert(
                "last_hurt_by_player_memory_time",
                core.last_hurt_by_player_time,
            );
        }
        if !core.not_a_mob {
            out.insert("CanPickUpLoot", core.can_pick_up_loot);
            out.insert("PersistenceRequired", core.persistence_required);
            out.insert("LeftHanded", core.left_handed);
            if core.no_ai {
                out.insert("NoAI", true);
            }
        }
        core.equipment.save(out);
        if let Some(leash) = &core.leash {
            leash.save(out);
        }
        if let Some(home) = core.home {
            out.insert("home_radius", home.radius);
            out.insert(
                "home_pos",
                Tag::IntArray(vec![home.pos.x, home.pos.y, home.pos.z]),
            );
        }
        if let Some(raider) = &core.raider {
            crate::raider::save(raider, out);
        }
        if let Some(age) = &core.age {
            out.insert("Age", age.age);
            out.insert("ForcedAge", age.forced_age);
            out.insert("AgeLocked", age.locked);
        }
        if let Some(love) = &core.love {
            out.insert("InLove", love.ticks);
            if let Some(cause) = love.cause {
                out.insert("LoveCause", Tag::uuid(cause));
            }
        }
        if let Some(tame) = &core.tame {
            if let Some(owner) = tame.owner {
                out.insert("Owner", Tag::uuid(owner));
            }
            out.insert("Sitting", tame.sitting);
        }
        self.species.save(core, out);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        self.full_metadata()
    }

    fn bounds(&self) -> [f64; 6] {
        self.core.bounds()
    }

    fn blue_sheep(&self) -> bool {
        self.species.blue_wool()
    }

    fn head_yaw(&self) -> f32 {
        self.core.head_yaw
    }

    fn waypoint_transmit_range(&self) -> f64 {
        self.core
            .attribute(tuff_vanilla_data::registry::Attribute::WaypointTransmitRange)
    }

    fn living(&self) -> Option<LivingSummary> {
        Some(self.core.summary())
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let changed = std::mem::take(&mut self.core.changed);
        if changed.is_empty() {
            return Vec::new();
        }
        let species = self.species.metadata(&self.core);
        changed
            .into_iter()
            .filter_map(|index| {
                species
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map(|(i, v)| (*i, v.clone()))
                    .or_else(|| self.core.core_metadata(index).map(|v| (index, v)))
                    .or_else(|| {
                        self.species
                            .changed_metadata(&self.core, index)
                            .map(|v| (index, v))
                    })
            })
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.core.changed(index);
    }

    fn hurt(&mut self, amount: f32, damage_type: &'static DamageType) -> bool {
        self.core.pending_hurts.push((amount, damage_type));
        false
    }

    fn hurt_blamed(
        &mut self,
        amount: f32,
        damage_type: &'static DamageType,
        culprit: Option<&tuff_core::world::entity::Culprit>,
    ) -> bool {
        match culprit {
            Some(culprit) => {
                self.core
                    .pending_blamed_hurts
                    .push((amount, damage_type, culprit.clone()))
            }
            None => self.core.pending_hurts.push((amount, damage_type)),
        }
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        if !self.core.is_alive() {
            return None;
        }
        let damage = Damage {
            kind: &damage_type::PLAYER_ATTACK,
            player: Some((attack.attacker, attack.attacker_uuid)),
            attacker: Some((attack.attacker, EntityType::Player)),
            attacker_charged: None,
            attacker_name: None,
            projectile: None,
            projectile_id: None,
            position: Some(attack.attacker_position),
            weapon: attack.weapon.clone(),
            aim: Some(crate::Aim {
                eye: [
                    attack.attacker_position[0],
                    attack.attacker_position[1] + PLAYER_EYE_HEIGHT,
                    attack.attacker_position[2],
                ],
                look: tuff_core::world::raycast::view_vector(
                    attack.attacker_yaw,
                    attack.attacker_pitch,
                ),
            }),
            burning_projectile: false,
        };
        let before = self.core.health;
        let velocity = self.core.base.velocity;
        let hurt = self.hurt_by(&damage, attack.damage, cx);
        if !hurt {
            return Some(AttackOutcome {
                hurt: false,
                health_lost: 0.0,
            });
        }
        let (dx, dz) = combat::facing_knockback_direction(attack.attacker_yaw);
        let extra = crate::Knockback {
            power: f64::from(attack.knockback),
            dx,
            dz,
            damage: attack.damage,
            extra: true,
        };
        if attack.knockback > 0.0
            && !self.core.no_knockback
            && !self.species.knockback(&mut self.core, &extra, &damage, cx)
        {
            let resistance = self.core.attribute(Attribute::KnockbackResistance);
            let _ = velocity;
            self.core.base.velocity = combat::knockback(
                self.core.base.velocity,
                attack.knockback as f64,
                dx,
                dz,
                self.core.base.on_ground,
                resistance,
            );
        }
        Some(AttackOutcome {
            hurt: true,
            health_lost: before - self.core.health,
        })
    }

    fn hit(&mut self, hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        if !self.core.is_alive() {
            return None;
        }
        let before = self.core.health;
        let damage = Damage::from_hit(hit);
        if !self.hurt_by(&damage, hit.amount, cx) {
            return Some(AttackOutcome {
                hurt: false,
                health_lost: 0.0,
            });
        }
        if let Some((strength, dx, dz)) = hit.knockback.filter(|_| !self.core.no_knockback)
            && !self.species.knockback(
                &mut self.core,
                &crate::Knockback {
                    power: strength,
                    dx,
                    dz,
                    damage: hit.amount,
                    extra: true,
                },
                &damage,
                cx,
            )
        {
            let resistance = self.core.attribute(Attribute::KnockbackResistance);
            self.core.base.velocity = combat::knockback(
                self.core.base.velocity,
                strength,
                dx,
                dz,
                self.core.base.on_ground,
                resistance,
            );
        }
        if let Some([x, y, z]) = hit.push.filter(|_| !self.core.no_knockback) {
            let resistance = self.core.attribute(Attribute::KnockbackResistance);
            let v = &mut self.core.base.velocity;
            *v = match hit.push_power {
                Some(power) => {
                    let keep = ((power - resistance) / power).max(0.0);
                    [v[0] + x * keep, v[1] + y * keep, v[2] + z * keep]
                }
                None => {
                    let keep = (1.0 - resistance).max(0.0);
                    [v[0] + x * keep, v[1] + y, v[2] + z * keep]
                }
            };
            self.core.impulse = true;
        }
        if hit.ignite > 0.0 && !self.core.base.kind.size().fire_immune {
            let ticks = (hit.ignite * 20.0).floor() as i16;
            self.core.base.fire = self.core.base.fire.max(ticks);
        }
        if hit.sticks {
            self.core.stuck_arrows.count += 1;
            self.core
                .changed(tuff_core::protocol::play::metadata::ARROW_COUNT);
        }
        for effect in &hit.effects {
            self.give_effect(effect.clone());
        }
        Some(AttackOutcome {
            hurt: true,
            health_lost: before - self.core.health,
        })
    }

    fn equipment(&self) -> Vec<(u8, Option<tuff_item::item::ItemView<'_>>)> {
        self.core.equipment.visible()
    }

    fn main_hand_item(&self) -> Option<Option<tuff_item::item::ItemView<'_>>> {
        Some(self.core.equipment.main_hand().map(ItemStack::view))
    }

    fn replace_main_hand_item(&mut self, stack: Option<ItemStack>) -> Option<ItemStack> {
        self.core
            .equipment
            .replace(tuff_vanilla_data::item::EquipmentSlot::Mainhand, stack)
    }

    fn take_equipment_changed(&mut self) -> bool {
        self.core.equipment.take_changed()
    }

    fn shear(&mut self, cx: &mut TickContext) -> bool {
        self.core.is_alive() && self.species.shear(&mut self.core, cx)
    }

    fn dispensed_on(
        &mut self,
        stack: &ItemStack,
        cx: &mut TickContext,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        if !self.core.is_alive() {
            return None;
        }
        if let Some(outcome) = self.species.dispensed_on(&mut self.core, stack, cx) {
            return Some(outcome);
        }
        crate::equipment::dispense_equip(&mut self.core, stack, cx)
    }

    fn interact(
        &mut self,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if !self.core.is_alive() {
            return None;
        }
        if let Some(effect) = self.shear_equipment(interaction, cx) {
            return Some(effect);
        }
        if let Some(effect) = self.species.interact(&mut self.core, interaction, cx) {
            return Some(effect);
        }
        if let Some(effect) = animal::interact(self, interaction, cx) {
            return Some(effect);
        }
        let stack = interaction.stack.as_deref()?;
        let name = stack.components.custom_name.as_ref()?;
        if stack.item != tuff_vanilla_data::Item::NameTag {
            return None;
        }
        self.core.base.custom_name = Some(name.to_nbt());
        self.core.persistence_required = true;
        self.core
            .changed(tuff_core::protocol::play::metadata::CUSTOM_NAME);
        Some(tuff_core::world::messages::HandEffect::Consume(1))
    }

    fn took_item(&mut self, stack: ItemStack) {
        if crate::raider::took_banner(&mut self.core, &stack) {
            stack.burn(tuff_item::item::BurnReason::Transform);
            return;
        }
        self.species.took_item(&mut self.core, stack);
    }

    fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Discard | Notice::Reel | Notice::Struck => {}
            Notice::Bred => {
                if let Some(age) = &mut self.core.age {
                    age.set(animal::PARENT_AGE_AFTER_BREEDING);
                }
                if let Some(love) = &mut self.core.love {
                    love.reset();
                }
            }
            Notice::Alert { attacker } => {
                if self.core.is_alive() && self.core.target.is_none() {
                    self.core.target = Some(attacker);
                }
            }
            Notice::UniversalAnger { attacker } => {
                if self.core.is_alive() {
                    self.core.stop_being_angry();
                    self.core.anger = Some(crate::target::Anger {
                        target: attacker,
                        ticks: 400,
                        universal: true,
                    });
                }
            }
            Notice::HitLanded => self.core.landed_hits += 1,
            Notice::PlayerDied { player, nearby } => self.core.player_died(player, nearby),
            Notice::Courted { .. }
            | Notice::Charged
            | Notice::Rehydrate
            | Notice::BellRung
            | Notice::Jockey
            | Notice::Retreat { .. }
            | Notice::ShieldBlocked { .. }
            | Notice::Gifted(_)
            | Notice::Wololo
            | Notice::Heard { .. } => self.species.heard(&mut self.core, notice),
        }
    }

    fn trade(
        &mut self,
        request: &crate::world::trading::TradeRequest,
        cx: &mut TickContext,
    ) -> Option<bool> {
        if !self.species.is_merchant() {
            return None;
        }
        Some(self.species.trade(&mut self.core, request, cx))
    }

    fn stop_trading(&mut self, player: u128) {
        self.species.stop_trading(&mut self.core, player);
    }

    fn poi_claims(&self) -> Vec<BlockPos> {
        self.species.poi_claims(&self.core)
    }

    fn sleeping_in(&self) -> Option<BlockPos> {
        self.species.sleeping_in(&self.core)
    }

    fn wake_from_bed(&mut self, cx: &mut TickContext) {
        self.species.wake_from_bed(&mut self.core, cx);
    }

    fn add_effects(&mut self, effects: &[tuff_core::world::effects::ActiveEffect]) {
        for effect in effects {
            self.core.add_effect(effect.clone());
        }
    }

    fn synced_attributes(&self) -> Vec<(Attribute, f64)> {
        self.core.synced_attributes()
    }

    fn edit_attribute(
        &mut self,
        command: &tuff_core::world::messages::AttributeCommand,
    ) -> tuff_core::world::messages::AttributeAnswer {
        self.core.edit_attribute(command)
    }

    fn debug_path(
        &mut self,
        target: tuff_world_model::BlockPos,
        cx: &mut TickContext,
    ) -> tuff_core::world::entity::DebugPath {
        self.core.debug_path(target, cx)
    }

    fn give_effect(&mut self, effect: tuff_core::world::effects::ActiveEffect) -> bool {
        if !self.core.is_alive() {
            return false;
        }
        if let Some(action) = effects::instant_action(effect.effect, effect.amplifier, 1.0) {
            self.core.pending_effect_actions.push(action);
            return true;
        }
        self.core.add_effect(effect)
    }

    fn clear_effects(&mut self, effect: Option<MobEffect>) -> bool {
        let changes = match effect {
            None => self.core.effects.clear(),
            Some(effect) => self.core.effects.remove(effect).into_iter().collect(),
        };
        let affected = !changes.is_empty();
        self.core.pending_effects.extend(changes);
        affected
    }

    fn breed(
        &mut self,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        S::breed_with(self, partner, cx)
    }

    fn finalize_spawn(&mut self, cx: &mut TickContext, group_size: u32) {
        let mut group = cx.state.get::<crate::spawning::SpawnGroup>();
        if group_size == 0 {
            *group = crate::spawning::SpawnGroup::default();
        }
        group.index = group_size;
        drop(group);
        self.core.left_handed = cx.rng.r#gen::<f32>() < 0.05;
        crate::raider::finalize_spawn(&mut self.core, cx);
        self.species.finalize_spawn(&mut self.core, cx);
        if S::RANDOM_BABIES
            && self.core.age.is_some()
            && group_size > 0
            && cx.rng.r#gen::<f32>() <= 0.05
        {
            self.core.set_baby(true);
        }
        self.core.changed.clear();
        self.core.equipment.take_changed();
    }

    fn melee_strike(&mut self, victim: u128, cx: &mut TickContext) -> bool {
        self.core.is_alive() && self.species.melee_strike(&mut self.core, victim, cx)
    }

    fn kill(&mut self, cx: &mut TickContext) -> bool {
        if !self.core.is_alive() {
            return false;
        }
        self.hurt_by(&Damage::of(&damage_type::GENERIC_KILL), f32::MAX, cx);
        !self.core.dead
    }

    fn take_position_sync(&mut self) -> bool {
        std::mem::take(&mut self.core.position_sync)
    }

    fn knocked(&mut self, power: f32, dx: f64, dz: f64) {
        if !self.core.is_alive() || self.core.no_knockback {
            return;
        }
        let resistance = self.core.attribute(Attribute::KnockbackResistance);
        self.core.base.velocity = combat::knockback(
            self.core.base.velocity,
            f64::from(power),
            dx,
            dz,
            self.core.base.on_ground,
            resistance,
        );
        self.core.impulse = true;
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.core.impulse)
    }

    fn block_changed(&mut self, pos: BlockPos) {
        if self
            .core
            .navigation
            .should_recompute(pos, self.core.base.position)
        {
            self.core.recompute_requested = true;
        }
    }

    fn is_persistent(&self) -> bool {
        self.core.persistence_required
            || self.core.leash.is_some()
            || !self.species.remove_when_far_away()
            || self.core.raider.as_ref().is_some_and(|r| r.raid.is_some())
    }

    fn persistence_required(&self) -> bool {
        self.core.persistence_required
            || self.core.base.vehicle.is_some()
            || self.core.leash.is_some()
            || self.core.raider.as_ref().is_some_and(|r| r.raid.is_some())
            || self.species.custom_persistence(&self.core)
    }

    fn leash(&self) -> Option<&tuff_core::world::leash::Leash> {
        self.core.leash.as_ref()
    }

    fn leash_mut(&mut self) -> Option<&mut Option<tuff_core::world::leash::Leash>> {
        (!self.core.not_a_mob).then_some(&mut self.core.leash)
    }

    fn leash_profile(&self) -> Option<tuff_core::world::leash::LeashProfile> {
        if self.core.not_a_mob {
            return None;
        }
        self.species.leash_profile(&self.core)
    }

    fn seats(&self) -> usize {
        self.species.seats(&self.core)
    }

    fn dismount_location(
        &self,
        width: f64,
        yaw: f32,
        block: &dyn Fn(tuff_world_model::BlockPos) -> Option<tuff_vanilla_data::BlockState>,
    ) -> [f64; 3] {
        let bounds = self.bounds();
        if self.core.base.kind == EntityType::Strider {
            return tuff_vanilla_common::dismount::lava_dismount(
                self.core.base.position,
                bounds,
                width,
                yaw,
                block,
            );
        }
        let [x, _, z] = self.core.base.position;
        [x, bounds[4], z]
    }

    fn seat(&self, index: usize, _riders: usize) -> [f64; 3] {
        let seats = self.core.base.kind.passenger_attachments();
        let seat = seats[index.min(seats.len() - 1)];
        tuff_core::world::entity::turned(
            seat.map(|c| c * self.core.size_ratio()),
            self.core.base.yaw,
        )
    }

    fn vehicle_attachment(&self) -> [f64; 3] {
        let point = self.core.base.kind.vehicle_attachment();
        tuff_core::world::entity::turned(
            point.map(|c| c * self.core.size_ratio()),
            self.core.base.yaw,
        )
    }

    fn accepts_rider(&self) -> bool {
        self.species.accepts_rider(&self.core)
    }

    fn rideable_by_input(&self) -> bool {
        self.species.drivable(&self.core)
    }

    fn flying_vehicle(&self) -> bool {
        self.species.flying_vehicle(&self.core)
    }

    fn controls_movement(&self) -> bool {
        self.species.drivable(&self.core)
            && self
                .core
                .rider_input
                .is_none_or(|input| self.species.controlled_by(&self.core, &input))
    }

    fn steer_with_input(&mut self, input: RiderInput) {
        self.core.rider_input = Some(input);
    }

    fn ridden_speed(&self) -> Option<tuff_core::world::entity::RiddenSpeed> {
        self.species
            .drivable(&self.core)
            .then(|| self.species.ridden_speed(&self.core))
            .flatten()
    }

    fn clamp_driven_move(&mut self, reported: [f64; 3]) -> [f64; 3] {
        self.core.clamp_to_shadow(reported)
    }

    fn on_mounted(&mut self) {
        self.core.rider_input = None;
        self.core.shadow = None;
    }

    fn crowd(&mut self) {
        self.core.crowd_marked = true;
    }

    fn shulker_box_pushed(&mut self) {
        self.species.shulker_box_pushed(&mut self.core);
    }

    fn boost(&mut self, roll: u32) -> bool {
        self.species.boost(&mut self.core, roll)
    }

    fn mount_refused_by(&self, stack: Option<&ItemStack>) -> bool {
        let shears = stack.is_some_and(|s| s.item == tuff_vanilla_data::Item::Shears)
            && SHEARABLE_SLOTS
                .iter()
                .any(|slot| shearable(self.core.equipment.get(*slot), true));
        shears || self.species.mount_refused(&self.core, stack)
    }

    fn container(&self) -> Option<ContainerSlots<'_>> {
        let (chest, _) = self.species.inventory()?;
        Some(ContainerSlots::with_head(
            self.core.equipment.mount_slots(),
            chest,
        ))
    }

    fn container_mut(&mut self) -> Option<ContainerSlotsMut<'_>> {
        let chest = self.species.inventory_mut()?;
        Some(ContainerSlotsMut::with_head(
            self.core.equipment.mount_slots_mut(),
            chest,
        ))
    }

    fn take_items(&mut self) -> Vec<ItemStack> {
        let mut items = self.species.take_items();
        items.append(&mut self.core.piglin.inventory);
        items.extend(self.core.equipment.take_all());
        items
    }

    fn held_items(&self) -> Option<Vec<Option<tuff_item::item::ItemView<'_>>>> {
        Some(
            self.core
                .equipment
                .by_number()
                .into_iter()
                .map(|stack| stack.map(ItemStack::view))
                .collect(),
        )
    }

    fn set_held_items(&mut self, slots: Vec<(usize, Option<ItemStack>)>) -> bool {
        let mut any = false;
        for (slot, stack) in slots {
            let replaced = match self.core.equipment.replace_number(slot, stack) {
                Ok(old) => {
                    any = true;
                    old
                }
                Err(unused) => unused,
            };
            if let Some(gone) = replaced {
                gone.burn(tuff_item::item::BurnReason::Plugin);
            }
        }
        any
    }

    fn container_menu(&self) -> tuff_core::world::menu::MenuKind {
        self.species.inventory().map_or(
            tuff_core::world::menu::MenuKind::Generic { rows: 0 },
            |(_, kind)| kind,
        )
    }

    fn container_opens_for(&self, player: i32, sneaking: bool) -> bool {
        self.species
            .inventory_opens_for(&self.core, player, sneaking)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.core.min_y = cx.min_y;
        self.core.max_y = cx.max_y;
        if self.check_despawn(cx) {
            return Tick::Discard;
        }
        self.core.world_tick = cx.tick;
        self.core.crowded = std::mem::take(&mut self.core.crowd_marked);
        if let Some((end, target)) = self.core.anger_loaded.take() {
            self.core.restore_anger(end, target, cx);
        }
        self.core.tick_count += 1;
        if self.core.stuck_arrows.tick() {
            self.core.changed(play::metadata::ARROW_COUNT);
        }
        self.core.body.previous = self.core.base.position;
        for (amount, kind) in std::mem::take(&mut self.core.pending_hurts) {
            if self.hurt_by(&Damage::of(kind), amount, cx) && kind.name == damage_type::LAVA.name {
                let pitch = 2.0 + cx.rng.r#gen::<f32>() * 0.4;
                self.core
                    .sound(SoundEvent::EntityGenericBurn, 0.4, pitch, cx);
            }
        }
        for (amount, kind, culprit) in std::mem::take(&mut self.core.pending_blamed_hurts) {
            let damage = Damage {
                attacker: Some((culprit.id, culprit.kind)),
                attacker_charged: culprit.charged.clone(),
                attacker_name: Some(culprit.name.clone()),
                ..Damage::of(kind)
            };
            self.hurt_by(&damage, amount, cx);
        }
        if std::mem::take(&mut self.core.recompute_requested) {
            self.core.recompute_path(cx);
        }
        for action in std::mem::take(&mut self.core.pending_effect_actions) {
            self.apply_effect_action(action, cx);
        }
        for change in std::mem::take(&mut self.core.pending_effects) {
            self.apply_effect_change(change, cx);
        }
        if !self.core.effects.is_empty() {
            let age = self.core.tick_count as i64;
            let (actions, changes) = self.core.effects.tick(age, self.core.absorption);
            for action in actions {
                self.apply_effect_action(action, cx);
            }
            for change in changes {
                self.apply_effect_change(change, cx);
            }
        }
        let heard = cx
            .state
            .get::<villager::ReputationMail>()
            .take(self.core.base.id);
        for (event, source) in heard {
            self.species.reputation(&mut self.core, event, source, cx);
        }
        self.species.tick(&mut self.core, cx);
        if self.core.discarded {
            return Tick::Discard;
        }
        if let Some(damage) = self.species.take_death(&mut self.core) {
            self.die(damage, cx);
            if self.core.health > 0.0 {
                self.core.health = 0.0;
                self.core.changed(data::HEALTH);
            }
        }
        self.base_tick(cx);
        self.core.tick_using_item();
        if !self.core.is_alive() {
            match self.species.tick_death(&mut self.core, cx) {
                Some(true) => return Tick::Discard,
                Some(false) => {}
                None => {
                    self.core.death_time += 1;
                    if self.core.death_time >= DEATH_TICKS {
                        self.core.entity_event(event::POOF, cx);
                        crate::death_effects::removed(&self.core, cx);
                        self.species.remove_dead(&mut self.core, cx);
                        return Tick::Discard;
                    }
                }
            }
        }
        self.ai_step(cx);
        self.core.tick_body_rotation();
        if self.core.tick_count.is_multiple_of(5) {
            self.core.update_control_flags();
        }
        self.core.body.first_tick = false;
        if self.core.discarded {
            return Tick::Discard;
        }
        Tick::Alive
    }
}

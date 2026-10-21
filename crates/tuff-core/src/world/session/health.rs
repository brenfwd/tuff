//! Health, damage entry points, equipment, attributes and death.

use super::*;
use crate::protocol::play::{self, GameMode, MetadataValue, metadata};
use crate::world::attributes;
use crate::world::effects::ActiveEffects;
use crate::world::entity::{Culprit, EntityHit, Notice};
use crate::world::inventory::Inventory;
use crate::world::menu::BundleSound;
use crate::world::messages::{
    CoordinatorMessage, ManagerMessage, PlayerId, RegionInternal, RegionRequest,
};
use crate::world::player::{self, Fall};
use crate::world::region_api::{PlayerEffectEvent, PlayerEffectTrigger, RegionTask};
use crate::world::stats::Stat;
use crate::world::{EYE_HEIGHT, block_contact, framed, message_plain, packets};
use rand::Rng;
use tuff_item::item::DupUnchecked;
use tuff_item::item::{Hurt, ItemSnapshot, ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, MobEffect, SoundEvent};
use tuff_vanilla_data::registry::{CustomStat, StatType};

impl PlayerSession {
    /// The active effects and their amplifiers, for attribute values.
    pub fn effect_levels(&self) -> Vec<(MobEffect, u8)> {
        self.body
            .effects
            .iter()
            .map(|effect| (effect.effect, effect.amplifier))
            .collect()
    }

    /// How this player's gear and effects change its mining speed.
    pub fn miner(&self) -> player::Miner {
        let worn = self.worn();
        let effects = self.effect_levels();
        let modifiers = attributes::Modifiers {
            equipment: &worn,
            effects: &effects,
        };
        let haste = [MobEffect::Haste, MobEffect::ConduitPower]
            .into_iter()
            .filter_map(|effect| self.body.effects.amplifier(effect))
            .max();
        player::Miner {
            on_ground: self.pose.on_ground,
            efficiency: modifiers.value(Attribute::MiningEfficiency) as f32,
            haste,
            fatigue: self.body.effects.amplifier(MobEffect::MiningFatigue),
            break_speed: modifiers.value(Attribute::BlockBreakSpeed) as f32,
            submerged_speed: modifiers.value(Attribute::SubmergedMiningSpeed) as f32,
        }
    }

    /// The value of one of this player's attributes.
    pub fn attribute(&self, attribute: Attribute) -> f64 {
        let worn = self.worn();
        let effects = self.effect_levels();
        attributes::Modifiers {
            equipment: &worn,
            effects: &effects,
        }
        .value(attribute)
    }

    /// Damage caused by another player's doing that is not an attack.
    pub async fn hurt_by(
        &mut self,
        amount: f32,
        kind: &'static DamageType,
        attacker: Option<(PlayerId, String)>,
        position: Option<[f64; 3]>,
    ) {
        if self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let hurt = self
            .hurt(
                DamageSource {
                    kind,
                    attacker: attacker.map(|(id, name)| Culprit::player(id, 0, name)),
                    projectile: None,
                    position,
                    weapon: None,
                    direct: true,
                },
                amount,
            )
            .await;
        if let Some(motion) = hurt.motion {
            self.motion.velocity_sent.push(self.tick, motion);
            let packet = packets([framed(&play::SetEntityMotion {
                entity_id: self.id,
                velocity: motion,
            })]);
            self.broadcast_entity(packet, true).await;
        }
    }

    /// A mob's or projectile's hit: hurt, pushed back (with any extra
    /// knockback of the attacker's), and set alight if it burns.
    pub async fn hurt_by_entity(&mut self, hit: EntityHit) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let hurt = self
            .hurt(
                DamageSource {
                    kind: hit.kind,
                    attacker: hit.attacker.clone(),
                    projectile: hit.projectile,
                    position: hit.position,
                    weapon: hit.weapon.clone(),
                    direct: hit.projectile.is_none(),
                },
                hit.amount,
            )
            .await;
        if hurt.shield_blocked
            && hit.report_blocked
            && let Some(attacker) = hit.attacker.as_ref()
            && let Some(handle) = self.region(self.chunk().region()).await
        {
            handle
                .request(RegionRequest::Notify {
                    target: attacker.id,
                    notice: crate::world::entity::Notice::ShieldBlocked { defender: self.id },
                })
                .await;
        }
        if !hurt.landed {
            if let Some(entity) = hit.rebound.as_ref().and_then(|r| r.take()) {
                let region = ChunkPos::of_position(entity.base().position).region();
                if let Some(handle) = self.region(region).await {
                    handle.request(RegionRequest::SpawnEntity(entity)).await;
                }
            }
            return;
        }
        if hit.sticks {
            self.body.stuck_arrows.count += 1;
        }
        if hit.attacker.as_ref().is_some_and(|a| a.id != self.id)
            && let Some(rules) = self.shared.player_damage.clone()
        {
            rules.entity_hit_landed(self, &hit).await;
        }
        if hit.report_landed
            && let Some(attacker) = hit.attacker.as_ref()
            && let Some(handle) = self.region(self.chunk().region()).await
        {
            handle
                .request(RegionRequest::Notify {
                    target: attacker.id,
                    notice: crate::world::entity::Notice::HitLanded,
                })
                .await;
        }
        let mut motion = hurt.motion;
        if let Some([x, y, z]) = hit.push {
            let resistance = self.attribute(Attribute::KnockbackResistance);
            let base = motion.unwrap_or([0.0; 3]);
            motion = Some(match hit.push_power {
                Some(power) => {
                    let keep = ((power - resistance) / power).max(0.0);
                    [base[0] + x * keep, base[1] + y * keep, base[2] + z * keep]
                }
                None => {
                    let keep = (1.0 - resistance).max(0.0);
                    [base[0] + x * keep, base[1] + y, base[2] + z * keep]
                }
            });
        }
        if let Some(mut motion) = motion {
            if let Some((strength, dx, dz)) = hit.knockback {
                motion = attributes::knockback(
                    motion,
                    strength,
                    dx,
                    dz,
                    self.pose.on_ground,
                    self.attribute(Attribute::KnockbackResistance),
                );
            }
            self.motion.velocity_sent.push(self.tick, motion);
            let packet = packets([framed(&play::SetEntityMotion {
                entity_id: self.id,
                velocity: motion,
            })]);
            self.broadcast_entity(packet, true).await;
        }
        if hit.ignite > 0.0 {
            self.ignite(hit.ignite);
        }
        for effect in hit.effects {
            Box::pin(self.add_effect(effect)).await;
        }
    }

    /// A sound for everyone nearby, at `position`.
    pub async fn sound_at(&mut self, sound: SoundEvent, position: [f64; 3]) {
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::PLAYERS,
            position,
            volume: 1.0,
            pitch: 1.0,
            seed,
        })]);
        self.broadcast_chunk(packet).await;
    }

    /// Damage with no source position or weapon. Returns whether it landed.
    pub async fn damage(
        &mut self,
        amount: f32,
        kind: &'static DamageType,
        attacker: Option<(PlayerId, String)>,
    ) -> bool {
        self.hurt(
            DamageSource {
                kind,
                attacker: attacker.map(|(id, name)| Culprit::player(id, 0, name)),
                projectile: None,
                position: None,
                weapon: None,
                direct: true,
            },
            amount,
        )
        .await
        .landed
    }

    pub fn equipment_snapshot(&self) -> [Option<ItemStack>; 6] {
        EQUIPMENT_SLOTS.map(|slot| {
            self.inventory
                .equipped(slot)
                .map(DupUnchecked::dup_unchecked)
        })
    }

    /// The equipment worn and held, by slot.
    pub fn worn(&self) -> Vec<attributes::Equipped<'_>> {
        EQUIPMENT_SLOTS
            .into_iter()
            .filter_map(|slot| Some((slot, self.inventory.equipped(slot)?)))
            .collect()
    }

    pub fn equipment(&self) -> Vec<(EquipmentSlot, ItemSnapshot)> {
        EQUIPMENT_SLOTS
            .into_iter()
            .filter_map(|slot| Some((slot, ItemSnapshot::of(self.inventory.equipped(slot)?))))
            .collect()
    }

    /// Wears the item in an equipment slot by `amount` points, breaking it
    /// with the matching animation if it runs out.
    pub async fn wear_slot(&mut self, slot: EquipmentSlot, amount: u32) {
        if amount == 0 || self.mode.abilities.instabuild {
            return;
        }
        let window = match slot {
            EquipmentSlot::Mainhand => Inventory::hotbar_slot(self.inventory.selected()),
            other => match Inventory::armor_slot(other) {
                Some(window) => window,
                None => return,
            },
        };
        let held = self.inventory.slots()[window]
            .as_ref()
            .map(|stack| stack.item);
        match self.hurt_slot_reporting(window, amount) {
            Hurt::Unchanged => {}
            Hurt::Damaged => self.send_inventory(),
            Hurt::Broken => {
                if let Some(item) = held {
                    self.item_action_hooks(
                        super::hooks::ItemAction::Broken,
                        ItemStack::mint(item, 1, MintReason::Template).view(),
                    );
                    self.progress
                        .stats
                        .award(Stat::item(StatType::Broken, item), 1);
                }
                let event = packets([framed(&play::EntityEvent {
                    entity_id: self.id,
                    status: break_event(slot),
                })]);
                self.broadcast_entity(event, true).await;
                self.send_inventory();
            }
        }
    }

    /// A sound at this player that everyone nearby hears.
    pub async fn player_sound(&mut self, sound: SoundEvent, volume: f32, pitch: f32) {
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::PLAYERS,
            position: self.pose.position,
            volume,
            pitch,
            seed,
        })]);
        self.broadcast_chunk(packet).await;
    }

    /// A sound the player's own client plays itself: heard by everyone
    /// around but the player.
    pub async fn player_sound_to_others(&mut self, sound: SoundEvent, volume: f32, pitch: f32) {
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::PLAYERS,
            position: self.pose.position,
            volume,
            pitch,
            seed,
        })]);
        self.broadcast_chunk_to_others(packet).await;
    }

    /// The sounds bundle clicks made, to the players nearby.
    pub async fn flush_bundle_sounds(&mut self) {
        for sound in self.inventory.take_bundle_sounds() {
            let pitch = 0.8 + self.rng.r#gen::<f32>() * 0.4;
            let (event, volume, pitch) = match sound {
                BundleSound::Insert => (SoundEvent::ItemBundleInsert, 0.8, pitch),
                BundleSound::InsertFail => (SoundEvent::ItemBundleInsertFail, 1.0, 1.0),
                BundleSound::RemoveOne => (SoundEvent::ItemBundleRemoveOne, 0.8, pitch),
            };
            self.player_sound_to_others(event, volume, pitch).await;
        }
    }

    /// A totem of undying in either hand cheats a fatal blow: it is
    /// used up, health drops to one half heart and the effects change.
    pub async fn pop_totem(&mut self, kind: &DamageType) -> bool {
        if crate::world::damage_type_in_tag(kind, "minecraft:bypasses_invulnerability") {
            return false;
        }
        let used = self.inventory.take_death_protection();
        let Some((used, death_effects)) = used else {
            return false;
        };
        self.progress
            .stats
            .award(Stat::item(StatType::Used, used.item), 1);
        self.body.health.value = 1.0;
        for effect in death_effects {
            match effect {
                tuff_item::item::DeathEffect::ClearAllEffects => {
                    for change in self.body.effects.clear() {
                        self.apply_effect_change(Some(change)).await;
                    }
                }
                tuff_item::item::DeathEffect::ApplyEffects {
                    effects,
                    probability,
                } => {
                    if probability < 1.0 && rand::random::<f32>() >= probability {
                        continue;
                    }
                    for (kind, duration, amplifier) in effects {
                        let change =
                            self.body
                                .effects
                                .add(crate::world::effects::ActiveEffect::timed(
                                    kind, duration, amplifier,
                                ));
                        self.apply_effect_change(change).await;
                    }
                }
            }
        }
        self.fire(crate::world::advancement::trigger::Event::UsedTotem(&used));
        self.send_inventory();
        let event = packets([framed(&play::EntityEvent {
            entity_id: self.id,
            status: play::entity_event::TOTEM_OF_UNDYING,
        })]);
        self.broadcast_entity(event, true).await;
        true
    }

    pub async fn die(&mut self, kind: &'static DamageType, killer: Option<&Culprit>) {
        self.died_hooks(kind, killer);
        let death_dimension = self.shared.dimension(self.dimension).key.to_string();
        self.mode.last_death = Some((death_dimension, BlockPos::containing(self.pose.position)));
        self.drop_shoulder_entities().await;
        self.leave_vehicle().await;
        self.score_death().await;
        self.progress.stats.award_custom(CustomStat::Deaths, 1);
        self.progress.stats.reset_custom(CustomStat::TimeSinceDeath);
        self.progress.stats.reset_custom(CustomStat::TimeSinceRest);
        let teams = self.shared.scoreboard.teams();
        let victim = teams.player_name(&self.profile.name, self.profile.id);
        let killer_culprit = killer;
        let killer = killer.map(|killer| match killer.player_name() {
            Some(name) => teams.player_name(name, killer.uuid),
            None => teams.owned_entity_name(
                killer.kind.name(),
                killer.uuid,
                killer.name.clone(),
                killer.owner_name.as_deref(),
            ),
        });
        let held = killer_culprit.and_then(|killer| killer.held_name.as_deref().cloned());
        let credit = match (&killer_culprit, &self.body.kill_credit) {
            (None, Some((name, uuid, tick))) if self.tick.saturating_sub(*tick) <= 100 => {
                Some(teams.player_name(name, *uuid))
            }
            _ => None,
        };
        if let Some(duration) = self.body.combat.recheck(self.tick, true) {
            self.send(&play::PlayerCombatEnd {
                duration: duration as i32,
            });
        }
        let entry = self.combat_entry(kind, 0.0, killer_culprit);
        self.body.combat.push(entry);
        let fell = (crate::world::effective_damage_type(kind).death_message_type
            == Some(tuff_vanilla_data::damage_type::DeathMessageType::FallVariants))
        .then(|| {
            let killer = killer_culprit.map(|culprit| self.attacker_of(culprit));
            crate::world::death::fall_death_message(
                self.body.combat.entries(),
                victim.clone(),
                killer.as_ref(),
            )
        })
        .flatten();
        let message = fell.unwrap_or_else(|| {
            crate::world::death::death_message(kind, victim, killer, held, credit)
        });
        self.body.combat.clear_entries();
        self.body.kill_credit = None;
        self.pose.sprinting = false;
        self.body.fire_ticks = 0;
        self.body.ticks_frozen = 0;
        self.body.fall = Fall::default();
        self.surroundings.in_lava = false;
        self.surroundings.contact = block_contact::Contact::default();
        self.surroundings.enchantment_modifiers.clear();
        self.surroundings.in_fire = None;
        self.actions.using = None;
        self.react_to_mob_effects(PlayerEffectTrigger::Died).await;
        self.body.effects = ActiveEffects::default();
        self.body.absorption = 0.0;
        let keep_inventory = self.keeps_inventory();
        let drops = if keep_inventory {
            Vec::new()
        } else {
            self.inventory.clear_dropping_survivors()
        };
        let show_death_messages = self.shared.rule_bool("show_death_messages");
        let empty = TextComponent::default();
        self.send(&play::PlayerCombatKill {
            player_id: self.id,
            message: if show_death_messages {
                &message
            } else {
                &empty
            },
        });
        self.send_inventory();
        let mut values = self.flags_metadata(false);
        values.push((metadata::HEALTH, MetadataValue::Float(0.0)));
        let effects = packets([
            framed(&play::SetEntityData {
                entity_id: self.id,
                values: &values,
            }),
            framed(&play::EntityEvent {
                entity_id: self.id,
                status: play::entity_event::DEATH,
            }),
        ]);
        self.broadcast_entity(effects, false).await;
        self.send(&play::EntityEvent {
            entity_id: self.id,
            status: play::entity_event::DEATH,
        });
        if self.shared.rule_bool("forgive_dead_players") {
            self.tell_mobs_of_death().await;
        }
        log::info!("{}", message_plain(&message));
        if show_death_messages {
            self.coordinator(CoordinatorMessage::AnnounceDeath {
                victim: self.profile.name.clone(),
                message,
            })
            .await;
        }
        let position = self.pose.position;
        let reward = if self.mode.game_mode == GameMode::Spectator || keep_inventory {
            0
        } else {
            self.body.experience.death_reward()
        };
        if reward > 0
            && let Some(handle) = self.region(self.chunk().region()).await
        {
            handle
                .request(RegionRequest::SpawnExperience {
                    position,
                    amount: reward,
                })
                .await;
        }
        for stack in drops {
            let angle = self.rng.gen_range(0.0..std::f64::consts::TAU);
            let speed = self.rng.gen_range(0.0..0.5);
            let velocity = [-angle.sin() * speed, 0.2, angle.cos() * speed];
            let at = [position[0], position[1] + EYE_HEIGHT - 0.3, position[2]];
            self.spawn_item(stack, at, velocity, player::THROWN_PICKUP_DELAY)
                .await;
        }
        self.publish(false).await;
    }

    /// Tells every mob of this dimension that this player died, and which
    /// of them stood within 32 blocks across and 10 up or down of it.
    async fn tell_mobs_of_death(&mut self) {
        let (reply, regions) = oneshot::channel();
        if self
            .shared
            .manager
            .send(ManagerMessage::Regions { reply })
            .await
            .is_err()
        {
            return;
        }
        let [x, y, z] = self.pose.position.map(f64::floor);
        let area = [x - 32.0, y - 10.0, z - 32.0, x + 33.0, y + 11.0, z + 33.0];
        let player = self.profile.id;
        let dimension = self.dimension;
        for region in regions.await.unwrap_or_default() {
            if region.dimension != dimension {
                continue;
            }
            region.internal(RegionInternal::Run(
                crate::world::region_api::RegionTask::new(move |world| {
                    for id in world.entity_ids() {
                        if let Some(tracked) = world.entity_mut(id) {
                            let nearby =
                                crate::world::physics::intersects(tracked.entity.bounds(), area);
                            tracked.entity.notice(Notice::PlayerDied { player, nearby });
                        }
                    }
                }),
            ));
        }
    }

    /// Wears the stack in a window slot and reports the durability change
    /// to advancement criteria, including the hit that breaks it.
    pub fn hurt_slot_reporting(&mut self, window: usize, amount: u32) -> Hurt {
        let before = self
            .inventory
            .slots()
            .get(window)
            .and_then(DupUnchecked::dup_unchecked);
        let outcome = self.inventory.hurt_slot(window, amount, &mut self.rng);
        let Some(mut stack) = before else {
            return outcome;
        };
        let damage_before = stack.damage();
        match outcome {
            Hurt::Unchanged => {}
            Hurt::Damaged => {
                if let Some(now) = self
                    .inventory
                    .slots()
                    .get(window)
                    .and_then(DupUnchecked::dup_unchecked)
                {
                    let delta = damage_before as i32 - now.damage() as i32;
                    self.fire(
                        crate::world::advancement::trigger::Event::ItemDurabilityChanged {
                            item: &now,
                            delta,
                        },
                    );
                }
            }
            Hurt::Broken => {
                let max = stack.max_damage().unwrap_or(damage_before);
                stack.set_damage(max);
                let delta = damage_before as i32 - max as i32;
                self.fire(
                    crate::world::advancement::trigger::Event::ItemDurabilityChanged {
                        item: &stack,
                        delta,
                    },
                );
            }
        }
        outcome
    }

    /// Wears the stack in a hand as vanilla's `hurtAndBreak`: nothing in
    /// creative, and a stack that reaches its durability is consumed with
    /// the break animation (and, client-side, sound) on this player.
    pub async fn hurt_hand(&mut self, off_hand: bool, amount: u32) {
        if amount == 0 || self.mode.abilities.instabuild {
            return;
        }
        let held = self.inventory.hand(off_hand).map(|stack| stack.item);
        let window = self.inventory.hand_slot(off_hand);
        match self.hurt_slot_reporting(window, amount) {
            Hurt::Unchanged => {}
            Hurt::Damaged => self.send_inventory(),
            Hurt::Broken => {
                if let Some(item) = held {
                    self.item_action_hooks(
                        super::hooks::ItemAction::Broken,
                        ItemStack::mint(item, 1, MintReason::Template).view(),
                    );
                    self.progress
                        .stats
                        .award(Stat::item(StatType::Broken, item), 1);
                }
                let event = packets([framed(&play::EntityEvent {
                    entity_id: self.id,
                    status: if off_hand {
                        play::entity_event::OFF_HAND_BREAK
                    } else {
                        play::entity_event::MAIN_HAND_BREAK
                    },
                })]);
                self.broadcast_entity(event, true).await;
                self.send_inventory();
            }
        }
    }

    /// Hurts this player through the registered damage rules, or with no
    /// rules by taking `amount` off the health.
    pub async fn hurt(&mut self, source: DamageSource, amount: f32) -> HurtResult {
        if !self.client_loaded() {
            return HurtResult::default();
        }
        if !self.shared.rule_bool("fire_damage")
            && tuff_datapack::tags::has(
                tuff_vanilla_data::tag::damage_type::key::IS_FIRE,
                source.kind.name,
            )
        {
            return HurtResult::default();
        }
        if let Some(attacker) = source
            .attacker
            .as_ref()
            .and_then(|a| a.player_name().or(a.owner_name.as_deref()))
            && !self.can_be_harmed_by(attacker)
        {
            return HurtResult::default();
        }
        let hooks = self.shared.session_hooks.clone();
        if !hooks
            .iter()
            .all(|hook| hook.allows_player_hurt(self, &source))
        {
            return HurtResult::default();
        }
        let amount = hooks.iter().fold(amount, |amount, hook| {
            amount * hook.player_hurt_scale(self, &source)
        });
        let was_dead = self.is_dead();
        let by = HurtBy {
            kind: source.kind,
            attacker: source.attacker.clone(),
            projectile: source.projectile,
            direct: source.direct,
        };
        let result = match self.shared.player_damage.clone() {
            Some(rules) => rules.hurt(self, source, amount).await,
            None => self.plain_hurt(source, amount).await,
        };
        let died = !was_dead && self.is_dead();
        if !was_dead && (result.landed || !self.mode.abilities.invulnerable) {
            self.drop_shoulder_entities().await;
        }
        if result.landed {
            if !died {
                let entry = self.combat_entry(by.kind, result.health_lost, by.attacker.as_ref());
                self.body.combat.push(entry);
            }
            self.record_combat_damage(by.attacker.is_some());
            let retaliable = !tuff_datapack::tags::has(
                tuff_vanilla_data::tag::damage_type::key::NO_WOLF_RETALIATION,
                by.kind.name,
            );
            if let Some(name) = by.attacker.as_ref().and_then(|a| a.player_name()) {
                let uuid = by.attacker.as_ref().map_or(0, |a| a.uuid);
                self.body.kill_credit = Some((name.to_string(), uuid, self.tick));
            }
            self.body.combat.record_attacker(
                self.tick,
                by.attacker.as_ref().map(|a| a.id),
                retaliable,
            );
        }
        if result.landed && result.health_lost > 0.0 {
            self.react_to_mob_effects(PlayerEffectTrigger::Hurt).await;
        }
        if result.landed || died {
            for hook in hooks.iter() {
                hook.player_hurt(self, by.kind, &result, died);
            }
            for hook in hooks.iter() {
                hook.player_damaged(self, &by, &result, died);
            }
        }
        result
    }

    /// Weaving, Oozing and Wind Charged act on death and Infested on a hurt:
    /// the region around the player carries them out.
    async fn react_to_mob_effects(&self, trigger: PlayerEffectTrigger) {
        use tuff_vanilla_data::registry::MobEffect;
        let reacting: &[MobEffect] = match trigger {
            PlayerEffectTrigger::Died => &[
                MobEffect::Weaving,
                MobEffect::Oozing,
                MobEffect::WindCharged,
            ],
            PlayerEffectTrigger::Hurt => &[MobEffect::Infested],
        };
        let effects: Vec<MobEffect> = reacting
            .iter()
            .copied()
            .filter(|effect| self.body.effects.has(*effect))
            .collect();
        if effects.is_empty() {
            return;
        }
        let position = self.pose.position;
        let Some(region) = self.region_of(BlockPos::containing(position)) else {
            return;
        };
        let event = PlayerEffectEvent {
            trigger,
            effects,
            culprit: Culprit::player(self.id, self.profile.id, self.profile.name.clone()),
            position,
            height: crate::world::PLAYER_HEIGHT,
            yaw: self.pose.yaw,
            pitch: self.pose.pitch,
        };
        region
            .request(RegionRequest::Run(RegionTask::new(move |world| {
                world.player_effect(event);
            })))
            .await;
    }

    fn combat_entry(
        &mut self,
        kind: &'static DamageType,
        damage: f32,
        attacker: Option<&Culprit>,
    ) -> crate::world::death::CombatEntry {
        use crate::world::death::FallLocation;
        let location = self
            .surroundings
            .fall_location
            .or_else(|| self.body.fall.in_water().then_some(FallLocation::Water));
        let fall_distance = if crate::world::damage_type_in_tag(kind, "minecraft:is_fall")
            && self.body.fall.impact_distance > 0.0
        {
            std::mem::take(&mut self.body.fall.impact_distance)
        } else {
            self.body.fall.distance
        };
        crate::world::death::CombatEntry {
            kind,
            damage,
            location,
            fall_distance: fall_distance as f32,
            attacker: attacker.map(|culprit| self.attacker_of(culprit)),
        }
    }

    fn attacker_of(&self, culprit: &Culprit) -> crate::world::death::Attacker {
        let teams = self.shared.scoreboard.teams();
        let name = match culprit.player_name() {
            Some(name) => teams.player_name(name, culprit.uuid),
            None => teams.owned_entity_name(
                culprit.kind.name(),
                culprit.uuid,
                culprit.name.clone(),
                culprit.owner_name.as_deref(),
            ),
        };
        crate::world::death::Attacker {
            uuid: culprit.uuid,
            name,
            held: culprit.held_name.as_deref().cloned(),
        }
    }

    fn record_combat_damage(&mut self, by_living: bool) {
        self.tick_combat();
        let alive = !self.is_dead();
        if self.body.combat.record_damage(self.tick, alive, by_living) {
            self.send(&play::PlayerCombatEnter);
        }
    }

    /// Ends the fight once no damage has landed for long enough.
    pub fn tick_combat(&mut self) {
        let alive = !self.is_dead();
        if let Some(duration) = self.body.combat.recheck(self.tick, alive) {
            self.send(&play::PlayerCombatEnd {
                duration: duration as i32,
            });
        }
    }

    /// Whether the player named `attacker`, or its projectile, may hurt
    /// this one: not with PvP off, nor as a teammate without friendly fire.
    pub fn can_be_harmed_by(&self, attacker: &str) -> bool {
        use crate::world::scoreboard::Member;
        self.shared.rule_bool("pvp")
            && self
                .shared
                .scoreboard
                .teams()
                .allows_damage(Member::Player(&self.profile.name), Member::Player(attacker))
    }

    async fn plain_hurt(&mut self, source: DamageSource, amount: f32) -> HurtResult {
        if self.is_dead() || self.mode.abilities.is_immune_to(source.kind) {
            return HurtResult::default();
        }
        let Some((through, _)) = self.body.health.window(amount, false) else {
            return HurtResult::default();
        };
        let before = self.body.health.value;
        self.body.health.lose(through);
        let health_lost = before - self.body.health.value;
        if self.is_dead() && !self.pop_totem(source.kind).await {
            self.die(source.kind, source.attacker.as_ref()).await;
        }
        self.publish(false).await;
        HurtResult {
            landed: true,
            motion: None,
            health_lost,
            shield_blocked: false,
        }
    }
}

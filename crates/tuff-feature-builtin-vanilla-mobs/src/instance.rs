//! [`Species`] is a mob type's own state and behaviour; [`Mob`] joins it
//! with [`MobCore`] into a [`RegionEntity`], so a new mob type (vanilla or
//! feature) is one `Species` impl and a registry entry.

use std::fmt::Debug;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_core::world::messages::PlayerEvent;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::entity::MAX_AIR_SUPPLY;
use tuff_vanilla_data::registry::{Attribute, CustomStat, EntityType, MobEffect, SoundEvent};

use super::{
    AIR_REFILL, DAMAGE_COOLDOWN, DEATH_TICKS, DEFAULT_AMBIENT_INTERVAL, DROWNING_AIR, Damage,
    HURT_DURATION, MobCore, MobSounds, NO_ACTION_DESPAWN_TICKS, OwnAirRules, PLAYER_MEMORY_TICKS,
    data, event,
};
use super::{animal, physics, villager};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::effects::{self, EffectAction, EffectChange};
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, EntityInteraction, InteractOutcome,
    LivingSummary, Notice, RegionEntity, RiderInput, Tick, TickContext, take, take_bool,
    take_number,
};
use tuff_core::world::{block_contact, contact, framed, freezing, packets};
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::combat;
use tuff_vanilla_data::item::{EquipmentSlot, ModifierOperation};
use tuff_world_model::BlockPos;

const PLAYER_EYE_HEIGHT: f64 = 1.62;
const POWDER_SNOW_DRIFT: f32 = 1.0 / 12.0;

mod enchant;
mod region_entity;

pub use enchant::LocationEffects;
mod species;
pub use species::Species;

/// A mob of species `S`.
#[derive(Debug)]
pub struct Mob<S: Species> {
    pub core: MobCore,
    pub species: S,
}

impl<S: Species> Mob<S> {
    pub fn new(base: EntityBase, rng: &mut StdRng) -> Self {
        let mut core = MobCore::new(base);
        S::configure(&mut core);
        core.refresh_size();
        core.health = core.max_health();
        Self {
            core,
            species: S::create(rng),
        }
    }

    fn full_metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        for index in [data::HEALTH, data::MOB_FLAGS] {
            values.extend(self.core.core_metadata(index).map(|v| (index, v)));
        }
        if self.core.stuck_arrows.count > 0 {
            values.push((
                play::metadata::ARROW_COUNT,
                MetadataValue::VarInt(self.core.stuck_arrows.count),
            ));
        }
        if self.core.using_item.is_some() {
            let flags = self.core.core_metadata(data::LIVING_FLAGS);
            values.push((data::LIVING_FLAGS, flags.expect("core index")));
        }
        if self.core.age.is_some() {
            for index in [data::BABY, data::AGE_LOCKED] {
                values.push((index, self.core.core_metadata(index).expect("core index")));
            }
        }
        let species = self.species.metadata(&self.core);
        let species_pose = species.iter().any(|(i, _)| *i == play::metadata::POSE);
        if !species_pose && (self.core.swimming || self.core.dead || self.core.sleeping) {
            values.extend(
                self.core
                    .core_metadata(play::metadata::POSE)
                    .map(|pose| (play::metadata::POSE, pose)),
            );
        }
        values.extend(species);
        values
    }

    pub(super) fn play_eating_sound(&mut self, cx: &mut TickContext) {
        if let Some(sound) = self.species.eating_sound(&self.core) {
            let volume = self.species.sounds(&self.core).volume;
            let pitch = self.core.voice_pitch(cx.rng);
            self.core.sound(sound, volume, pitch, cx);
        }
    }

    fn play_hurt_sound(&mut self, cx: &mut TickContext) {
        self.core.ambient_sound_time = -self.species.ambient_sound_interval();
        let sounds = self.species.sounds(&self.core);
        if let Some(sound) = sounds.hurt {
            let pitch = self.core.voice_pitch(cx.rng);
            self.core.sound(sound, sounds.volume, pitch, cx);
        }
    }

    /// The game's hurt pipeline for mobs: invulnerability, the damage
    /// cooldown, armor, knockback and sounds. Returns whether it took
    /// damage.
    pub fn hurt_by(&mut self, damage: &Damage, amount: f32, cx: &mut TickContext) -> bool {
        let overridden = self.species.damage_override(&self.core, damage);
        if !self.core.is_alive() {
            return false;
        }
        if let Some(taken) = self
            .species
            .intercept_hurt(&mut self.core, damage, amount, cx)
        {
            return taken;
        }
        self.species
            .incoming_damage(&mut self.core, damage, amount, cx);
        let core = &mut self.core;
        if core.base.invulnerable && !damage.is("bypasses_invulnerability") {
            return false;
        }
        if combat::immune_to_damage(
            &core.equipment.worn(),
            &damage.context(core.base.kind),
            cx.rng,
        ) {
            return false;
        }
        let mut amount = overridden.unwrap_or(amount);
        if damage.is("is_freezing") {
            amount = freezing::scaled(core.base.kind, amount);
        }
        if overridden.is_none() && core.base.kind.size().fire_immune && damage.is("is_fire") {
            return false;
        }
        if !self.species.before_hurt(&mut self.core, damage, cx) {
            return false;
        }
        let core = &mut self.core;
        core.no_action_time = 0;
        let amount = if amount.is_finite() {
            amount.max(0.0)
        } else {
            f32::MAX
        };
        let full = if core.base.invulnerable_time > 10 && !damage.is("bypasses_cooldown") {
            if amount <= core.last_hurt {
                return false;
            }
            let extra = amount - core.last_hurt;
            core.last_hurt = amount;
            self.actually_hurt(damage, extra, cx);
            false
        } else {
            core.last_hurt = amount;
            core.base.invulnerable_time = DAMAGE_COOLDOWN;
            self.actually_hurt(damage, amount, cx);
            self.core.hurt_time = HURT_DURATION;
            true
        };
        let core = &mut self.core;
        if let Some((id, _)) = damage.attacker
            && id != core.base.id
            && !damage.is("no_anger")
        {
            core.last_hurt_by_mob = Some((id, cx.tick));
        }
        if let Some((_, uuid)) = damage.player {
            core.last_hurt_by_player = Some(uuid);
            core.last_hurt_by_player_time = PLAYER_MEMORY_TICKS;
        }
        if full {
            let cause = damage.attacker.map(|(id, _)| id);
            cx.effects.push(Effect::EntityPackets(packets([framed(
                &play::DamageEvent {
                    entity_id: core.base.id,
                    damage_type: tuff_core::world::damage_type_id(damage.kind),
                    cause,
                    direct: cause,
                    source_position: damage.position.filter(|_| cause.is_none()),
                },
            )])));
            if !damage.is("no_knockback") {
                let (dx, dz) = match damage.position {
                    Some(from) => (
                        from[0] - core.base.position[0],
                        from[2] - core.base.position[2],
                    ),
                    None => (0.0, 0.0),
                };
                let push = crate::Knockback {
                    power: 0.4,
                    dx,
                    dz,
                    damage: amount,
                    extra: false,
                };
                if !self.species.knockback(&mut self.core, &push, damage, cx) {
                    self.core.knockback(0.4, dx, dz, cx.rng);
                }
            }
        }
        self.core.last_damage = Some((damage.clone(), cx.tick));
        crate::death_effects::hurt(&self.core, cx);
        if !self.core.is_alive() {
            if full {
                let sounds = self.species.sounds(&self.core);
                if let Some(sound) = sounds.death {
                    let pitch = self.core.voice_pitch(cx.rng);
                    self.core.sound(sound, sounds.volume, pitch, cx);
                }
            }
            self.die(damage.clone(), cx);
        } else {
            if full {
                self.play_hurt_sound(cx);
            }
            cx.effects.push(Effect::GameEvent {
                event: tuff_core::world::game_event::GameEvent::EntityDamage,
                position: self.core.base.position,
                source: Some(self.core.base.id),
            });
            self.species.after_hurt(&mut self.core, damage, cx);
        }
        true
    }

    /// A lasting effect's periodic tick action (poison, regeneration,
    /// wither): mobs have no hunger, so `Exhaust`/`Feed` are dropped.
    fn apply_effect_action(&mut self, action: EffectAction, cx: &mut TickContext) {
        match action {
            EffectAction::Heal(amount) => self.core.heal(amount),
            EffectAction::Poison(amount) => {
                if self.core.health > 1.0 {
                    self.hurt_by(&Damage::of(&damage_type::MAGIC), amount, cx);
                }
            }
            EffectAction::MagicDamage(amount) => {
                self.hurt_by(&Damage::of(&damage_type::MAGIC), amount, cx);
            }
            EffectAction::WitherDamage(amount) => {
                self.hurt_by(&Damage::of(&damage_type::WITHER), amount, cx);
            }
            EffectAction::Exhaust(_) | EffectAction::Feed { .. } => {}
        }
    }

    /// Broadcasts a lasting effect's addition, refresh or expiry to
    /// viewers, and keeps absorption in step like a player's.
    fn apply_effect_change(&mut self, change: EffectChange, cx: &mut TickContext) {
        match change {
            EffectChange::Added(effect) | EffectChange::Updated(effect) => {
                if effect.effect == MobEffect::Glowing {
                    self.core.base.glowing_effect = true;
                }
                if effect.effect == MobEffect::Absorption {
                    self.core.absorption = self
                        .core
                        .absorption
                        .max(effects::max_absorption(effect.amplifier));
                }
                let packet = framed(&play::UpdateMobEffect {
                    entity_id: self.core.base.id,
                    effect: effect.effect.id() as i32,
                    amplifier: effect.amplifier as i32,
                    duration: effect.duration.clamp(-1, i32::MAX as i64) as i32,
                    flags: effect.flags(),
                });
                cx.effects.push(Effect::EntityPackets(packets([packet])));
            }
            EffectChange::Removed(kind) => {
                if kind == MobEffect::Glowing {
                    self.core.base.glowing_effect = false;
                }
                if kind == MobEffect::Absorption {
                    self.core.absorption = 0.0;
                }
                let packet = framed(&play::RemoveMobEffect {
                    entity_id: self.core.base.id,
                    effect: kind.id() as i32,
                });
                cx.effects.push(Effect::EntityPackets(packets([packet])));
            }
        }
    }

    fn actually_hurt(&mut self, damage: &Damage, amount: f32, cx: &mut TickContext) {
        if self.species.absorb_hurt(&mut self.core, damage, amount, cx) {
            return;
        }
        let kind = self.core.base.kind;
        let core = &mut self.core;
        let mut amount = amount;
        if !damage.is("bypasses_armor") {
            amount = combat::after_armor(
                amount,
                core.attribute(Attribute::Armor) as f32,
                core.attribute(Attribute::ArmorToughness) as f32,
                damage.weapon.as_deref().map(ItemStack::any),
                &damage.context(kind),
                cx.rng,
            );
        }
        let mut credit = |stat, value: f32| {
            if let Some((player, _)) = damage.player
                && let Some(tenths) = combat::stat_tenths(value)
            {
                cx.effects.push(Effect::TellPlayer {
                    player,
                    event: Box::new(PlayerEvent::AwardStat(stat, tenths)),
                });
            }
        };
        if !damage.is("bypasses_effects") {
            if let Some(amplifier) = core.effects.amplifier(MobEffect::Resistance)
                && !damage.is("bypasses_resistance")
            {
                let resisted = combat::after_resistance(amount, amplifier);
                credit(CustomStat::DamageDealtResisted, amount - resisted);
                amount = resisted;
            }
            if amount <= 0.0 {
                amount = 0.0;
            } else if !damage.is("bypasses_enchantments") {
                let protection = combat::enchantment_protection(
                    &core.equipment.worn(),
                    &damage.context(kind),
                    cx.rng,
                );
                if protection > 0.0 {
                    amount = combat::after_protection(amount, protection);
                }
            }
        }
        let mut amount = self.species.scale_damage(core, damage, amount);
        let absorbed = amount.min(core.absorption).max(0.0);
        core.absorption -= absorbed;
        amount -= absorbed;
        credit(CustomStat::DamageDealtAbsorbed, absorbed);
        if amount != 0.0 {
            core.health = (core.health - amount).clamp(0.0, core.max_health());
            core.changed(data::HEALTH);
        }
        if let Some(love) = &mut core.love {
            love.reset();
        }
    }

    /// This mob's death as the killer's statistics and advancements see it.
    fn kill_credit(&self, damage: &Damage) -> tuff_core::world::messages::Kill {
        use tuff_core::world::predicate::{self as facts, DamageFacts};
        let kind_only = |kind| facts::EntityFacts {
            kind: Some(kind),
            ..Default::default()
        };
        let source = damage.attacker.map(|(_, kind)| kind_only(kind));
        tuff_core::world::messages::Kill {
            kind: self.core.base.kind,
            holder: tuff_storage::admin::format_uuid(self.core.base.uuid),
            victim: self.core.facts(),
            killing_blow: DamageFacts {
                kind: damage.kind,
                direct: damage
                    .projectile
                    .map(|kind| facts::EntityFacts {
                        entity_id: damage.projectile_id,
                        equipment: damage
                            .weapon
                            .iter()
                            .map(|w| (tuff_vanilla_data::item::EquipmentSlot::Mainhand, w.clone()))
                            .collect(),
                        ..kind_only(kind)
                    })
                    .or_else(|| source.clone()),
                source,
            },
        }
    }

    fn die(&mut self, damage: Damage, cx: &mut TickContext) {
        if self.core.dead {
            return;
        }
        self.core.dead = true;
        self.core.changed(play::metadata::POSE);
        cx.effects.push(Effect::GameEvent {
            event: tuff_core::world::game_event::GameEvent::EntityDie,
            position: self.core.base.position,
            source: Some(self.core.base.id),
        });
        if let Some(owner) = self.core.tame.as_ref().and_then(|t| t.owner) {
            cx.effects.push(Effect::PetDied {
                owner,
                victim: tuff_core::world::entity::Culprit::entity(&self.core.base).name,
                kind: damage.kind,
                killer: damage.attacker_name.clone(),
                killer_player: damage.player.map(|(_, uuid)| uuid),
            });
        }
        cx.effects.push(Effect::Died {
            player: damage.player.map(|(_, uuid)| uuid),
            attacker: damage.attacker.map(|(_, kind)| kind),
            damage: damage.kind.name,
        });
        let kill = damage
            .player
            .map(|(player, _)| (player, Box::new(self.kill_credit(&damage))));
        if let Some((player, kill)) = &kill {
            cx.effects.push(Effect::KillCredit {
                player: *player,
                kill: kill.clone(),
            });
        }
        super::raider::died(&self.core, damage.player.map(|(_, uuid)| uuid), cx);
        if let Some((player, _)) = damage.player {
            super::axolotl::support_killer(self.core.base.id, player, cx);
        }
        self.species.died(&mut self.core, &damage, cx);
        if self.core.discarded {
            return;
        }
        let drops_allowed = cx.shared.game_rules.borrow().bool("mob_drops");
        let player_killed = self.core.last_hurt_by_player_time > 0;
        let drops = self.core.hostile || !self.core.is_baby();
        if drops && drops_allowed {
            let mut facts = EntityFacts {
                kind: Some(self.core.base.kind),
                on_fire: self.core.base.on_fire(),
                attacker_weapon: damage.weapon.clone(),
                attacker_kind: damage.attacker.map(|(_, kind)| kind),
                killed_by_player: player_killed,
                damage_kind: Some(damage.kind),
                damage_direct: damage.projectile.is_none(),
                recipes: Some(cx.shared.recipes()),
                tables: Some(cx.shared.loot_tables()),
                ..Default::default()
            };
            self.species.loot_facts(&mut facts);
            facts.raider = super::raider::loot_facts(&self.core, cx);
            if let Some(table) = self.core.base.kind.mob().loot_table {
                for stack in cx.shared.block_loot().entity_drops(table, &facts, cx.rng) {
                    self.core.drop_item(stack, 0.0, cx);
                }
            }
            if let Some(heads) = damage.attacker_charged.as_ref().filter(|h| h.available()) {
                let drops = cx.shared.block_loot().entity_drops(
                    "minecraft:charged_creeper/root",
                    &facts,
                    cx.rng,
                );
                if !drops.is_empty() && heads.claim() {
                    for stack in drops {
                        self.core.drop_item(stack, 0.0, cx);
                    }
                } else {
                    for stack in drops {
                        stack.burn(tuff_item::item::BurnReason::Loot);
                    }
                }
            }
            let context = damage.context(self.core.base.kind);
            let weapon = damage.weapon.clone();
            let boost = |chance: f32, rng: &mut rand::rngs::StdRng| match &weapon {
                Some(weapon) => combat::equipment_drop_chance(weapon, &context, chance, rng),
                None => chance,
            };
            for stack in self
                .core
                .equipment
                .death_drops(player_killed, cx.rng, boost)
            {
                self.core.drop_item(stack, 0.0, cx);
            }
        }
        if damage
            .attacker
            .is_some_and(|(_, kind)| kind == EntityType::Wither)
        {
            super::death_effects::wither_rose(&self.core, cx);
        }
        if drops && drops_allowed {
            let amount = self.species.experience(&self.core, cx.rng);
            if amount > 0 {
                cx.effects.push(Effect::MobExperience {
                    position: self.core.base.position,
                    amount,
                    player_killed,
                    killer: kill,
                });
            }
        }
        self.core.entity_event(event::DEATH, cx);
    }

    fn ai_step(&mut self, cx: &mut TickContext) {
        super::raider::ai_step(&mut self.core, cx);
        if self.core.hostile {
            self.core.monster_ai_step(cx);
        }
        let driving = (!self.core.base.riders.is_empty() && self.species.drivable(&self.core))
            .then_some(self.core.rider_input)
            .flatten()
            .filter(|input| self.species.controlled_by(&self.core, input));
        if driving.is_some() {
            self.core.resync_shadow();
            self.core.swap_shadow();
        } else {
            self.core.shadow = None;
        }
        let core = &mut self.core;
        if core.body.no_jump_delay > 0 {
            core.body.no_jump_delay -= 1;
        }
        for axis in [0, 2] {
            if core.base.velocity[axis].abs() < 0.003 {
                core.base.velocity[axis] = 0.0;
            }
        }
        if core.base.velocity[1].abs() < 0.003 {
            core.base.velocity[1] = 0.0;
        }
        if core.is_alive() && !self.species.immobile(core) {
            core.body.xxa *= 0.98;
            core.body.zza *= 0.98;
            if !core.no_ai {
                if let Some(input) = driving {
                    if !self.species.ride(&mut self.core, input, cx) {
                        let (forward, strafe) = self.species.drive_input(&self.core, input);
                        self.core.base.yaw = input.yaw;
                        self.core.head_yaw = input.yaw;
                        self.core.body_yaw = input.yaw;
                        let speed = self.species.ride_speed(&mut self.core, &input, cx);
                        self.core.strafe_full(forward, -strafe);
                        self.core.movement.speed_modifier = f64::from(speed);
                        self.core.tick_move_control(cx);
                        self.core.jump.jump = input.jumping && self.species.hops_when_ridden();
                        self.core.tick_jump_control();
                    }
                } else {
                    self.server_ai_step(cx);
                }
            }
        } else {
            let core = &mut self.core;
            core.body.jumping = false;
            core.body.xxa = 0.0;
            core.body.zza = 0.0;
        }
        self.core.apply_jump(cx);
        self.core.landed_fall = None;
        let input = [
            self.core.body.xxa as f64,
            self.core.body.yya as f64,
            self.core.body.zza as f64,
        ];
        if !self.core.no_physics && self.core.base.vehicle.is_none() {
            if driving.is_some() {
                let kept = cx.effects.len();
                self.core.travel(input, cx);
                cx.effects.truncate(kept);
                self.core.fall_damage = None;
                self.core.stepped = false;
                self.core.swap_shadow();
                self.core.base.velocity = [0.0; 3];
                self.core.land_from_client_move(cx);
            } else {
                self.core.travel(input, cx);
            }
        }
        if let Some((amount, kind)) = self.core.fall_damage.take() {
            self.hurt_by(&Damage::of(kind), amount, cx);
        }
        let inside = self.block_contact(cx);
        self.tick_freezing(inside, cx);
        if std::mem::take(&mut self.core.stepped)
            && let Some(step) = self.species.sounds(&self.core).step
        {
            self.core.sound(step, 0.15, 1.0, cx);
        }
        if self.core.is_alive()
            && !self.core.unpushable
            && crate::crowd::pushes_this_tick(&self.core)
            && let Some(crush) = self.core.push_entities(cx)
        {
            let max = cx.shared.rule_int("max_entity_cramming");
            cx.effects.push(Effect::Crammed {
                counted: crush.counted,
                players: crush.players,
                max,
            });
        }
        self.core.tick_age(cx);
        self.core.tick_love(cx);
        if tuff_core::world::entity_in_tag(self.core.kind(), "burn_in_daylight") {
            self.core.burn_in_daylight(cx);
        }
        if self.core.is_alive() {
            self.species.ai_step(&mut self.core, cx);
        }
    }

    fn server_ai_step(&mut self, cx: &mut TickContext) {
        self.core.no_action_time += 1;
        self.core.vehicle_bounds = self.core.base.vehicle.as_ref().and_then(|mount| {
            cx.entities
                .iter()
                .find(|e| e.id == mount.id)
                .map(|e| e.bounds)
        });
        if !self.core.base.entity_riders.is_empty() || self.core.steered {
            self.core.take_back_steering(cx);
        }
        let borrowed = self
            .core
            .base
            .vehicle
            .is_some()
            .then(|| self.core.borrow_steering(cx))
            .flatten();
        let mut goals = std::mem::take(&mut self.core.goals);
        let id_tick = self.core.tick_count + self.core.base.id as u64;
        let period = crate::crowd::goal_period(&self.core, cx);
        if id_tick.is_multiple_of(period) || self.core.tick_count <= 1 {
            goals.tick(&mut self.core, cx);
        } else {
            goals.tick_running(false, &mut self.core, cx);
        }
        self.core.goals = goals;
        if let Some(borrowed) = borrowed {
            self.core.give_back_steering(borrowed, cx);
        }
        if std::mem::take(&mut self.core.ate) {
            self.species.ate(&mut self.core);
        }
        self.core.tick_navigation(cx);
        self.species.custom_ai_step(&mut self.core, cx);
        if let Some(love) = &mut self.core.love
            && self.core.age.as_ref().is_some_and(|a| a.age != 0)
        {
            love.reset();
        }
        self.core.tick_move_control(cx);
        self.core.turn_head();
        self.core.tick_jump_control();
        self.core.lend_steering(cx);
    }

    fn base_tick(&mut self, cx: &mut TickContext) {
        let core = &mut self.core;
        let block = cx.block;
        let immersion = contact::immersion(cx, core.bounds(), core.eye_position());
        core.body.water_height = immersion.submersion.water;
        core.body.lava_height = immersion.submersion.lava;
        core.body.in_water = immersion.submersion.water > 0.0;
        core.body.eyes_in_water = immersion.eyes_in_water;
        if self.species.pushed_by_fluid() && !core.swimming {
            immersion.push(&mut core.base.velocity);
        }
        if core.body.in_water {
            core.base.fall_distance = 0.0;
            if core.base.fire > 0 {
                core.base.fire = 0;
                let pitch = 1.6 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.4;
                core.sound(SoundEvent::EntityGenericExtinguishFire, 0.7, pitch, cx);
            }
        }
        if core.in_lava() {
            core.base.fall_distance *= 0.5;
        }
        if core.body.eyes_in_water
            && let Some(vehicle) = core.base.vehicle.as_ref().map(|m| m.id)
            && cx.entities.iter().any(|e| {
                e.id == vehicle
                    && tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::entity_type::key::DISMOUNTS_UNDERWATER,
                        e.kind,
                    )
            })
        {
            cx.effects.push(Effect::StopRiding);
        }
        if tuff_core::world::fall::resets_fall(core.bounds(), &block) {
            core.base.fall_distance = 0.0;
        }
        let bounds = core.bounds();
        if block_contact::bubble_column_push(bounds, &mut core.base.velocity, &block) {
            core.base.fall_distance = 0.0;
        }
        if core.is_alive() {
            self.enchantment_effects(cx);
            if self.in_wall(cx) {
                self.hurt_by(
                    &Damage::of(&damage_type::IN_WALL),
                    block_contact::SUFFOCATION_DAMAGE,
                    cx,
                );
            }
            match self.species.air_rules() {
                Some(rules) => self.own_breathe(rules, cx),
                None => self.breathe(cx),
            }
        }
        let core = &mut self.core;
        if core.hurt_time > 0 {
            core.hurt_time -= 1;
        }
        if core.base.invulnerable_time > 0 {
            core.base.invulnerable_time -= 1;
        }
        if core.last_hurt_by_player_time > 0 {
            core.last_hurt_by_player_time -= 1;
        } else {
            core.last_hurt_by_player = None;
        }
        self.tick_leash(cx);
        if self.core.is_alive() {
            let roll = cx.rng.gen_range(0..1000);
            let time = self.core.ambient_sound_time;
            self.core.ambient_sound_time += 1;
            if roll < time {
                self.core.ambient_sound_time = -self.species.ambient_sound_interval();
                let sounds = self.species.sounds(&self.core);
                if let Some(sound) = sounds.ambient {
                    let pitch = self.core.voice_pitch(cx.rng);
                    self.core.sound(sound, sounds.volume, pitch, cx);
                }
            }
        }
    }

    /// Its lead's tick: the holder it keeps to, the pull, and the walk back
    /// toward the holder once the lead is slack.
    fn tick_leash(&mut self, cx: &mut TickContext) {
        use super::leash::Pull;
        if self.core.leash.is_some() && self.core.base.vehicle.is_some() {
            let core = &mut self.core;
            super::leash::untie(&mut core.leash, &core.base, true, cx);
        }
        if self.core.leash.is_none() {
            if std::mem::take(&mut self.core.was_leashed) {
                self.core.home = None;
            }
            return;
        }
        self.core.was_leashed = true;
        let profile = self
            .species
            .leash_profile(&self.core)
            .unwrap_or_else(|| super::leash::profile_of(self.core.kind()));
        let core = &mut self.core;
        let on_ground = core.base.on_ground;
        let block_friction = if on_ground {
            let below = core.on_pos(0.500_001, cx);
            (cx.block)(below)
                .unwrap_or(Block::Air.default_state())
                .block()
                .movement()
                .friction
        } else {
            0.6
        };
        let in_liquid = core.in_liquid();
        let body = super::leash::Body {
            bounds: core.bounds(),
            profile,
            alive: core.is_alive(),
            angular_friction: super::leash::angular_friction(on_ground, in_liquid, block_friction),
            motion: if core.no_ai {
                [0.0; 3]
            } else {
                core.base.velocity
            },
            base: &mut core.base,
        };
        let pull = super::leash::tick(&mut core.leash, body, cx);
        let holder = match pull {
            Pull::Snapped(holder) | Pull::Elastic(holder) | Pull::Close(holder) => holder,
            Pull::Idle | Pull::Dropped => return,
        };
        self.core.home = Some(super::Home {
            pos: holder.block_pos(),
            radius: profile.elastic as i32 - 1,
        });
        match pull {
            Pull::Snapped(_) => self.core.goals.set_disabled(super::goal::Flags::MOVE, true),
            Pull::Elastic(_) => {
                let base = &mut self.core.base;
                if base.velocity[1] > -0.5 && base.fall_distance > 1.0 {
                    base.fall_distance = 1.0;
                }
                self.species.lead_pulled(&mut self.core, cx);
            }
            _ => {
                if self.species.follows_lead() && !self.core.is_panicking() {
                    self.core
                        .goals
                        .set_disabled(super::goal::Flags::MOVE, self.core.steered);
                    let [x, y, z] = self.core.base.position;
                    let d = [
                        holder.position[0] - x,
                        holder.position[1] - y,
                        holder.position[2] - z,
                    ];
                    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                    let step = (length as f32 - 2.0).max(0.0) as f64;
                    let unit = if length < 1.0e-5 {
                        [0.0; 3]
                    } else {
                        d.map(|c| c / length)
                    };
                    let target = [x + unit[0] * step, y + unit[1] * step, z + unit[2] * step];
                    let speed = self.species.lead_speed();
                    self.core.move_to_position(target, speed, cx);
                }
            }
        }
    }

    /// Magma under the feet, lit campfires, berry bushes and cactus around
    /// the body, after this tick's move.
    fn block_contact(&mut self, cx: &mut TickContext) -> freezing::Inside {
        if !self.core.is_alive() {
            return freezing::Inside::default();
        }
        let core = &self.core;
        let ground = core
            .base
            .on_ground
            .then(|| (cx.block)(core.on_pos(block_contact::STEP_OFFSET, cx)))
            .flatten();
        let bounds = core.bounds();
        let delta = [0, 1, 2].map(|axis| core.body.previous[axis] - core.base.position[axis]);
        let previous = if delta.iter().map(|d| d * d).sum::<f64>() <= 100.0 {
            physics::offset(bounds, delta)
        } else {
            bounds
        };
        let contact =
            block_contact::swept_contact(core.base.kind, previous, bounds, ground, cx.block);
        if contact.hot_floor {
            self.hurt_by(
                &Damage::of(&damage_type::HOT_FLOOR),
                block_contact::HOT_FLOOR_DAMAGE,
                cx,
            );
        }
        if let Some(amount) = contact.campfire {
            self.hurt_by(&Damage::of(&damage_type::CAMPFIRE), amount, cx);
        }
        let (from, to) = (self.core.body.previous, self.core.base.position);
        let movement = [0, 1, 2].map(|axis| from[axis] - to[axis]);
        if contact.pricks(movement) {
            self.hurt_by(
                &Damage::of(&damage_type::SWEET_BERRY_BUSH),
                block_contact::BERRY_BUSH_DAMAGE,
                cx,
            );
        }
        if contact.cactus {
            self.hurt_by(
                &Damage::of(&damage_type::CACTUS),
                block_contact::CACTUS_DAMAGE,
                cx,
            );
        }
        if contact.wither_rose
            && *cx.shared.difficulty.borrow() != tuff_core::world::player::Difficulty::Peaceful
            && !matches!(
                self.core.base.kind,
                tuff_vanilla_data::registry::EntityType::Wither
                    | tuff_vanilla_data::registry::EntityType::WitherSkeleton
            )
        {
            self.core
                .add_effect(tuff_core::world::effects::ActiveEffect::timed(
                    tuff_vanilla_data::registry::MobEffect::Wither,
                    block_contact::WITHER_ROSE_TICKS,
                    0,
                ));
        }
        if contact.bee_poison
            && self.core.base.kind == tuff_vanilla_data::registry::EntityType::Bee
            && *cx.shared.difficulty.borrow() != tuff_core::world::player::Difficulty::Peaceful
            && !self.core.effects.has(MobEffect::Poison)
        {
            self.core
                .add_effect(tuff_core::world::effects::ActiveEffect::timed(
                    MobEffect::Poison,
                    block_contact::BEE_POISON_TICKS,
                    0,
                ));
        }
        if let Some(stuck) = contact.stuck {
            let weaving = self.core.effects.has(MobEffect::Weaving);
            self.core.body.stuck = Some(stuck.multiplier(weaving));
            self.core.base.fall_distance = 0.0;
        }
        if contact.honey_side.is_some()
            && !self.core.base.on_ground
            && self.core.base.velocity[1] < -0.08
        {
            let v = &mut self.core.base.velocity;
            if v[1] < -0.13 {
                let scale = -0.05 / v[1];
                *v = [v[0] * scale, -0.05, v[2] * scale];
            } else {
                v[1] = -0.05;
            }
            self.core.base.fall_distance = 0.0;
            if cx.rng.gen_range(0..5) == 0 {
                let position = self.core.base.position;
                let sound = framed(&play::Sound {
                    sound_event: SoundEvent::BlockHoneyBlockSlide.id(),
                    category: play::sound_category::NEUTRAL,
                    position,
                    volume: 1.0,
                    pitch: 1.0,
                    seed: cx.rng.r#gen(),
                });
                let event = framed(&play::EntityEvent {
                    entity_id: self.core.base.id,
                    status: 53,
                });
                cx.effects
                    .push(Effect::ChunkPackets(packets([sound, event])));
            }
        }
        contact.freezing
    }

    /// Powder snow freezes a mob inside it and slows it once it stands on
    /// something; fully frozen, it takes freeze damage every two seconds.
    fn tick_freezing(&mut self, inside: freezing::Inside, cx: &mut TickContext) {
        let core = &mut self.core;
        let armor = [
            EquipmentSlot::Head,
            EquipmentSlot::Chest,
            EquipmentSlot::Legs,
            EquipmentSlot::Feet,
        ]
        .map(|slot| core.equipment.get(slot).map(|stack| stack.item()));
        let can_freeze = freezing::can_freeze(core.kind(), false, armor.iter().map(Option::as_ref));
        let before = core.base.ticks_frozen;
        if inside.thaws {
            core.base.ticks_frozen = 0;
        }
        core.base.ticks_frozen =
            freezing::step(core.base.ticks_frozen, inside.powder_snow, can_freeze);
        if core.base.ticks_frozen != before {
            core.changed(play::metadata::TICKS_FROZEN);
        }
        core.body.in_powder_snow = inside.powder_snow;
        let on_something = (cx.block)(core.on_pos(block_contact::STEP_OFFSET, cx))
            .is_some_and(|s| !tuff_world_model::chunk::is_air(s));
        let slowdown = (core.base.ticks_frozen > 0 && on_something).then(|| {
            (
                freezing::SLOWDOWN * freezing::percent(core.base.ticks_frozen),
                ModifierOperation::AddValue,
            )
        });
        core.set_modifier(
            Attribute::MovementSpeed,
            freezing::SLOWDOWN_MODIFIER,
            slowdown,
        );
        if core.is_alive() && freezing::hurts(core.tick_count, core.base.ticks_frozen, can_freeze) {
            self.hurt_by(
                &Damage::of(&damage_type::FREEZE),
                freezing::FREEZE_DAMAGE,
                cx,
            );
        }
        if let Some(pos) = inside.powder_snow_at {
            self.inside_powder_snow(pos, cx);
        }
    }

    /// Powder snow puts out a burning mob, melting away under it where
    /// mobs may grief, and puffs snowflakes as it wades through.
    fn inside_powder_snow(&mut self, pos: BlockPos, cx: &mut TickContext) {
        let core = &mut self.core;
        let moving = core.base.position[0] != core.body.previous[0]
            || core.base.position[2] != core.body.previous[2];
        if moving && cx.rng.r#gen::<bool>() {
            let [x, _, z] = core.base.position;
            let mut drift = || (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * POWDER_SNOW_DRIFT;
            let (dx, dz) = (drift(), drift());
            let flake = framed(&play::Particles {
                particle: tuff_vanilla_data::registry::ParticleType::Snowflake.id(),
                position: [x, f64::from(pos.y + 1), z],
                spread: [dx, 0.05, dz],
                speed: 1.0,
                count: 0,
                alternative: true,
            });
            cx.effects.push(Effect::ChunkPackets(packets([flake])));
        }
        if core.base.fire > 0 {
            if cx.shared.game_rules.borrow().bool("mob_griefing") {
                cx.effects.push(Effect::DestroyBlock { pos, drop: false });
            }
            core.base.fire = 0;
            let pitch = 1.6 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.4;
            core.sound(SoundEvent::EntityGenericExtinguishFire, 0.7, pitch, cx);
        }
    }

    /// Under water a mob that cannot breathe there runs out of air and
    /// then drowns every 20 ticks; anywhere else its air refills.
    fn breathe(&mut self, cx: &mut TickContext) {
        let core = &mut self.core;
        let before = core.base.air;
        let eye = physics::block_pos(core.eye_position());
        let in_bubbles = (cx.block)(eye).is_some_and(|state| state.block() == Block::BubbleColumn);
        let max = self.species.max_air();
        let breathes = tuff_core::world::entity_in_tag(core.base.kind, "can_breathe_under_water");
        if core.body.eyes_in_water && !in_bubbles && !breathes {
            let bonus = core.attribute(Attribute::OxygenBonus);
            if bonus <= 0.0 || cx.rng.r#gen::<f64>() < 1.0 / (bonus + 1.0) {
                core.base.air -= 1;
            }
            if core.base.air <= DROWNING_AIR {
                core.base.air = 0;
                core.entity_event(event::DROWN_PARTICLES, cx);
                self.hurt_by(&Damage::of(&damage_type::DROWN), 2.0, cx);
            }
        } else if core.base.air < max {
            core.base.air = if self.species.refills_air_at_once() {
                max
            } else {
                (core.base.air + AIR_REFILL).min(max)
            };
        }
        if self.core.base.air != before {
            self.core.changed(play::metadata::AIR_SUPPLY);
        }
    }

    /// A species' own breathing rule ([`Species::air_rules`]): wet (in
    /// water, or in rain too when `in_rain`) refills air to `max`; dry
    /// drains it and, at the common drowning threshold, deals `damage`
    /// and resets to 0, repeating every 20 ticks like drowning does.
    fn own_breathe(&mut self, rules: OwnAirRules, cx: &mut TickContext) {
        let core = &mut self.core;
        let before = core.base.air;
        let wet = core.body.in_water || (rules.in_rain && cx.raining);
        if wet || rules.damage <= 0.0 {
            core.base.air = rules.max;
        } else {
            core.base.air -= 1;
            if core.base.air <= DROWNING_AIR {
                core.base.air = 0;
                self.hurt_by(&Damage::of(rules.damage_kind), rules.damage, cx);
            }
        }
        if self.core.base.air != before {
            self.core.changed(play::metadata::AIR_SUPPLY);
        }
    }

    fn in_wall(&self, cx: &TickContext) -> bool {
        if self.core.phasing {
            return false;
        }
        block_contact::suffocates(self.core.eye_position(), self.core.size.width, cx.block)
    }

    /// Despawning: far from every player, or idle and far enough, for mobs
    /// that may despawn. True when it should go.
    fn check_despawn(&mut self, cx: &mut TickContext) -> bool {
        let peaceful =
            *cx.shared.difficulty.borrow() == tuff_core::world::player::Difficulty::Peaceful;
        if peaceful && !self.core.base.kind.mob().allowed_in_peaceful {
            return true;
        }
        let despawnable = !self.is_persistent();
        let nearest = cx
            .players
            .iter()
            .map(|p| self.core.distance_sq(p.position))
            .reduce(f64::min);
        let Some(distance) = nearest else {
            return despawnable && cx.players_in_dimension > 0;
        };
        let category = self.core.base.kind.category().info();
        let far = (category.despawn_distance * category.despawn_distance) as f64;
        if despawnable && distance > far {
            return true;
        }
        let near = (category.no_despawn_distance * category.no_despawn_distance) as f64;
        let patrolling = self.core.raider.as_ref().is_some_and(|r| r.patrolling);
        if despawnable
            && !patrolling
            && self.core.no_action_time > NO_ACTION_DESPAWN_TICKS
            && cx.rng.gen_range(0..800) == 0
            && distance > near
        {
            return true;
        }
        if distance < near {
            self.core.no_action_time = 0;
        }
        false
    }
}

/// Ticks of Nausea a mob that turned into another kind starts with.
pub const CONVERSION_NAUSEA: i64 = 200;

/// A new mob of species `T` and type `kind` taking over from `from` (a
/// hoglin turning into a zoglin): same place, motion, name, flags, age,
/// effects and memory of a player's hit; the equipment moves over when
/// `keep_equipment`. The caller discards `from`.
pub fn convert<T: Species>(
    from: &mut MobCore,
    kind: EntityType,
    keep_equipment: bool,
    cx: &mut TickContext,
) -> Mob<T> {
    let old = &from.base;
    let mut base = EntityBase::new(
        kind,
        cx.shared.next_entity_id(),
        rand::Rng::r#gen(cx.rng),
        old.position,
        old.velocity,
    );
    base.yaw = old.yaw;
    base.pitch = old.pitch;
    base.on_ground = old.on_ground;
    base.fall_distance = old.fall_distance;
    base.custom_name = old.custom_name.clone();
    base.custom_name_visible = old.custom_name_visible;
    base.fire = old.fire;
    base.invulnerable = old.invulnerable;
    base.invulnerable_time = old.invulnerable_time;
    base.no_gravity = old.no_gravity;
    base.portal_cooldown = old.portal_cooldown;
    base.silent = old.silent;
    base.tags = old.tags.clone();
    let mut to = Mob::<T>::new(base, cx.rng);
    let core = &mut to.core;
    if keep_equipment {
        core.equipment = std::mem::take(&mut from.equipment);
    }
    core.absorption = from.absorption;
    for effect in from.effects.iter() {
        core.add_effect(effect.clone());
    }
    core.last_hurt_by_player = from.last_hurt_by_player;
    core.last_hurt_by_player_time = from.last_hurt_by_player_time;
    core.hurt_time = from.hurt_time;
    core.body_yaw = from.body_yaw;
    core.head_yaw = from.head_yaw;
    core.no_ai = from.no_ai;
    core.persistence_required = from.persistence_required;
    core.leash = from.leash.clone();
    core.left_handed = from.left_handed;
    core.anger = from.anger;
    carry_team_and_seats(from, &to_base_ids(core), cx);
    if from.is_baby() {
        core.set_baby(true);
    }
    if let (Some(old), Some(age)) = (&from.age, &mut core.age) {
        age.age = old.age;
        age.forced_age = old.forced_age;
        age.forced_age_timer = old.forced_age_timer;
    }
    core.refresh_size();
    to
}

fn to_base_ids(core: &MobCore) -> (i32, u128) {
    (core.base.id, core.base.uuid)
}

/// The old mob's team, vehicle and first passenger go to its replacement.
fn carry_team_and_seats(from: &MobCore, to: &(i32, u128), cx: &mut TickContext) {
    let (id, uuid) = *to;
    let teams = cx.shared.scoreboard.teams();
    if let Some(team) = teams.team_of(tuff_core::world::scoreboard::Member::Entity(from.base.uuid))
    {
        cx.shared
            .scoreboard
            .join_entity_to_team(uuid, team.name.clone());
    }
    if let Some(vehicle) = &from.base.vehicle {
        cx.effects.push(Effect::Mount {
            passenger: id,
            vehicle: vehicle.id,
        });
    }
    if let Some(&passenger) = from.base.entity_riders.first() {
        cx.effects.push(Effect::Mount {
            passenger,
            vehicle: id,
        });
    }
}

fn nbt_double(tag: &Tag) -> Option<f64> {
    match *tag {
        Tag::Double(v) => Some(v),
        Tag::Float(v) => Some(v.into()),
        _ => None,
    }
}

/// Saved attribute bases and permanent modifiers; entries naming no known
/// attribute are dropped.
fn load_attributes(core: &mut MobCore, list: &[Tag]) {
    for entry in list {
        let Some(compound) = entry.as_compound() else {
            continue;
        };
        let Some(attribute) = compound.get_str("id").and_then(Attribute::from_name) else {
            continue;
        };
        if let Some(base) = compound.get("base").and_then(nbt_double) {
            core.set_attribute_base(attribute, base);
        }
        let Some(Tag::List(modifiers)) = compound.get("modifiers") else {
            continue;
        };
        for modifier in modifiers.iter().filter_map(Tag::as_compound) {
            let (Some(id), Some(amount), Some(operation)) = (
                modifier.get_str("id"),
                modifier.get("amount").and_then(nbt_double),
                modifier
                    .get_str("operation")
                    .and_then(tuff_vanilla_data::item::ModifierOperation::from_name),
            ) else {
                continue;
            };
            core.put_modifier(
                attribute,
                std::borrow::Cow::Owned(id.to_owned()),
                Some((amount, operation)),
                true,
            );
        }
    }
}

/// The `attributes` list: every base set on the mob and every attribute
/// carrying permanent modifiers.
fn save_attributes(core: &MobCore) -> Vec<Tag> {
    let mut attributes: Vec<Attribute> = core.attribute_bases.iter().map(|(a, _)| *a).collect();
    for modifier in core.modifiers.iter().filter(|m| m.permanent) {
        if !attributes.contains(&modifier.attribute) {
            attributes.push(modifier.attribute);
        }
    }
    attributes
        .into_iter()
        .map(|attribute| {
            let mut entry = Compound::new()
                .with("id", attribute.name())
                .with("base", core.attribute_base(attribute));
            let modifiers: Vec<Tag> = core
                .modifiers
                .iter()
                .filter(|m| m.permanent && m.attribute == attribute)
                .map(|m| {
                    Tag::Compound(
                        Compound::new()
                            .with("id", m.id.as_ref())
                            .with("amount", m.amount)
                            .with("operation", m.operation.name()),
                    )
                })
                .collect();
            if !modifiers.is_empty() {
                entry.insert("modifiers", Tag::List(modifiers));
            }
            Tag::Compound(entry)
        })
        .collect()
}

/// Loads a saved mob of species `S`; the registry's loader for the type.
pub fn load<S: Species>(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(base.uuid as u64);
    let mut mob = Mob::<S>::new(base, &mut rng);
    let core = &mut mob.core;
    if let Some(list) = take(nbt, "attributes", |tag| tag.as_list().map(<[Tag]>::to_vec)) {
        load_attributes(core, &list);
    }
    core.absorption = take_number(nbt, "AbsorptionAmount").unwrap_or(0.0) as f32;
    if let Some(Tag::List(list)) = nbt.remove("active_effects") {
        core.effects = effects::ActiveEffects::from_nbt(&list);
        core.base.glowing_effect = core.effects.has(MobEffect::Glowing);
    }
    core.health = take_number(nbt, "Health")
        .map_or_else(|| core.max_health(), |v| v as f32)
        .clamp(0.0, core.max_health());
    core.hurt_time = take_number(nbt, "HurtTime").map_or(0, |v| v as i16);
    core.death_time = take_number(nbt, "DeathTime").map_or(0, |v| v as i16);
    core.last_hurt_by_player = take(nbt, "last_hurt_by_player", Tag::as_uuid);
    core.last_hurt_by_player_time =
        take_number(nbt, "last_hurt_by_player_memory_time").map_or(0, |v| v as i32);
    core.can_pick_up_loot = take_bool(nbt, "CanPickUpLoot").unwrap_or(false);
    let anger_end = take_number(nbt, "anger_end_time");
    let angry_at = take(nbt, "angry_at", Tag::as_uuid);
    if let (Some(end), Some(target)) = (anger_end, angry_at) {
        core.anger_loaded = Some((end as i64, target));
    }
    core.persistence_required = take_bool(nbt, "PersistenceRequired").unwrap_or(false);
    core.left_handed = take_bool(nbt, "LeftHanded").unwrap_or(false);
    core.no_ai = take_bool(nbt, "NoAI").unwrap_or(false);
    core.leash = tuff_core::world::leash::Leash::load(nbt);
    let radius = take_number(nbt, "home_radius").map_or(-1, |v| v as i32);
    if radius >= 0 {
        let pos = take(nbt, "home_pos", |tag| match tag.as_int_array()? {
            &[x, y, z] => Some(BlockPos::new(x, y, z)),
            _ => None,
        })
        .unwrap_or_default();
        core.home = Some(super::Home { pos, radius });
    }
    core.equipment.load(nbt);
    if let Some(raider) = &mut core.raider {
        super::raider::load(raider, nbt);
    }
    if let Some(age) = &mut core.age {
        age.age = take_number(nbt, "Age").map_or(0, |v| v as i32);
        age.forced_age = take_number(nbt, "ForcedAge").map_or(0, |v| v as i32);
        age.locked = take_bool(nbt, "AgeLocked").unwrap_or(false);
    }
    if let Some(love) = &mut core.love {
        love.ticks = take_number(nbt, "InLove").map_or(0, |v| v as i32);
        love.cause = take(nbt, "LoveCause", Tag::as_uuid);
    }
    if let Some(tame) = &mut core.tame {
        tame.owner = take(nbt, "Owner", Tag::as_uuid);
        tame.sitting = take_bool(nbt, "Sitting").unwrap_or(false);
        tame.pose = tame.sitting;
    }
    core.refresh_size();
    let (core, species) = (&mut mob.core, &mut mob.species);
    species.load(core, nbt);
    core.changed.clear();
    Some(Box::new(mob))
}

const SHEARABLE_SLOTS: [tuff_vanilla_data::item::EquipmentSlot; 8] = {
    use tuff_vanilla_data::item::EquipmentSlot::*;
    [Mainhand, Offhand, Feet, Legs, Chest, Head, Body, Saddle]
};

/// Whether shears take `worn` off: its equippable says so, and an item
/// bound by an enchantment comes off only in creative.
fn shearable<M: tuff_item::item::Mode>(worn: Option<&ItemStack<M>>, creative: bool) -> bool {
    let Some(worn) = worn else {
        return false;
    };
    if !worn.equippable().is_some_and(|eq| eq.can_be_sheared) {
        return false;
    }
    let bound = worn.has_enchantment_effect(|effects| effects.prevent_armor_change);
    !bound || creative
}

impl<S: Species> Mob<S> {
    /// A shears click takes off the first piece of gear that comes off,
    /// dropping it from where riders sit.
    fn shear_equipment(
        &mut self,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item() != tuff_vanilla_data::Item::Shears
            || interaction.sneaking
            || !self.species.can_shear_equipment(&self.core, interaction)
        {
            return None;
        }
        let slot = SHEARABLE_SLOTS
            .into_iter()
            .find(|slot| shearable(self.core.equipment.get(*slot), interaction.creative))?;
        let sound = self.core.equipment.get(slot)?.equippable()?.shearing_sound;
        let worn = self.core.equipment.take(slot)?;
        cx.effects.push(Effect::GameEvent {
            event: tuff_core::world::game_event::GameEvent::Shear,
            position: self.core.base.position,
            source: Some(interaction.player),
        });
        let seats = self.core.kind().passenger_attachments();
        let height = seats.iter().map(|seat| seat[1]).sum::<f64>() / seats.len().max(1) as f64;
        let shown = tuff_item::item::ItemSnapshot::of(&worn);
        self.core
            .drop_item(worn, height * self.core.size_ratio(), cx);
        self.core.sound(sound, 1.0, 1.0, cx);
        cx.effects.push(Effect::Advancement {
            player: interaction.player_uuid,
            event: Box::new(
                tuff_core::world::messages::AdvancementEvent::ShearedEquipment {
                    item: shown,
                    entity: self.core.facts(),
                },
            ),
        });
        Some(tuff_core::world::messages::HandEffect::Damage(1))
    }
}

#[cfg(test)]
mod convert_tests {
    use super::*;
    use tuff_core::world::entity::Mount;

    #[tuff_core::watchdog]
    #[test]
    fn a_converted_mob_keeps_its_hand_grudge_seats_and_growth_timer() {
        crate::tests::with_context(|core, cx| {
            core.left_handed = true;
            core.anger = Some(crate::target::Anger {
                target: 5,
                ticks: 100,
                universal: false,
            });
            core.base.vehicle = Some(Mount {
                id: 99,
                passengers: vec![core.base.id],
            });
            core.base.entity_riders = vec![7, 8];
            core.age = Some(crate::animal::Age {
                forced_age_timer: 40,
                ..Default::default()
            });
            let to = convert::<crate::farm::Pig>(core, EntityType::Pig, false, cx);
            assert!(to.core.left_handed);
            assert_eq!(to.core.anger.as_ref().map(|a| a.ticks), Some(100));
            assert_eq!(to.core.age.as_ref().map(|a| a.forced_age_timer), Some(40));
            let id = to.core.base.id;
            let mounts: Vec<_> = cx
                .effects
                .iter()
                .filter_map(|e| match e {
                    Effect::Mount { passenger, vehicle } => Some((*passenger, *vehicle)),
                    _ => None,
                })
                .collect();
            assert_eq!(mounts, vec![(id, 99), (7, id)]);
        });
    }
}

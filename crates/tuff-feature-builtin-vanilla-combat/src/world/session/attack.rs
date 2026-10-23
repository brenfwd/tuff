//! Melee combat and the damage rules: attacks, sweeps, knockback, armor,
//! shields and enchantment protection.

use rand::Rng;
use tuff_core::world::stats::Stat;
use tuff_vanilla_data::registry::{CustomStat, StatType};

use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};

use std::future::Future;
use std::pin::Pin;

use tuff_core::protocol::play::Serverbound;
use tuff_core::protocol::play::{self, GameMode, PlayerActionStatus};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{Culprit, EntityHit};
use tuff_core::world::inventory::Inventory;
use tuff_core::world::messages::{
    Attack, AttackKind, PlayerEvent, PlayerHandle, PlayerId, RegionRequest,
};
use tuff_core::world::player::exhaustion;
use tuff_core::world::session::hooks::{HookFuture, PlayerDamage, SessionHooks, SessionTick};
use tuff_core::world::session::*;
use tuff_core::world::{damage_type_id, damage_type_in_tag, distance_sq, framed, packets};
use tuff_item::item::{Hurt, ItemStack};
use tuff_vanilla_common::combat;
use tuff_world_model::BlockPos;

#[allow(async_fn_in_trait)]
pub trait CombatExt {
    async fn attack(&mut self, target: PlayerId);
    /// A spear's stab, from the client's Stab player action: the held
    /// item's `piercing_weapon` component gates it (a plain weapon never
    /// reaches here since normal attacks aren't ignored for it), the
    /// attack cooldown must have refilled past `minimum_attack_charge`,
    /// and it hits every player and region entity within `attack_range`
    /// in front of the eye, all at once, with the item's own `damage_type`
    /// (not `player_attack`) instead of the usual cooldown-scaled hit.
    async fn stab(&mut self);
    async fn post_piercing_attack(&mut self, weapon: &tuff_item::item::ItemPicture);
    /// One tick of a held kinetic weapon (a spear charged by holding use):
    /// past the item's delay, whatever lies along the look ray inside the
    /// attack range is hit by the user's own speed, once per contact
    /// cooldown, with damage, knockback and dismounting gated by the
    /// component's speed and duration conditions. Targets are treated as
    /// standing still, so the relative speed is the user's own.
    async fn kinetic_tick(&mut self);
    /// A mace smash landed on something at `impact`: cancels this
    /// player's own fall damage from the fall that powered it, plays the
    /// smash sound and particles, and pushes every other nearby player
    /// (not the primary target) away from the impact.
    async fn smash_attack(&mut self, fall_distance: f64, impact: [f64; 3], primary_target: i32);
    /// The held weapon's Wind Burst `explode` effects after a smash.
    async fn wind_burst(&mut self, victim: EntityType, fall_distance: f64);
    /// The entity a clicked id belongs to: a dragon's parts carry the ids
    /// after the dragon's own. The region checks the reach to the part.
    fn part_owner(&self, target: i32) -> (i32, Option<usize>);
    /// Sends an attack on a region entity to the region its chunk belongs
    /// to.
    async fn attack_entity(
        &mut self,
        target: i32,
        attack: tuff_core::world::entity::EntityAttack,
        sweep: Option<combat::Sweep>,
    );
    /// A right click on an entity: the region decides what it does with the
    /// held item.
    async fn interact(&mut self, target: i32, off_hand: bool, sneaking: bool, location: [f64; 3]);
    /// An attack of this player's landed on `target`: the weapon wears, a
    /// knockback hit ends the sprint, and a sweep hits the players beside
    /// the target.
    async fn attack_landed(
        &mut self,
        weapon: Option<tuff_item::item::ItemSnapshot>,
        target: PlayerId,
        target_position: [f64; 3],
        knockback: bool,
        sweep: Option<combat::Sweep>,
        health_lost: f32,
    );
    async fn sweep(
        &mut self,
        weapon: Option<&tuff_item::item::ItemPicture>,
        target: PlayerId,
        target_position: [f64; 3],
        sweep: combat::Sweep,
    );
    /// A pure knockback push (a mace smash's area knockback, or Wind Burst
    /// launching this player): applies this player's own knockback
    /// resistance to `direction`/`power` before pushing.
    async fn knocked(&mut self, direction: [f64; 2], power: f64);
    async fn attacked(&mut self, attack: Attack);
    /// The enchantments on this player's equipment and on the attacker's
    /// weapon that act once a hit lands (Thorns, Fire Aspect).
    async fn post_attack(
        &mut self,
        attack: &Attack,
        context: &combat::DamageContext<'_>,
        attacker: (PlayerId, String),
    );
    /// The item held up as a shield, once raised long enough to block.
    fn blocking_with(
        &self,
    ) -> Option<(bool, Item, &'static tuff_vanilla_data::item::BlocksAttacks)>;
    /// Hurts this player the way the game does: difficulty, a raised
    /// shield, the invulnerability window, armor, effects and enchantment
    /// protection, then absorption; and works out the push the hit gives.
    async fn resolve_hurt(&mut self, source: DamageSource, amount: f32) -> HurtResult;
    /// Damage after armor, effects and enchantments, soaked by absorption
    /// and taken off the health.
    async fn actually_hurt(&mut self, source: &DamageSource, amount: f32);
    /// Wears the armor pieces (or other damageable equipment) in `slots` as
    /// a hit of `damage` does.
    async fn hurt_equipment(&mut self, kind: &DamageType, damage: f32, slots: &[EquipmentSlot]);
}

impl CombatExt for PlayerSession {
    async fn attack(&mut self, target: PlayerId) {
        if target == self.id || self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        if self
            .inventory
            .hand(false)
            .is_some_and(|stack| stack.piercing_weapon().is_some())
        {
            // A spear's `piercing_weapon` component makes the game ignore
            // the normal Attack packet entirely: it only deals damage
            // through the Stab player action.
            return;
        }
        let (target, part) = self.part_owner(target);
        let (position, victim, handle) = match self.view.entities.visible_player(target) {
            Some((position, handle)) => (position, EntityType::Player, Some(handle.clone())),
            None => match self.view.entities.visible_entity(target) {
                Some((kind, _, position)) => (position, kind, None),
                None => return,
            },
        };
        if part.is_none() && distance_sq(self.eye(), position) > ATTACK_REACH * ATTACK_REACH {
            return;
        }
        if handle.is_none() && tuff_core::world::entity_in_tag(victim, "redirectable_projectile") {
            self.actions.attack_reset_tick = self.tick;
            let deflect = tuff_core::world::entity::EntityAttack {
                attacker: self.id,
                attacker_uuid: self.profile.id,
                attacker_position: self.pose.position,
                attacker_yaw: self.pose.yaw,
                attacker_pitch: self.pose.pitch,
                damage: 0.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            };
            self.attack_entity(target, deflect, None).await;
            return;
        }
        let equipment = self.equipment();
        let effects = self.effect_levels();
        let worn: Vec<combat::Equipped> = equipment
            .iter()
            .map(|(slot, stack)| (*slot, stack.any()))
            .collect();
        let attacker = combat::Attacker {
            modifiers: combat::Modifiers {
                equipment: &worn,
                effects: &effects,
            },
            ticks_since_reset: self.tick.saturating_sub(self.actions.attack_reset_tick) as u32,
            fall_distance: self.body.fall.distance,
            on_ground: self.pose.on_ground,
            sprinting: self.pose.sprinting,
            in_water: self.body.fall.in_water(),
            climbable: self.surroundings.climbable,
            known_speed_sq: self.sync.known_movement[0].powi(2)
                + self.sync.known_movement[2].powi(2),
        };
        let melee = combat::melee(&attacker, victim, &mut self.rng);
        self.actions.attack_reset_tick = self.tick;
        let Some(melee) = melee else {
            return;
        };
        if melee.sprint {
            self.sound_at(SoundEvent::EntityPlayerAttackKnockback, self.pose.position)
                .await;
        }
        if let Some(fall_distance) = melee.smash {
            self.smash_attack(fall_distance, position, target).await;
            self.wind_burst(victim, fall_distance).await;
        }
        let weapon = self
            .inventory
            .hand(false)
            .map(tuff_item::item::ItemSnapshot::of);
        match handle {
            Some(handle) => {
                handle.send(PlayerEvent::Attacked(Attack {
                    attacker: self.conn.handle.clone(),
                    attacker_name: self.profile.name.clone(),
                    attacker_uuid: self.profile.id,
                    weapon,
                    attacker_position: self.pose.position,
                    attacker_yaw: self.pose.yaw,
                    damage: melee.damage,
                    knockback: melee.knockback,
                    kind: AttackKind::Direct {
                        critical: melee.critical,
                        magic: melee.magic,
                        full_strength: melee.full_strength,
                        sweep: melee.sweep,
                        smash: melee.smash,
                    },
                }));
            }
            None => {
                let attack = tuff_core::world::entity::EntityAttack {
                    attacker: self.id,
                    attacker_uuid: self.profile.id,
                    attacker_position: self.pose.position,
                    attacker_yaw: self.pose.yaw,
                    attacker_pitch: self.pose.pitch,
                    damage: melee.damage,
                    knockback: melee.knockback,
                    weapon,
                    critical: melee.critical,
                    magic: melee.magic,
                    sweep: false,
                    part,
                };
                self.attack_entity(target, attack, melee.sweep).await;
            }
        }
    }

    /// A spear's stab, from the client's Stab player action: the held
    /// item's `piercing_weapon` component gates it (a plain weapon never
    /// reaches here since normal attacks aren't ignored for it), the
    /// attack cooldown must have refilled past `minimum_attack_charge`,
    /// and it hits every player and region entity within `attack_range`
    /// in front of the eye, all at once, with the item's own `damage_type`
    /// (not `player_attack`) instead of the usual cooldown-scaled hit.
    async fn stab(&mut self) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(stack) = self
            .inventory
            .hand(false)
            .map(tuff_item::item::ItemSnapshot::of)
        else {
            return;
        };
        let Some(piercing) = stack.piercing_weapon() else {
            return;
        };
        let attack_speed = self.attribute(Attribute::AttackSpeed);
        let ticks_since_reset = self.tick.saturating_sub(self.actions.attack_reset_tick) as u32;
        let strength = combat::attack_strength(ticks_since_reset, attack_speed);
        if strength < stack.item().minimum_attack_charge() {
            return;
        }
        self.actions.attack_reset_tick = self.tick;
        let Some(range) = stack.item().attack_range() else {
            return;
        };
        let kind = stack
            .item()
            .damage_type()
            .unwrap_or(&damage_type::PLAYER_ATTACK);
        let base = self.attribute(Attribute::AttackDamage) as f32;
        let knockback_attribute = self.attribute(Attribute::AttackKnockback) as f32;
        let forward = tuff_core::world::raycast::view_vector(self.pose.yaw, self.pose.pitch);
        let eye = self.eye();
        let creative = self.mode.abilities.instabuild;
        let clear = |this: &Self, position: [f64; 3]| {
            let along = [0, 1, 2]
                .map(|i| (position[i] - eye[i]) * forward[i])
                .iter()
                .sum::<f64>()
                .max(0.0);
            !super::kinetic::stab_occluded(this, eye, forward, along)
        };
        let players: Vec<(PlayerId, [f64; 3], PlayerHandle)> = self
            .view
            .entities
            .visible_players()
            .filter(|(id, position, _)| {
                *id != self.id
                    && combat::in_stab_reach(eye, forward, *position, range, creative)
                    && clear(self, *position)
            })
            .map(|(id, position, handle)| (id, position, handle.clone()))
            .collect();
        let entities: Vec<(i32, EntityType, [f64; 3])> = self
            .view
            .entities
            .visible_entities()
            .filter(|(_, _, _, position)| {
                combat::in_stab_reach(eye, forward, *position, range, creative)
                    && clear(self, *position)
            })
            .map(|(id, kind, _, position)| (id, kind, position))
            .collect();
        let weapon = Some(stack.clone());
        let mut hit_something = false;
        let dismountable = |victim: EntityType| {
            piercing.dismounts
                && !tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::entity_type::key::CANNOT_BE_DISMOUNTED_BY_ITEM_USAGE,
                    victim,
                )
        };
        for (_, _, handle) in &players {
            hit_something = true;
            if dismountable(EntityType::Player) {
                handle.send(PlayerEvent::StabDismounted);
            }
            let context = combat::DamageContext {
                kind,
                victim: EntityType::Player,
                attacker: Some(EntityType::Player),
                direct_attacker: Some(EntityType::Player),
                is_direct: true,
                direct_attacker_fall_distance: None,
                direct_attacker_flying: None,
            };
            let damage = combat::modify_damage(&stack, &context, base, &mut self.rng);
            let knockback = if piercing.deals_knockback {
                combat::modify_knockback(&stack, &context, knockback_attribute, &mut self.rng)
            } else {
                0.0
            };
            handle.send(PlayerEvent::Attacked(Attack {
                attacker: self.conn.handle.clone(),
                attacker_name: self.profile.name.clone(),
                attacker_uuid: self.profile.id,
                weapon: weapon.as_ref().map(tuff_item::item::ItemSnapshot::of),
                attacker_position: self.pose.position,
                attacker_yaw: self.pose.yaw,
                damage,
                knockback,
                kind: AttackKind::Direct {
                    critical: false,
                    magic: false,
                    full_strength: true,
                    sweep: None,
                    smash: None,
                },
            }));
        }
        for (id, victim, _) in entities {
            hit_something = true;
            if dismountable(victim)
                && let Some((_, chunk, _)) = self.view.entities.visible_entity(id)
                && let Some(region) = self.region(chunk.region()).await
            {
                region
                    .request(RegionRequest::StabDismount { target: id })
                    .await;
            }
            let context = combat::DamageContext::melee(victim);
            let damage = combat::modify_damage(&stack, &context, base, &mut self.rng);
            let knockback = if piercing.deals_knockback {
                combat::modify_knockback(&stack, &context, knockback_attribute, &mut self.rng)
            } else {
                0.0
            };
            let attack = tuff_core::world::entity::EntityAttack {
                attacker: self.id,
                attacker_uuid: self.profile.id,
                attacker_position: self.pose.position,
                attacker_yaw: self.pose.yaw,
                attacker_pitch: self.pose.pitch,
                damage,
                knockback,
                weapon: weapon.as_ref().map(tuff_item::item::ItemSnapshot::of),
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            };
            self.attack_entity(id, attack, None).await;
        }
        if let Some(sound) = piercing.sound {
            self.sound_at(sound, self.pose.position).await;
        }
        if hit_something && let Some(hit_sound) = piercing.hit_sound {
            self.sound_at(hit_sound, self.pose.position).await;
        }
        self.post_piercing_attack(&stack).await;
    }

    async fn post_piercing_attack(&mut self, weapon: &tuff_item::item::ItemPicture) {
        let effects: Pin<Box<dyn Future<Output = ()> + Send + '_>> = Box::pin(
            crate::world::session::enchant::post_piercing_attack(self, weapon),
        );
        effects.await;
    }

    async fn kinetic_tick(&mut self) {
        super::kinetic::KineticExt::kinetic_stab(self).await;
    }

    /// A mace smash landed on something at `impact`: cancels this
    /// player's own fall damage from the fall that powered it, plays the
    /// smash sound and particles, and pushes every other nearby player
    /// (not the primary target) away from the impact.
    async fn smash_attack(&mut self, fall_distance: f64, impact: [f64; 3], primary_target: i32) {
        self.body.fall.distance = 0.0;
        let sound = if fall_distance > combat::SMASH_ATTACK_HEAVY_THRESHOLD {
            SoundEvent::ItemMaceSmashGroundHeavy
        } else {
            SoundEvent::ItemMaceSmashGround
        };
        self.sound_at(sound, self.pose.position).await;
        let particles = packets([framed(&play::LevelEvent {
            event: play::level_event::PARTICLES_SMASH_ATTACK,
            position: BlockPos::new(
                impact[0].floor() as i32,
                impact[1].floor() as i32,
                impact[2].floor() as i32,
            ),
            data: 750,
            global: false,
        })]);
        self.broadcast_chunk(particles).await;
        let pushed: Vec<(PlayerHandle, [f64; 2], f64)> = self
            .view
            .entities
            .visible_players()
            .filter(|(id, _, _)| *id != self.id && *id != primary_target)
            .filter_map(|(_, position, handle)| {
                let distance = distance_sq(impact, position).sqrt();
                let power = combat::smash_knockback_power(distance, fall_distance, 0.0);
                (power > 0.0).then(|| {
                    (
                        handle.clone(),
                        [position[0] - impact[0], position[2] - impact[2]],
                        power,
                    )
                })
            })
            .collect();
        for (handle, direction, power) in pushed {
            handle.send(PlayerEvent::Knocked { direction, power });
        }
    }

    async fn wind_burst(&mut self, victim: EntityType, fall_distance: f64) {
        let Some(weapon) = self
            .inventory
            .hand(false)
            .map(tuff_item::item::ItemSnapshot::of)
        else {
            return;
        };
        let context =
            combat::DamageContext::melee(victim).with_attacker_state(fall_distance, false);
        let bursts: Vec<_> = combat::post_attack(
            &[(EquipmentSlot::Mainhand, weapon.any())],
            combat::Side::Attacker,
            &context,
            &mut self.rng,
        )
        .into_iter()
        .filter_map(|effect| match effect.effect {
            combat::PostAttack::Explode {
                radius,
                knockback,
                offset,
            } if effect.affected == combat::Side::Attacker => Some((radius, knockback, offset)),
            _ => None,
        })
        .collect();
        let position = self.pose.position;
        let Some(region) = self.region_of(BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        )) else {
            return;
        };
        for (radius, knockback, offset) in bursts {
            let igniter = tuff_core::world::messages::Igniter {
                id: self.id,
                uuid: self.profile.id,
                name: self.profile.name.clone(),
            };
            let center = [0, 1, 2].map(|i| position[i] + offset[i]);
            region
                .request(tuff_core::world::messages::RegionRequest::Run(
                    tuff_core::world::region_api::RegionTask::new(move |world| {
                        world.explode(tuff_core::world::explosion::Explosion {
                            center,
                            radius,
                            fire: false,
                            blocks: tuff_core::world::explosion::BlockInteraction::Trigger(
                                tuff_core::world::explosion::WindBurst {
                                    knockback,
                                    sound: SoundEvent::EntityWindChargeWindBurst,
                                    source: EntityType::Player,
                                    spares_fall: true,
                                },
                            ),
                            damage_type: &damage_type::WIND_CHARGE,
                            igniter: Some(igniter),
                            culprit: None,
                            dangerous_wither_skull: false,
                            spares_nether_portals: false,
                        });
                    }),
                ))
                .await;
        }
    }

    /// The entity a clicked id belongs to: a dragon's parts carry the ids
    /// after the dragon's own. The region checks the reach to the part.
    fn part_owner(&self, target: i32) -> (i32, Option<usize>) {
        if self.view.entities.visible_entity(target).is_some() {
            return (target, None);
        }
        for part in 0..tuff_vanilla_data::dragon::PARTS.len() {
            let owner = target - 1 - part as i32;
            if let Some((EntityType::EnderDragon, _, _)) = self.view.entities.visible_entity(owner)
            {
                return (owner, Some(part));
            }
        }
        (target, None)
    }

    /// Sends an attack on a region entity to the region its chunk belongs
    /// to.
    async fn attack_entity(
        &mut self,
        target: i32,
        attack: tuff_core::world::entity::EntityAttack,
        sweep: Option<combat::Sweep>,
    ) {
        let Some((_, chunk, _)) = self.view.entities.visible_entity(target) else {
            return;
        };
        if let Some(region) = self.region(chunk.region()).await {
            region
                .request(RegionRequest::AttackEntity {
                    attacker: self.conn.handle.clone(),
                    target,
                    attack,
                    sweep,
                })
                .await;
        }
    }

    /// A right click on an entity: the region decides what it does with the
    /// held item.
    async fn interact(&mut self, target: i32, off_hand: bool, sneaking: bool, location: [f64; 3]) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let riding = self.ride.riding.is_some() || self.ride.boarding_cooldown > 0;
        let Some((_, chunk, position)) = self.view.entities.visible_entity(target) else {
            return;
        };
        if distance_sq(self.eye(), position) > (ATTACK_REACH + 3.0).powi(2) {
            return;
        }
        let Some(region) = self.region(chunk.region()).await else {
            return;
        };
        let creative = self.mode.abilities.instabuild;
        let slot = self.inventory.hand_slot(off_hand);
        let hand = if creative {
            self.inventory.hand(off_hand).map(|stack| {
                ItemStack::mint_like(stack, stack.count(), tuff_item::item::MintReason::Creative)
            })
        } else {
            self.inventory.take_hand(off_hand)
        };
        let interaction = tuff_core::world::entity::EntityInteraction {
            player: self.id,
            player_uuid: self.profile.id,
            position: self.pose.position,
            location,
            stack: hand.as_ref().map(tuff_item::item::ItemSnapshot::of),
            hand: tuff_core::world::entity::HandOffer::from_slot(slot, hand),
            off_hand,
            creative,
            sneaking,
            riding,
            hero: self.body.effects.amplifier(MobEffect::HeroOfTheVillage),
        };
        if let Err(refused) = region
            .try_request(RegionRequest::InteractEntity {
                player: self.conn.handle.clone(),
                target,
                interaction,
            })
            .await
            && let RegionRequest::InteractEntity { interaction, .. } = *refused
            && let Some(stack) = interaction.hand.into_inner()
        {
            if creative {
                stack.burn(tuff_item::item::BurnReason::Creative);
            } else if let Some(leftover) = self.inventory.put_back(slot, stack) {
                let at = self.pose.position;
                self.spawn_item(leftover, at, [0.0; 3], 0).await;
            }
        }
    }

    /// An attack of this player's landed on `target`: the weapon wears, a
    /// knockback hit ends the sprint, and a sweep hits the players beside
    /// the target.
    async fn attack_landed(
        &mut self,
        weapon: Option<tuff_item::item::ItemSnapshot>,
        target: PlayerId,
        target_position: [f64; 3],
        knockback: bool,
        sweep: Option<combat::Sweep>,
        health_lost: f32,
    ) {
        if knockback {
            self.pose.sprinting = false;
        }
        if let Some(sweep) = sweep
            && !self.is_dead()
        {
            self.sweep(weapon.as_deref(), target, target_position, sweep)
                .await;
        }
        if let Some(weapon) = weapon
            && let Some(held) = self.inventory.hand(false)
            && held.item() == weapon.item()
        {
            let wear = held.weapon().map_or(0, |w| w.item_damage_per_attack as u32);
            self.hurt_hand(false, wear).await;
        }
        if health_lost < 3.402_823_5e37 {
            self.progress
                .stats
                .award_custom(CustomStat::DamageDealt, (health_lost * 10.0).round() as i32);
        }
        self.exhaust(exhaustion::ATTACK);
    }

    async fn sweep(
        &mut self,
        weapon: Option<&tuff_item::item::ItemPicture>,
        target: PlayerId,
        target_position: [f64; 3],
        sweep: combat::Sweep,
    ) {
        self.sound_at(SoundEvent::EntityPlayerAttackSweep, self.pose.position)
            .await;
        let caught: Vec<PlayerHandle> = self
            .view
            .entities
            .visible_players()
            .filter(|(id, position, _)| {
                *id != target
                    && *id != self.id
                    && combat::in_sweep(self.pose.position, target_position, *position)
            })
            .map(|(_, _, handle)| handle.clone())
            .collect();
        let caught_entities: Vec<(i32, EntityType, [f64; 3])> = self
            .view
            .entities
            .visible_entities()
            .filter(|(id, _, _, position)| {
                *id != target && combat::in_sweep(self.pose.position, target_position, *position)
            })
            .map(|(id, kind, _, position)| (id, kind, position))
            .collect();
        for (id, kind, _) in caught_entities {
            let damage =
                combat::sweep_damage(weapon.map(ItemStack::any), sweep, kind, &mut self.rng);
            let attack = tuff_core::world::entity::EntityAttack {
                attacker: self.id,
                attacker_uuid: self.profile.id,
                attacker_position: self.pose.position,
                attacker_yaw: self.pose.yaw,
                attacker_pitch: self.pose.pitch,
                damage,
                knockback: combat::SWEEP_KNOCKBACK,
                weapon: weapon.map(tuff_item::item::ItemSnapshot::of),
                critical: false,
                magic: false,
                sweep: true,
                part: None,
            };
            self.attack_entity(id, attack, None).await;
        }
        for handle in caught {
            let damage = combat::sweep_damage(
                weapon.map(ItemStack::any),
                sweep,
                EntityType::Player,
                &mut self.rng,
            );
            handle.send(PlayerEvent::Attacked(Attack {
                attacker: self.conn.handle.clone(),
                attacker_name: self.profile.name.clone(),
                attacker_uuid: self.profile.id,
                weapon: weapon.map(tuff_item::item::ItemSnapshot::of),
                attacker_position: self.pose.position,
                attacker_yaw: self.pose.yaw,
                damage,
                knockback: combat::SWEEP_KNOCKBACK,
                kind: AttackKind::Sweep,
            }));
        }
        let (dx, dz) = combat::facing_knockback_direction(self.pose.yaw);
        let particle = packets([framed(&play::Particles {
            particle: tuff_vanilla_data::registry::ParticleType::SweepAttack.id(),
            position: [
                self.pose.position[0] - dx,
                self.pose.position[1] + self.height() / 2.0,
                self.pose.position[2] - dz,
            ],
            spread: [-dx as f32, 0.0, -dz as f32],
            speed: 0.0,
            count: 0,
            alternative: false,
        })]);
        self.broadcast_chunk(particle).await;
    }

    /// A pure knockback push (a mace smash's area knockback, or Wind Burst
    /// launching this player): applies this player's own knockback
    /// resistance to `direction`/`power` before pushing.
    async fn knocked(&mut self, direction: [f64; 2], power: f64) {
        if self.is_dead() {
            return;
        }
        let resistance = self.attribute(Attribute::KnockbackResistance);
        let power = power * (1.0 - resistance);
        let len = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
        if power <= 0.0 {
            return;
        }
        let push = if len < 1.0e-5 {
            [0.0, power, 0.0]
        } else {
            [
                direction[0] / len * power,
                combat::SMASH_KNOCKBACK_POWER,
                direction[1] / len * power,
            ]
        };
        self.motion.velocity_sent.push(self.tick, push);
        let effects = packets([framed(&play::SetEntityMotion {
            entity_id: self.id,
            velocity: push,
        })]);
        self.broadcast_entity(effects, true).await;
    }

    async fn attacked(&mut self, attack: Attack) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let smash_fall = match attack.kind {
            AttackKind::Direct { smash, .. } => smash,
            AttackKind::Sweep | AttackKind::Knockback => None,
        };
        if attack.kind == AttackKind::Knockback {
            let (dx, dz) = combat::facing_knockback_direction(attack.attacker_yaw);
            let motion = combat::knockback(
                self.sync.known_movement,
                attack.knockback as f64,
                dx,
                dz,
                self.pose.on_ground,
                self.attribute(Attribute::KnockbackResistance),
            );
            self.motion.velocity_sent.push(self.tick, motion);
            let effects = packets([framed(&play::SetEntityMotion {
                entity_id: self.id,
                velocity: motion,
            })]);
            self.broadcast_entity(effects, true).await;
            return;
        }
        let kind: &'static DamageType = if smash_fall.is_some() {
            &damage_type::MACE_SMASH
        } else {
            &damage_type::PLAYER_ATTACK
        };
        let mut context = combat::DamageContext::melee(EntityType::Player);
        context.kind = kind;
        if let Some(fall) = smash_fall {
            context = context.with_attacker_state(fall, false);
        }
        let attacker = (attack.attacker.id, attack.attacker_name.clone());
        let hurt = self
            .hurt(
                DamageSource {
                    kind,
                    attacker: Some(
                        Culprit::player(attacker.0, attack.attacker_uuid, attacker.1.clone())
                            .holding(attack.weapon.as_deref()),
                    ),
                    projectile: None,
                    position: Some(attack.attacker_position),
                    weapon: attack.weapon.clone(),
                    direct: true,
                },
                attack.damage,
            )
            .await;
        if !hurt.landed {
            if matches!(attack.kind, AttackKind::Direct { .. }) {
                self.sound_at(
                    SoundEvent::EntityPlayerAttackNodamage,
                    attack.attacker_position,
                )
                .await;
            }
            return;
        }
        let mut effects = Vec::new();
        if let Some(mut motion) = hurt.motion {
            if attack.knockback > 0.0 {
                let (dx, dz) = combat::facing_knockback_direction(attack.attacker_yaw);
                motion = combat::knockback(
                    motion,
                    attack.knockback as f64,
                    dx,
                    dz,
                    self.pose.on_ground,
                    self.attribute(Attribute::KnockbackResistance),
                );
            }
            self.motion.velocity_sent.push(self.tick, motion);
            effects.push(framed(&play::SetEntityMotion {
                entity_id: self.id,
                velocity: motion,
            }));
        }
        if let AttackKind::Direct {
            critical,
            magic,
            full_strength,
            sweep,
            smash: _,
        } = attack.kind
        {
            if critical {
                self.sound_at(SoundEvent::EntityPlayerAttackCrit, attack.attacker_position)
                    .await;
                effects.push(framed(&play::Animate {
                    entity_id: self.id,
                    animation: play::animation::CRITICAL_HIT,
                }));
            } else if sweep.is_none() {
                let sound = if full_strength {
                    SoundEvent::EntityPlayerAttackStrong
                } else {
                    SoundEvent::EntityPlayerAttackWeak
                };
                self.sound_at(sound, attack.attacker_position).await;
            }
            if magic {
                effects.push(framed(&play::Animate {
                    entity_id: self.id,
                    animation: play::animation::MAGIC_CRITICAL_HIT,
                }));
            }
            attack.attacker.send(PlayerEvent::AttackLanded {
                weapon: attack.weapon.clone(),
                target: self.id,
                target_position: self.pose.position,
                knockback: attack.knockback > 0.0,
                sweep,
                health_lost: hurt.health_lost,
                target_kind: tuff_vanilla_data::registry::EntityType::Player,
                dealt: attack.damage,
            });
        }
        if !effects.is_empty() {
            self.broadcast_entity(effects.into(), true).await;
        }
        if matches!(attack.kind, AttackKind::Direct { .. }) && hurt.health_lost > 2.0 {
            let particle = packets([framed(&play::Particles {
                particle: tuff_vanilla_data::registry::ParticleType::DamageIndicator.id(),
                position: [
                    self.pose.position[0],
                    self.pose.position[1] + self.height() / 2.0,
                    self.pose.position[2],
                ],
                spread: [0.1, 0.0, 0.1],
                speed: 0.2,
                count: (hurt.health_lost as f64 * 0.5) as i32,
                alternative: false,
            })]);
            self.broadcast_chunk(particle).await;
        }
        self.post_attack(&attack, &context, attacker).await;
        if self.is_dead() {
            let kill = self.killed_by_player(&attack);
            attack
                .attacker
                .send(PlayerEvent::KilledEntity(Box::new(kill)));
        }
    }

    /// The enchantments on this player's equipment and on the attacker's
    /// weapon that act once a hit lands (Thorns, Fire Aspect).
    async fn post_attack(
        &mut self,
        attack: &Attack,
        context: &combat::DamageContext<'_>,
        attacker: (PlayerId, String),
    ) {
        let equipment = self.equipment();
        let worn: Vec<combat::Equipped> = equipment
            .iter()
            .map(|(slot, stack)| (*slot, stack.any()))
            .collect();
        let own = combat::post_attack(&worn, combat::Side::Victim, context, &mut self.rng);
        let weapon = match &attack.weapon {
            Some(weapon) => combat::post_attack(
                &[(EquipmentSlot::Mainhand, weapon.any())],
                combat::Side::Attacker,
                context,
                &mut self.rng,
            ),
            None => Vec::new(),
        };
        for (effect, mine) in own
            .into_iter()
            .map(|effect| (effect, true))
            .chain(weapon.into_iter().map(|effect| (effect, false)))
        {
            let on_me = effect.affected == combat::Side::Victim;
            match effect.effect {
                combat::PostAttack::Ignite(seconds) if on_me => self.ignite(seconds),
                combat::PostAttack::Damage { kind, amount } if on_me => {
                    self.hurt_by(
                        amount,
                        kind,
                        Some(attacker.clone()),
                        Some(attack.attacker_position),
                    )
                    .await;
                }
                combat::PostAttack::Damage { kind, amount } if mine => {
                    attack.attacker.send(PlayerEvent::Hurt {
                        amount,
                        kind,
                        attacker: Some((self.id, self.profile.name.clone())),
                        position: Some(self.pose.position),
                    });
                }
                combat::PostAttack::ItemDamage(amount) if mine => {
                    self.wear_slot(effect.slot, amount).await;
                }
                combat::PostAttack::ApplyMobEffect {
                    effect: mob_effect,
                    amplifier,
                    ticks,
                } if on_me => {
                    self.add_effect(ActiveEffect::timed(mob_effect, i64::from(ticks), amplifier))
                        .await;
                }
                _ => {}
            }
        }
    }

    /// The item held up as a shield, once raised long enough to block.
    fn blocking_with(
        &self,
    ) -> Option<(bool, Item, &'static tuff_vanilla_data::item::BlocksAttacks)> {
        let using = self.actions.using?;
        let shield = using.item.blocks_attacks()?;
        let elapsed = using.total - using.remaining;
        (elapsed >= shield.block_delay_ticks()).then_some((using.off_hand, using.item, shield))
    }

    /// Hurts this player the way the game does: difficulty, a raised
    /// shield, the invulnerability window, armor, effects and enchantment
    /// protection, then absorption; and works out the push the hit gives.
    async fn resolve_hurt(&mut self, source: DamageSource, amount: f32) -> HurtResult {
        let kind = source.kind;
        if self.is_dead() || self.mode.abilities.is_immune_to(kind) {
            return HurtResult::default();
        }
        let equipment = self.equipment();
        let worn: Vec<combat::Equipped> = equipment
            .iter()
            .map(|(slot, stack)| (*slot, stack.any()))
            .collect();
        if combat::immune_to_damage(&worn, &damage_context(&source), &mut self.rng) {
            return HurtResult::default();
        }
        let by_living = source.attacker.is_some();
        let by_player = source.attacker.as_ref().is_some_and(Culprit::is_player);
        let mut amount = amount.max(0.0);
        if combat::scales_with_difficulty(kind, by_living, by_player) {
            amount = combat::scale_for_difficulty(amount, self.shared.difficulty());
        }
        if amount == 0.0 {
            return HurtResult::default();
        }
        if damage_type_in_tag(kind, "minecraft:is_fire")
            && self.body.effects.has(MobEffect::FireResistance)
        {
            return HurtResult::default();
        }
        if self.sleep.sleeping.is_some() {
            self.stop_sleeping(true).await;
        }
        let resistance = if source.position.is_some() {
            self.attribute(Attribute::KnockbackResistance)
        } else {
            0.0
        };
        let mut motion = [0.0; 3];
        let mut knocked = false;
        let mut blocked = 0.0;
        let mut shield_blocked = false;
        let mut shield_sound = None;
        if let Some((off_hand, item, shield)) = self.blocking_with() {
            let angle = combat::blocking_angle(self.pose.position, self.pose.yaw, source.position);
            if let Some(stopped) = combat::shield_blocks(shield, kind, amount, angle) {
                blocked = stopped;
                let wear = shield.item_damage.apply(stopped);
                if wear > 0 {
                    self.hurt_hand(off_hand, wear as u32).await;
                }
                if stopped > 0.0 {
                    shield_sound = shield.block_sound;
                }
                let by_hand = stopped > 0.0
                    && source.attacker.is_some()
                    && source.direct
                    && !damage_type_in_tag(kind, "minecraft:is_projectile");
                shield_blocked = by_hand;
                if by_hand
                    && stopped < amount
                    && let Some(from) = source.position
                {
                    motion = combat::knockback(
                        motion,
                        combat::SHIELD_KNOCKBACK,
                        self.pose.position[0] - from[0],
                        self.pose.position[2] - from[2],
                        self.pose.on_ground,
                        resistance,
                    );
                    knocked = true;
                }
                let disable = source
                    .weapon
                    .as_ref()
                    .and_then(|weapon| weapon.weapon())
                    .map_or(0, |weapon| {
                        shield.disable_ticks(weapon.disable_blocking_for_seconds)
                    });
                if by_hand && disable > 0 {
                    self.add_cooldown(item, disable);
                    self.stop_using();
                    if let Some(sound) = shield.disabled_sound {
                        let pitch = 0.8 + self.rng.r#gen::<f32>() * 0.4;
                        self.player_sound(sound, 0.8, pitch).await;
                    }
                }
            }
        }
        let mut dealt = amount - blocked;
        if damage_type_in_tag(kind, "minecraft:damages_helmet")
            && self.inventory.equipped(EquipmentSlot::Head).is_some()
        {
            self.hurt_equipment(kind, dealt, &[EquipmentSlot::Head])
                .await;
            dealt *= 0.75;
        }
        let bypasses = damage_type_in_tag(kind, "minecraft:bypasses_cooldown");
        let Some((through, full)) = self.body.health.window(dealt, bypasses) else {
            return HurtResult {
                shield_blocked,
                ..HurtResult::default()
            };
        };
        if let Some(tenths) = combat::stat_tenths(blocked) {
            self.progress
                .stats
                .award_custom(CustomStat::DamageBlockedByShield, tenths);
        }
        let health_before = self.body.health.value;
        self.actually_hurt(&source, through).await;
        let health_lost = health_before - self.body.health.value;
        let fully_blocked = blocked > 0.0 && dealt <= 0.0;
        let mut pushed = false;
        if full {
            match shield_sound {
                Some(sound) if blocked > 0.0 => {
                    let pitch = 0.8 + self.rng.r#gen::<f32>() * 0.4;
                    self.player_sound(sound, 1.0, pitch).await;
                }
                _ => {
                    let event = packets([framed(&play::DamageEvent {
                        entity_id: self.id,
                        damage_type: damage_type_id(kind),
                        cause: source.attacker.as_ref().map(|a| a.id),
                        direct: source.attacker.as_ref().map(|a| a.id),
                        source_position: source.position.filter(|_| source.attacker.is_none()),
                    })]);
                    self.broadcast_entity(event, true).await;
                    if blocked <= 0.0 {
                        let (dx, dz) = match source.position {
                            Some(from) => (
                                from[0] - self.pose.position[0],
                                from[2] - self.pose.position[2],
                            ),
                            None => (
                                f64::from(self.rng.r#gen::<f32>() - 0.5),
                                f64::from(self.rng.r#gen::<f32>() - 0.5),
                            ),
                        };
                        self.send(&play::HurtAnimation {
                            entity_id: self.id,
                            yaw: (dz.atan2(dx) * 57.295_776_367_187_5 - f64::from(self.pose.yaw))
                                as f32,
                        });
                    }
                }
            }
            pushed = !damage_type_in_tag(kind, "minecraft:no_impact") && !fully_blocked;
            if !damage_type_in_tag(kind, "minecraft:no_knockback")
                && !fully_blocked
                && let Some(from) = source.position
            {
                motion = combat::knockback(
                    motion,
                    combat::BASE_KNOCKBACK,
                    from[0] - self.pose.position[0],
                    from[2] - self.pose.position[2],
                    self.pose.on_ground,
                    resistance,
                );
                knocked = true;
            }
            if kind.name == damage_type::THORNS.name {
                self.player_sound(SoundEvent::EnchantThornsHit, 1.0, 1.0)
                    .await;
            }
        }
        self.hurt_by_source(&source, amount, dealt, blocked > 0.0);
        if self.is_dead() && self.pop_totem(kind).await {
        } else if self.is_dead() {
            if let Some(attacker) = &source.attacker {
                self.progress
                    .stats
                    .award(Stat::entity(StatType::KilledBy, attacker.kind), 1);
                self.killed_by(attacker.kind, kind, source.projectile, source.position);
            }
            self.die(kind, source.attacker.as_ref()).await;
        }
        self.publish(false).await;
        HurtResult {
            landed: blocked <= 0.0 || dealt > 0.0,
            motion: (pushed && knocked).then_some(motion),
            health_lost,
            shield_blocked,
        }
    }

    /// Damage after armor, effects and enchantments, soaked by absorption
    /// and taken off the health.
    async fn actually_hurt(&mut self, source: &DamageSource, amount: f32) {
        let kind = source.kind;
        let mut amount = amount;
        let equipment = self.equipment();
        let worn: Vec<tuff_core::world::attributes::Equipped<'_>> = equipment
            .iter()
            .map(|(slot, stack)| (*slot, stack.any()))
            .collect();
        if !damage_type_in_tag(kind, "minecraft:bypasses_armor") {
            self.hurt_equipment(kind, amount, &ARMOR_SLOTS).await;
            let (armor, toughness) = combat::armor(&worn);
            amount = combat::after_armor(
                amount,
                armor,
                toughness,
                source.weapon.as_deref().map(ItemStack::any),
                &damage_context(source),
                &mut self.rng,
            );
        }
        if !damage_type_in_tag(kind, "minecraft:bypasses_effects") {
            if let Some(amplifier) = self.body.effects.amplifier(MobEffect::Resistance)
                && !damage_type_in_tag(kind, "minecraft:bypasses_resistance")
            {
                let resisted = combat::after_resistance(amount, amplifier);
                if let Some(tenths) = combat::stat_tenths(amount - resisted) {
                    self.progress
                        .stats
                        .award_custom(CustomStat::DamageResisted, tenths);
                }
                amount = resisted;
            }
            if amount <= 0.0 {
                amount = 0.0;
            } else if !damage_type_in_tag(kind, "minecraft:bypasses_enchantments") {
                let protection =
                    combat::enchantment_protection(&worn, &damage_context(source), &mut self.rng);
                if protection > 0.0 {
                    amount = combat::after_protection(amount, protection);
                }
            }
        }
        let through = (amount - self.body.absorption).max(0.0);
        self.body.absorption = (self.body.absorption - (amount - through)).max(0.0);
        if let Some(tenths) = combat::stat_tenths(amount - through) {
            self.progress
                .stats
                .award_custom(CustomStat::DamageAbsorbed, tenths);
        }
        if through == 0.0 {
            return;
        }
        self.exhaust(tuff_core::world::effective_damage_type(kind).exhaustion);
        self.body.health.lose(through);
        if let Some(tenths) = combat::stat_tenths(through) {
            self.progress
                .stats
                .award_custom(CustomStat::DamageTaken, tenths);
        }
    }

    /// Wears the armor pieces (or other damageable equipment) in `slots` as
    /// a hit of `damage` does.
    async fn hurt_equipment(&mut self, kind: &DamageType, damage: f32, slots: &[EquipmentSlot]) {
        if damage <= 0.0 || self.mode.abilities.instabuild {
            return;
        }
        let wear = combat::armor_wear(damage);
        for slot in slots {
            let Some(stack) = self.inventory.equipped(*slot) else {
                continue;
            };
            let wears = stack
                .equippable()
                .is_some_and(|equippable| equippable.damage_on_hurt)
                && stack.max_damage().is_some()
                && !stack
                    .item()
                    .damage_resistant()
                    .is_some_and(|tag| damage_type_in_tag(kind, tag));
            let Some(window) = Inventory::armor_slot(*slot) else {
                continue;
            };
            if !wears {
                continue;
            }
            match self.hurt_slot_reporting(window, wear) {
                Hurt::Unchanged => {}
                Hurt::Damaged => self.send_inventory(),
                Hurt::Broken => {
                    let event = packets([framed(&play::EntityEvent {
                        entity_id: self.id,
                        status: break_event(*slot),
                    })]);
                    self.broadcast_entity(event, true).await;
                    self.send_inventory();
                }
            }
        }
    }
}

/// The enchantment view of a hit on a player.
fn damage_context(source: &DamageSource) -> combat::DamageContext<'static> {
    let attacker = source.attacker.as_ref().map(|a| a.kind);
    combat::DamageContext {
        kind: source.kind,
        victim: EntityType::Player,
        attacker,
        direct_attacker: if source.direct {
            attacker
        } else {
            source.projectile
        },
        is_direct: source.direct || attacker.is_none(),
        direct_attacker_fall_distance: None,
        direct_attacker_flying: None,
    }
}

mod hooks;
pub use hooks::CombatHooks;

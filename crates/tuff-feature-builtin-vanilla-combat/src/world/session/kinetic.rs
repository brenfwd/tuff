//! The charge of a held kinetic weapon (a spear): what it touches while
//! its user moves.

use tuff_core::world::messages::{Attack, AttackKind, PlayerEvent, PlayerHandle, RegionRequest};
use tuff_core::world::raycast;
use tuff_core::world::session::*;
use tuff_vanilla_common::combat;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::{Block, item::KineticCondition};

use super::attack::CombatExt;
use tuff_core::protocol::play::GameMode;

enum Victim {
    Player(PlayerHandle),
    Entity(EntityType),
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(super) trait KineticExt {
    async fn kinetic_stab(&mut self);
}

/// Whether a block stops the look ray before `along` blocks.
pub(super) fn stab_occluded(
    session: &PlayerSession,
    eye: [f64; 3],
    forward: [f64; 3],
    along: f64,
) -> bool {
    let to = [0, 1, 2].map(|i| eye[i] + forward[i] * along);
    let level = |pos| {
        session
            .client_world()
            .block_at(pos)
            .unwrap_or(Block::Air.default_state())
    };
    raycast::cast_colliders(eye, to, &level).is_some()
}

impl KineticExt for PlayerSession {
    async fn kinetic_stab(&mut self) {
        let Some(using) = self.actions.using else {
            self.ext.entry::<super::Stabbed>().0.clear();
            return;
        };
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(stack) = self
            .inventory
            .hand(using.off_hand)
            .map(tuff_item::item::ItemSnapshot::of)
        else {
            return;
        };
        if using.item != stack.item() {
            return;
        }
        let Some(kinetic) = stack.kinetic_weapon() else {
            return;
        };
        let used = u64::from(using.total - using.remaining);
        let started = self.tick.saturating_sub(used);
        self.ext
            .entry::<super::Stabbed>()
            .0
            .retain(|(_, at)| *at >= started);
        if used < kinetic.delay_ticks {
            return;
        }
        let used = used - kinetic.delay_ticks;
        let Some(range) = stack.item().attack_range() else {
            return;
        };
        let forward = tuff_core::world::raycast::view_vector(self.pose.yaw, self.pose.pitch);
        let speed = dot(self.sync.known_movement.map(|c| c * 20.0), forward);
        let eye = self.eye();
        let creative = self.mode.abilities.instabuild;
        let now = self.tick;
        let tick = self.tick;
        let reach = |id: i32, position: [f64; 3], this: &Self| {
            let recent = this.ext.get::<super::Stabbed>().is_some_and(|s| {
                s.0.iter()
                    .any(|(other, at)| *other == id && now - at < kinetic.contact_cooldown_ticks)
            });
            if recent || !combat::in_stab_reach(eye, forward, position, range, creative) {
                return false;
            }
            let along = dot([0, 1, 2].map(|i| position[i] - eye[i]), forward).max(0.0);
            !stab_occluded(this, eye, forward, along)
        };
        let mut victims: Vec<(i32, [f64; 3], Victim)> = Vec::new();
        for (id, position, handle) in self.view.entities.visible_players() {
            if id != self.id && reach(id, position, self) {
                victims.push((id, position, Victim::Player(handle.clone())));
            }
        }
        for (id, kind, _, position) in self.view.entities.visible_entities() {
            if reach(id, position, self) {
                victims.push((id, position, Victim::Entity(kind)));
            }
        }
        if victims.is_empty() {
            return;
        }
        let kind = stack
            .item()
            .damage_type()
            .unwrap_or(&damage_type::PLAYER_ATTACK);
        let knockback_attribute = self.attribute(Attribute::AttackKnockback) as f32;
        let weapon = Some(stack.clone());
        let test = |condition: &Option<KineticCondition>, relative: f64| {
            condition
                .as_ref()
                .is_some_and(|c| c.test(used, speed, relative, 1.0))
        };
        let mut living = 0u32;
        let mut affected = false;
        for (id, _, victim) in victims {
            let stabbed = &mut self.ext.entry::<super::Stabbed>().0;
            stabbed.retain(|(other, _)| *other != id);
            stabbed.push((id, now));
            let velocity = self.view.entities.visible_velocity(id, tick);
            let relative = (speed - dot(velocity.map(|c| c * 20.0), forward)).max(0.0);
            let dismounts = test(&kinetic.dismount_conditions, relative);
            let deals_knockback = test(&kinetic.knockback_conditions, relative);
            let deals_damage = test(&kinetic.damage_conditions, relative);
            if !(dismounts || deals_knockback || deals_damage) {
                continue;
            }
            affected = true;
            let base = 1.0 + (relative * f64::from(kinetic.damage_multiplier)).floor() as f32;
            let victim_kind = match &victim {
                Victim::Player(_) => EntityType::Player,
                Victim::Entity(kind) => *kind,
            };
            if !matches!(&victim, Victim::Entity(kind) if kind.mob().category == tuff_vanilla_data::entity::MobCategory::Misc)
            {
                living += 1;
            }
            let context = if matches!(victim, Victim::Player(_)) {
                combat::DamageContext {
                    kind,
                    victim: EntityType::Player,
                    attacker: Some(EntityType::Player),
                    direct_attacker: Some(EntityType::Player),
                    is_direct: true,
                    direct_attacker_fall_distance: None,
                    direct_attacker_flying: None,
                }
            } else {
                combat::DamageContext::melee(victim_kind)
            };
            let damage = if deals_damage {
                combat::modify_damage(&stack, &context, base, &mut self.rng)
            } else {
                0.0
            };
            let knockback = if deals_knockback {
                combat::modify_knockback(&stack, &context, knockback_attribute, &mut self.rng)
            } else {
                0.0
            };
            let can_dismount = dismounts
                && !tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::entity_type::key::CANNOT_BE_DISMOUNTED_BY_ITEM_USAGE,
                    victim_kind,
                );
            match victim {
                Victim::Player(handle) => {
                    if can_dismount {
                        handle.send(PlayerEvent::StabDismounted);
                    }
                    if deals_damage || deals_knockback {
                        let kind = if deals_damage {
                            AttackKind::Direct {
                                critical: false,
                                magic: false,
                                full_strength: true,
                                sweep: None,
                                smash: None,
                            }
                        } else {
                            AttackKind::Knockback
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
                            kind,
                        }));
                    }
                }
                Victim::Entity(_) => {
                    if can_dismount
                        && let Some((_, chunk, _)) = self.view.entities.visible_entity(id)
                        && let Some(region) = self.region(chunk.region()).await
                    {
                        region
                            .request(RegionRequest::StabDismount { target: id })
                            .await;
                    }
                    if !deals_damage && deals_knockback {
                        if let Some((_, chunk, _)) = self.view.entities.visible_entity(id)
                            && let Some(region) = self.region(chunk.region()).await
                        {
                            let (dx, dz) = combat::facing_knockback_direction(self.pose.yaw);
                            region
                                .request(RegionRequest::StabKnock {
                                    target: id,
                                    power: knockback,
                                    dx,
                                    dz,
                                })
                                .await;
                        }
                    } else if deals_damage {
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
                }
            }
        }
        if !affected {
            return;
        }
        if let Some(hit_sound) = kinetic.hit_sound {
            self.sound_at(hit_sound, self.pose.position).await;
        }
        let wear = stack
            .weapon()
            .map_or(0, |w| w.item_damage_per_attack as u32);
        self.hurt_hand(using.off_hand, wear * living).await;
        let count = self
            .ext
            .entry::<super::Stabbed>()
            .0
            .len()
            .min(u32::MAX as usize) as u32;
        self.fire(tuff_core::world::advancement::trigger::Event::SpearMobs { count });
    }
}

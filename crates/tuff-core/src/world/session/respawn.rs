//! Respawn, dimension changes, beds and sleeping.

use super::*;
use crate::protocol::play::{self, GameMode};
use crate::storage::player_data::RespawnSave;
use crate::world::dimension::DimensionId;
use crate::world::entity::Culprit;
use crate::world::messages::{AnchorUse, RegionRequest};
use crate::world::player::{Experience, Fall, Food, Health};
use crate::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT, block_contact, framed, packets};
use rand::Rng;
use tokio::sync::oneshot;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::entity::MAX_AIR_SUPPLY;
use tuff_vanilla_data::registry::CustomStat;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

impl PlayerSession {
    /// Where a player comes back: beside its bed or respawn anchor (or at
    /// its forced spawn point) if that still works, else at the world
    /// spawn, telling the client the spawn block was missing. A death
    /// respawn (`consume`) spends one of the anchor's charges and plays its
    /// deplete sound to the player.
    pub async fn spawn_block_point(&mut self, consume: bool) -> (DimensionId, [f64; 3], f32, f32) {
        if let Some(respawn) = self.mode.respawn.clone()
            && let Some(dimension) = self.shared.dimensions.by_key(&respawn.dimension)
        {
            let dimension = dimension.id;
            let pos = BlockPos::new(respawn.pos[0], respawn.pos[1], respawn.pos[2]);
            let region = ChunkPos::of_block(pos).region();
            let mut found = None;
            if let Some(handle) = self.region_in(dimension, region).await {
                let (reply, answer) = oneshot::channel();
                handle
                    .request(RegionRequest::RespawnPoint {
                        pos,
                        yaw: respawn.yaw,
                        pitch: respawn.pitch,
                        forced: respawn.forced,
                        consume,
                        reply,
                    })
                    .await;
                found = answer.await.ok().flatten();
            }
            if let Some(spot) = found {
                if consume && spot.anchor {
                    let seed = self.rng.r#gen();
                    self.send(&play::Sound {
                        sound_event: SoundEvent::BlockRespawnAnchorDeplete.id(),
                        category: play::sound_category::BLOCKS,
                        position: [pos.x as f64, pos.y as f64, pos.z as f64],
                        volume: 1.0,
                        pitch: 1.0,
                        seed,
                    });
                }
                return (dimension, spot.position, spot.yaw, spot.pitch);
            }
            self.mode.respawn = None;
            self.send(&play::GameEvent {
                event: play::game_event::NO_RESPAWN_BLOCK_AVAILABLE,
                value: 0.0,
            });
        }
        let spawn = self.shared.spawn();
        (
            self.shared.spawn_dimension(),
            self.world_spawn_position().await,
            spawn.yaw,
            spawn.pitch,
        )
    }

    /// Where a player without a spawn block appears: a random spot within
    /// the `respawn_radius` game rule around the world spawn that it can
    /// stand on, as the spawn's region finds it.
    pub async fn world_spawn_position(&mut self) -> [f64; 3] {
        let spawn = self.shared.spawn();
        let center = [
            spawn.pos.x as f64 + 0.5,
            spawn.pos.y as f64,
            spawn.pos.z as f64 + 0.5,
        ];
        let dimension = self.shared.spawn_dimension();
        let adventure = self.shared.live.default_game_mode() == play::GameMode::Adventure;
        let to_border = self
            .shared
            .dimension(dimension)
            .distance_to_border(spawn.pos.x, spawn.pos.z);
        let radius =
            spawn_search_radius(self.shared.rule_int("respawn_radius"), to_border, adventure);
        let offset = self.rng.r#gen::<u32>();
        let region = ChunkPos::of_block(spawn.pos).region();
        let Some(handle) = self.region_in(dimension, region).await else {
            return center;
        };
        let (reply, answer) = oneshot::channel();
        handle
            .request(RegionRequest::FindSpawn {
                center: spawn.pos,
                radius,
                offset,
                reply,
            })
            .await;
        answer.await.unwrap_or(center)
    }

    /// Chorus fruit's random teleport: asks the home region for a safe
    /// landing within `diameter` blocks, then moves there like an ender
    /// pearl (position sync, poof particles, teleport sound).
    pub async fn teleport_randomly(&mut self, diameter: f32, directional_particles: bool) {
        self.leave_vehicle().await;
        let chunk = self.chunk();
        let Some(region) = self.region(chunk.region()).await else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::RandomTeleport {
                origin: self.pose.position,
                diameter,
                width: PLAYER_HALF_WIDTH * 2.0,
                height: PLAYER_HEIGHT,
                reply,
            })
            .await;
        let Ok(Some(target)) = answer.await else {
            return;
        };
        self.body.fall = Fall::default();
        let origin = self.pose.position;
        self.emit_game_event(crate::world::game_event::GameEvent::Teleport, origin)
            .await;
        self.teleport(target);
        self.update_view().await;
        self.publish(true).await;
        let seed = self.rng.r#gen();
        let event = packets([
            framed(&play::EntityEvent {
                entity_id: self.id,
                status: play::entity_event::TELEPORT,
            }),
            framed(&play::Sound {
                sound_event: SoundEvent::ItemChorusFruitTeleport.id(),
                category: play::sound_category::PLAYERS,
                position: target,
                volume: 1.0,
                pitch: 1.0,
                seed,
            }),
        ]);
        self.broadcast_chunk(event).await;
        if directional_particles {
            let from = crate::world::physics::block_pos(origin);
            let to = crate::world::physics::block_pos(target);
            let event = framed(&play::LevelEvent {
                event: play::level_event::PARTICLES_CONSUME_EFFECT_TELEPORT,
                position: from,
                data: clamped_difference(from, to),
                global: false,
            });
            self.broadcast_chunk(packets([event])).await;
        }
    }

    pub async fn respawn(&mut self) {
        if !self.is_dead() {
            return;
        }
        let (dimension, position, yaw, pitch) = self.spawn_block_point(true).await;
        self.body.health = Health::default();
        self.body.health.value = self.max_health();
        self.body.food = Food::default();
        self.body.fire_ticks = 0;
        self.body.air = MAX_AIR_SUPPLY;
        self.body.ticks_frozen = 0;
        self.body.stuck_arrows = Default::default();
        self.pose.swimming = false;
        if !self.keeps_inventory() {
            self.body.experience = Experience::new(0, 0.0, 0, self.body.experience.seed);
        }
        self.pose.sneaking = false;
        self.actions.portal_cooldown = 0;
        self.await_client_load();
        if self
            .shared
            .hardcore
            .load(std::sync::atomic::Ordering::Relaxed)
            != self.mode.client_hardcore
        {
            self.send_login();
        }
        self.relocate(dimension, position, yaw, pitch, play::respawn::KEEP_NOTHING)
            .await;
        if self
            .shared
            .hardcore
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.set_game_mode(GameMode::Spectator).await;
        }
        self.respawned_hooks();
    }

    /// Puts the player somewhere else through the client's respawn flow,
    /// which is how the game changes dimension as well as revives: the
    /// entity leaves the world, the client resets, and everything about
    /// the new surroundings is sent afresh.
    pub async fn relocate(
        &mut self,
        dimension: DimensionId,
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        data_kept: i8,
    ) {
        self.await_chunk_loaded(dimension, position).await;
        self.close_menu(true).await;
        self.finish_menu_close().await;
        self.published.seq += 1;
        self.detach_from_regions().await;
        let from = (self.dimension().key.clone(), self.pose.position);
        self.dimension = dimension;
        self.pose.position = position;
        self.pose.yaw = yaw;
        self.pose.pitch = pitch;
        self.body.fall = Fall::default();
        self.surroundings.in_lava = false;
        self.surroundings.contact = block_contact::Contact::default();
        self.surroundings.enchantment_modifiers.clear();
        self.surroundings.in_fire = None;
        self.teleported_hooks();
        self.actions.using = None;
        self.actions.destroying = None;
        let shared = self.shared.clone();
        let target = shared.dimension(dimension);
        let last_death = self.mode.last_death.clone();
        self.send(&play::Respawn {
            dimension_type: target.type_id(),
            dimension: &target.key,
            game_mode: self.mode.game_mode,
            previous_game_mode: self.mode.previous_game_mode,
            sea_level: target.sea_level(),
            data_kept,
            last_death: last_death.as_ref().map(|(d, p)| (d.as_str(), *p)),
            is_debug: target.is_debug(),
            is_flat: target.is_flat(),
            portal_cooldown: self.actions.portal_cooldown.min(i32::MAX as u32) as i32,
        });
        self.send(&play::ChangeDifficulty {
            difficulty: self.shared.difficulty().id(),
            locked: self
                .shared
                .difficulty_locked
                .load(std::sync::atomic::Ordering::Relaxed),
        });
        self.send_abilities();
        self.send(&play::SetHeldSlot(self.inventory.selected() as i32));
        self.send_permission_level();
        self.send_world().await;
        if data_kept != 0 {
            self.changed_dimension(&from.0, from.1);
        }
        if from.0 != self.dimension().key {
            self.dimension_changed_hooks(&from.0);
        }
    }

    /// The region freed the bed this player left: it stands up at
    /// `position` facing the bed.
    pub async fn left_bed(&mut self, dimension: DimensionId, position: [f64; 3], yaw: f32) {
        if dimension == self.dimension && !self.is_dead() {
            self.pose.yaw = yaw;
            self.pose.pitch = 0.0;
            self.teleport(position);
            self.publish(true).await;
        }
    }

    /// The region found the bed usable: set the spawn point, then sleep if
    /// the rule and the player allow.
    pub async fn use_bed(&mut self, bed: crate::world::messages::BedUse) {
        if bed.dimension != self.dimension || self.sleep.sleeping.is_some() || self.is_dead() {
            return;
        }
        if bed.set_spawn {
            let key = self.dimension().key.clone();
            let same = self.mode.respawn.as_ref().is_some_and(|respawn| {
                respawn.dimension == key && respawn.pos == [bed.pos.x, bed.pos.y, bed.pos.z]
            });
            self.mode.respawn = Some(RespawnSave {
                dimension: key,
                pos: [bed.pos.x, bed.pos.y, bed.pos.z],
                yaw: self.pose.yaw,
                pitch: self.pose.pitch,
                forced: false,
            });
            if !same {
                self.system_message(&TextComponent::translate("block.minecraft.set_spawn", []));
            }
        }
        let height = match bed.sleep {
            Ok(_) if bed.monsters_near && self.mode.game_mode != GameMode::Creative => {
                self.send(&play::SystemChat {
                    content: &TextComponent::translate("block.minecraft.bed.not_safe", []),
                    overlay: true,
                });
                return;
            }
            Ok(height) => height,
            Err(message) => {
                if let Some(message) = message {
                    self.send(&play::SystemChat {
                        content: &message,
                        overlay: true,
                    });
                }
                return;
            }
        };
        if let Some(region) = self.region_of(bed.pos) {
            region
                .request(RegionRequest::OccupyBed { pos: bed.pos })
                .await;
        }
        self.sleep.sleeping = Some(Sleep {
            pos: bed.pos,
            rule: bed.rule,
        });
        let sleep_stat = match bed.rule {
            tuff_vanilla_data::block::BedRuleAttribute::Bed => CustomStat::SleepInBed,
            tuff_vanilla_data::block::BedRuleAttribute::StrawBed => CustomStat::SleepInStrawBed,
        };
        self.progress.stats.award_custom(sleep_stat, 1);
        self.progress.stats.reset_custom(CustomStat::TimeSinceRest);
        self.fire(crate::world::advancement::trigger::Event::SleptInBed);
        self.sleep.sleep_counter = 0;
        self.actions.using = None;
        self.pose.sprinting = false;
        self.teleport([bed.pos.x as f64 + 0.5, height, bed.pos.z as f64 + 0.5]);
        if self.shared.rule_int("players_sleeping_percentage") > 100 {
            self.send(&play::SystemChat {
                content: &TextComponent::translate("sleep.not_possible", []),
                overlay: true,
            });
        }
        self.publish(true).await;
        self.report_state().await;
    }

    /// The region found a charged respawn anchor that works here: it
    /// becomes the spawn point (facing straight ahead), announced with a
    /// message and the anchor's set-spawn sound if that moved it.
    pub async fn use_anchor(&mut self, anchor: AnchorUse) {
        if anchor.dimension != self.dimension || self.is_dead() {
            return;
        }
        let key = self.dimension().key.clone();
        let pos = [anchor.pos.x, anchor.pos.y, anchor.pos.z];
        let same = self
            .mode
            .respawn
            .as_ref()
            .is_some_and(|respawn| respawn.dimension == key && respawn.pos == pos);
        if same {
            return;
        }
        self.mode.respawn = Some(RespawnSave {
            dimension: key,
            pos,
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
        });
        self.system_message(&TextComponent::translate("block.minecraft.set_spawn", []));
        if let Some(region) = self.region_of(anchor.pos) {
            region
                .request(RegionRequest::AnchorSpawnSet { pos: anchor.pos })
                .await;
        }
    }

    /// Gets out of bed. A forceful wake-up (being hurt) forgets the sleep;
    /// otherwise the counter runs on to its reset.
    pub async fn stop_sleeping(&mut self, forceful: bool) {
        let Some(sleep) = self.sleep.sleeping.take() else {
            return;
        };
        self.sleep.sleep_counter = if forceful { 0 } else { DEEP_SLEEP_TICKS };
        let wake = packets([framed(&play::Animate {
            entity_id: self.id,
            animation: play::animation::WAKE_UP,
        })]);
        self.broadcast_entity(wake, true).await;
        if let Some(region) = self.region_of(sleep.pos) {
            region
                .request(RegionRequest::LeaveBed {
                    player: self.conn.handle.clone(),
                    pos: sleep.pos,
                    yaw: self.pose.yaw,
                })
                .await;
        }
        self.publish(true).await;
        self.report_state().await;
    }

    /// Counts sleep, and gets up once the bed's rule no longer allows
    /// sleeping (morning without everyone asleep).
    pub async fn tick_sleep(&mut self) {
        match self.sleep.sleeping {
            Some(sleep) => {
                self.sleep.sleep_counter = (self.sleep.sleep_counter + 1).min(DEEP_SLEEP_TICKS);
                let environment = self.dimension().environment();
                let rule = match sleep.rule {
                    tuff_vanilla_data::block::BedRuleAttribute::Bed => environment.bed_rule,
                    tuff_vanilla_data::block::BedRuleAttribute::StrawBed => {
                        environment.straw_bed_rule
                    }
                };
                if !crate::world::bed_condition(rule.can_sleep, &self.shared, self.dimension()) {
                    self.stop_sleeping(false).await;
                }
            }
            None if self.sleep.sleep_counter > 0 => {
                self.sleep.sleep_counter += 1;
                if self.sleep.sleep_counter >= SLEEP_COUNTER_RESET {
                    self.sleep.sleep_counter = 0;
                }
            }
            None => {}
        }
    }

    /// An explosion hurt this player; push it if the region's Explosion
    /// packet did not. The push goes in an Explosion packet of its own,
    /// silent and without block particles, since only that packet adds to
    /// the client's velocity instead of replacing it.
    pub async fn exploded(
        &mut self,
        damage: f32,
        blast: crate::world::messages::Blast,
        push: [f64; 3],
        push_sent: bool,
    ) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let cause = blast
            .wind
            .map(|wind| wind.source)
            .or(blast.culprit.as_ref().map(|culprit| culprit.kind))
            .map(|kind| crate::world::predicate::EntityFacts {
                kind: Some(kind),
                position: blast.center,
                dimension: self.dimension().key.clone(),
                ..Default::default()
            });
        self.progress.explosion_impact = Some((self.pose.position, cause));
        self.body.fall.explosion_hit(
            blast.wind.is_some_and(|w| w.spares_fall),
            self.pose.position[1],
        );
        if blast.wind.is_none() {
            let attacker = blast
                .igniter
                .map(|igniter| Culprit::player(igniter.id, igniter.uuid, igniter.name))
                .or(blast.culprit);
            self.hurt(
                DamageSource {
                    kind: blast.damage_type,
                    attacker,
                    projectile: None,
                    position: Some(blast.center),
                    weapon: None,
                    direct: false,
                },
                damage,
            )
            .await;
        }
        let flying = self.mode.abilities.flying && self.mode.abilities.instabuild;
        if !flying {
            self.motion.velocity_sent.push_added(self.tick, push);
        }
        if !push_sent && !flying {
            self.send(&play::Explode {
                center: blast.center,
                radius: blast.radius,
                block_count: 0,
                knockback: Some(push),
                particle: tuff_vanilla_data::registry::ParticleType::Smoke.id(),
                sound: SoundEvent::IntentionallyEmpty.id(),
                block_particles: &[],
            });
        }
    }
}

/// The radius a spawn search uses: none in an adventure world, else the
/// rule clipped to the distance to the world border (at least one block
/// when the spawn sits on the border).
fn spawn_search_radius(rule: i32, distance_to_border: f64, adventure: bool) -> i32 {
    if adventure {
        return 0;
    }
    let to_border = distance_to_border
        .floor()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let mut radius = rule.max(0);
    if to_border < radius {
        radius = to_border;
    }
    if to_border <= 1 {
        radius = 1;
    }
    radius
}

#[cfg(test)]
mod radius_tests {
    use super::spawn_search_radius;

    #[tuff_core::watchdog]
    #[test]
    fn the_spawn_search_radius_is_clipped_by_the_border_and_dropped_in_adventure() {
        assert_eq!(spawn_search_radius(10, 1000.0, false), 10);
        assert_eq!(spawn_search_radius(10, 4.7, false), 4);
        assert_eq!(spawn_search_radius(10, 1.0, false), 1);
        assert_eq!(spawn_search_radius(10, -3.0, false), 1);
        assert_eq!(spawn_search_radius(-5, 1000.0, false), 0);
        assert_eq!(spawn_search_radius(10, 1000.0, true), 0);
    }
}

/// The packed offset from `from` to `to`, each axis clamped to 127 blocks.
fn clamped_difference(from: BlockPos, to: BlockPos) -> i32 {
    let axis = |a: i32, b: i32| ((b - a).clamp(-127, 127) + 127) & 255;
    (axis(from.x, to.x) << 16) | (axis(from.y, to.y) << 8) | axis(from.z, to.z)
}

#[cfg(test)]
mod difference_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn differences_pack_with_a_127_bias_and_clamp() {
        let origin = BlockPos::new(10, 64, -3);
        assert_eq!(clamped_difference(origin, origin), 0x7f7f7f);
        assert_eq!(
            clamped_difference(origin, BlockPos::new(11, 62, -3 + 500)),
            (128 << 16) | (125 << 8) | 254
        );
    }
}

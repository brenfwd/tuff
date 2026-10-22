use super::*;

impl RegionActor {
    /// Applies what an entity's tick or an action on it did to the world.
    /// Applies what entity `entity` did, its block changes attributed to it
    /// (or to the cause already set when it is out of the map).
    pub(in crate::world::region) fn apply_effects(
        &mut self,
        entity: i32,
        chunk: ChunkPos,
        effects: Vec<Effect>,
    ) {
        match self.ents.entities.get(&entity) {
            Some(tracked) => {
                let cause = BlockCause::entity(&*tracked.entity);
                self.with_cause(cause, |region| {
                    region.apply_effects_now(entity, chunk, effects)
                });
            }
            None => self.apply_effects_now(entity, chunk, effects),
        }
    }

    pub(super) fn apply_effects_now(&mut self, entity: i32, chunk: ChunkPos, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::EntityPackets(packets) => {
                    self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
                        entity,
                        packets: packets.clone(),
                    });
                }
                Effect::ChunkPackets(packets) => self.send_chunk_packets(chunk, None, packets),
                Effect::NearbyPackets {
                    center,
                    range,
                    packets,
                } => {
                    let viewers: Vec<_> = self
                        .columns
                        .get(&chunk)
                        .map(|column| column.viewers.keys().copied().collect())
                        .unwrap_or_default();
                    self.send_chunk_packets(chunk, None, packets.clone());
                    self.send_to_players_near(center, range, &viewers, packets.clone());
                    crate::world::area::broadcast(
                        self,
                        crate::world::area::around(center, range),
                        move |world| {
                            world.send_to_players_near(center, range, &viewers, packets.clone());
                        },
                    );
                }
                Effect::HeldItems { player, slots } => {
                    if let Some(holder) = self.entity_holder(entity) {
                        let cause = player.map_or(self.sim.cause, BlockCause::Player);
                        self.with_cause(cause, |region| {
                            region.report_container_edit(holder, slots)
                        });
                    }
                }
                Effect::GameEvent {
                    event,
                    position,
                    source,
                } => {
                    use crate::world::game_event::{Context, Source};
                    crate::world::region_api::RegionWorld::game_event(
                        self,
                        event,
                        position,
                        Context {
                            source: source.map(Source::entity),
                            affected: None,
                        },
                    );
                }
                Effect::LightningStrike { pos } => {
                    crate::world::region_api::RegionWorld::game_event(
                        self,
                        crate::world::game_event::GameEvent::LightningStrike,
                        pos.center(),
                        crate::world::game_event::Context::default(),
                    );
                    self.lightning_struck_block(pos);
                }
                Effect::ProjectileHitBlock { hit, impact } => {
                    self.projectile_hit_block(hit, &impact);
                }
                Effect::ProjectileLanded(landing) => self.projectile_landed(&landing),
                Effect::SetBlock { pos, state } => {
                    if self.owns(pos) {
                        self.set_block(pos, state);
                    }
                }
                Effect::CopperGolemTake { pos, stack } => {
                    if self.owns(pos) {
                        self.copper_golem_take(entity, pos, &stack);
                    }
                }
                Effect::Run(task) => task.run(self),
                Effect::CopperGolemDeposit { pos, stack } => {
                    if self.owns(pos) {
                        self.copper_golem_deposit(pos, stack);
                    }
                }
                Effect::ContainerOpen { pos, open } => self.mob_container_open(entity, pos, open),
                Effect::PlaceFallenBlock { pos, state, data } => {
                    self.put_block(pos, state);
                    if let Some(data) = data {
                        self.load_block_entity_data(pos, state, &data);
                    }
                }
                Effect::PassedThrough(boxes) => self.entities_passed_through(boxes),
                Effect::Landed(landing) => self.entity_landed(&landing),
                Effect::RunCommand {
                    command,
                    name,
                    position,
                    track_output,
                } if self.shared.rule_bool("command_blocks_work") => {
                    let shared = self.shared.clone();
                    let handle = self.handle.clone();
                    let request = crate::world::commands::block::BlockCommand {
                        dimension: self.dimension,
                        position,
                        yaw: 0.0,
                        name: tuff_item::text::TextComponent::new(name),
                        command,
                        track_output,
                    };
                    tokio::spawn(async move {
                        let outcome = crate::world::commands::block::run(shared, request).await;
                        handle.internal(crate::world::messages::RegionInternal::Run(
                            crate::world::region_api::RegionTask::new(move |world| {
                                let Some(tracked) = world.entity_mut(entity) else {
                                    return;
                                };
                                let extra = &mut tracked.entity.base_mut().extra;
                                extra.insert("SuccessCount", outcome.success as i32);
                                if track_output && let Some(output) = outcome.output {
                                    extra.insert("LastOutput", output.to_nbt());
                                }
                            }),
                        ));
                    });
                }
                Effect::RunCommand { .. } => {}
                Effect::DisturbGuarded { player } => self.disturb_guarded_container(player),
                Effect::DestroyBlock { pos, drop } => self.destroy_block(pos, drop),
                Effect::DropItem {
                    stack,
                    position,
                    velocity,
                    pickup_delay,
                } => self.spawn_item_at(stack, position, velocity, pickup_delay),
                Effect::DropExtendedItem {
                    stack,
                    position,
                    velocity,
                    pickup_delay,
                } => self.spawn_extended_item_at(stack, position, velocity, pickup_delay),
                Effect::Experience { position, amount } => {
                    self.spawn_experience(position, amount);
                }
                Effect::MobExperience {
                    position,
                    amount,
                    player_killed,
                    killer,
                } => {
                    let hooks = self.shared.region_hooks.clone();
                    let taken = hooks
                        .iter()
                        .any(|hook| hook.mob_experience(self, position, amount));
                    if player_killed && !taken {
                        self.spawn_experience(position, amount);
                    }
                    if taken
                        && let Some((player, kill)) = killer
                        && let Some(handle) = self.seen_player(player)
                    {
                        handle.send(PlayerEvent::Advancement(Box::new(
                            AdvancementEvent::KillMobNearSculkCatalyst(*kill),
                        )));
                    }
                }
                Effect::Spawn(mut entity) => {
                    if entity.base().id == 0 {
                        entity.base_mut().id = self.shared.next_entity_id();
                    }
                    self.receive_entity(entity, 0)
                }
                Effect::Mount { passenger, vehicle } => {
                    self.ents.mount_requests.push((passenger, Some(vehicle)));
                }
                Effect::StopRiding => self.ents.mount_requests.push((entity, None)),
                Effect::StabDismount { target } => {
                    if self.ents.entities.contains_key(&target) {
                        self.ents.mount_requests.push((target, None));
                    } else if let Some(handle) = self.seen_player(target) {
                        handle.send(PlayerEvent::StabDismounted);
                    }
                }
                Effect::Breed { .. } => {}
                Effect::TakeItem { item, count } => self.mob_takes_item(entity, item, count),
                Effect::Hurt { target, hit } => self.hit(target, *hit),
                Effect::Explode(explosion) => self.explode(explosion),
                Effect::Crammed { .. } => {}
                Effect::Push { target, push } => self.push_target(target, push, false),
                Effect::Nudge { target, push } => self.push_target(target, push, true),
                Effect::ShellPush {
                    area,
                    direction,
                    amount,
                } => self.shell_push(entity, area, direction, amount),
                Effect::Dismount { rider, position } => {
                    rider.handle.send(PlayerEvent::Dismounted {
                        vehicle: entity,
                        position,
                    });
                    let packet = packets([framed(&play::SetPassengers {
                        vehicle: entity,
                        passengers: &[],
                    })]);
                    self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
                        entity,
                        packets: packet.clone(),
                    });
                }
                Effect::Advancement { player, mut event } => {
                    if let AdvancementEvent::AllayDropItemOnBlock { location, .. } = &mut *event {
                        let [x, y, z] = location.position;
                        *location = self.location_facts(BlockPos::new(
                            x.floor() as i32,
                            y.floor() as i32,
                            z.floor() as i32,
                        ));
                    }
                    if let Some(presence) = self.ents.presence.values().find(|p| p.uuid == player) {
                        presence.handle.send(PlayerEvent::Advancement(event));
                    }
                }
                Effect::Died {
                    player,
                    attacker,
                    damage,
                } => self.report_death(entity, player, attacker, damage),
                Effect::PetDied {
                    owner,
                    victim,
                    kind,
                    killer,
                    killer_player,
                } => {
                    if self.shared.rule_bool("show_death_messages") {
                        let _ = self.shared.coordinator.try_send(
                            crate::world::CoordinatorMessage::PetDied {
                                owner,
                                victim,
                                kind,
                                killer,
                                killer_player,
                            },
                        );
                    }
                }
                Effect::KillCredit { player, kill } => {
                    if let Some(handle) = self.seen_player(player) {
                        handle.send(PlayerEvent::KilledEntity(kill));
                    }
                }
                Effect::Potion { target, hit } => {
                    if let Some(handle) = self.seen_player(target) {
                        handle.send(PlayerEvent::Potion(hit));
                    }
                }
                Effect::Notify { target, notice } => {
                    if let Some(tracked) = self.ents.entities.get_mut(&target) {
                        tracked.entity.notice(notice);
                    }
                }
                Effect::MobEffects { target, effects } => {
                    if let Some(tracked) = self.ents.entities.get_mut(&target) {
                        tracked.entity.add_effects(&effects);
                    }
                }
                Effect::AlertNeighbours {
                    kinds,
                    area,
                    attacker,
                } => self.alert_neighbours(kinds, area, attacker),
                Effect::InNeighbours { area, run } => {
                    crate::world::area::broadcast(self, area, move |world| (run.0)(world));
                }
                Effect::UseDoor { pos, open } => {
                    if self.owns(pos) {
                        self.use_door(pos, open);
                    }
                }
                Effect::RingBell { pos } => {
                    if self.owns(pos) {
                        self.ring_bell(pos, None);
                    }
                }
                Effect::OccupyBed { pos, occupied } => {
                    if self.owns(pos) {
                        self.occupy_bed(pos, occupied);
                    }
                }
                Effect::TellPlayer { player, event } => {
                    if let Some(handle) = self.seen_player(player) {
                        handle.send(*event);
                    }
                }
                Effect::TeleportPlayer {
                    player,
                    handle,
                    position,
                    fall_damage,
                } => {
                    let handle = handle.or_else(|| self.seen_player(player));
                    if let Some(handle) = handle {
                        handle.send(PlayerEvent::PearlTeleport {
                            dimension: self.dimension,
                            position,
                            fall_damage,
                        });
                    }
                }
                // Handled by the caller before effects are applied, since
                // it needs the tracked entity itself removed.
                Effect::Discard => {}
                Effect::SitOnShoulder { player } => self.offer_shoulder(entity, player),
            }
        }
    }

    /// An entity's hit on a player or another entity of this region.
    pub(in crate::world::region) fn hit(&mut self, target: i32, mut hit: EntityHit) {
        let target = match self.ents.part_owner(target) {
            Some((owner, part)) => {
                hit.part = Some(part);
                owner
            }
            None => target,
        };
        if let Some(handle) = self.seen_player(target) {
            handle.send(PlayerEvent::EntityHurt(Box::new(hit)));
            return;
        }
        if self.ents.presence.contains_key(&target) {
            return;
        }
        let Some(mut tracked) = self.ents.entities.remove(&target) else {
            return;
        };
        if tracked.entity.base().invulnerable {
            self.ents.entities.insert(target, tracked);
            self.rebound(&hit);
            return;
        }
        let hooks = self.shared.region_hooks.clone();
        let scale: f32 = hooks
            .iter()
            .map(|hook| {
                let by = hit.attacker.as_ref().map(|a| (a.uuid, a.kind));
                hook.entity_hurt_scale(self, &*tracked.entity, hit.kind, by)
            })
            .product();
        if scale <= 0.0 {
            self.ents.entities.insert(target, tracked);
            self.rebound(&hit);
            return;
        }
        hit.amount *= scale;
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (outcome, effects) =
            self.with_context(&summaries, &mut budget, |cx| tracked.entity.hit(&hit, cx));
        let chunk = tracked.chunk();
        self.send_changed_data(&mut tracked);
        if let (Some(outcome), Some(culprit)) = (outcome, &hit.attacker)
            && outcome.hurt
            && culprit.kind == EntityType::Player
            && let Some(shooter) = self.seen_player(culprit.id)
        {
            let at = |kind| EntityFacts {
                kind: Some(kind),
                position: hit.position.unwrap_or_default(),
                ..EntityFacts::default()
            };
            shooter.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::HurtEntity {
                    victim: tracked.entity.facts(),
                    damage: DamageFacts {
                        kind: hit.kind,
                        source: None,
                        direct: hit.projectile.map(at),
                    },
                    weapon: hit.weapon.as_ref().map(tuff_item::item::ItemSnapshot::of),
                    dealt: hit.amount,
                    taken: outcome.health_lost,
                },
            )));
        }
        if outcome.is_some_and(|o| o.hurt)
            && hit.report_landed
            && let Some(attacker) = hit.attacker.as_ref()
            && let Some(source) = self.ents.entities.get_mut(&attacker.id)
        {
            source
                .entity
                .notice(crate::world::entity::Notice::HitLanded);
        }
        if outcome.is_some_and(|o| o.hurt) {
            if let Some(flags) = tracked.flags_packets() {
                self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
                    entity: target,
                    packets: flags.clone(),
                });
            }
            tracked.sync.impulse();
            let track = tracked.broadcast(self.dimension, chunk);
            self.send_to_viewers(&[chunk], || PlayerEvent::Track(track.clone()));
        }
        self.ents.entities.insert(target, tracked);
        self.apply_effects(target, chunk, effects);
        if !outcome.is_some_and(|o| o.hurt) {
            self.rebound(&hit);
        } else if let Some(mut spent) = hit.rebound.as_ref().and_then(|r| r.take()) {
            for stack in spent.take_items() {
                stack.burn(tuff_item::item::BurnReason::Consumed);
            }
        }
    }

    /// Sends a projectile whose hit failed back the way it came.
    pub(super) fn rebound(&mut self, hit: &EntityHit) {
        if let Some(entity) = hit.rebound.as_ref().and_then(|r| r.take()) {
            self.receive_entity(entity, 0);
        }
    }

    /// Breeds the entity being handled with `partner`, if both still can.
    pub(super) fn breed(
        &mut self,
        tracked: &mut TrackedEntity,
        partner: i32,
        summaries: &[EntitySummary],
        budget: &mut u32,
    ) {
        let Some(mut other) = self.ents.entities.remove(&partner) else {
            return;
        };
        let (child, effects) = self.with_context(summaries, budget, |cx| {
            tracked.entity.breed(&mut *other.entity, cx)
        });
        let other_chunk = other.chunk();
        let other_data = other.entity.take_changed_data();
        if let Some(data) = other.delta_packets(&other_data) {
            self.send_to_viewers(&[other_chunk], || PlayerEvent::EntityPackets {
                entity: partner,
                packets: data.clone(),
            });
        }
        self.ents.entities.insert(partner, other);
        self.apply_effects(tracked.id(), tracked.chunk(), effects);
        if let Some(child) = child {
            self.receive_entity(child, 0);
        }
    }
}

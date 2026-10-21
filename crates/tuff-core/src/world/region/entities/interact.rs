use super::*;

impl RegionActor {
    /// A player's melee hit on one of this region's entities.
    pub(in crate::world::region) fn attack_entity(
        &mut self,
        attacker: PlayerHandle,
        target: i32,
        mut attack: EntityAttack,
        sweep: Option<Sweep>,
    ) {
        let Some(mut tracked) = self.ents.entities.remove(&target) else {
            return;
        };
        if tracked.entity.base().kind == EntityType::Interaction {
            attacker.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::HurtEntity {
                    victim: tracked.entity.facts(),
                    damage: DamageFacts {
                        kind: &tuff_vanilla_data::damage_type::GENERIC,
                        source: None,
                        direct: None,
                    },
                    weapon: None,
                    dealt: 1.0,
                    taken: 1.0,
                },
            )));
            self.ents.entities.insert(target, tracked);
            return;
        }
        if tracked.entity.base().invulnerable {
            self.ents.entities.insert(target, tracked);
            return;
        }
        let hooks = self.shared.region_hooks.clone();
        let scale: f32 = hooks
            .iter()
            .map(|hook| {
                let by = Some((attack.attacker_uuid, EntityType::Player));
                let melee = &tuff_vanilla_data::damage_type::PLAYER_ATTACK;
                hook.entity_hurt_scale(self, &*tracked.entity, melee, by)
            })
            .product();
        if scale <= 0.0 {
            self.ents.entities.insert(target, tracked);
            return;
        }
        attack.damage *= scale;
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let kind = tracked.entity.base().kind;
        let (outcome, mut effects) = self.with_context(&summaries, &mut budget, |cx| {
            let outcome = tracked.entity.attacked(&attack, cx)?;
            if outcome.hurt {
                for hook in hooks.iter() {
                    hook.entity_melee_hit(&mut *tracked.entity, &attack, cx);
                }
            }
            Some(outcome)
        });
        let Some(outcome) = outcome else {
            self.ents.entities.insert(target, tracked);
            return;
        };
        let position = tracked.entity.base().position;
        let height = tracked.entity.bounds()[4] - position[1];
        let sound = |sound: SoundEvent, rng: &mut rand::rngs::StdRng| {
            framed(&play::Sound {
                sound_event: sound.id(),
                category: play::sound_category::PLAYERS,
                position: attack.attacker_position,
                volume: 1.0,
                pitch: 1.0,
                seed: rng.r#gen(),
            })
        };
        let mut chunk_packets = Vec::new();
        let mut particle_packets = Vec::new();
        let mut entity_packets = Vec::new();
        if !outcome.hurt {
            if !attack.sweep {
                chunk_packets.push(sound(SoundEvent::EntityPlayerAttackNodamage, &mut self.rng));
            }
        } else {
            if attack.critical {
                chunk_packets.push(sound(SoundEvent::EntityPlayerAttackCrit, &mut self.rng));
                entity_packets.push(framed(&play::Animate {
                    entity_id: target,
                    animation: play::animation::CRITICAL_HIT,
                }));
            } else if !attack.sweep && sweep.is_none() {
                chunk_packets.push(sound(SoundEvent::EntityPlayerAttackStrong, &mut self.rng));
            }
            if attack.magic {
                entity_packets.push(framed(&play::Animate {
                    entity_id: target,
                    animation: play::animation::MAGIC_CRITICAL_HIT,
                }));
            }
            if !attack.sweep && outcome.health_lost > 2.0 {
                particle_packets.push(framed(&play::Particles {
                    particle: tuff_vanilla_data::registry::ParticleType::DamageIndicator.id(),
                    position: [position[0], position[1] + height * 0.5, position[2]],
                    spread: [0.1, 0.0, 0.1],
                    speed: 0.2,
                    count: (outcome.health_lost as f64 * 0.5) as i32,
                    alternative: false,
                }));
            }
            tracked.sync.impulse();
            attacker.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::HurtEntity {
                    victim: tracked.entity.facts(),
                    damage: DamageFacts {
                        kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
                        source: None,
                        direct: None,
                    },
                    weapon: attack.weapon.clone(),
                    dealt: attack.damage,
                    taken: outcome.health_lost,
                },
            )));
            if !attack.sweep {
                attacker.send(PlayerEvent::AttackLanded {
                    weapon: attack.weapon.clone(),
                    target,
                    target_position: position,
                    knockback: attack.knockback > 0.0,
                    sweep,
                    health_lost: outcome.health_lost,
                    target_kind: kind,
                    dealt: attack.damage,
                });
            }
        }
        if !chunk_packets.is_empty() {
            self.send_sound_near(attack.attacker_position, 1.0, None, chunk_packets.into());
        }
        if !particle_packets.is_empty() {
            self.send_near(
                position,
                crate::world::region::blocks::PARTICLE_RANGE,
                None,
                particle_packets.into(),
            );
        }
        if !entity_packets.is_empty() {
            effects.push(Effect::EntityPackets(entity_packets.into()));
        }
        let chunk = tracked.chunk();
        let flags_changed = tracked.entity.base().on_fire();
        self.send_changed_data(&mut tracked);
        if flags_changed && let Some(flags) = tracked.flags_packets() {
            self.send_to_viewers(&[chunk], || PlayerEvent::EntityPackets {
                entity: target,
                packets: flags.clone(),
            });
        }
        let track = tracked.broadcast(self.dimension, chunk);
        self.send_to_viewers(&[chunk], || PlayerEvent::Track(track.clone()));
        self.ents.entities.insert(target, tracked);
        self.apply_effects(target, chunk, effects);
    }

    /// Runs `act` on each entity in the block at `pos` in id order until
    /// it answers, as a dispenser acts on what stands in front of it.
    pub(in crate::world::region) fn act_on_entities_in<T>(
        &mut self,
        pos: BlockPos,
        mut act: impl FnMut(&mut dyn RegionEntity, &mut TickContext) -> Option<T>,
    ) -> Option<T> {
        let block = [
            f64::from(pos.x),
            f64::from(pos.y),
            f64::from(pos.z),
            f64::from(pos.x + 1),
            f64::from(pos.y + 1),
            f64::from(pos.z + 1),
        ];
        let mut ids: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter(|(_, t)| crate::world::physics::intersects(t.entity.bounds(), block))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            let Some(mut tracked) = self.ents.entities.remove(&id) else {
                continue;
            };
            let summaries = self.entity_summaries();
            let mut budget = 0;
            let (done, effects) = self.with_context(&summaries, &mut budget, |cx| {
                act(tracked.entity.as_mut(), cx)
            });
            let chunk = tracked.chunk();
            self.send_changed_data(&mut tracked);
            self.ents.entities.insert(id, tracked);
            self.apply_effects(id, chunk, effects);
            if done.is_some() {
                return done;
            }
        }
        None
    }

    pub(in crate::world::region) fn shear_entity_in(&mut self, pos: BlockPos) -> bool {
        self.act_on_entities_in(pos, |entity, cx| entity.shear(cx).then_some(()))
            .is_some()
    }

    pub(in crate::world::region) fn interact_entity(
        &mut self,
        player: PlayerHandle,
        target: i32,
        interaction: EntityInteraction,
    ) {
        self.interact_entity_with(&player, target, &interaction);
        self.return_hand(&player, &interaction);
    }

    /// Hands what is left of the clicking player's hand stack back, ahead
    /// of the click's other answers, or drops it if the player is gone.
    fn return_hand(&mut self, player: &PlayerHandle, interaction: &EntityInteraction) {
        let Some(stack) = interaction
            .hand
            .take_all()
            .filter(|stack| !stack.is_empty())
        else {
            return;
        };
        let creative = interaction.creative;
        if let Err(refused) = player.try_send(PlayerEvent::HandReturned {
            slot: interaction.hand.slot,
            stack,
            creative,
        }) && let PlayerEvent::HandReturned { stack, .. } = *refused
        {
            if creative {
                stack.burn(tuff_item::item::BurnReason::Creative);
            } else {
                self.spawn_item_at(stack, interaction.position, [0.0; 3], 0);
            }
        }
    }

    fn interact_entity_with(
        &mut self,
        player: &PlayerHandle,
        target: i32,
        interaction: &EntityInteraction,
    ) {
        let content = self.shared.custom.clone();
        let (custom, hidden) = crate::custom::items::seen_by_entity(&content, interaction);
        let seen = hidden.as_ref().unwrap_or(interaction);
        if !self.ents.entities.contains_key(&target) {
            return;
        }
        let hooks = self.shared.region_hooks.clone();
        if let Some(effect) = hooks
            .iter()
            .find_map(|hook| hook.interact_entity(self, player, target, seen))
        {
            self.return_hand(player, interaction);
            if let Some(facts) = self.ents.entities.get(&target).map(|t| t.entity.facts()) {
                player.send(PlayerEvent::Advancement(Box::new(
                    crate::world::messages::AdvancementEvent::InteractedWithEntity {
                        item: interaction
                            .stack
                            .as_ref()
                            .map(tuff_item::item::ItemSnapshot::of),
                        entity: facts,
                    },
                )));
            }
            player.send(PlayerEvent::EntityInteracted {
                off_hand: interaction.off_hand,
                item: interaction.stack.as_deref().map(|stack| stack.item),
                effect,
            });
            return;
        }
        let Some(mut tracked) = self.ents.entities.remove(&target) else {
            return;
        };
        if tracked.entity.seats() > 0
            && !tracked.entity.mount_refused_by(seen.stack.as_deref())
            && !interaction.sneaking
            && !interaction.riding
            && self.mount(&mut tracked, player, interaction.player_uuid)
        {
            self.ents.entities.insert(target, tracked);
            return;
        }
        if tracked.entity.container().is_some()
            && tracked
                .entity
                .container_opens_for(player.id, interaction.sneaking)
        {
            self.ents.entities.insert(target, tracked);
            self.return_hand(player, interaction);
            self.open_entity_container(player, target);
            return;
        }
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (outcome, effects) = self.with_context(&summaries, &mut budget, |cx| {
            tracked.entity.interact(seen, cx)
        });
        let custom_behavior = custom.as_ref().is_some_and(|def| def.behavior.is_some());
        if outcome.is_some() || !custom_behavior {
            self.return_hand(player, interaction);
        }
        let chunk = tracked.chunk();
        let facts = tracked.entity.facts();
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(target, tracked);
        if outcome.is_some() {
            player.send(PlayerEvent::Advancement(Box::new(
                crate::world::messages::AdvancementEvent::InteractedWithEntity {
                    item: interaction
                        .stack
                        .as_ref()
                        .map(tuff_item::item::ItemSnapshot::of),
                    entity: facts,
                },
            )));
        }
        let mut discard = false;
        let effects = effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::TellPlayer { player: id, event } if id == interaction.player => {
                    player.send(*event);
                    None
                }
                Effect::Discard => {
                    discard = true;
                    None
                }
                other => Some(other),
            })
            .collect();
        self.apply_effects(target, chunk, effects);
        if discard {
            self.remove_entity(target, &[chunk]);
        }
        let outcome = match (outcome, custom.and_then(|def| def.behavior.clone())) {
            (None, Some(behavior)) if !discard => {
                behavior.0.use_on_entity(self, target, interaction)
            }
            (outcome, _) => outcome,
        };
        if let Some(effect) = outcome {
            player.send(PlayerEvent::EntityInteracted {
                off_hand: interaction.off_hand,
                item: interaction.stack.as_deref().map(|stack| stack.item),
                effect,
            });
        }
    }

    /// A player right-clicked a bed occupied by a mob (a sleeping villager),
    /// not a player: wakes it instead of showing "this bed is occupied".
    /// `pos` is the bed's head position, `foot` its other half; returns
    /// true when a mob occupant was found and woken.
    pub(in crate::world::region) fn wake_bed_occupant(
        &mut self,
        pos: BlockPos,
        foot: BlockPos,
    ) -> bool {
        let Some(target) = self.ents.entities.iter().find_map(|(id, tracked)| {
            tracked
                .entity
                .sleeping_in()
                .is_some_and(|bed| bed == pos || bed == foot)
                .then_some(*id)
        }) else {
            return false;
        };
        let Some(mut tracked) = self.ents.entities.remove(&target) else {
            return false;
        };
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (_, effects) = self.with_context(&summaries, &mut budget, |cx| {
            tracked.entity.wake_from_bed(cx);
        });
        let chunk = tracked.chunk();
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(target, tracked);
        self.apply_effects(target, chunk, effects);
        true
    }

    /// A player took a merchant's offer: the merchant applies it (uses,
    /// its experience) or refuses, and the player hears which.
    pub(in crate::world::region) fn trade(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        request: crate::world::trading::TradeRequest,
    ) {
        let Some(mut tracked) = self.ents.entities.remove(&request.merchant) else {
            player.send(PlayerEvent::TradeAnswered {
                window_id,
                accepted: false,
            });
            return;
        };
        let summaries = self.entity_summaries();
        let mut budget = 0;
        let (accepted, effects) = self.with_context(&summaries, &mut budget, |cx| {
            tracked.entity.trade(&request, cx)
        });
        let chunk = tracked.chunk();
        self.send_changed_data(&mut tracked);
        self.ents.entities.insert(request.merchant, tracked);
        player.send(PlayerEvent::TradeAnswered {
            window_id,
            accepted: accepted.unwrap_or(false),
        });
        self.apply_effects(request.merchant, chunk, effects);
    }

    pub(in crate::world::region) fn stop_trading(&mut self, merchant: i32, player: u128) {
        if let Some(tracked) = self.ents.entities.get_mut(&merchant) {
            tracked.entity.stop_trading(player);
        }
    }
}

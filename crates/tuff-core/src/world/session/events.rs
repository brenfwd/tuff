use super::*;

impl PlayerSession {
    pub async fn handle_event(&mut self, event: PlayerEvent) {
        self.on_event(event).await;
    }

    /// Handles the events this player receives until one matches `done`
    /// (which is handled too); false if none came within ten seconds.
    pub async fn handle_events_until(
        &mut self,
        mut done: impl FnMut(&PlayerEvent) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.conn.events.recv()).await
        {
            let finished = done(&event);
            self.on_event(event).await;
            if finished {
                return true;
            }
        }
        false
    }

    pub async fn on_event(&mut self, event: PlayerEvent) {
        if self.inventory_busy() && menus::deferred::defers_event(&event) {
            self.defer(menus::Deferred::Event(Box::new(event)));
            return;
        }
        let _timer = tuff_timings::timer!("session.event");
        let tick = self.tick;
        let Some(event) = self.event_hooks(event).await else {
            return;
        };
        if matches!(
            event,
            PlayerEvent::Hurt { .. }
                | PlayerEvent::EntityHurt(_)
                | PlayerEvent::Attacked(_)
                | PlayerEvent::Exploded { .. }
                | PlayerEvent::Knocked { .. }
                | PlayerEvent::Pushed(_)
                | PlayerEvent::Nudged(_)
                | PlayerEvent::PistonPushed { .. }
        ) {
            self.motion.pushed_at = Some(self.tick);
        }
        match event {
            PlayerEvent::Chunk {
                dimension,
                pos,
                packet,
                snapshot,
            } => {
                if dimension != self.dimension || !self.view.subscribed.contains(&pos) {
                    return;
                }
                if let Some(queued) = self.view.queue.iter_mut().find(|q| q.pos == pos) {
                    queued.packet = packet;
                    queued.snapshot = snapshot;
                    queued.followups.clear();
                } else {
                    self.view.sent.remove(&pos);
                    self.view.queue.push_back(QueuedChunk {
                        pos,
                        packet,
                        snapshot,
                        followups: Vec::new(),
                    });
                }
            }
            PlayerEvent::BlockChanges {
                dimension,
                pos,
                changes,
            } => {
                if dimension != self.dimension {
                    return;
                }
                if self.view.sent.contains(&pos) {
                    self.view.mirror.apply(pos, &changes);
                } else if let Some(chunk) = self
                    .view
                    .queue
                    .iter_mut()
                    .find(|q| q.pos == pos)
                    .and_then(|queued| queued.snapshot.as_mut())
                {
                    mirror::apply_changes(chunk, &changes);
                }
            }
            PlayerEvent::ChunkPackets {
                dimension,
                pos,
                packets,
            } => {
                if dimension != self.dimension {
                    return;
                }
                if self.view.sent.contains(&pos) {
                    self.send_all(packets.iter().cloned());
                } else if let Some(queued) = self.view.queue.iter_mut().find(|q| q.pos == pos) {
                    queued.followups.extend(packets.iter().cloned());
                }
            }
            PlayerEvent::Track(track) => {
                if track.dimension != self.dimension {
                    return;
                }
                let driving = self
                    .ride
                    .riding
                    .as_ref()
                    .is_some_and(|r| r.driver && r.vehicle == track.entity);
                let mut out = Vec::new();
                let mut entities = std::mem::take(&mut self.view.entities);
                entities.track(track, &self.area(), tick, driving, &mut out);
                self.view.entities = entities;
                self.send_all(out);
            }
            PlayerEvent::Untrack { entity, seq } => {
                let mut out = Vec::new();
                self.view.entities.untrack(entity, seq, tick, &mut out);
                self.send_all(out);
            }
            PlayerEvent::Packets(packets) => {
                for packet in packets.iter() {
                    self.send_raw(packet.clone());
                }
            }
            PlayerEvent::Border { dimension, packet } => {
                if dimension == self.dimension {
                    self.send_raw(packet);
                }
            }
            PlayerEvent::EntityPackets { entity, packets } => {
                let mut out = Vec::new();
                self.view
                    .entities
                    .entity_packets(entity, &packets, &mut out);
                self.send_all(out);
            }
            PlayerEvent::DebugEntityValue {
                entity,
                feed,
                packet,
            } => {
                let mut out = Vec::new();
                self.view
                    .entities
                    .debug_value(entity, feed, packet, &mut out);
                self.send_all(out);
            }
            PlayerEvent::ActionResult { sequence, outcome } => {
                self.on_action_result(sequence, outcome).await;
            }
            PlayerEvent::ItemUsedOnBlock {
                off_hand,
                item,
                used_on,
            } => {
                self.used_on_block(
                    off_hand,
                    item,
                    &crate::world::messages::Placement {
                        used_on: *used_on,
                        placed: None,
                    },
                );
            }
            PlayerEvent::EntityInteracted {
                off_hand,
                item,
                effect,
            } => {
                if self.inventory.hand(off_hand).map(|s| s.item()) == item {
                    self.apply_hand_effect(off_hand, effect).await;
                }
                self.send_inventory();
                let swing = packets([framed(&play::SwingAnimation::whack(
                    self.id,
                    i32::from(off_hand),
                ))]);
                self.broadcast_entity(swing, false).await;
            }
            PlayerEvent::HandReturned {
                slot,
                stack,
                creative,
            } => {
                if creative {
                    stack.burn(tuff_item::item::BurnReason::Creative);
                    return;
                }
                let leftover = if self.is_dead() {
                    Some(stack)
                } else {
                    self.inventory.put_back(slot, stack)
                };
                self.send_inventory();
                if let Some(leftover) = leftover {
                    let at = self.pose.position;
                    self.spawn_item(leftover, at, [0.0; 3], player::THROWN_PICKUP_DELAY)
                        .await;
                }
            }
            PlayerEvent::ToolWorn { slot, stack, wear } => {
                self.return_worn_tool(slot, stack, wear).await;
            }
            PlayerEvent::OfferItem {
                item,
                offer,
                stack,
                region,
            } => {
                let hooks = self.shared.session_hooks.clone();
                let taken = if self.is_dead()
                    || self.mode.game_mode == GameMode::Spectator
                    || region.dimension != self.dimension
                    || !hooks
                        .iter()
                        .all(|hook| hook.allows_item_pickup(self, stack.any()))
                {
                    0
                } else {
                    self.inventory.room_for(&stack)
                };
                region.internal(RegionInternal::ItemTaken {
                    item,
                    offer,
                    player: self.conn.handle.clone(),
                    taken,
                });
            }
            PlayerEvent::OfferShoulder {
                entity,
                offer,
                region,
            } => self.offer_shoulder(entity, offer, region),
            PlayerEvent::ShoulderGranted { offer, entity } => {
                self.shoulder_granted(offer, entity).await;
            }
            PlayerEvent::Run(task) => task.run(self).await,
            PlayerEvent::DispenserEquip { slot, stack } => {
                let window = Inventory::armor_slot(slot);
                match window {
                    Some(window) if !self.is_dead() && self.inventory.equipped(slot).is_none() => {
                        self.inventory.set_slot(window, Some(stack));
                        self.send_inventory();
                    }
                    _ => {
                        let leftover = if self.is_dead() {
                            Some(stack)
                        } else {
                            self.inventory.add(stack)
                        };
                        self.send_inventory();
                        if let Some(leftover) = leftover {
                            let at = self.pose.position;
                            self.spawn_item(leftover, at, [0.0; 3], player::THROWN_PICKUP_DELAY)
                                .await;
                        }
                    }
                }
            }
            PlayerEvent::ItemGranted { stack, picked_up } => {
                let item = stack.item();
                let offered = stack.count();
                let leftover = if self.is_dead() {
                    Some(stack)
                } else {
                    self.inventory.add(stack)
                };
                let taken = offered - leftover.as_ref().map_or(0, |l| l.count());
                if taken > 0 {
                    self.send_inventory();
                    if picked_up {
                        self.progress
                            .stats
                            .award(Stat::item(StatType::PickedUp, item), i32::from(taken));
                    }
                }
                if let Some(leftover) = leftover {
                    let at = self.pose.position;
                    self.spawn_item(leftover, at, [0.0; 3], player::THROWN_PICKUP_DELAY)
                        .await;
                }
            }
            // Left to session hooks.
            PlayerEvent::AttackLanded { target, .. } => {
                self.body.combat.record_victim(self.tick, target);
            }
            PlayerEvent::Attacked(_) | PlayerEvent::Knocked { .. } => {}
            PlayerEvent::Exploded {
                damage,
                blast,
                push,
                push_sent,
            } => self.exploded(damage, blast, push, push_sent).await,
            PlayerEvent::Overlay(message) => self.send(&play::SystemChat {
                content: &message,
                overlay: true,
            }),
            PlayerEvent::SignCommands {
                dimension,
                position,
                commands,
            } => self.run_sign_commands(dimension, position, commands).await,
            PlayerEvent::WakeUp => self.stop_sleeping(false).await,
            PlayerEvent::Hurt {
                amount,
                kind,
                attacker,
                position,
            } => self.hurt_by(amount, kind, attacker, position).await,
            PlayerEvent::EntityHurt(hit) => self.hurt_by_entity(*hit).await,
            PlayerEvent::Pushed(push) => {
                if !self.is_dead() && self.mode.game_mode != GameMode::Spectator {
                    self.motion.velocity_sent.push(self.tick, push);
                    let packet = packets([framed(&play::SetEntityMotion {
                        entity_id: self.id,
                        velocity: push,
                    })]);
                    self.broadcast_entity(packet, true).await;
                }
            }
            PlayerEvent::Nudged(push) => {
                if !self.is_dead() && self.mode.game_mode != GameMode::Spectator {
                    self.motion.velocity_sent.push_added(self.tick, push);
                }
            }
            PlayerEvent::PistonPushed { dimension, delta } => {
                if dimension == self.dimension
                    && !self.is_dead()
                    && self.mode.game_mode != GameMode::Spectator
                    && self.ride.riding.is_none()
                    && self.sync.pending_teleport.is_none()
                {
                    for axis in 0..3 {
                        self.pose.position[axis] += delta[axis];
                    }
                    self.motion.shoves.push(tick, ShoveKind::Moved(delta));
                    self.motion.piston_at = Some(tick);
                    if self.chunk() != self.view.center {
                        self.update_view().await;
                    }
                    self.publish(true).await;
                }
            }
            // Left to session hooks.
            PlayerEvent::Reeled { .. }
            | PlayerEvent::BobberGone(_)
            | PlayerEvent::Potion(_)
            | PlayerEvent::SpinAttackTouched { .. } => {}
            PlayerEvent::GiveEffect {
                effect,
                duration,
                amplifier,
                ambient,
            } => {
                let mut active = crate::world::effects::ActiveEffect::timed(
                    effect,
                    i64::from(duration),
                    amplifier,
                );
                active.ambient = ambient;
                self.add_effect(active).await;
            }
            PlayerEvent::ConvertEffect {
                from,
                to,
                ticks_per_level,
            } => {
                if let Some(level) = self.body.effects.amplifier(from) {
                    let removed = self.body.effects.remove(from);
                    self.apply_effect_change(removed).await;
                    let duration = ticks_per_level * (i64::from(level) + 1);
                    self.add_effect(crate::world::effects::ActiveEffect::timed(to, duration, 0))
                        .await;
                }
            }
            PlayerEvent::SwapEffect {
                from,
                to,
                duration,
                taken,
            } => {
                if let Some(level) = self.body.effects.amplifier(from) {
                    let removed = self.body.effects.remove(from);
                    self.apply_effect_change(removed).await;
                    self.add_effect(crate::world::effects::ActiveEffect::timed(
                        to, duration, level,
                    ))
                    .await;
                    if let Some(taken) = taken {
                        let _ = taken.send(level);
                    }
                }
            }
            PlayerEvent::AddEffect(effect) => self.add_effect(effect).await,
            PlayerEvent::StackEffect { effect, add, cap } => {
                let left = self.body.effects.get(effect).map_or(0, |e| e.duration);
                if left < cap {
                    let duration = (left + add).min(cap);
                    self.add_effect(crate::world::effects::ActiveEffect::timed(
                        effect, duration, 0,
                    ))
                    .await;
                }
            }
            PlayerEvent::RemoveEffect(effect) => {
                if self.body.effects.has(effect) {
                    let removed = self.body.effects.remove(effect);
                    self.apply_effect_change(removed).await;
                }
            }
            PlayerEvent::Riding(riding) => self.ride(riding).await,
            PlayerEvent::VehicleMotion { vehicle, motion } => self.vehicle_motion(vehicle, motion),
            PlayerEvent::Advancement(event) => self.reported(*event),
            PlayerEvent::VehicleBoosted { .. } => self.boosted().await,
            PlayerEvent::KilledEntity(kill) => {
                let kind = kill.kind;
                self.progress
                    .stats
                    .award(Stat::entity(StatType::Killed, kind), 1);
                let counter = if kind == EntityType::Player {
                    CustomStat::PlayerKills
                } else {
                    CustomStat::MobKills
                };
                self.progress.stats.award_custom(counter, 1);
                self.score_kill(&kill.holder, kind == EntityType::Player)
                    .await;
                self.killed_entity(*kill);
            }
            PlayerEvent::Dismounted { vehicle, position } => {
                self.dismounted(vehicle, position).await;
            }
            PlayerEvent::StabDismounted => self.leave_vehicle().await,
            PlayerEvent::PortalRide {
                dimension,
                position,
                turn,
            } => self.portal_ride(dimension, position, turn).await,
            PlayerEvent::PushedUp { dimension, y } => {
                if dimension == self.dimension && !self.is_dead() && self.pose.position[1] < y {
                    let [x, _, z] = self.pose.position;
                    self.teleport([x, y, z]);
                    self.publish(true).await;
                }
            }
            PlayerEvent::PearlTeleport {
                dimension,
                position,
                fall_damage,
            } => {
                if dimension == self.dimension && !self.is_dead() {
                    self.body.fall = Fall::default();
                    let origin = self.pose.position;
                    self.emit_game_event(crate::world::game_event::GameEvent::Teleport, origin)
                        .await;
                    self.teleport(position);
                    self.update_view().await;
                    self.publish(true).await;
                    if fall_damage > 0.0 {
                        self.damage(fall_damage, &damage_type::ENDER_PEARL, None)
                            .await;
                    }
                }
            }
            PlayerEvent::Surroundings {
                dimension,
                seq,
                in_water,
                submersion,
                in_lava,
                in_fire,
                climbable,
                climb_location,
                resets_fall,
                landed_on,
                portal,
                biome,
                stepping_on,
                contact,
            } => {
                if dimension != self.dimension {
                    return;
                }
                self.surroundings.submersion = submersion;
                self.surroundings.climbable = climbable;
                if climb_location.is_some() {
                    self.surroundings.fall_location = climb_location;
                } else if self.pose.on_ground {
                    self.surroundings.fall_location = None;
                }
                self.surroundings.in_lava = in_lava;
                self.surroundings.contact = contact;
                self.surroundings.in_fire = in_fire;
                if in_water {
                    self.extinguish().await;
                }
                let report = crate::world::player::FallReport {
                    in_water,
                    resets: climbable || resets_fall,
                    landed_on,
                    suppressing_bounce: self.pose.sneaking,
                };
                if let Some((damage, kind)) = self.body.fall.surroundings(seq, report)
                    && self.shared.rule_bool("fall_damage")
                {
                    self.damage(damage, kind, None).await;
                }
                if let Some(distance) = self.body.fall.powder_landing.take()
                    && distance >= 4.0
                {
                    let sound = if distance < 7.0 {
                        SoundEvent::EntityGenericSmallFall
                    } else {
                        SoundEvent::EntityGenericBigFall
                    };
                    self.sound_at(sound, self.pose.position).await;
                }
                self.surroundings.biome = biome;
                self.surroundings.stepping_on = stepping_on;
                if in_water {
                    self.fire(crate::world::advancement::trigger::Event::EnterBlock(
                        tuff_vanilla_data::Block::Water.default_state(),
                    ));
                }
                self.surroundings_hooks(portal).await;
            }
            PlayerEvent::Suffocating { dimension } => {
                if dimension == self.dimension {
                    self.surroundings.contact.suffocating = true;
                }
            }
            PlayerEvent::FluidContact {
                dimension,
                in_water,
                submersion,
                in_lava,
                in_fire,
            } => {
                if dimension != self.dimension {
                    return;
                }
                self.surroundings.submersion = submersion;
                self.surroundings.in_lava = in_lava;
                self.surroundings.in_fire = in_fire;
                self.body.fall.fluid_contact(in_water);
                if in_water {
                    self.extinguish().await;
                    self.fire(crate::world::advancement::trigger::Event::EnterBlock(
                        tuff_vanilla_data::Block::Water.default_state(),
                    ));
                }
            }
            PlayerEvent::DestroyEnded(pos) => {
                if self.actions.destroying.is_some_and(|d| d.pos == pos) {
                    self.actions.destroying = None;
                }
            }
            PlayerEvent::MenuOpen(open) => self.open_menu(open).await,
            PlayerEvent::TradeAnswered {
                window_id,
                accepted,
            } => self.trade_answered(window_id, accepted).await,
            PlayerEvent::AwardStat(stat, amount) => self.progress.stats.award_custom(stat, amount),
            PlayerEvent::WardenTracker(tracker) => self.progress.warden_tracker = tracker,
            PlayerEvent::WardenWarningLevel(Some(level)) => {
                self.progress.warden_tracker.set_warning_level(level)
            }
            PlayerEvent::WardenWarningLevel(None) => {
                self.progress.warden_tracker = Default::default()
            }
            PlayerEvent::ContainerSlots {
                dimension,
                pos,
                slots,
            } => {
                if dimension == self.dimension {
                    self.container_slots_changed(pos, slots);
                }
            }
            PlayerEvent::ContainerData {
                dimension,
                pos,
                data,
            } => {
                if dimension == self.dimension {
                    self.container_data_changed(pos, data);
                }
            }
            PlayerEvent::Experience { amount } => self.give_experience(amount).await,
            PlayerEvent::RecipesAwarded(recipes) => {
                self.award_recipes(recipes.iter().map(|recipe| &**recipe))
            }
            PlayerEvent::ContainerClosed { dimension, pos } => {
                let over = self
                    .menus
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.backing.positions().contains(&pos));
                if dimension == self.dimension && over {
                    self.close_menu(true).await;
                }
            }
            PlayerEvent::TransactionPrepared {
                window_id,
                txn,
                prepared,
                contents,
                taken,
            } => {
                self.transaction_prepared(window_id, txn, prepared, contents, taken)
                    .await
            }
            PlayerEvent::Transaction {
                window_id,
                accepted,
                contents,
                pool,
            } => {
                self.transaction_answered(window_id, accepted, contents, pool)
                    .await
            }
            PlayerEvent::EntityTransaction {
                window_id,
                accepted,
                contents,
                pool,
            } => {
                self.entity_transaction_answered(window_id, accepted, contents, pool)
                    .await
            }
            PlayerEvent::EntitySlots {
                dimension,
                entity,
                slots,
            } => {
                if dimension == self.dimension {
                    self.entity_slots_changed(entity, slots);
                }
            }
            PlayerEvent::EntityContainerMoved {
                dimension,
                entity,
                region,
            } => {
                if dimension == self.dimension
                    && let Some(menu) = self.menus.menu.as_mut()
                    && menu.backing == MenuBacking::Entity(entity)
                {
                    menu.region = region;
                }
            }
            PlayerEvent::EntityContainerClosed { dimension, entity } => {
                let over = self
                    .menus
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.backing == MenuBacking::Entity(entity));
                if dimension == self.dimension && over {
                    self.close_menu(true).await;
                }
            }
            PlayerEvent::Message(message) => self.system_message(&message),
            PlayerEvent::LowDiskSpace => self.send(&play::LowDiskSpaceWarning),
            PlayerEvent::Kick(reason) => self.kick(reason),
            PlayerEvent::HardcoreChanged => self.sync_hardcore().await,
            PlayerEvent::LiveSetting(change) => self.apply_live_change(change).await,
            PlayerEvent::ResourcePack(offer) => self.offer_resource_pack(offer),
            PlayerEvent::ResourcePackPop(id) => self.pop_resource_pack(id),
            PlayerEvent::Directive {
                directive,
                source,
                reply,
            } => {
                let outcome = self.apply_directive(directive, source).await;
                let _ = reply.send(outcome);
            }
            PlayerEvent::PluginTeleport(teleport) => {
                self.leave_vehicle().await;
                self.teleport_by_command(teleport).await;
                self.actions.portal_cooldown = crate::world::contact::PLUGIN_PORTAL_COOLDOWN;
            }
            PlayerEvent::PluginInventory(ops) => self.apply_inventory_ops(ops).await,
            PlayerEvent::SetPermission(level) => {
                self.mode.permission = level;
                self.send_permission_level();
            }
            PlayerEvent::PermissionsChanged => self.send_permission_level(),
            PlayerEvent::ReducedDebugInfo(on) => self.send(&play::EntityEvent {
                entity_id: self.id,
                status: if on {
                    play::entity_event::REDUCED_DEBUG_INFO
                } else {
                    play::entity_event::NO_REDUCED_DEBUG_INFO
                },
            }),
            PlayerEvent::Save(barrier) => {
                if self.inventory_busy() {
                    self.menus.save_barrier = Some(barrier);
                } else {
                    self.save_at_barrier(barrier).await;
                }
            }
            PlayerEvent::Shutdown => self.kick(crate::restart::shutdown_reason()),
        }
    }

    pub async fn on_action_result(&mut self, sequence: i32, outcome: ActionOutcome) {
        self.answered(sequence);
        match outcome {
            ActionOutcome::Broken { tool_damage, state } => {
                if !self.mode.abilities.instabuild {
                    self.progress
                        .stats
                        .award(Stat::block(StatType::Mined, state.block()), 1);
                }
                self.exhaust(exhaustion::BLOCK_BREAK);
                if self
                    .inventory
                    .hand(false)
                    .is_some_and(|stack| stack.tool().is_some())
                {
                    self.hurt_hand(false, tool_damage).await;
                }
            }
            ActionOutcome::Mining => {}
            ActionOutcome::Placed {
                off_hand,
                item,
                placement,
            } => {
                self.progress
                    .stats
                    .award(Stat::item(StatType::Used, item), 1);
                if let Some(placement) = placement {
                    self.used_on_block(off_hand, item, &placement);
                }
                let damageable = self
                    .inventory
                    .hand(off_hand)
                    .is_some_and(|s| s.item() == item && s.max_damage().is_some());
                if damageable {
                    self.hurt_hand(off_hand, 1).await;
                }
                self.send_inventory();
                let swing = packets([framed(&play::SwingAnimation::whack(
                    self.id,
                    i32::from(off_hand),
                ))]);
                self.broadcast_entity(swing, false).await;
            }
            ActionOutcome::Ate {
                nutrition,
                saturation,
            } => {
                let before = self.body.food.level;
                self.body.food.eat(nutrition, saturation);
                self.food_changed_hooks(before);
                self.send_inventory();
            }
            ActionOutcome::Used {
                off_hand,
                item,
                effect,
            } => {
                self.progress
                    .stats
                    .award(Stat::item(StatType::Used, item), 1);
                if item == Item::Bucket
                    && let HandEffect::Exchange(filled) | HandEffect::Spent(Some(filled)) = effect
                {
                    let filled = tuff_item::item::ItemPicture::new(filled, 1);
                    self.fire(crate::world::advancement::trigger::Event::FilledBucket(
                        filled.any(),
                    ));
                }
                let costly = matches!(
                    effect,
                    HandEffect::Consume(_)
                        | HandEffect::Damage(_)
                        | HandEffect::Emptied(_)
                        | HandEffect::Exchange(_)
                        | HandEffect::ExchangeStack(_)
                        | HandEffect::Split(_)
                );
                if !matches!(effect, HandEffect::Spent(_))
                    && (!costly || self.mode.abilities.instabuild)
                    && self
                        .inventory
                        .hand(off_hand)
                        .is_some_and(|s| s.item() == item)
                {
                    self.apply_hand_effect(off_hand, effect).await;
                }
                self.send_inventory();
                let swing = packets([framed(&play::SwingAnimation::whack(
                    self.id,
                    i32::from(off_hand),
                ))]);
                self.broadcast_entity(swing, false).await;
            }
            ActionOutcome::Interacted { off_hand } => {
                self.send_inventory();
                let swing = packets([framed(&play::SwingAnimation::whack(
                    self.id,
                    i32::from(off_hand),
                ))]);
                self.broadcast_entity(swing, false).await;
            }
            ActionOutcome::StartUsing {
                off_hand,
                item,
                ticks,
            } => {
                self.progress
                    .stats
                    .award(Stat::item(StatType::Used, item), 1);
                if self
                    .inventory
                    .hand(off_hand)
                    .is_some_and(|s| s.item() == item)
                {
                    self.actions.using = Some(UsingItem::new(off_hand, item, ticks));
                    self.publish(false).await;
                    if self
                        .inventory
                        .hand(off_hand)
                        .is_none_or(|s| s.use_effects().interact_vibrations)
                    {
                        self.emit_game_event(
                            crate::world::game_event::GameEvent::ItemInteractStart,
                            self.pose.position,
                        )
                        .await;
                    }
                }
            }
            ActionOutcome::Rejected(corrections) => {
                self.send_all(corrections.iter().cloned());
                self.send_inventory();
            }
        }
        self.send(&play::BlockChangedAck(sequence));
    }
}

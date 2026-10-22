use super::*;

const CHAT_FILTER_QUEUE: usize = 32;

impl PlayerSession {
    pub async fn on_packet(&mut self, packet: Serverbound) {
        let _timer = tuff_timings::timer!("session.packet");
        if self.inventory_busy() && menus::deferred::defers_packet(&packet) {
            self.defer(menus::Deferred::Packet(packet));
            return;
        }
        if self.over_rate_limit() {
            return;
        }
        let Some(packet) = self.secure_chat(packet) else {
            return;
        };
        self.note_activity(&packet);
        if !self.admit_move(&packet) {
            return;
        }
        if let Serverbound::Attack(target) = packet
            && self.mode.game_mode == GameMode::Spectator
        {
            self.spectate(Some(target)).await;
            return;
        }
        let Some(packet) = self.intercept(packet).await else {
            return;
        };
        let Some(packet) = self.packet_hooks(packet).await else {
            return;
        };
        match packet {
            Serverbound::AcceptTeleportation {
                id,
                position,
                rotation,
            } => self.accept_teleport(id, position, rotation).await,
            Serverbound::Move(_) if self.hooks_suspended() => {}
            Serverbound::Move(movement) => self.handle_move(movement).await,
            // Left to session hooks.
            Serverbound::Attack(_) | Serverbound::Interact { .. } => {}
            Serverbound::Spectate(target) => self.spectate(target).await,
            Serverbound::TeleportToEntity(uuid) => {
                let index = self.shared.players.borrow().clone();
                self.teleport_to_player(&index, uuid).await;
            }
            Serverbound::MoveVehicle {
                position,
                yaw,
                pitch,
                on_ground,
            } => self.drive(position, yaw, pitch, on_ground).await,
            Serverbound::PaddleBoat { left, right } => self.paddle(left, right).await,
            Serverbound::Chat(message) => {
                if self
                    .mode
                    .permission
                    .grants_chat(ChatPermission::SendMessages)
                {
                    self.detect_spam(false);
                    self.chat(message).await
                }
            }
            Serverbound::ChatCommand(input) => {
                if self
                    .mode
                    .permission
                    .grants_chat(ChatPermission::SendCommands)
                {
                    self.detect_spam(true);
                    self.run_command(&input).await
                }
            }
            Serverbound::SetGameRule(edits) => self.apply_game_rule_edits(&edits).await,
            Serverbound::CommandSuggestion { id, text } => self.suggest(id, &text).await,
            Serverbound::ChangeGameMode(mode) => {
                if self.mode.permission >= PermissionLevel::Gamemasters
                    && self.set_game_mode(mode).await
                {
                    self.system_message(&gamemode::set_self(mode));
                }
            }
            Serverbound::ChangeDifficulty(difficulty) => {
                if self.mode.permission >= PermissionLevel::Gamemasters {
                    let (reply, _) = oneshot::channel();
                    self.coordinator(CoordinatorMessage::Difficulty {
                        difficulty: Some(difficulty),
                        reply,
                    })
                    .await;
                }
            }
            Serverbound::LockDifficulty(locked) => {
                if self.mode.permission >= PermissionLevel::Gamemasters {
                    self.coordinator(CoordinatorMessage::LockDifficulty { locked })
                        .await;
                }
            }
            Serverbound::PlayerAbilities { flying } => {
                self.mode.abilities.flying = flying && self.mode.abilities.may_fly;
            }
            Serverbound::SetCreativeModeSlot { slot, stack } => {
                self.set_creative_slot(slot, stack).await;
            }
            Serverbound::ClientCommand(action) if self.client_command_hooks(action).await => {}
            Serverbound::ClientCommand(0) => self.respawn().await,
            Serverbound::ClientCommand(1) => self.send_stats(),
            Serverbound::ClientCommand(2) => self.send_game_rule_values(),
            Serverbound::ClientCommand(_) => {}
            Serverbound::ClientInformation(info) => {
                let skin_changed = self.info.skin_parts != info.skin_parts
                    || self.info.main_hand != info.main_hand;
                let view_changed = self.info.view_distance != info.view_distance;
                self.info = info;
                if skin_changed {
                    let values = [
                        (
                            metadata::MAIN_ARM,
                            MetadataValue::HumanoidArm(i32::from(info.main_hand != 0)),
                        ),
                        (
                            metadata::SKIN_PARTS,
                            MetadataValue::Byte(info.skin_parts as i8),
                        ),
                    ];
                    let data = packets([framed(&play::SetEntityData {
                        entity_id: self.id,
                        values: &values,
                    })]);
                    self.broadcast_entity(data, true).await;
                }
                self.coordinator(CoordinatorMessage::Info {
                    player: self.id,
                    actions: play::player_info::UPDATE_HAT,
                    entry: self.info_entry(),
                    listed: self.info.allow_server_listings,
                })
                .await;
                if view_changed {
                    self.update_view().await;
                }
            }
            Serverbound::ContainerClick {
                window_id,
                state_id,
                slot,
                button,
                mode,
                changed,
                carried,
            } => {
                if self.menus.menu.is_some() {
                    let click = QueuedClick {
                        state_id,
                        slot,
                        button,
                        mode,
                        changed,
                        carried,
                    };
                    self.menu_click(window_id, click).await;
                    return;
                }
                if window_id != 0 || self.is_dead() {
                    return;
                }
                if self.mode.game_mode == GameMode::Spectator {
                    self.send_inventory();
                    return;
                }
                let creative = self.mode.abilities.instabuild;
                let mut hook = GridHook::new(MenuKind::Player, self.shared.recipes()).limited_to(
                    menus::crafting_gate(&self.shared, &self.progress.recipe_book),
                );
                let result_before = self.inventory.slots()[0].as_ref().map(|s| s.item());
                let held_before = result_before.map(|item| self.held_count(item));
                let drops = self.inventory.click(
                    slot,
                    button,
                    mode,
                    creative,
                    &mut hook,
                    &self.shared.recipes(),
                );
                if hook.took
                    && let (Some(item), Some(before)) = (result_before, held_before)
                {
                    let dropped: i32 = drops
                        .iter()
                        .filter(|stack| stack.item() == item)
                        .map(|stack| i32::from(stack.count()))
                        .sum();
                    self.count_crafted(item, self.held_count(item) - before + dropped);
                }
                self.crafted_recipes(&hook.crafted);
                self.refresh_inventory_result();
                self.flush_bundle_sounds().await;
                let in_sync = changed.iter().all(|(slot, view)| {
                    usize::try_from(*slot)
                        .ok()
                        .and_then(|s| self.inventory.slots().get(s))
                        .is_some_and(|stack| {
                            crate::protocol::codec::HashedSlot::matches(
                                view.as_ref(),
                                stack.as_ref().map(ItemStack::view),
                            )
                        })
                }) && crate::protocol::codec::HashedSlot::matches(
                    carried.as_ref(),
                    self.inventory.carried().map(ItemStack::view),
                );
                if !in_sync {
                    self.send_inventory();
                }
                for stack in drops {
                    self.throw_item(stack).await;
                }
            }
            Serverbound::ContainerClose(window_id) => {
                if let Some(menu) = &self.menus.menu {
                    if menu.window_id == window_id {
                        if menu.closing.is_none() && !self.allows_menu_close_hooks(menu.kind) {
                            self.reopen_menu();
                            return;
                        }
                        self.close_menu(false).await;
                    }
                    return;
                }
                let drops = self.inventory.close();
                self.send_inventory();
                for stack in drops {
                    self.throw_item(stack).await;
                }
            }
            Serverbound::ContainerButtonClick { window_id, button } => {
                self.menu_button(window_id, button).await;
            }
            Serverbound::ContainerSlotState {
                slot,
                window_id,
                enabled,
            } => self.crafter_slot_state(window_id, slot, enabled).await,
            Serverbound::SelectTrade(index) => self.select_trade(index),
            Serverbound::SetBeacon { primary, secondary } => {
                self.set_beacon(primary, secondary).await
            }
            Serverbound::SelectBundleItem { slot, index } => self.select_bundle_item(slot, index),
            Serverbound::PlaceRecipe {
                window_id,
                recipe,
                use_max_items,
            } => self.place_recipe(window_id, recipe, use_max_items).await,
            Serverbound::SeenAdvancements(tab) => self.seen_advancements(tab),
            Serverbound::RecipeBookSeenRecipe(recipe) => {
                if let Ok(recipe) = u32::try_from(recipe) {
                    self.progress
                        .recipe_book
                        .seen(&self.shared.recipes(), recipe);
                }
            }
            Serverbound::RecipeBookChangeSettings {
                book,
                open,
                filtering,
            } => {
                if let Ok(book) = usize::try_from(book) {
                    self.progress.recipe_book.set_setting(book, open, filtering);
                }
            }
            Serverbound::KeepAlive(value) => {
                if let Some((expected, sent)) = self.net.keep_alive
                    && expected == value
                {
                    self.net.keep_alive = None;
                    self.net.latency_ms =
                        (self.net.latency_ms * 3 + sent.elapsed().as_millis() as i32) / 4;
                    self.coordinator(CoordinatorMessage::Info {
                        player: self.id,
                        actions: play::player_info::UPDATE_LATENCY,
                        entry: self.info_entry(),
                        listed: self.info.allow_server_listings,
                    })
                    .await;
                }
            }
            Serverbound::PlayerAction {
                status,
                position,
                face,
                sequence,
            } => self.player_action(status, position, face, sequence).await,
            Serverbound::PlayerCommand(action) => match action {
                0 => self.stop_sleeping(false).await,
                1 => self.pose.sprinting = true,
                2 => self.pose.sprinting = false,
                5 => self.open_vehicle_inventory().await,
                6 => self.try_start_gliding().await,
                _ => {}
            },
            Serverbound::RidingJump(charge) => {
                self.sync.pending_jump = u8::try_from(charge).ok().filter(|c| *c > 0);
            }
            Serverbound::PlayerInput(flags) => {
                self.sync.input_flags = flags;
                if self.client_loaded() {
                    let sneaking = flags & 0x20 != 0;
                    let changed = sneaking != self.pose.sneaking;
                    self.pose.sneaking = sneaking;
                    if sneaking && changed {
                        self.reset_camera();
                    }
                    if changed {
                        self.sneak_changed_hooks(sneaking);
                    }
                }
            }
            Serverbound::Punch => {
                self.actions.attack_reset_tick = self.tick;
                let (kind, duration_ticks) = self
                    .inventory
                    .hand(false)
                    .map_or((tuff_item::item::SwingKind::Whack, 6), |stack| {
                        stack.attack_animation()
                    });
                let swing = packets([framed(&play::SwingAnimation {
                    entity_id: self.id,
                    hand: 0,
                    kind,
                    duration_ticks,
                })]);
                self.broadcast_entity(swing, false).await;
            }
            Serverbound::SetCarriedItem(slot) => {
                let previous = self.inventory.selected();
                if self.inventory.set_selected(slot.max(0) as usize)
                    && previous != self.inventory.selected()
                    && self.actions.using.is_some_and(|using| !using.off_hand)
                {
                    self.stop_using();
                }
            }
            Serverbound::UseItemOn {
                hand,
                position,
                face,
                cursor,
                sequence,
            } => {
                self.use_item_on(hand == 1, position, face, cursor, sequence)
                    .await
            }
            Serverbound::UseItem { off_hand, sequence } => {
                if self.custom_use_item(off_hand, sequence).await
                    || self.place_on_water(off_hand, sequence).await
                    || self.use_bucket(off_hand, sequence).await
                    || self.place_boat(off_hand, sequence).await
                {
                    return;
                }
                self.send(&play::BlockChangedAck(sequence));
                if self.try_boost(off_hand).await {
                    return;
                }
                self.resolve_held_book(off_hand).await;
                if self.open_book(off_hand) {
                    return;
                }
                self.use_item_hooks(off_hand).await;
            }
            Serverbound::PickItemFromBlock {
                position,
                include_data,
            } => self.pick_block(position, include_data).await,
            Serverbound::BlockEntityTagQuery {
                transaction,
                position,
            } => self.query_block_entity_tag(transaction, position).await,
            Serverbound::EntityTagQuery {
                transaction,
                entity,
            } => self.query_entity_tag(transaction, entity).await,
            Serverbound::PickItemFromEntity {
                entity,
                include_data,
            } => self.pick_entity(entity, include_data).await,
            Serverbound::SignUpdate {
                position,
                front,
                lines,
            } => {
                let filtered = self.filter_sign_lines(&lines).await;
                if let Some(region) = self.region_of(position) {
                    region
                        .request(RegionRequest::UpdateSign {
                            player: self.conn.handle.clone(),
                            pos: position,
                            front,
                            lines,
                            filtered,
                            eye: self.eye(),
                        })
                        .await;
                }
            }
            Serverbound::SetCommandBlock {
                position,
                command,
                mode,
                track_output,
                conditional,
                automatic,
            } => {
                if !(self.mode.abilities.instabuild
                    && self.mode.permission >= PermissionLevel::Gamemasters)
                {
                    self.send(&play::SystemChat {
                        content: &TextComponent::translate("advMode.notAllowed", []),
                        overlay: false,
                    });
                } else if let Some(region) = self.region_of(position) {
                    region
                        .request(RegionRequest::SetCommandBlock {
                            player: self.conn.handle.clone(),
                            pos: position,
                            edit: crate::world::messages::CommandBlockEdit {
                                command,
                                mode: crate::world::messages::CommandMode::from_id(mode),
                                track_output,
                                conditional,
                                automatic,
                            },
                        })
                        .await;
                }
            }
            Serverbound::JigsawGenerate {
                position,
                levels,
                keep_jigsaws,
            } => {
                if self.mode.abilities.instabuild
                    && self.mode.permission >= PermissionLevel::Gamemasters
                {
                    self.generate_jigsaw(position, levels, keep_jigsaws).await;
                }
            }
            Serverbound::SetJigsawBlock(edit) => {
                if self.mode.abilities.instabuild
                    && self.mode.permission >= PermissionLevel::Gamemasters
                    && let Some(region) = self.region_of(edit.position)
                {
                    let task = crate::world::region_api::RegionTask::new(move |world| {
                        crate::world::block_entity::editors::apply_jigsaw_edit(world, &edit);
                    });
                    region
                        .request(crate::world::messages::RegionRequest::Run(task))
                        .await;
                }
            }
            Serverbound::SetTestBlock {
                position,
                mode,
                message,
            } => {
                if self.mode.abilities.instabuild
                    && self.mode.permission >= PermissionLevel::Gamemasters
                    && let Some(region) = self.region_of(position)
                {
                    let task = crate::world::region_api::RegionTask::new(move |world| {
                        crate::world::block_entity::editors::apply_test_block_edit(
                            world, position, mode, &message,
                        );
                    });
                    region
                        .request(crate::world::messages::RegionRequest::Run(task))
                        .await;
                }
            }
            Serverbound::SetStructureBlock(edit) => {
                if self.mode.abilities.instabuild
                    && self.mode.permission >= PermissionLevel::Gamemasters
                {
                    self.edit_structure_block(*edit).await;
                }
            }
            Serverbound::Pong(_) => {}
            Serverbound::ClientTickEnd => {
                if !self.sync.moved_this_tick {
                    self.sync.known_movement = [0.0; 3];
                }
                self.sync.moved_this_tick = false;
            }
            Serverbound::ChunkBatchReceived(desired) => {
                self.view.unacked_batches = self.view.unacked_batches.saturating_sub(1);
                self.view.max_unacked_batches = MAX_UNACKED_BATCHES;
                self.view.chunks_per_tick = if desired.is_nan() {
                    0.01
                } else {
                    desired.clamp(0.01, MAX_CHUNKS_PER_TICK)
                };
                if self.view.unacked_batches == 0 {
                    self.view.batch_quota = 1.0;
                }
            }
            Serverbound::ResourcePack { id, status } => self.resource_pack_status(id, status),
            Serverbound::PlayerLoaded => self.sync.load_deadline = None,
            Serverbound::EditBook { slot, pages, title } => {
                self.edit_book(slot, pages, title).await
            }
            Serverbound::DebugSubscriptionRequest(ids) => {
                let _ = self
                    .shared
                    .coordinator
                    .send(CoordinatorMessage::DebugSubscriptions {
                        player: self.conn.handle.id,
                        mask: crate::world::debug_feeds::mask(&ids),
                    })
                    .await;
            }
            Serverbound::RenameItem(name) => self.rename_anvil_item(name),
            Serverbound::ChatSessionUpdate(session) => self.chat_session_update(session),
            Serverbound::SignedChat(_)
            | Serverbound::SignedCommand(_)
            | Serverbound::ChatAck(_) => {}
            Serverbound::CustomClickAction(action) => {
                log::debug!(
                    "received custom click action {} with payload {:?}",
                    action.id,
                    action.payload
                );
            }
            Serverbound::SetCommandMinecart { .. }
            | Serverbound::CookieResponse(_)
            | Serverbound::ConfigurationAcknowledged
            | Serverbound::Ignored
            | Serverbound::CustomPayload { .. } => {}
        }
    }

    fn send_game_rule_values(&mut self) {
        if self.mode.permission < PermissionLevel::Gamemasters {
            log::warn!(
                "{} tried to request game rule values without required permissions",
                self.profile.name
            );
            return;
        }
        let values = game_rule_values(&self.shared.game_rules.borrow());
        self.send(&play::GameRuleValues(&values));
    }

    /// Applies the client's game rule edits through the `/gamerule` command, so
    /// permission checks, validation, feedback and broadcast match the command.
    async fn apply_game_rule_edits(&mut self, edits: &[(String, String)]) {
        if self.mode.permission < PermissionLevel::Gamemasters {
            return;
        }
        for (rule, value) in edits {
            let rule = rule.strip_prefix("minecraft:").unwrap_or(rule);
            let plain = |text: &str| {
                !text.is_empty() && text.chars().all(|c| c.is_ascii_graphic() && c != '/')
            };
            if plain(rule) && plain(value) {
                self.run_command(&format!("gamerule {rule} {value}")).await;
            }
        }
    }

    async fn filter_sign_lines(&self, lines: &[String]) -> Option<Vec<String>> {
        let filter = self.shared.text_filter.clone()?;
        let stripped: Vec<String> = lines
            .iter()
            .map(|line| crate::world::block_entity::sign::strip_formatting(line))
            .collect();
        let line = |i: usize| {
            let text = stripped.get(i).cloned().unwrap_or_default();
            let filter = filter.clone();
            async move {
                filter
                    .filter(&self.profile, &text)
                    .await
                    .apply(&text)
                    .unwrap_or_default()
            }
        };
        let (a, b, c, d) = tokio::join!(line(0), line(1), line(2), line(3));
        Some(vec![a, b, c, d])
    }

    async fn queue_filtered_chat(&mut self, message: String) {
        let Some(filter) = self.shared.text_filter.clone() else {
            return;
        };
        if self.sync.chat_filter.is_none() {
            let (queue, mut inbox) = tokio::sync::mpsc::channel::<String>(CHAT_FILTER_QUEUE);
            let (profile, id) = (self.profile.clone(), self.id);
            let coordinator = self.shared.coordinator.clone();
            tokio::spawn(async move {
                while let Some(message) = inbox.recv().await {
                    let audience = match filter.filter(&profile, &message).await {
                        crate::text_filter::FilterMask::PassThrough => {
                            crate::world::messages::ChatAudience::Everyone
                        }
                        mask => mask.apply(&message).map_or(
                            crate::world::messages::ChatAudience::SenderOnly,
                            crate::world::messages::ChatAudience::Masked,
                        ),
                    };
                    let chat = CoordinatorMessage::Chat {
                        player: id,
                        message,
                        audience,
                    };
                    if coordinator.send(chat).await.is_err() {
                        break;
                    }
                }
            });
            self.sync.chat_filter = Some(queue);
        }
        if let Some(queue) = &self.sync.chat_filter
            && queue.try_send(message).is_err()
        {
            self.kick(TextComponent::translate("disconnect.spam", []));
        }
    }

    pub async fn chat(&mut self, message: String) {
        let illegal = message
            .chars()
            .any(|c| c == '\u{00A7}' || c < ' ' || c == '\u{7F}');
        if illegal || message.chars().count() > MAX_CHAT_CHARS {
            self.kick(TextComponent::translate(
                "multiplayer.disconnect.illegal_characters",
                [],
            ));
            return;
        }
        if self.shared.text_filter.is_some() {
            self.queue_filtered_chat(message).await;
            return;
        }
        self.coordinator(CoordinatorMessage::Chat {
            player: self.id,
            message,
            audience: crate::world::messages::ChatAudience::Everyone,
        })
        .await;
    }
}

impl PlayerSession {
    /// The jigsaw block's Generate button: assembles its pool from the
    /// block it faces and places the result.
    async fn generate_jigsaw(&mut self, position: BlockPos, levels: i32, keep_jigsaws: bool) {
        use tuff_vanilla_data::block::Facing;
        let Some(region) = self.region_of(position) else {
            return;
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let task = crate::world::region_api::RegionTask::new(move |world| {
            let state = crate::world::growth::GrowthLevel::block(&*world, position);
            let data = world.saved_block_entity(position);
            let front = state
                .and_then(|state| state.get("orientation"))
                .and_then(|orientation| {
                    let front = orientation.split('_').next()?;
                    [
                        Facing::Down,
                        Facing::Up,
                        Facing::North,
                        Facing::South,
                        Facing::West,
                        Facing::East,
                    ]
                    .into_iter()
                    .find(|facing| facing.name() == front)
                });
            let found = data.zip(front).and_then(|(data, front)| {
                let pool = data.get_str("pool")?.to_owned();
                let target = data.get_str("target")?.to_owned();
                Some((pool, target, front))
            });
            let _ = reply.send(found);
        });
        if !region
            .request(crate::world::messages::RegionRequest::Run(task))
            .await
        {
            return;
        }
        let Ok(Some((pool, target, front))) = answer.await else {
            return;
        };
        let (dx, dy, dz) = front.step();
        let anchor = BlockPos::new(position.x + dx, position.y + dy, position.z + dz);
        let dimension = self.shared.dimension(self.dimension);
        let (generator, shape) = (dimension.generator.clone(), dimension.shape);
        let tags = self.shared.datapacks.contents().tags.clone();
        let (plan_reply, planned) = tokio::sync::oneshot::channel();
        rayon::spawn(move || {
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tuff_datapack::tags::sync_scope(tags, || {
                    generator.plan_jigsaw(&pool, &target, levels, anchor, shape, keep_jigsaws)
                })
            }));
            if let Ok(built) = built {
                let _ = plan_reply.send(built);
            }
        });
        let Ok(Some(plan)) = planned.await else {
            return;
        };
        let (min, max) = plan.chunks();
        let mut targets = Vec::new();
        for z in (min.z - 1)..=(max.z + 1) {
            for x in (min.x - 1)..=(max.x + 1) {
                let chunk = ChunkPos::new(x, z);
                if let Some(region) = self.region_in(self.dimension, chunk.region()).await {
                    targets.push((chunk, region));
                }
            }
        }
        let work = std::sync::Arc::new(crate::world::place_work::Work::Plan(plan));
        crate::world::place_work::place_chunks(
            &targets,
            &work,
            crate::world::redstone::UPDATE_CLIENTS,
        )
        .await;
    }
}

impl PlayerSession {
    /// The structure block editor: stores the edit in the block, then
    /// saves, loads or reports as asked.
    async fn edit_structure_block(&mut self, edit: crate::protocol::play::StructureBlockEdit) {
        let position = edit.position;
        let Some(region) = self.region_of(position) else {
            return;
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        let task = crate::world::region_api::RegionTask::new(move |world| {
            let _ = reply.send(structure_block::apply_edit(world, &edit));
        });
        if !region
            .request(crate::world::messages::RegionRequest::Run(task))
            .await
        {
            return;
        }
        let Ok(outcome) = answer.await else {
            return;
        };
        let message = match outcome {
            structure_block::StructureOutcome::Gone => return,
            structure_block::StructureOutcome::Say(message) => message,
            structure_block::StructureOutcome::Place {
                name,
                request,
                extent,
            } => {
                structure_block::place(self.shared.clone(), self.dimension, request, extent).await;
                TextComponent::translate("structure_block.load_success", [TextComponent::new(name)])
            }
        };
        self.send(&play::SystemChat {
            content: &message,
            overlay: false,
        });
    }
}

fn game_rule_values(rules: &crate::world::game_rules::GameRules) -> Vec<(String, String)> {
    rules
        .iter()
        .map(|(name, value)| {
            let key = if name.contains(':') {
                name.to_string()
            } else {
                format!("minecraft:{name}")
            };
            (key, value.serialize())
        })
        .collect()
}

#[cfg(test)]
mod game_rule_value_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_available_rule_is_listed_with_a_namespaced_key() {
        let rules = crate::world::game_rules::GameRules::default();
        let values = game_rule_values(&rules);
        assert_eq!(values.len(), rules.iter().count());
        assert!(values.contains(&("minecraft:random_tick_speed".into(), "3".into())));
        assert!(values.contains(&("minecraft:keep_inventory".into(), "false".into())));
    }
}

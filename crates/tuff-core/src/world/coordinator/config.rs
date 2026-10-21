use super::*;
use crate::admin::ServerPropertiesExt as _;

impl Coordinator {
    pub(super) fn run_clock(&mut self, clock: ClockId, action: &ClockAction) -> ClockOutcome {
        let mut result = None;
        let changed = self.clocks.send_if_modified(|clocks| {
            let (outcome, changed) = crate::world::clock::run_clock(clocks, clock, action);
            result = Some(outcome);
            changed
        });
        let outcome = result.expect("send_if_modified runs its closure");
        if changed {
            let world_age = *self.clock.borrow() as i64;
            let packet = self.clocks.borrow().packet(world_age, Some(clock));
            self.broadcast(&packet);
        }
        outcome
    }

    pub(super) fn game_rule(&mut self, name: &str, value: Option<RuleValue>) -> GameRuleOutcome {
        let Some(rule) = GameRules::rule(name) else {
            return GameRuleOutcome::Unknown;
        };
        let Some(current) = self.game_rules.borrow().get(name) else {
            return GameRuleOutcome::Disabled;
        };
        let rule_name = rule.short_name();
        let Some(value) = value else {
            return GameRuleOutcome::Query {
                rule: rule_name,
                value: current.serialize(),
            };
        };
        if value == current {
            return GameRuleOutcome::AlreadySet {
                rule: rule_name,
                value: value.serialize(),
            };
        }
        let mut set = Ok(());
        self.game_rules
            .send_modify(|rules| set = rules.set(name, value));
        if set.is_err() {
            return GameRuleOutcome::Unknown;
        }
        let event = match (rule_name, value) {
            ("immediate_respawn", RuleValue::Bool(on)) => Some(play::GameEvent {
                event: game_event::IMMEDIATE_RESPAWN,
                value: f32::from(u8::from(on)),
            }),
            ("limited_crafting", RuleValue::Bool(on)) => Some(play::GameEvent {
                event: game_event::LIMITED_CRAFTING,
                value: f32::from(u8::from(on)),
            }),
            _ => None,
        };
        if let Some(event) = event {
            self.broadcast(&event);
        }
        if let ("advance_time", RuleValue::Bool(on)) = (rule_name, value) {
            self.clocks.send_modify(|clocks| clocks.set_advancing(on));
            let world_age = *self.clock.borrow() as i64;
            let packet = self.clocks.borrow().packet(world_age, None);
            self.broadcast(&packet);
        }
        if rule_name == "locator_bar" {
            self.waypoints_stale = true;
        }
        if let ("reduced_debug_info", RuleValue::Bool(on)) = (rule_name, value) {
            for player in self.players.values().filter(|p| p.entry.is_some()) {
                player.handle.send(PlayerEvent::ReducedDebugInfo(on));
            }
        }
        GameRuleOutcome::Set {
            rule: rule_name,
            value: value.serialize(),
        }
    }

    pub(super) fn set_difficulty(&mut self, difficulty: Option<Difficulty>) -> DifficultyOutcome {
        let current = *self.difficulty.borrow();
        match difficulty {
            None => DifficultyOutcome::Query(current),
            Some(difficulty) if difficulty == current => DifficultyOutcome::Unchanged(difficulty),
            Some(_) if self.level.difficulty_locked => DifficultyOutcome::Unchanged(current),
            Some(difficulty) => {
                self.difficulty.send_replace(difficulty);
                self.broadcast(&play::ChangeDifficulty {
                    difficulty: difficulty.id(),
                    locked: self.level.difficulty_locked,
                });
                DifficultyOutcome::Set(difficulty)
            }
        }
    }

    pub(super) fn lock_difficulty(&mut self, locked: bool) {
        self.level.difficulty_locked = locked;
        self.shared
            .difficulty_locked
            .store(locked, std::sync::atomic::Ordering::Relaxed);
        self.broadcast(&play::ChangeDifficulty {
            difficulty: self.difficulty.borrow().id(),
            locked,
        });
    }

    pub(super) fn save_config(&self, request: ConfigRequest) {
        if let Some(config) = &self.config {
            let _ = config.send(request);
        }
    }

    pub(super) fn save_user_cache(&self) {
        self.save_config(ConfigRequest::UserCache(self.profiles.entries()));
    }

    pub(super) fn save_lists(&self, which: &str) {
        self.sync_gameplay_properties();
        self.save_config(match which {
            "ops" => ConfigRequest::Ops(self.lists.ops.clone()),
            "whitelist" => ConfigRequest::Whitelist(self.lists.whitelist.clone()),
            "bans" => ConfigRequest::Bans(self.lists.bans.clone()),
            _ => ConfigRequest::IpBans(self.lists.ip_bans.clone()),
        });
    }

    pub(super) fn whitelist_reloaded(
        &mut self,
        whitelist: std::io::Result<Vec<NameAndId>>,
        reply: oneshot::Sender<ListOutcome>,
    ) {
        match whitelist {
            Ok(whitelist) => self.lists.whitelist = whitelist,
            Err(err) => log::error!("failed to reload the whitelist: {err}"),
        }
        let _ = reply.send(ListOutcome::WhitelistReloaded);
        self.kick_unlisted_players();
    }

    /// The properties to write: command-line overrides are for this run
    /// only, so their keys go back to what the file held.
    pub(super) fn properties_to_save(&self) -> ServerProperties {
        if self.property_baseline.is_empty() {
            return self.properties.clone();
        }
        let mut entries = self.properties.entries();
        for (key, saved) in &self.property_baseline {
            match saved {
                Some(value) => entries.insert(key.clone(), value.clone()),
                None => entries.remove(key),
            };
        }
        ServerProperties::from_entries(entries)
    }

    pub(super) fn set_property(&mut self, key: String, value: String) {
        let mut entries = self.properties.entries();
        if entries.get(&key) == Some(&value) && !self.property_baseline.contains_key(&key) {
            return;
        }
        entries.insert(key.clone(), value);
        self.property_baseline.remove(&key);
        self.properties = ServerProperties::from_entries(entries);
        self.sync_gameplay_properties();
        self.apply_property(&key);
        self.save_config(ConfigRequest::Properties(self.properties_to_save()));
    }

    /// Makes a changed property take effect on the running server. Keys
    /// the spec marks restart-only, creation-only or unsupported are only
    /// saved.
    pub(super) fn apply_property(&mut self, key: &str) {
        use crate::world::live::LiveChange;
        let live = &self.shared.live;
        let properties = &self.properties;
        live.update_network(|network| network.apply(properties));
        match key {
            "hardcore" => self.apply_hardcore(self.properties.hardcore()),
            "pvp" => self.apply_pvp(self.properties.pvp()),
            "white-list" | "enforce-whitelist" => self.kick_unlisted_players(),
            "motd" => {
                let motd = TextComponent::new(self.properties.motd());
                let _ = self.status.try_send(StatusCommand::SetMotd(motd));
            }
            "max-players" => {
                let max = self.properties.max_players();
                live.set_max_players(max);
                let _ = self.status.try_send(StatusCommand::SetMaxPlayers(max));
            }
            "hide-online-players" => {
                let hide = self.properties.hide_online_players();
                let _ = self.status.try_send(StatusCommand::SetHidePlayers(hide));
            }
            "view-distance" => {
                live.set_view_distance(self.properties.view_distance());
                self.tell_players(LiveChange::ViewDistance);
            }
            "simulation-distance" => {
                live.set_simulation_distance(self.properties.simulation_distance());
                self.tell_players(LiveChange::SimulationDistance);
            }
            "entity-broadcast-range-percentage" => {
                live.set_broadcast_range_percent(
                    self.properties.entity_broadcast_range_percentage(),
                );
                self.tell_players(LiveChange::BroadcastRange);
            }
            "pause-when-empty-seconds" => {
                live.set_pause_when_empty_seconds(self.properties.pause_when_empty_seconds());
            }
            "player-idle-timeout"
            | "rate-limit"
            | "chat-spam-threshold-seconds"
            | "command-spam-threshold-seconds" => self.tell_players(LiveChange::Connection),
            "difficulty" => {
                let difficulty = self.properties.difficulty();
                self.set_difficulty(Some(difficulty));
                self.level.record_applied_difficulty(difficulty);
            }
            "gamemode" => {
                let mode = self.properties.gamemode();
                self.level.set_default_game_mode(mode);
                live.set_default_game_mode(mode);
                self.tell_players(LiveChange::GameMode);
            }
            "force-gamemode" => self.tell_players(LiveChange::GameMode),
            "function-permission-level" => {
                let level = self.properties.function_permission_level;
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    if let Err(err) = shared.datapacks.set_function_permission(level).await {
                        log::warn!("function-permission-level: {err}");
                    }
                });
            }
            "max-chained-neighbor-updates" => {
                live.set_max_chained_neighbor_updates(
                    self.properties.max_chained_neighbor_updates(),
                );
            }
            "max-world-size" => self.apply_world_size(self.properties.max_world_size()),
            "enable-code-of-conduct" if self.properties.enable_code_of_conduct() => {
                self.load_code_of_conduct();
            }
            _ => {}
        }
    }

    pub(super) fn tell_players(&self, change: crate::world::live::LiveChange) {
        for player in self.players.values() {
            player.handle.send(PlayerEvent::LiveSetting(change));
        }
    }

    /// Caps every dimension's border at `size` from the origin and sends
    /// each player their dimension's border again.
    pub(super) fn apply_world_size(&mut self, size: i32) {
        for (dimension, border) in self.shared.dimensions.iter().zip(&self.borders) {
            border.send_modify(|border| border.absolute_max = size);
            let packet = framed(&border.borrow().initialize());
            let dimension = dimension.id;
            for player in self.players.values().filter(|p| p.entry.is_some()) {
                player.handle.send(PlayerEvent::Border {
                    dimension,
                    packet: packet.clone(),
                });
            }
        }
    }

    /// Reads the code of conduct texts off the coordinator; joins see them
    /// once read, unless the property was turned off meanwhile.
    pub(super) fn load_code_of_conduct(&self) {
        let Some(dir) = self.shared.live.network().instance_dir.clone() else {
            return;
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let read =
                tokio::task::spawn_blocking(move || crate::code_of_conduct::read(&dir)).await;
            match read {
                Ok(Ok(texts)) => shared.live.update_network(|network| {
                    if network.code_of_conduct_enabled {
                        network.code_of_conduct = texts.into_iter().collect();
                    }
                }),
                Ok(Err(err)) => log::warn!("enable-code-of-conduct: {err}"),
                Err(err) => log::warn!("enable-code-of-conduct: {err}"),
            }
        });
    }

    /// Applies a changed pvp property to the running world; clients need no
    /// packet since pvp is enforced server side.
    pub(super) fn apply_pvp(&mut self, pvp: bool) {
        self.game_rules.send_modify(|rules| {
            let _ = rules.set("pvp", RuleValue::Bool(pvp));
        });
        self.level
            .record_applied_gameplay(self.properties.hardcore(), pvp);
    }

    /// Applies a changed hardcore property to the running world: hard
    /// difficulty, locked. The client's hardcore flag only travels in the
    /// Login packet, so online players are sent a fresh one; players already
    /// dead get it on respawn.
    pub(super) fn apply_hardcore(&mut self, hardcore: bool) {
        self.level.set_hardcore(hardcore);
        self.level
            .record_applied_gameplay(hardcore, self.properties.pvp());
        self.shared
            .hardcore
            .store(hardcore, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .difficulty_locked
            .store(hardcore, std::sync::atomic::Ordering::Relaxed);
        let difficulty = self.level.difficulty;
        self.difficulty.send_replace(difficulty);
        self.broadcast(&play::ChangeDifficulty {
            difficulty: difficulty.id(),
            locked: hardcore,
        });
        for player in self.players.values() {
            player.handle.send(PlayerEvent::HardcoreChanged);
        }
    }

    /// Kicks players the whitelist no longer admits, when it is enforced.
    pub(super) fn kick_unlisted_players(&mut self) {
        if !self.properties.white_list || !self.properties.enforce_whitelist {
            return;
        }
        for player in self.players.values().filter(|p| p.fake.is_none()) {
            let uuid = player.profile.id;
            if !(self.ops_trusted() && self.lists.is_op(uuid)) && !self.lists.is_whitelisted(uuid) {
                player
                    .handle
                    .send(PlayerEvent::Kick(TextComponent::translate(
                        "multiplayer.disconnect.not_whitelisted",
                        [],
                    )));
            }
        }
    }
}

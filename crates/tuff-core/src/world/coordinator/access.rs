use super::*;

impl Coordinator {
    pub(super) fn lists(
        &mut self,
        source: String,
        op: ListOp,
        reply: oneshot::Sender<ListOutcome>,
    ) {
        if let ListOp::WhitelistReload = op {
            match &self.config {
                Some(config) => {
                    let _ = config.send(ConfigRequest::ReloadWhitelist { reply });
                }
                None => self.whitelist_reloaded(Err(std::io::Error::other("stopping")), reply),
            }
            return;
        }
        let outcome = self.change_lists(source, op);
        let _ = reply.send(outcome);
    }

    pub(super) fn change_lists(&mut self, source: String, op: ListOp) -> ListOutcome {
        match op {
            ListOp::Op(users) => {
                let level = self.properties.op_permission_level;
                let mut opped = Vec::new();
                for user in users {
                    if self.lists.is_op(user.uuid) {
                        continue;
                    }
                    self.lists.ops.push(OpEntry {
                        user: user.clone(),
                        level,
                        bypasses_player_limit: false,
                    });
                    self.set_permission(user.uuid, level);
                    opped.push(user);
                }
                if !opped.is_empty() {
                    self.save_lists("ops");
                }
                ListOutcome::Opped(opped)
            }
            ListOp::Deop(users) => {
                let mut deopped = Vec::new();
                for user in users {
                    if !self.lists.is_op(user.uuid) {
                        continue;
                    }
                    self.lists.ops.retain(|op| op.user.uuid != user.uuid);
                    self.set_permission(user.uuid, PermissionLevel::All);
                    deopped.push(user);
                }
                if !deopped.is_empty() {
                    self.save_lists("ops");
                    self.kick_unlisted_players();
                }
                ListOutcome::Deopped(deopped)
            }
            ListOp::WhitelistAdd(users) => {
                let mut added = Vec::new();
                for user in users {
                    if self.lists.is_whitelisted(user.uuid) {
                        continue;
                    }
                    self.lists.whitelist.push(user.clone());
                    added.push(user);
                }
                if !added.is_empty() {
                    self.save_lists("whitelist");
                }
                ListOutcome::WhitelistAdded(added)
            }
            ListOp::WhitelistRemove(users) => {
                let mut removed = Vec::new();
                for user in users {
                    if !self.lists.is_whitelisted(user.uuid) {
                        continue;
                    }
                    self.lists.whitelist.retain(|u| u.uuid != user.uuid);
                    removed.push(user);
                }
                if !removed.is_empty() {
                    self.save_lists("whitelist");
                    self.kick_unlisted_players();
                }
                ListOutcome::WhitelistRemoved(removed)
            }
            ListOp::WhitelistList => ListOutcome::Whitelist(
                self.lists
                    .whitelist
                    .iter()
                    .map(|u| u.name.clone())
                    .collect(),
            ),
            ListOp::WhitelistOn | ListOp::WhitelistOff => {
                let on = matches!(op, ListOp::WhitelistOn);
                if self.properties.white_list == on {
                    return ListOutcome::WhitelistEnabled { on, changed: false };
                }
                self.properties.white_list = on;
                self.save_config(ConfigRequest::Properties(self.properties_to_save()));
                if on {
                    self.kick_unlisted_players();
                }
                ListOutcome::WhitelistEnabled { on, changed: true }
            }
            ListOp::WhitelistReload => unreachable!("answered by the config actor"),
            ListOp::Ban { users, reason } => {
                let mut banned = Vec::new();
                for user in users {
                    if self.lists.ban(user.uuid).is_some() {
                        continue;
                    }
                    let entry = BanEntry::new(user.clone(), source.clone(), reason.clone());
                    banned.push((user.clone(), crate::admin::reason_message(&entry)));
                    self.lists.bans.push(entry);
                    if let Some(online) = self.online_by_uuid(user.uuid) {
                        online
                            .handle
                            .send(PlayerEvent::Kick(TextComponent::translate(
                                "multiplayer.disconnect.banned",
                                [],
                            )));
                    }
                }
                if !banned.is_empty() {
                    self.save_lists("bans");
                }
                ListOutcome::Banned(banned)
            }
            ListOp::BanIp { target, reason } => {
                let ip = if target.parse::<IpAddr>().is_ok() {
                    Some(target.clone())
                } else {
                    self.players
                        .values()
                        .find(|p| p.entry.is_some() && p.profile.name.eq_ignore_ascii_case(&target))
                        .map(|p| p.address.to_string())
                };
                let Some(ip) = ip else {
                    return ListOutcome::IpBanInvalid;
                };
                if self.lists.ip_ban(&ip).is_some() {
                    return ListOutcome::IpAlreadyBanned;
                }
                let entry = BanEntry::new(ip.clone(), source, reason);
                let reason = crate::admin::reason_message(&entry);
                self.lists.ip_bans.push(entry);
                self.save_lists("ip_bans");
                let affected: Vec<&Online> = self
                    .players
                    .values()
                    .filter(|p| p.entry.is_some() && p.address.to_string() == ip)
                    .collect();
                let names = affected.iter().map(|p| p.profile.name.clone()).collect();
                for player in affected {
                    player
                        .handle
                        .send(PlayerEvent::Kick(TextComponent::translate(
                            "multiplayer.disconnect.ip_banned",
                            [],
                        )));
                }
                ListOutcome::IpBanned {
                    ip,
                    reason,
                    affected: names,
                }
            }
            ListOp::BanList { players, ips } => {
                self.lists.expire();
                let mut entries = Vec::new();
                if players {
                    entries.extend(self.lists.bans.iter().map(|ban| {
                        (
                            ban.key.name.clone(),
                            ban.source.clone(),
                            crate::admin::reason_message(ban),
                        )
                    }));
                }
                if ips {
                    entries.extend(self.lists.ip_bans.iter().map(|ban| {
                        (
                            ban.key.clone(),
                            ban.source.clone(),
                            crate::admin::reason_message(ban),
                        )
                    }));
                }
                ListOutcome::BanList(entries)
            }
            ListOp::Pardon(users) => {
                let mut pardoned = Vec::new();
                for user in users {
                    if self.lists.ban(user.uuid).is_none() {
                        continue;
                    }
                    self.lists.bans.retain(|ban| ban.key.uuid != user.uuid);
                    pardoned.push(user);
                }
                if !pardoned.is_empty() {
                    self.save_lists("bans");
                }
                ListOutcome::Pardoned(pardoned)
            }
            ListOp::PardonIp(ip) => {
                if ip.parse::<IpAddr>().is_err() {
                    return ListOutcome::IpPardonInvalid;
                }
                if self.lists.ip_ban(&ip).is_none() {
                    return ListOutcome::IpNotBanned;
                }
                self.lists.ip_bans.retain(|ban| ban.key != ip);
                self.save_lists("ip_bans");
                ListOutcome::IpPardoned(ip)
            }
        }
    }

    /// Tells an online player its new permission level and records it.
    pub(super) fn set_permission(&mut self, uuid: u128, level: PermissionLevel) {
        let level = if self.ops_trusted() {
            level
        } else {
            PermissionLevel::All
        };
        if let Some(online) = self.online_by_uuid(uuid) {
            online.handle.send(PlayerEvent::SetPermission(level));
        }
        if let Some(online) = self.players.values_mut().find(|p| p.profile.id == uuid) {
            online.permission = level;
        }
    }

    pub(super) fn can_login(
        &mut self,
        user: &NameAndId,
        address: IpAddr,
    ) -> Result<PermissionLevel, TextComponent> {
        if self.shutdown.is_some() {
            return Err(crate::restart::shutdown_reason());
        }
        self.profiles.add(user.clone(), tuff_storage::admin::now());
        self.save_user_cache();
        let online = self
            .players
            .values()
            .filter(|p| p.profile.id != user.uuid && p.fake.is_none())
            .count();
        let ops_trusted = self.ops_trusted();
        crate::admin::can_login(
            &mut self.lists,
            user,
            address,
            self.properties.white_list,
            online,
            self.shared.live.max_players() as usize,
            ops_trusted,
        )?;
        Ok(self.permission_of(user.uuid))
    }

    /// The level ops.json grants, unless names are unverified (offline mode
    /// without proxy forwarding) and `tuff.offlineOps` is off.
    pub(super) fn permission_of(&self, uuid: u128) -> PermissionLevel {
        if self.ops_trusted() {
            self.lists.permission(uuid)
        } else {
            PermissionLevel::All
        }
    }

    pub(super) fn ops_trusted(&self) -> bool {
        self.shared.online_mode
            || self
                .shared
                .config
                .get_bool(crate::config::OFFLINE_OPS)
                .unwrap_or(false)
    }

    /// With `tuff.refuseDuplicateLogin`, a second login for an online
    /// player is turned away instead of kicking the first.
    pub(super) fn refuse_duplicate(&self, user: &NameAndId) -> Result<(), TextComponent> {
        let refuse = self
            .shared
            .config
            .get_bool(crate::config::REFUSE_DUPLICATE_LOGIN)
            .unwrap_or(false);
        let online = self.players.values().any(|p| {
            p.fake.is_none()
                && (p.profile.id == user.uuid || p.profile.name.eq_ignore_ascii_case(&user.name))
        });
        if refuse && online {
            return Err(TextComponent::new(
                "You are already logged in to this server",
            ));
        }
        Ok(())
    }

    pub(super) fn gate_login(
        &mut self,
        user: &NameAndId,
        address: IpAddr,
        permission: PermissionLevel,
    ) -> Result<(), TextComponent> {
        let attempt = crate::world::login_gate::Attempt {
            user,
            address,
            permission,
            now: std::time::Instant::now(),
            shared: &self.shared,
        };
        self.gates
            .iter_mut()
            .try_for_each(|gate| gate.admit(&attempt))
    }

    pub(super) fn join(
        &mut self,
        profile: GameProfile,
        info: crate::protocol::configuration::ClientInformation,
        brand: Option<String>,
        address: IpAddr,
        outbound: mpsc::UnboundedSender<Bytes>,
    ) -> Result<Joined, TextComponent> {
        let user = NameAndId {
            uuid: profile.id,
            name: profile.name.clone(),
        };
        for fake in self
            .players
            .values()
            .filter(|p| p.fake.is_some() && p.profile.name.eq_ignore_ascii_case(&profile.name))
        {
            fake.handle.send(PlayerEvent::Kick(TextComponent::translate(
                "multiplayer.disconnect.duplicate_login",
                [],
            )));
        }
        let permission = self.can_login(&user, address)?;
        Ok(self.admit(profile, info, brand, address, outbound, permission, None))
    }

    pub(super) fn spawn_fake(
        &mut self,
        owner: String,
        spec: crate::world::fake::FakeSpec,
    ) -> Result<crate::world::fake::FakeHandle, String> {
        use crate::world::fake;
        if self
            .players
            .values()
            .filter(|p| p.fake.as_deref() == Some(owner.as_str()))
            .count()
            >= fake::OWNER_LIMIT
        {
            return Err(format!(
                "{owner} already has {} fake players",
                fake::OWNER_LIMIT
            ));
        }
        let uuid = spec
            .uuid
            .unwrap_or_else(|| fake::derive_uuid(&owner, &spec.name, spec.model));
        if self
            .players
            .values()
            .any(|p| p.profile.id == uuid || p.profile.name.eq_ignore_ascii_case(&spec.name))
        {
            return Err(format!("{} is already online", spec.name));
        }
        let profile = GameProfile {
            id: uuid,
            name: spec.name.clone(),
            properties: Vec::new(),
        };
        let info = crate::protocol::configuration::ClientInformation {
            view_distance: spec.view_distance,
            allow_server_listings: spec.listed,
            ..Default::default()
        };
        let (outbound, outbound_rx) = mpsc::unbounded_channel();
        let (observed, observed_rx) = watch::channel(Default::default());
        let (held, held_rx) = mpsc::unbounded_channel();
        let (actions, actions_rx) = mpsc::unbounded_channel();
        let driver = fake::driver::Driver::new(held_rx, actions_rx, outbound_rx, observed);
        let joined = self.admit(
            profile,
            info,
            None,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            outbound,
            PermissionLevel::All,
            Some((owner.clone(), spec, driver)),
        );
        let _ = joined
            .packets
            .try_send(crate::protocol::play::Serverbound::PlayerLoaded);
        Ok(fake::FakeHandle {
            uuid,
            entity: joined.player,
            owner,
            packets: joined.packets,
            observed: observed_rx,
            held,
            actions,
        })
    }

    pub(super) fn remove_fakes(&mut self, owner: &str, uuid: Option<u128>) -> usize {
        let mut removed = 0;
        for fake in self.players.values().filter(|p| {
            p.fake.as_deref() == Some(owner) && uuid.is_none_or(|uuid| p.profile.id == uuid)
        }) {
            fake.handle.send(PlayerEvent::Kick(TextComponent::translate(
                "multiplayer.disconnect.kicked",
                [],
            )));
            removed += 1;
        }
        removed
    }

    #[allow(clippy::too_many_arguments)]
    fn admit(
        &mut self,
        profile: GameProfile,
        info: crate::protocol::configuration::ClientInformation,
        brand: Option<String>,
        address: IpAddr,
        outbound: mpsc::UnboundedSender<Bytes>,
        permission: PermissionLevel,
        fake: Option<(
            String,
            crate::world::fake::FakeSpec,
            crate::world::fake::driver::Driver,
        )>,
    ) -> Joined {
        for existing in self.players.values().filter(|p| p.profile.id == profile.id) {
            existing
                .handle
                .send(PlayerEvent::Kick(TextComponent::translate(
                    "multiplayer.disconnect.duplicate_login",
                    [],
                )));
        }
        let id = self.shared.next_entity_id();
        let (mut handle, events) = PlayerHandle::new(id);
        handle.mirror_only = fake.is_some();
        let (packets, packets_rx) = mpsc::channel(PACKET_QUEUE);
        let (done, done_rx) = watch::channel(false);
        let finished = done_rx.clone();
        let previous = self
            .sessions
            .insert(profile.id, done_rx)
            .filter(|done| !*done.borrow());
        let mut session = PlayerSession::new(
            self.shared.clone(),
            id,
            profile.clone(),
            info,
            outbound.clone(),
            handle.clone(),
            events,
            packets_rx,
            permission,
        );
        let mut owner = None;
        if let Some((name, spec, driver)) = fake {
            owner = Some(name);
            session.fake = Some(spec);
            session.fake_driver = Some(driver);
        }
        if session.fake.is_some() {
            session.net.idle_timeout = None;
        }
        let coordinator = self.shared.coordinator.clone();
        tokio::spawn(async move {
            // A session panicking outside its play loop (which runs the
            // normal leave path itself) would otherwise never report
            // leaving, keeping it online and blocking shutdown.
            if let Err(err) = tokio::spawn(tuff_timings::scope(session.run(previous, done))).await {
                log::error!("session of player {id} ended abnormally while leaving: {err}");
                let _ = coordinator
                    .send(CoordinatorMessage::Left { player: id })
                    .await;
            }
        });
        self.sessions.retain(|_, done| !*done.borrow());
        self.players.insert(
            id,
            Online {
                profile,
                outbound,
                handle,
                entry: None,
                listed: info.allow_server_listings,
                latency_dirty: false,
                address,
                permission,
                state: None,
                brand,
                debug: 0,
                fake: owner,
            },
        );
        Joined {
            player: id,
            packets,
            permission,
            done: finished,
        }
    }

    /// The player-list name role attributes give `uuid`, if any.
    fn role_list_name(&self, uuid: u128, name: &str) -> Option<TextComponent> {
        let plain = TextComponent::new(name);
        let decorated = self
            .shared
            .permissions
            .view()
            .decorate_name(uuid, plain.clone());
        (decorated != plain).then_some(decorated)
    }

    /// The nametag decoration role attributes give `uuid`, if any.
    fn role_nametag(&self, uuid: u128) -> Option<crate::world::scoreboard::Nametag> {
        use crate::permission::Attribute;
        let view = self.shared.permissions.view();
        let prefix = view.attribute_of(uuid, Attribute::Prefix);
        let suffix = view.attribute_of(uuid, Attribute::Suffix);
        let color = view
            .attribute_of(uuid, Attribute::NameColor)
            .and_then(crate::minimessage::color)
            .and_then(|c| nearest_team_color(&c));
        if prefix.is_none() && suffix.is_none() && color.is_none() {
            return None;
        }
        Some(crate::world::scoreboard::Nametag {
            prefix: prefix.map(crate::minimessage::parse).unwrap_or_default(),
            suffix: suffix.map(crate::minimessage::parse).unwrap_or_default(),
            color,
        })
    }

    pub(super) fn refresh_role_names(&mut self, affected: &crate::permission::Affected) {
        let tags = self
            .players
            .values()
            .filter(|p| p.entry.is_some() && affected.includes(p.profile.id))
            .map(|p| (p.profile.name.clone(), self.role_nametag(p.profile.id)))
            .collect();
        self.shared.scoreboard.set_nametags(tags);
        let mut changed = Vec::new();
        let ids: Vec<PlayerId> = self.players.keys().copied().collect();
        for id in ids {
            let online = &self.players[&id];
            let Some(entry) = &online.entry else {
                continue;
            };
            if !affected.includes(online.profile.id) {
                continue;
            }
            let display_name = self.role_list_name(online.profile.id, &online.profile.name);
            if entry.display_name == display_name {
                continue;
            }
            let online = self.players.get_mut(&id).expect("listed");
            let entry = online.entry.as_mut().expect("checked");
            entry.display_name = display_name;
            changed.push(entry.clone());
        }
        if changed.is_empty() {
            return;
        }
        let packet = framed(&play::PlayerInfoUpdate {
            actions: play::player_info::UPDATE_DISPLAY_NAME,
            entries: &changed,
        });
        for online in self.players.values().filter(|p| p.entry.is_some()) {
            let _ = online.outbound.send(packet.clone());
        }
    }

    pub(super) fn spawned(&mut self, player: PlayerId, mut entry: PlayerInfoEntry, listed: bool) {
        if entry.display_name.is_none() {
            entry.display_name = self.role_list_name(entry.uuid, &entry.name);
        }
        let tag = self
            .players
            .get(&player)
            .and_then(|p| self.role_nametag(p.profile.id));
        let Some(online) = self.players.get_mut(&player) else {
            return;
        };
        online.entry = Some(entry.clone());
        online.listed = listed;
        let (name, uuid) = (online.profile.name.clone(), online.profile.id);
        self.shared
            .scoreboard
            .set_nametags(vec![(name.clone(), tag)]);
        let outbound = online.outbound.clone();
        let scoreboard = self.shared.scoreboard.clone();
        let subscriber = outbound.clone();
        tokio::spawn(async move { scoreboard.subscribe(subscriber).await });
        let entries: Vec<PlayerInfoEntry> = self
            .players
            .values()
            .filter_map(|p| p.entry.clone())
            .collect();
        let all_actions = play::player_info::ADD_PLAYER
            | play::player_info::UPDATE_GAME_MODE
            | play::player_info::UPDATE_LISTED
            | play::player_info::UPDATE_LATENCY
            | play::player_info::UPDATE_DISPLAY_NAME
            | play::player_info::UPDATE_LIST_ORDER
            | play::player_info::UPDATE_HAT;
        let _ = outbound.send(framed(&play::PlayerInfoUpdate {
            actions: all_actions,
            entries: &entries,
        }));
        let own = framed(&play::PlayerInfoUpdate {
            actions: all_actions,
            entries: &[entry],
        });
        for other in self.players.values() {
            if other.entry.is_some() && other.handle.id != player {
                let _ = other.outbound.send(own.clone());
            }
        }
        self.broadcast(&play::SystemChat {
            content: &TextComponent::translate(
                "multiplayer.player.joined",
                [self.shared.scoreboard.teams().player_name(&name, uuid)],
            )
            .color("yellow"),
            overlay: false,
        });
        self.publish_status();
    }
}

fn nearest_team_color(color: &str) -> Option<crate::world::scoreboard::TeamColor> {
    use crate::world::scoreboard::TeamColor;
    if let Some(named) = TeamColor::from_name(color) {
        return Some(named);
    }
    let hex = color.strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    let rgb = [
        (value >> 16) as i32,
        (value >> 8 & 0xff) as i32,
        (value & 0xff) as i32,
    ];
    TeamColor::ALL.into_iter().min_by_key(|c| {
        let [r, g, b] = c.rgb();
        (rgb[0] - i32::from(r)).pow(2)
            + (rgb[1] - i32::from(g)).pow(2)
            + (rgb[2] - i32::from(b)).pow(2)
    })
}

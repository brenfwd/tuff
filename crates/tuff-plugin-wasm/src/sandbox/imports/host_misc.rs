use super::super::*;

impl State {
    pub(in crate::sandbox) fn host_declare_permission(
        &mut self,
        node: &str,
        level: u8,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_node(&self.plugin, node) {
                return Err(RegisterError::Invalid(format!(
                    "{node:?} is not a permission node of {}",
                    self.plugin
                )));
            }
            if level > 4 {
                return Err(RegisterError::Invalid(format!(
                    "{level} is not a permission level"
                )));
            }
            let permissions = &mut self.registration.permissions;
            if permissions.iter().any(|declared| declared.node == node) {
                return Err(RegisterError::Invalid(format!(
                    "{node} is already declared"
                )));
            }
            if permissions.len() >= abi::MAX_PERMISSIONS {
                return Err(RegisterError::LimitReached(abi::MAX_PERMISSIONS as u32));
            }
            permissions.push(Permission {
                node: node.to_owned(),
                level,
            });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_declare_config(
        &mut self,
        decl: ConfigDecl,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_setting(&decl.key) {
                return Err(RegisterError::Invalid(format!(
                    "{:?} is not a setting key",
                    decl.key
                )));
            }
            if decl.description.len() > abi::MAX_SETTING_DESCRIPTION {
                return Err(RegisterError::Invalid(format!(
                    "the description of {} is over {} bytes",
                    decl.key,
                    abi::MAX_SETTING_DESCRIPTION
                )));
            }
            let settings = &mut self.registration.settings;
            if settings.iter().any(|known| known.key == decl.key) {
                return Err(RegisterError::Invalid(format!(
                    "{} is already declared",
                    decl.key
                )));
            }
            if settings.len() >= abi::MAX_SETTINGS {
                return Err(RegisterError::LimitReached(abi::MAX_SETTINGS as u32));
            }
            settings.push(Setting {
                key: decl.key,
                default: setting_value(decl.default),
                description: decl.description,
                secret: decl.secret,
                text_component: decl.text_component,
            });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_register_resource_pack(
        &mut self,
        pack: ResourcePack,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_subcommand(&pack.name) {
                return Err(RegisterError::Invalid(format!(
                    "{:?} is not a resource pack name",
                    pack.name
                )));
            }
            if pack
                .prompt
                .as_ref()
                .is_some_and(|p| p.len() > abi::MAX_PACK_PROMPT)
            {
                return Err(RegisterError::Invalid(format!(
                    "the prompt of {} is over {} bytes",
                    pack.name,
                    abi::MAX_PACK_PROMPT
                )));
            }
            let packs = &self.registration.packs;
            if packs.iter().any(|known| known.name == pack.name) {
                return Err(RegisterError::Invalid(format!(
                    "{} is already registered",
                    pack.name
                )));
            }
            if packs.len() >= abi::MAX_RESOURCE_PACKS {
                return Err(RegisterError::LimitReached(abi::MAX_RESOURCE_PACKS as u32));
            }
            tuff_core::panel_host::resource_packs::inspect(&pack.data).map_err(|err| {
                RegisterError::Invalid(format!("resource pack {}: {err}", pack.name))
            })?;
            self.registration.packs.push(Pack {
                name: pack.name,
                data: bytes::Bytes::from(pack.data),
                required: pack.required,
                prompt: pack.prompt,
            });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_register_panel_page(
        &mut self,
        page: wit::PanelPage,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.panel_register_page(page))
    }

    pub(in crate::sandbox) fn host_register_panel_api(
        &mut self,
        api: wit::PanelApi,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.panel_register_api(api))
    }

    pub(in crate::sandbox) fn host_register_panel_card(
        &mut self,
        card: wit::PanelCard,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.panel_register_card(card))
    }

    pub(in crate::sandbox) fn host_panel_card_set(
        &mut self,
        series: &str,
        value: i64,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.panel_set(series, value))
    }

    pub(in crate::sandbox) fn host_config_get(
        &mut self,
        key: &str,
    ) -> value::Result<Option<ConfigValue>> {
        let declared = self.registration.settings.iter().any(|s| s.key == key);
        Ok(match (&self.env, declared) {
            (Some(env), true) => env
                .shared
                .config
                .get(&abi::setting_name(&self.plugin, key))
                .map(config_value),
            _ => None,
        })
    }

    pub(in crate::sandbox) fn host_config_set(
        &mut self,
        key: &str,
        value: ConfigValue,
    ) -> value::Result<Result<(), HostError>> {
        Ok((|| {
            if !self.running() {
                return Err(HostError::WrongPhase);
            }
            let setting = self
                .registration
                .settings
                .iter()
                .find(|s| s.key == key)
                .ok_or(HostError::NotDeclared)?;
            let value = setting_value(value);
            if value.kind() != setting.default.kind() {
                return Err(HostError::Invalid(format!(
                    "{key} takes a {:?} value",
                    setting.default.kind()
                )));
            }
            let env = self.env.as_ref().ok_or(HostError::Unavailable)?;
            env.shared
                .config
                .set(&abi::setting_name(&self.plugin, key), value)
                .map_err(|_| HostError::Unavailable)
        })())
    }

    pub(in crate::sandbox) fn host_refuse_login(
        &mut self,
        reason: &str,
    ) -> value::Result<Result<(), HostError>> {
        if !self.deciding_login {
            return Ok(Err(HostError::WrongContext));
        }
        self.effects
            .login_refusal
            .get_or_insert_with(|| reason.chars().take(256).collect());
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_reply(
        &mut self,
        message: &str,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.message(message, false))
    }

    pub(in crate::sandbox) fn host_broadcast(
        &mut self,
        message: &str,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.message(message, true))
    }

    pub(in crate::sandbox) fn host_tell(
        &mut self,
        player: Uuid,
        message: &str,
    ) -> value::Result<Result<(), HostError>> {
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        if self.messages() >= self.limits.messages {
            return Ok(Err(HostError::LimitReached));
        }
        self.effects
            .tells
            .push((uuid_of(player), message.to_owned()));
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_tell_rich(
        &mut self,
        player: Uuid,
        spans: Vec<RichSpan>,
    ) -> value::Result<Result<(), HostError>> {
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        if self.messages() >= self.limits.messages || spans.len() > abi::MAX_RICH_SPANS {
            return Ok(Err(HostError::LimitReached));
        }
        let message = match rich_component(spans) {
            Ok(message) => message,
            Err(error) => return Ok(Err(error)),
        };
        self.effects.rich_tells.push((uuid_of(player), message));
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_show_title(
        &mut self,
        player: Uuid,
        title: TitleSpec,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.queue_display(DisplayOp::Title {
            player: uuid_of(player),
            title: title.title,
            subtitle: title.subtitle,
            times: [title.fade_in, title.stay, title.fade_out],
        }))
    }

    pub(in crate::sandbox) fn host_clear_title(
        &mut self,
        player: Uuid,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.queue_display(DisplayOp::ClearTitle(uuid_of(player))))
    }

    pub(in crate::sandbox) fn host_action_bar(
        &mut self,
        player: Uuid,
        text: &str,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.queue_display(DisplayOp::ActionBar(uuid_of(player), text.to_owned())))
    }

    pub(in crate::sandbox) fn host_sidebar_set(
        &mut self,
        title: &str,
        lines: Vec<String>,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.queue_display(DisplayOp::Sidebar(Some((title.to_owned(), lines)))))
    }

    pub(in crate::sandbox) fn host_sidebar_clear(
        &mut self,
    ) -> value::Result<Result<(), HostError>> {
        Ok(self.queue_display(DisplayOp::Sidebar(None)))
    }

    pub(in crate::sandbox) fn host_team_set(
        &mut self,
        name: &str,
        team: TeamSpec,
    ) -> value::Result<Result<(), HostError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(HostError::NotGranted));
        }
        Ok(self.queue_display(DisplayOp::Team {
            name: name.to_owned(),
            color: team.color,
            prefix: team.prefix,
            suffix: team.suffix,
            members: team.members,
        }))
    }

    pub(in crate::sandbox) fn host_team_remove(
        &mut self,
        name: &str,
    ) -> value::Result<Result<(), HostError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(HostError::NotGranted));
        }
        Ok(self.queue_display(DisplayOp::TeamRemove(name.to_owned())))
    }

    pub(in crate::sandbox) fn host_provide(
        &mut self,
        interface: &str,
        methods: Vec<u64>,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_interface(interface) {
                return Err(RegisterError::Invalid(format!(
                    "{interface:?} is not an interface name"
                )));
            }
            let provides = &mut self.registration.provides;
            if provides.iter().any(|(known, _)| known == interface) {
                return Err(RegisterError::Invalid(format!(
                    "{interface} is already provided"
                )));
            }
            if provides.len() >= abi::MAX_PROVIDED {
                return Err(RegisterError::LimitReached(abi::MAX_PROVIDED as u32));
            }
            provides.push((interface.to_owned(), methods));
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_request(
        &mut self,
        target: Option<String>,
        interface: &str,
        method: u64,
        payload: &[u8],
        timeout_ticks: u32,
    ) -> value::Result<Result<Handle, CallError>> {
        if !self.group_granted(Kind::CrossPlugin) {
            return Ok(Err(CallError::Refused(
                "cap.cross-plugin is not granted".into(),
            )));
        }
        let timeout = match timeout_ticks {
            0 => abi::DEFAULT_REQUEST_TICKS,
            ticks => ticks,
        };
        Ok((|| {
            if timeout > abi::MAX_REQUEST_TICKS {
                return Err(CallError::Refused(format!(
                    "timeouts are at most {} ticks",
                    abi::MAX_REQUEST_TICKS
                )));
            }
            if self.outstanding.len() >= abi::MAX_OUTSTANDING_REQUESTS {
                return Err(CallError::Refused("too many outstanding requests".into()));
            }
            let providers =
                self.resolve_providers(target.as_deref(), interface, method, payload)?;
            let [provider] = providers.as_slice() else {
                return Err(CallError::Ambiguous);
            };
            self.next_request += 1;
            let handle = self.next_request;
            self.outstanding.insert(handle);
            self.effects.messages.push(OutMessage {
                target: provider.clone(),
                interface: interface.to_owned(),
                method,
                payload: payload.to_vec(),
                request: Some((handle, timeout)),
            });
            Ok(handle)
        })())
    }

    pub(in crate::sandbox) fn host_notify(
        &mut self,
        target: Option<String>,
        interface: &str,
        method: u64,
        payload: &[u8],
    ) -> value::Result<Result<u32, CallError>> {
        if !self.group_granted(Kind::CrossPlugin) {
            return Ok(Err(CallError::Refused(
                "cap.cross-plugin is not granted".into(),
            )));
        }
        Ok((|| {
            let providers =
                self.resolve_providers(target.as_deref(), interface, method, payload)?;
            for provider in &providers {
                self.effects.messages.push(OutMessage {
                    target: provider.clone(),
                    interface: interface.to_owned(),
                    method,
                    payload: payload.to_vec(),
                    request: None,
                });
            }
            Ok(providers.len() as u32)
        })())
    }

    pub(in crate::sandbox) fn host_respond(
        &mut self,
        request: u64,
        outcome: Result<Vec<u8>, String>,
    ) -> value::Result<Result<(), HostError>> {
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        if let Ok(bytes) = &outcome
            && bytes.len() > self.message_bytes()
        {
            return Ok(Err(HostError::TooLarge));
        }
        if !self.owed.remove(&request) {
            return Ok(Err(HostError::Invalid(
                "no such request is waiting for an answer".into(),
            )));
        }
        self.effects.answers.push((request, outcome));
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_plugin_loaded(
        &mut self,
        id: &str,
    ) -> value::Result<Option<String>> {
        Ok(self
            .index
            .as_ref()
            .and_then(|index| index.borrow().versions.get(id).cloned()))
    }

    pub(in crate::sandbox) fn host_schedule(
        &mut self,
        delay: u64,
        token: u32,
    ) -> value::Result<Result<(), HostError>> {
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        if delay > abi::MAX_DELAY {
            return Ok(Err(HostError::Invalid(format!(
                "delays are at most {} ticks",
                abi::MAX_DELAY
            ))));
        }
        if self.effects.timers.len() >= self.limits.schedules {
            return Ok(Err(HostError::LimitReached));
        }
        self.effects.timers.push((delay, token));
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_acting_player(&mut self) -> value::Result<Option<wit::Player>> {
        Ok(self.acting().ok().map(Player::to_wit))
    }

    pub(in crate::sandbox) fn host_acting_location(&mut self) -> value::Result<Option<Location>> {
        let Some(position) = self.acting().ok().and_then(|p| p.position.as_ref()) else {
            return Ok(None);
        };
        let [x, y, z] = position.at;
        Ok(Some(Location {
            dimension: position.dimension.clone(),
            pos: Vec3 { x, y, z },
            rotation: position.look.map(|[yaw, pitch]| Rotation { yaw, pitch }),
            eye_height: position.eye_height,
        }))
    }

    pub(in crate::sandbox) fn host_has_permission(
        &mut self,
        node: &str,
    ) -> value::Result<Option<bool>> {
        let Ok(player) = self.acting() else {
            return Ok(None);
        };
        Ok(Some(permitted(self, player, node)))
    }

    pub(in crate::sandbox) fn host_acting_inventory(&mut self) -> value::Result<Option<Inventory>> {
        if !self.group_granted(Kind::PlayerRead) {
            return Ok(None);
        }
        let Some(items) = self.acting().ok().and_then(|p| p.items.as_ref()) else {
            return Ok(None);
        };
        let stack = |item: &str, count: u8| ItemStack {
            item: item.to_owned(),
            count,
        };
        Ok(Some(Inventory {
            held: items.held.as_ref().map(|(item, count)| stack(item, *count)),
            slots: items
                .slots
                .iter()
                .map(|(index, item, count)| Slot {
                    index: *index,
                    stack: stack(item, *count),
                })
                .collect(),
        }))
    }
}

fn rich_component(spans: Vec<RichSpan>) -> Result<tuff_item::text::TextComponent, HostError> {
    use tuff_item::text::{ClickEvent, TextComponent};
    let long = |text: &str| text.len() > abi::MAX_DISPLAY_TEXT;
    let valid_color = |color: &str| {
        let hex = color
            .strip_prefix('#')
            .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        hex || (!color.is_empty()
            && color.len() <= 16
            && color
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'))
    };
    let mut root = TextComponent::new("");
    for span in spans {
        let RichSpan {
            text,
            color,
            bold,
            italic,
            underlined,
            click,
            hover,
            insertion,
        } = span;
        let click_value = match &click {
            Some(
                RichClick::SuggestCommand(value)
                | RichClick::RunCommand(value)
                | RichClick::OpenUrl(value)
                | RichClick::Copy(value),
            ) => value.as_str(),
            None => "",
        };
        if long(&text)
            || long(click_value)
            || hover.as_deref().is_some_and(long)
            || insertion.as_deref().is_some_and(long)
        {
            return Err(HostError::TooLarge);
        }
        let mut part = TextComponent::new(text);
        if let Some(color) = color {
            if !valid_color(&color) {
                return Err(HostError::Invalid(format!("{color:?} is not a color")));
            }
            part = part.color(color);
        }
        if bold {
            part = part.bold(true);
        }
        if italic {
            part = part.italic(true);
        }
        if underlined {
            part = part.underlined(true);
        }
        part = match click {
            Some(RichClick::SuggestCommand(value)) => part.click(ClickEvent::SuggestCommand(value)),
            Some(RichClick::RunCommand(value)) => {
                if !value.starts_with('/') {
                    return Err(HostError::Invalid(
                        "a run-command click must start with /".to_owned(),
                    ));
                }
                part.click(ClickEvent::RunCommand(value))
            }
            Some(RichClick::OpenUrl(value)) => {
                if !(value.starts_with("http://") || value.starts_with("https://")) {
                    return Err(HostError::Invalid(
                        "an open-url click needs an http or https URL".to_owned(),
                    ));
                }
                part.click(ClickEvent::OpenUrl(value))
            }
            Some(RichClick::Copy(value)) => part.click(ClickEvent::CopyToClipboard(value)),
            None => part,
        };
        if let Some(hover) = hover {
            part = part.hover_text(TextComponent::new(hover));
        }
        if let Some(insertion) = insertion {
            part = part.insertion(insertion);
        }
        root = root.append(part);
    }
    Ok(root)
}

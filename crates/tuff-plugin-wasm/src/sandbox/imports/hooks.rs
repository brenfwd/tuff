use super::super::*;

impl State {
    pub(in crate::sandbox) fn host_subscribe(
        &mut self,
        kinds: EventKinds,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().map(|()| {
            self.registration.events |= kinds;
        }))
    }

    pub(in crate::sandbox) fn host_intercept_click(
        &mut self,
        filter: wit::ClickFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.clicks.len() >= abi::MAX_CLICK_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_CLICK_FILTERS as u32));
            }
            self.registration.events |= match filter.kind {
                ClickKind::Left => EventKinds::LEFT_CLICK,
                ClickKind::Right => EventKinds::RIGHT_CLICK,
                ClickKind::Air if filter.block.is_some() => {
                    return Err(RegisterError::Invalid("an air click has no block".into()));
                }
                ClickKind::Air => EventKinds::USE_ITEM,
            };
            self.registration.clicks.push(ClickFilter {
                kind: filter.kind,
                item: filter.item,
                tag: filter.tag,
                block: filter.block.map(|block| crate::portal::qualified(&block)),
                scope: crate::combat::Scope::parse(filter.scope),
            });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_register_channel(
        &mut self,
        channel: &str,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_channel(channel) {
                return Err(RegisterError::Invalid(
                    "a channel is `namespace:path` outside `minecraft:`".into(),
                ));
            }
            if self.registration.channels.iter().any(|c| c == channel) {
                return Ok(());
            }
            if self.registration.channels.len() >= abi::MAX_PAYLOAD_CHANNELS {
                return Err(RegisterError::LimitReached(
                    abi::MAX_PAYLOAD_CHANNELS as u32,
                ));
            }
            self.registration.events |= EventKinds::PAYLOAD;
            self.registration.channels.push(channel.to_owned());
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_send_payload(
        &mut self,
        player: Uuid,
        channel: &str,
        data: &[u8],
    ) -> value::Result<Result<(), HostError>> {
        if !self.registration.channels.iter().any(|c| c == channel) {
            return Ok(Err(HostError::Invalid("channel not registered".into())));
        }
        Ok(self.queue_display(DisplayOp::Payload {
            player: uuid_of(player),
            channel: channel.to_owned(),
            data: data.to_vec(),
        }))
    }

    pub(in crate::sandbox) fn host_intercept_item(
        &mut self,
        filter: wit::ItemFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.items.len() >= abi::MAX_ITEM_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_ITEM_FILTERS as u32));
            }
            self.registration.events |= crate::items::kind_flag(filter.kind);
            self.registration.items.push(ItemFilter {
                kind: filter.kind,
                item: filter.item,
                tag: filter.tag,
                scope: crate::combat::Scope::parse(filter.scope),
            });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_inventory(
        &mut self,
        filter: wit::InventoryFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.inventory.len() >= abi::MAX_INVENTORY_FILTERS {
                return Err(RegisterError::LimitReached(
                    abi::MAX_INVENTORY_FILTERS as u32,
                ));
            }
            self.registration.events |= EventKinds::INVENTORY_CLICK;
            self.registration.inventory.push(filter);
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_state(
        &mut self,
        filter: wit::StateFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.state.len() >= abi::MAX_STATE_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_STATE_FILTERS as u32));
            }
            self.registration.events |= EventKinds::PLAYER_STATE;
            self.registration.state.push(filter);
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_entity_interact(
        &mut self,
        filter: wit::EntityFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.entity_filters.len() >= abi::MAX_ENTITY_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_ENTITY_FILTERS as u32));
            }
            self.registration.events |= EventKinds::ENTITY_INTERACT;
            self.registration
                .entity_filters
                .push(crate::menus::EntityFilter {
                    kind: filter.kind.map(|kind| crate::portal::qualified(&kind)),
                    tag: filter.tag,
                    scope: crate::combat::Scope::parse(filter.scope),
                });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_command(
        &mut self,
        filter: wit::CommandFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.command_filters.len() >= abi::MAX_LINE_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_LINE_FILTERS as u32));
            }
            if filter.prefix.trim().is_empty() {
                return Err(RegisterError::Invalid("empty command prefix".into()));
            }
            self.registration
                .command_filters
                .push(crate::line_filters::CommandFilter {
                    prefix: filter.prefix,
                    action: filter.action.into(),
                });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_attack(
        &mut self,
        filter: wit::AttackFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.attack_filters.len() >= crate::combat::MAX_FILTERS {
                return Err(RegisterError::LimitReached(
                    crate::combat::MAX_FILTERS as u32,
                ));
            }
            self.registration.events |= EventKinds::PLAYER_ATTACK;
            self.registration
                .attack_filters
                .push(crate::combat::AttackFilter::parse(filter));
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_chat(
        &mut self,
        filter: wit::ChatFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.chat_filters.len() >= abi::MAX_LINE_FILTERS {
                return Err(RegisterError::LimitReached(abi::MAX_LINE_FILTERS as u32));
            }
            self.registration
                .chat_filters
                .push(crate::line_filters::ChatFilter {
                    prefix: filter.prefix,
                    contains: filter.contains,
                    action: filter.action.into(),
                });
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_intercept_damage(
        &mut self,
        filter: wit::DamageFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self.registration.damage_filters.len() >= crate::combat::MAX_FILTERS {
                return Err(RegisterError::LimitReached(
                    crate::combat::MAX_FILTERS as u32,
                ));
            }
            let parsed =
                crate::combat::DamageFilter::parse(filter).map_err(RegisterError::Invalid)?;
            self.registration.events |= EventKinds::DAMAGE_FILTERED;
            self.registration.damage_filters.push(parsed);
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_open_menu(
        &mut self,
        player: Uuid,
        menu: &str,
        rows: u8,
        title: &str,
        items: Vec<wit::MenuItem>,
    ) -> value::Result<Result<(), wit::MenuError>> {
        use wit::MenuError;
        Ok((|| {
            if !self.group_granted(Kind::PlayerWrite) {
                return Err(MenuError::NotGranted);
            }
            if self.effects.menus.len() >= abi::MAX_MENU_OPS
                || title.len() > abi::MAX_GIVE_TEXT
                || menu.len() > abi::MAX_GIVE_TEXT
            {
                return Err(MenuError::OverBudget);
            }
            if !crate::menus::valid_rows(rows) || items.len() > usize::from(rows) * 9 {
                return Err(MenuError::Invalid);
            }
            let mut slots = Vec::with_capacity(items.len());
            for item in &items {
                if usize::from(item.slot) >= usize::from(rows) * 9 {
                    return Err(MenuError::Invalid);
                }
                let stack = crate::menus::build_stack(item).ok_or(MenuError::Invalid)?;
                slots.push((item.slot, stack));
            }
            self.effects.registered_menu(crate::menus::MenuOp::Open {
                player: uuid_of(player),
                menu: menu.to_owned(),
                rows,
                title: title.to_owned(),
                slots,
            });
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_update_menu_slot(
        &mut self,
        player: Uuid,
        slot: u8,
        item: Option<wit::MenuItem>,
    ) -> value::Result<Result<(), wit::MenuError>> {
        use wit::MenuError;
        Ok((|| {
            if !self.group_granted(Kind::PlayerWrite) {
                return Err(MenuError::NotGranted);
            }
            if self.effects.menus.len() >= abi::MAX_MENU_OPS {
                return Err(MenuError::OverBudget);
            }
            if slot >= 54 {
                return Err(MenuError::Invalid);
            }
            let stack = match item {
                Some(item) => Some(crate::menus::build_stack(&item).ok_or(MenuError::Invalid)?),
                None => None,
            };
            self.effects.registered_menu(crate::menus::MenuOp::Set {
                player: uuid_of(player),
                slot,
                stack,
            });
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_close_menu(&mut self, player: Uuid) -> value::Result<()> {
        if self.effects.menus.len() < abi::MAX_MENU_OPS {
            self.effects.registered_menu(crate::menus::MenuOp::Close {
                player: uuid_of(player),
            });
        }
        Ok(())
    }

    pub(in crate::sandbox) fn host_watch_block(
        &mut self,
        block: &str,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            let name = crate::portal::qualified(block);
            if world::parse_state(&name).is_none() {
                return Err(RegisterError::Invalid(format!("{block} is not a block")));
            }
            let watched = &mut self.registration.watched;
            if watched.contains(&name) {
                return Ok(());
            }
            if watched.len() >= abi::MAX_WATCHED_BLOCKS {
                return Err(RegisterError::LimitReached(abi::MAX_WATCHED_BLOCKS as u32));
            }
            watched.push(name);
            self.registration.events |= EventKinds::INSIDE_BLOCK;
            Ok(())
        }))
    }

    pub(in crate::sandbox) fn host_watch_region(
        &mut self,
        region: wit::RegionWatch,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if self
                .registration
                .regions
                .iter()
                .any(|r| r.name == region.name)
            {
                return Err(RegisterError::Invalid(format!(
                    "region {} is already watched",
                    region.name
                )));
            }
            if self.registration.regions.len() >= abi::MAX_WATCHED_REGIONS {
                return Err(RegisterError::LimitReached(abi::MAX_WATCHED_REGIONS as u32));
            }
            self.registration.events |= EventKinds::PLAYER_STATE;
            self.registration.regions.push(region);
            Ok(())
        }))
    }
}

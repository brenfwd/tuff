//! Permission, roster and queueing helpers over a plugin sandbox state.

use super::*;

/// Whether the acting player holds `node`: an explicit grant or group
/// setting first, then a node this plugin declared, then the server's.
pub(super) fn permitted(state: &State, player: &Player, node: &str) -> bool {
    let view = state
        .env
        .as_ref()
        .map_or_else(View::default, |env| env.shared.permissions.view());
    if let Some(setting) = view.setting(player.uuid, node) {
        return setting.held;
    }
    let level = player.level.unwrap_or_else(|| {
        state
            .env
            .as_ref()
            .map_or(PermissionLevel::All, |env| env.level_of(player.uuid))
    });
    match state
        .registration
        .permissions
        .iter()
        .find(|declared| declared.node == node)
    {
        Some(declared) => declared.to_node().fallback(level),
        None => view
            .decide(
                Subject::Player {
                    uuid: player.uuid,
                    level,
                },
                node,
            )
            .held(),
    }
}

/// Online players; with `dimension`, only those standing there keep a position.
pub(super) fn online_players(
    state: &State,
    dimension: Option<&str>,
) -> Vec<(wit::Player, Option<[f64; 3]>)> {
    let Some(env) = state.env.as_ref() else {
        return Vec::new();
    };
    let shared = env.shared.clone();
    env.wait(async move {
        let (reply, players) = tokio::sync::oneshot::channel();
        let _ = shared
            .coordinator
            .send(tuff_core::world::messages::CoordinatorMessage::Players { reply })
            .await;
        players.await.unwrap_or_default()
    })
    .unwrap_or_default()
    .into_iter()
    .map(|p| {
        let at = p.state.position;
        let here = dimension.is_none_or(|key| env.shared.dimension(p.state.dimension).key == key);
        (
            wit::Player {
                uuid: uuid(p.uuid),
                name: p.name,
            },
            here.then_some(at),
        )
    })
    .collect()
}

pub(super) fn unavailable<T>(what: &str) -> Result<T, Denied> {
    Err(Denied::Failed(format!("{what} is not available yet")))
}

impl State {
    pub(super) fn queue_inventory(
        &mut self,
        player: u128,
        op: InventoryOp,
    ) -> Result<(), WorldError> {
        if self.effects.inventory_ops.len() >= abi::MAX_INVENTORY_OPS {
            return Err(WorldError::OverBudget);
        }
        self.effects.inventory_ops.push((player, op));
        Ok(())
    }

    pub(super) fn queue_give(
        &mut self,
        player: u128,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> Result<(), WorldError> {
        let gives = self
            .effects
            .inventory_ops
            .iter()
            .filter(|(_, op)| matches!(op, InventoryOp::Give { .. }))
            .count();
        if gives >= abi::MAX_GIVES {
            return Err(WorldError::OverBudget);
        }
        if !(1..=abi::MAX_GIVE_COUNT).contains(&count) {
            return Err(WorldError::InvalidState);
        }
        let too_long =
            |text: &Option<String>| text.as_ref().is_some_and(|t| t.len() > abi::MAX_GIVE_TEXT);
        if too_long(&custom_data) || too_long(&custom_name) {
            return Err(WorldError::OverBudget);
        }
        let item =
            <tuff_vanilla_data::Item as tuff_vanilla_data::registry::Registry>::from_name(item)
                .filter(|item| *item != tuff_vanilla_data::Item::Air)
                .ok_or(WorldError::InvalidState)?;
        let mut components = tuff_nbt::Compound::new();
        if let Some(data) = custom_data {
            match tuff_nbt::Tag::from_snbt(&data) {
                Ok(tag @ tuff_nbt::Tag::Compound(_)) => {
                    components.insert("minecraft:custom_data", tag);
                }
                _ => return Err(WorldError::InvalidState),
            }
        }
        if let Some(name) = custom_name {
            components.insert("minecraft:custom_name", tuff_nbt::Tag::String(name));
        }
        let mut nbt = tuff_nbt::Compound::new();
        nbt.insert("id", tuff_nbt::Tag::String(item.name().to_owned()));
        nbt.insert("count", tuff_nbt::Tag::Int(1));
        nbt.insert("components", tuff_nbt::Tag::Compound(components));
        let stack =
            tuff_item::item::ItemStack::mint_from_nbt(&nbt, tuff_item::item::MintReason::Template)
                .ok_or(WorldError::InvalidState)?;
        let template = tuff_item::item::ItemSnapshot::of(&stack);
        stack.burn(tuff_item::item::BurnReason::Template);
        self.queue_inventory(
            player,
            InventoryOp::Give {
                stack: template,
                count: count as i32,
            },
        )
    }

    pub(super) fn queue_advancement(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: Option<String>,
        grant: bool,
    ) -> Result<(), WorldError> {
        let qualified = if advancement.contains(':') {
            advancement.to_owned()
        } else {
            format!("minecraft:{advancement}")
        };
        let (namespace, path) = qualified.split_once(':').unwrap_or_default();
        let valid = |text: &str, extra: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(c))
        };
        if !valid(namespace, "_-.") || !valid(path, "_-./") {
            return Err(WorldError::InvalidState);
        }
        let granted = self
            .policy
            .as_ref()
            .is_some_and(|policy| policy.borrow().check_content(&qualified).is_ok());
        if !granted {
            return Err(WorldError::NotGranted);
        }
        if self.effects.advancements.len() >= abi::MAX_ADVANCEMENT_OPS {
            return Err(WorldError::OverBudget);
        }
        if criterion
            .as_ref()
            .is_some_and(|c| c.len() > abi::MAX_GIVE_TEXT)
        {
            return Err(WorldError::OverBudget);
        }
        self.effects.advancements.push(AdvancementOp {
            player: uuid_of(player),
            grant,
            advancement: qualified,
            criterion,
        });
        Ok(())
    }

    pub(super) fn queue_display(&mut self, op: DisplayOp) -> Result<(), HostError> {
        if !self.running() {
            return Err(HostError::WrongPhase);
        }
        if self.effects.displays.len() >= abi::MAX_DISPLAY_OPS {
            return Err(HostError::LimitReached);
        }
        let long = |text: &str| text.len() > abi::MAX_DISPLAY_TEXT;
        let bad = match &op {
            DisplayOp::Title {
                title, subtitle, ..
            } => long(title) || long(subtitle),
            DisplayOp::ActionBar(_, text) => long(text),
            DisplayOp::Payload { data, .. } => data.len() > abi::MAX_PAYLOAD_BYTES,
            DisplayOp::Sidebar(Some((title, lines))) => {
                if lines.len() > abi::MAX_SIDEBAR_LINES {
                    return Err(HostError::LimitReached);
                }
                long(title) || lines.iter().any(|line| long(line))
            }
            DisplayOp::Team {
                name,
                color,
                prefix,
                suffix,
                members,
            } => {
                if members.len() > abi::MAX_TEAM_MEMBERS {
                    return Err(HostError::LimitReached);
                }
                if color.as_deref().is_some_and(|c| {
                    tuff_core::world::scoreboard::TeamColor::from_name(c).is_none()
                }) {
                    return Err(HostError::Invalid("unknown team color".into()));
                }
                name.is_empty()
                    || name.len() > abi::MAX_TEAM_NAME
                    || long(prefix)
                    || long(suffix)
                    || members.iter().any(|m| m.len() > 64)
            }
            DisplayOp::TeamRemove(name) => name.is_empty() || name.len() > abi::MAX_TEAM_NAME,
            _ => false,
        };
        if bad {
            return Err(HostError::TooLarge);
        }
        self.effects.displays.push(op);
        Ok(())
    }

    pub(super) fn queue_player_op(
        &mut self,
        player: Uuid,
        op: crate::player_state::PlayerOp,
    ) -> Result<(), WorldError> {
        if !self.running() {
            return Err(WorldError::Unavailable);
        }
        if self.effects.player_ops.len() >= abi::MAX_PLAYER_OPS {
            return Err(WorldError::OverBudget);
        }
        self.effects.player_ops.push((uuid_of(player), op));
        Ok(())
    }

    pub(super) fn player_snapshot(
        &mut self,
        player: Uuid,
    ) -> Option<tuff_core::world::messages::PlayerSnapshot> {
        let env = self.env.clone()?;
        let wanted = uuid_of(player);
        let shared = env.shared.clone();
        env.wait(crate::player_state::snapshots(&shared))?
            .into_iter()
            .find(|snapshot| snapshot.uuid == wanted)
    }

    pub(super) fn registering(&self) -> Result<(), RegisterError> {
        match self.phase {
            Phase::Register => Ok(()),
            _ => Err(RegisterError::WrongPhase),
        }
    }

    pub(super) fn group_granted(&self, kind: crate::caps::Kind) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.borrow().check_group(kind).is_ok())
    }

    pub(super) fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// The dimension called `key` with the world to read it from.
    pub(super) fn dimension(
        &self,
        key: &str,
    ) -> Result<(Env, tuff_core::world::dimension::DimensionId), WorldError> {
        if !self.running() {
            return Err(WorldError::Unavailable);
        }
        let env = self.env.clone().ok_or(WorldError::UnknownDimension)?;
        let dimension = env.dimension(key).ok_or(WorldError::UnknownDimension)?;
        Ok((env, dimension))
    }

    pub(super) fn queue_entity_op(&mut self, op: EntityOp) -> Result<(), WorldError> {
        if !self.running() {
            return Err(WorldError::Unavailable);
        }
        if self.effects.entity_ops.len() >= abi::MAX_ENTITY_OPS {
            return Err(WorldError::OverBudget);
        }
        self.effects.entity_ops.push(op);
        Ok(())
    }

    pub(super) fn queue_teleport(
        &mut self,
        target: Target,
        dimension: &str,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> Result<(), WorldError> {
        let (_, dimension) = self.dimension(dimension)?;
        let at = [pos.x, pos.y, pos.z];
        let turn = rotation.map(|r| [r.yaw, r.pitch]);
        if at.iter().any(|v| !v.is_finite())
            || turn.is_some_and(|t| t.iter().any(|v| !v.is_finite()))
        {
            return Err(WorldError::OutsideWorld);
        }
        if self.effects.teleports.len() >= abi::MAX_TELEPORTS {
            return Err(WorldError::OverBudget);
        }
        self.effects.teleports.push(Teleport {
            target,
            dimension,
            position: at,
            rotation: turn,
        });
        Ok(())
    }

    /// Counts `blocks` against the call's read budget.
    pub(super) fn spend_reads(&mut self, blocks: u64) -> Result<(), WorldError> {
        if self.reads + blocks > self.limits.world_reads {
            return Err(WorldError::OverBudget);
        }
        self.reads += blocks;
        Ok(())
    }

    pub(super) fn messages(&self) -> usize {
        let effects = &self.effects;
        effects.replies.len()
            + effects.broadcasts.len()
            + effects.tells.len()
            + effects.rich_tells.len()
    }

    pub(super) fn message(&mut self, text: &str, broadcast: bool) -> Result<(), HostError> {
        if !self.running() {
            return Err(HostError::WrongPhase);
        }
        if self.messages() >= self.limits.messages {
            return Err(HostError::LimitReached);
        }
        if broadcast {
            self.effects.broadcasts.push(text.to_owned());
        } else if self.caller.is_some() {
            self.effects.replies.push(text.to_owned());
        } else {
            return Err(HostError::NoActingPlayer);
        }
        Ok(())
    }

    pub(super) fn acting(&self) -> Result<&Player, HostError> {
        if !self.running() {
            return Err(HostError::WrongPhase);
        }
        self.caller.as_ref().ok_or(HostError::NoActingPlayer)
    }

    pub(super) fn register_tree(
        &mut self,
        tree: CommandTree,
        tuff: bool,
    ) -> Result<(), RegisterError> {
        self.registering()?;
        tree::validate(&tree).map_err(RegisterError::Invalid)?;
        let name = tree::name(&tree);
        let (valid, limit, trees) = match tuff {
            false => (
                abi::valid_command(name),
                abi::MAX_COMMANDS,
                &self.registration.commands,
            ),
            true => (
                abi::valid_subcommand(name),
                abi::MAX_TUFF_SUBCOMMANDS,
                &self.registration.tuff_subcommands,
            ),
        };
        if !valid {
            return Err(RegisterError::Invalid(format!(
                "{name:?} is not a valid command name"
            )));
        }
        if trees.iter().any(|known| tree::name(known) == name) {
            return Err(RegisterError::Invalid(format!(
                "{name} is already registered"
            )));
        }
        if trees.len() >= limit {
            return Err(RegisterError::LimitReached(limit as u32));
        }
        match tuff {
            false => self.registration.commands.push(tree),
            true => self.registration.tuff_subcommands.push(tree),
        }
        Ok(())
    }
}

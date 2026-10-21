use super::super::*;

impl State {
    pub(in crate::sandbox) fn host_spawn_particles(
        &mut self,
        dimension: &str,
        spawn: wit::ParticleSpawn,
        viewer: Option<Uuid>,
    ) -> value::Result<Result<(), WorldError>> {
        Ok((|| {
            let (_, dimension) = self.dimension(dimension)?;
            let spawn = crate::particles::resolve(dimension, &spawn, viewer.map(uuid_of))
                .ok_or(WorldError::InvalidState)?;
            if self.effects.particles.len() >= abi::MAX_PARTICLE_SPAWNS {
                return Err(WorldError::OverBudget);
            }
            self.effects.particles.push(spawn);
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_teleport_player(
        &mut self,
        player: Uuid,
        dimension: &str,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_teleport(Target::Player(uuid_of(player)), dimension, pos, rotation))
    }

    pub(in crate::sandbox) fn host_teleport_entity(
        &mut self,
        entity: Uuid,
        dimension: &str,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_teleport(Target::Entity(uuid_of(entity)), dimension, pos, rotation))
    }

    pub(in crate::sandbox) fn host_spawn_entity(
        &mut self,
        dimension: &str,
        kind: &str,
        pos: Vec3,
        snbt: Option<String>,
    ) -> value::Result<Result<Uuid, WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            let (_, dimension) = self.dimension(dimension)?;
            let at = [pos.x, pos.y, pos.z];
            if at.iter().any(|v| !v.is_finite()) {
                return Err(WorldError::OutsideWorld);
            }
            if snbt
                .as_ref()
                .is_some_and(|t| t.len() > abi::MAX_ENTITY_TEXT)
            {
                return Err(WorldError::OverBudget);
            }
            let kind = <tuff_vanilla_data::registry::EntityType as tuff_vanilla_data::registry::Registry>::from_name(kind)
                .ok_or(WorldError::InvalidState)?;
            let data = match snbt {
                Some(text) => match tuff_nbt::Tag::from_snbt(&text) {
                    Ok(tuff_nbt::Tag::Compound(data)) => Some(data),
                    _ => return Err(WorldError::InvalidState),
                },
                None => None,
            };
            let id: u128 = rand::random();
            let (nbt, fresh) = crate::entities::fresh_nbt(kind.name(), data, id);
            self.queue_entity_op(EntityOp::Spawn {
                dimension,
                nbt,
                position: at,
                fresh,
            })?;
            Ok(uuid(id))
        })())
    }

    pub(in crate::sandbox) fn host_remove_entity(
        &mut self,
        entity: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_entity_op(EntityOp::Remove(uuid_of(entity))))
    }

    pub(in crate::sandbox) fn host_entity_data(
        &mut self,
        entity: Uuid,
    ) -> value::Result<Result<Option<String>, WorldError>> {
        Ok((|| {
            if !self.running() {
                return Err(WorldError::Unavailable);
            }
            let env = self.env.clone().ok_or(WorldError::Unavailable)?;
            self.spend_reads(64)?;
            let data = env
                .wait(crate::entities::data(env.shared.clone(), uuid_of(entity)))
                .ok_or(WorldError::Unavailable)?;
            match data {
                Some(data) => {
                    let text = tuff_nbt::Tag::Compound(data).to_string();
                    match text.len() > abi::MAX_ENTITY_TEXT * 16 {
                        true => Err(WorldError::OverBudget),
                        false => Ok(Some(text)),
                    }
                }
                None => Ok(None),
            }
        })())
    }

    pub(in crate::sandbox) fn host_entity_merge(
        &mut self,
        entity: Uuid,
        snbt: &str,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if snbt.len() > abi::MAX_ENTITY_TEXT {
                return Err(WorldError::OverBudget);
            }
            let Ok(tuff_nbt::Tag::Compound(patch)) = tuff_nbt::Tag::from_snbt(snbt) else {
                return Err(WorldError::InvalidState);
            };
            self.queue_entity_op(EntityOp::Merge(uuid_of(entity), patch))
        })())
    }

    pub(in crate::sandbox) fn host_entity_set_name(
        &mut self,
        entity: Uuid,
        name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        if name
            .as_ref()
            .is_some_and(|n| n.len() > abi::MAX_ENTITY_TEXT)
        {
            return Ok(Err(WorldError::OverBudget));
        }
        Ok(self.queue_entity_op(EntityOp::Rename(uuid_of(entity), name)))
    }

    pub(in crate::sandbox) fn host_entity_strike(
        &mut self,
        attacker: Uuid,
        victim: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_entity_op(EntityOp::Strike {
            attacker: uuid_of(attacker),
            victim: uuid_of(victim),
        }))
    }

    pub(in crate::sandbox) fn host_glide_entities(
        &mut self,
        moves: Vec<bindings::host::EntityGlide>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if moves.len() > abi::MAX_ENTITY_GLIDES {
                return Err(WorldError::OverBudget);
            }
            let moves = moves
                .into_iter()
                .map(|m| crate::entities::Glide {
                    uuid: uuid_of(m.entity),
                    position: [m.pos.x, m.pos.y, m.pos.z],
                    rotation: m.rotation.map(|r| [r.yaw, r.pitch]),
                })
                .collect();
            self.queue_entity_op(EntityOp::Glide(moves))
        })())
    }

    pub(in crate::sandbox) fn host_entity_states(
        &mut self,
        entities: Vec<Uuid>,
    ) -> value::Result<Result<Vec<Option<bindings::host::EntityState>>, WorldError>> {
        Ok((|| {
            if !self.running() {
                return Err(WorldError::Unavailable);
            }
            if entities.len() > abi::MAX_ENTITY_GLIDES {
                return Err(WorldError::OverBudget);
            }
            let env = self.env.clone().ok_or(WorldError::Unavailable)?;
            self.spend_reads(8 * entities.len() as u64)?;
            let uuids = entities.into_iter().map(uuid_of).collect();
            let states = env
                .wait(crate::entities::states(env.shared.clone(), uuids))
                .ok_or(WorldError::Unavailable)?;
            Ok(states
                .into_iter()
                .map(|state| {
                    state.map(|s| bindings::host::EntityState {
                        pos: Vec3 {
                            x: s.position[0],
                            y: s.position[1],
                            z: s.position[2],
                        },
                        motion: Vec3 {
                            x: s.motion[0],
                            y: s.motion[1],
                            z: s.motion[2],
                        },
                        yaw: s.yaw,
                        pitch: s.pitch,
                        on_ground: s.on_ground,
                        health: s.health,
                    })
                })
                .collect())
        })())
    }

    pub(in crate::sandbox) fn host_teleport_self(
        &mut self,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        let target = self.acting().ok().and_then(|player| {
            let dimension = player.position.as_ref()?.dimension.clone();
            Some((player.uuid, dimension))
        });
        Ok(match target {
            Some((uuid, dimension)) => {
                self.queue_teleport(Target::Player(uuid), &dimension, pos, rotation)
            }
            None => Err(WorldError::Unavailable),
        })
    }

    pub(in crate::sandbox) fn host_give_item(
        &mut self,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(match self.acting() {
            Ok(player) => {
                let player = player.uuid;
                self.queue_give(player, item, count, custom_data, custom_name)
            }
            Err(_) => Err(WorldError::Unavailable),
        })
    }

    pub(in crate::sandbox) fn host_give_item_to(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_give(uuid_of(player), item, count, custom_data, custom_name))
    }

    pub(in crate::sandbox) fn host_take_items(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        all_or_nothing: bool,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if !(1..=abi::MAX_GIVE_COUNT).contains(&count) {
                return Err(WorldError::InvalidState);
            }
            let item =
                <tuff_vanilla_data::Item as tuff_vanilla_data::registry::Registry>::from_name(item)
                    .filter(|item| *item != tuff_vanilla_data::Item::Air)
                    .ok_or(WorldError::InvalidState)?;
            self.queue_inventory(
                uuid_of(player),
                InventoryOp::Take {
                    item,
                    count,
                    all_or_nothing,
                },
            )
        })())
    }

    pub(in crate::sandbox) fn host_clear_inventory(
        &mut self,
        player: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_inventory(uuid_of(player), InventoryOp::Clear))
    }

    pub(in crate::sandbox) fn host_set_slot(
        &mut self,
        player: Uuid,
        slot: u8,
        stack: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if !(5..=45).contains(&slot) {
                return Err(WorldError::InvalidState);
            }
            let stack = match stack {
                None => None,
                Some(text) => {
                    if text.len() > abi::MAX_STACK_SNBT {
                        return Err(WorldError::OverBudget);
                    }
                    match tuff_nbt::Tag::from_snbt(&text) {
                        Ok(tuff_nbt::Tag::Compound(nbt)) => Some(
                            tuff_item::item::ItemStack::mint_from_nbt(
                                &nbt,
                                tuff_item::item::MintReason::Plugin,
                            )
                            .ok_or(WorldError::InvalidState)?,
                        ),
                        _ => return Err(WorldError::InvalidState),
                    }
                }
            };
            self.queue_inventory(uuid_of(player), InventoryOp::SetSlot { slot, stack })
        })())
    }

    pub(in crate::sandbox) fn host_set_keep_inventory(
        &mut self,
        player: Uuid,
        keep: Option<bool>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_inventory(uuid_of(player), InventoryOp::KeepInventory(keep)))
    }

    pub(in crate::sandbox) fn host_acting_inventory_snbt(
        &mut self,
    ) -> value::Result<Option<Vec<SnbtSlot>>> {
        if !self.group_granted(Kind::PlayerRead) {
            return Ok(None);
        }
        Ok(self
            .acting()
            .ok()
            .and_then(|p| p.items.as_ref())
            .map(|items| {
                items
                    .stacks
                    .iter()
                    .map(|(index, stack)| SnbtSlot {
                        index: *index,
                        stack: tuff_nbt::Tag::Compound(stack.to_nbt()).to_string(),
                    })
                    .collect()
            }))
    }

    pub(in crate::sandbox) fn host_advancement_grant(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_advancement(player, advancement, criterion, true))
    }

    pub(in crate::sandbox) fn host_advancement_revoke(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_advancement(player, advancement, criterion, false))
    }

    pub(in crate::sandbox) fn host_online_players(&mut self) -> value::Result<Vec<wit::Player>> {
        if !self.group_granted(Kind::PlayerRead) {
            return Ok(Vec::new());
        }
        Ok(online_players(self, None)
            .into_iter()
            .map(|(p, _)| p)
            .collect())
    }

    pub(in crate::sandbox) fn host_player_info(
        &mut self,
        player: Uuid,
    ) -> value::Result<Option<wit::PlayerState>> {
        if !self.group_granted(Kind::PlayerRead) {
            return Ok(None);
        }
        let Some(env) = self.env.clone() else {
            return Ok(None);
        };
        Ok(self
            .player_snapshot(player)
            .map(|snapshot| crate::player_state::state_of(&env.shared, &snapshot)))
    }

    pub(in crate::sandbox) fn host_player_location(
        &mut self,
        player: Uuid,
    ) -> value::Result<Option<Location>> {
        if !self.group_granted(Kind::PlayerRead) {
            return Ok(None);
        }
        let Some(env) = self.env.clone() else {
            return Ok(None);
        };
        Ok(self
            .player_snapshot(player)
            .map(|snapshot| crate::player_state::location(&env.shared, &snapshot)))
    }

    pub(in crate::sandbox) fn host_set_game_mode(
        &mut self,
        player: Uuid,
        mode: wit::GameMode,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        let op = crate::player_state::PlayerOp::GameMode(crate::player_state::game_mode_of(mode));
        Ok(self.queue_player_op(player, op))
    }

    pub(in crate::sandbox) fn host_set_vitals(
        &mut self,
        player: Uuid,
        health: Option<f32>,
        food: Option<i32>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        if health.is_some_and(|h| !h.is_finite() || h <= 0.0) {
            return Ok(Err(WorldError::InvalidState));
        }
        let op = crate::player_state::PlayerOp::Vitals { health, food };
        Ok(self.queue_player_op(player, op))
    }

    pub(in crate::sandbox) fn host_set_xp(
        &mut self,
        player: Uuid,
        level: i32,
        points: i32,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        if level < 0 || points < 0 {
            return Ok(Err(WorldError::InvalidState));
        }
        let op = crate::player_state::PlayerOp::Xp { level, points };
        Ok(self.queue_player_op(player, op))
    }

    pub(in crate::sandbox) fn host_add_effect(
        &mut self,
        player: Uuid,
        effect: &str,
        duration: i32,
        amplifier: u8,
        particles: bool,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        let Some(effect) = crate::player_state::effect_of(effect).filter(|_| duration > 0) else {
            return Ok(Err(WorldError::InvalidState));
        };
        let op = crate::player_state::PlayerOp::EffectGive {
            effect,
            duration,
            amplifier,
            particles,
        };
        Ok(self.queue_player_op(player, op))
    }

    pub(in crate::sandbox) fn host_clear_effects(
        &mut self,
        player: Uuid,
        effect: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        let effect = match effect {
            Some(name) => match crate::player_state::effect_of(&name) {
                Some(effect) => Some(effect),
                None => return Ok(Err(WorldError::InvalidState)),
            },
            None => None,
        };
        Ok(self.queue_player_op(player, crate::player_state::PlayerOp::EffectClear(effect)))
    }

    pub(in crate::sandbox) fn host_kill_player(
        &mut self,
        player: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_player_op(player, crate::player_state::PlayerOp::Kill))
    }

    pub(in crate::sandbox) fn host_respawn_player(
        &mut self,
        player: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.queue_player_op(player, crate::player_state::PlayerOp::Respawn))
    }

    pub(in crate::sandbox) fn host_create_dimension(
        &mut self,
        key: &str,
        base: &str,
        copy: Option<wit::ChunkArea>,
    ) -> value::Result<Result<(), wit::DimensionError>> {
        use wit::DimensionError;
        Ok((|| {
            if !self.running() {
                return Err(DimensionError::Unavailable);
            }
            if !crate::dimensions::valid_key(key) {
                return Err(DimensionError::InvalidKey);
            }
            let granted = self
                .policy
                .as_ref()
                .is_some_and(|policy| policy.borrow().check_dimension(key).is_ok());
            if !granted {
                return Err(DimensionError::NotGranted);
            }
            let (env, base) = self.dimension(base).map_err(|err| match err {
                WorldError::Unavailable => DimensionError::Unavailable,
                _ => DimensionError::UnknownDimension,
            })?;
            let copy = match copy {
                Some(area) => crate::dimensions::chunks(&area).ok_or(DimensionError::OverBudget)?,
                None => Vec::new(),
            };
            let shared = env.shared.clone();
            let plugin = self.plugin.clone();
            env.wait(
                async move { crate::dimensions::create(&shared, &plugin, key, base, &copy).await },
            )
            .unwrap_or(Err(DimensionError::Unavailable))
            .map(|_| ())
        })())
    }

    pub(in crate::sandbox) fn host_discard_dimension(
        &mut self,
        key: &str,
    ) -> value::Result<Result<(), wit::DimensionError>> {
        use wit::DimensionError;
        Ok((|| {
            if !self.running() {
                return Err(DimensionError::Unavailable);
            }
            let env = self.env.as_ref().ok_or(DimensionError::Unavailable)?;
            let dimensions = &env.shared.dimensions;
            let id = dimensions
                .by_key(key)
                .map(|d| d.id)
                .filter(|id| dimensions.owner(*id) == Some(self.plugin.as_str()))
                .ok_or(DimensionError::NotOwned)?;
            if !dimensions.discard(id) {
                return Err(DimensionError::NotOwned);
            }
            self.effects.discards.push(id);
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_runtime_dimensions(&mut self) -> value::Result<Vec<String>> {
        Ok(self.env.as_ref().map_or_else(Vec::new, |env| {
            crate::dimensions::owned(&env.shared, &self.plugin)
        }))
    }

    pub(in crate::sandbox) fn host_set_respawn_point(
        &mut self,
        player: Uuid,
        dimension: &str,
        pos: wit::BlockPos,
        rotation: Rotation,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            let (_, dimension) = self.dimension(dimension)?;
            let op = crate::player_state::PlayerOp::SpawnPoint {
                dimension,
                pos: [pos.x, pos.y, pos.z],
                yaw: rotation.yaw,
                pitch: rotation.pitch,
            };
            self.queue_player_op(player, op)
        })())
    }
}

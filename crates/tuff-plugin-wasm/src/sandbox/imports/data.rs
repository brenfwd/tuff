use super::super::*;

impl State {
    pub(in crate::sandbox) fn host_state_get(
        &mut self,
        key: &str,
    ) -> value::Result<Result<Option<Vec<u8>>, HostError>> {
        let player = match self.acting() {
            Ok(player) => player.uuid,
            Err(err) => return Ok(Err(err)),
        };
        Ok(Ok(self
            .players
            .get(&player)
            .and_then(|values| values.get(key))
            .cloned()))
    }

    pub(in crate::sandbox) fn host_state_set(
        &mut self,
        key: &str,
        value: Option<Vec<u8>>,
    ) -> value::Result<Result<(), HostError>> {
        let player = match self.acting() {
            Ok(player) => player.uuid,
            Err(err) => return Ok(Err(err)),
        };
        if key.len() > crate::storage::MAX_KEY {
            return Ok(Err(HostError::TooLarge));
        }
        let values = self.players.entry(player).or_default();
        match value {
            None => {
                values.remove(key);
            }
            Some(value) => {
                if !values.contains_key(key) && values.len() >= self.limits.state_keys {
                    return Ok(Err(HostError::LimitReached));
                }
                values.insert(key.to_owned(), value);
            }
        }
        if values.is_empty() {
            self.players.remove(&player);
        }
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_storage_get(
        &mut self,
        key: &str,
    ) -> value::Result<Option<Vec<u8>>> {
        Ok(self.storage.get(key).map(<[u8]>::to_vec))
    }

    pub(in crate::sandbox) fn host_storage_set(
        &mut self,
        key: &str,
        value: Option<Vec<u8>>,
    ) -> value::Result<Result<(), HostError>> {
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        let slot = self.effects.stored.iter().position(|(k, _)| k == key);
        if slot.is_none() && self.effects.stored.len() >= self.limits.storage.keys {
            return Ok(Err(HostError::LimitReached));
        }
        if !self.storage.set(key, value.clone(), self.limits.storage) {
            return Ok(Err(HostError::TooLarge));
        }
        match slot {
            Some(slot) => self.effects.stored[slot].1 = value,
            None => self.effects.stored.push((key.to_owned(), value)),
        }
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn host_block_get(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
    ) -> value::Result<Result<String, WorldError>> {
        Ok((|| {
            let (env, dimension) = self.dimension(dimension)?;
            if !env.inside(dimension, pos.y) {
                return Err(WorldError::OutsideWorld);
            }
            self.spend_reads(1)?;
            let positions = vec![BlockPos::new(pos.x, pos.y, pos.z)];
            let states = env
                .wait(world::read(env.shared.clone(), dimension, positions))
                .ok_or(WorldError::Unavailable)?;
            let state = states.into_iter().next().flatten();
            state
                .map(|state| state.to_string())
                .ok_or(WorldError::OutsideWorld)
        })())
    }

    pub(in crate::sandbox) fn host_blocks_get(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
    ) -> value::Result<Result<BlockBox, WorldError>> {
        Ok(self.read_box(dimension, a, b, None))
    }

    pub(in crate::sandbox) fn host_generated_blocks_get(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
        seed: Option<i64>,
        biome: Option<String>,
    ) -> value::Result<Result<BlockBox, WorldError>> {
        Ok(self.read_box(dimension, a, b, Some((seed, biome))))
    }

    fn read_box(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
        generated: Option<(Option<i64>, Option<String>)>,
    ) -> Result<BlockBox, WorldError> {
        (|| {
            let (env, dimension) = self.dimension(dimension)?;
            let (min, max) = (
                [a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)],
                [a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)],
            );
            let size = |axis: usize| (i64::from(max[axis]) - i64::from(min[axis]) + 1) as u64;
            let volume = size(0)
                .checked_mul(size(1))
                .and_then(|v| v.checked_mul(size(2)))
                .unwrap_or(u64::MAX);
            if volume > abi::MAX_BOX_VOLUME {
                return Err(WorldError::OverBudget);
            }
            let variant = match &generated {
                Some((seed, biome)) if seed.is_some() || biome.is_some() => Some(
                    env.shared
                        .dimension(dimension)
                        .generator
                        .variant(*seed, biome.as_deref())
                        .ok_or(WorldError::InvalidState)?,
                ),
                _ => None,
            };
            self.spend_reads(volume)?;
            let mut positions = Vec::with_capacity(volume as usize);
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    for x in min[0]..=max[0] {
                        positions.push(BlockPos::new(x, y, z));
                    }
                }
            }
            let states = match generated {
                Some(_) => env.wait(world::generate(
                    env.shared.clone(),
                    dimension,
                    positions,
                    variant,
                )),
                None => env.wait(world::read(env.shared.clone(), dimension, positions)),
            }
            .ok_or(WorldError::Unavailable)?;
            let mut palette = Vec::new();
            let mut known = HashMap::new();
            let indices = states
                .into_iter()
                .map(|state| match state {
                    None => abi::UNAVAILABLE,
                    Some(state) => *known.entry(state).or_insert_with(|| {
                        palette.push(state.to_string());
                        (palette.len() - 1) as u16
                    }),
                })
                .collect();
            Ok(BlockBox { palette, indices })
        })()
    }

    pub(in crate::sandbox) fn host_block_set(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
        state: &str,
        flags: SetFlags,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            let (env, dimension) = self.dimension(dimension)?;
            let state = world::parse_state(state).ok_or(WorldError::InvalidState)?;
            if !env.inside(dimension, pos.y) {
                return Err(WorldError::OutsideWorld);
            }
            if self.effects.blocks.len() >= self.limits.world_writes {
                return Err(WorldError::OverBudget);
            }
            self.effects.blocks.push(BlockWrite {
                dimension,
                pos: BlockPos::new(pos.x, pos.y, pos.z),
                state,
                updates: !flags.contains(SetFlags::NO_UPDATES),
            });
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_biome_get(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
    ) -> value::Result<Result<String, WorldError>> {
        Ok((|| {
            let (env, dimension) = self.dimension(dimension)?;
            if !env.inside(dimension, pos.y) {
                return Err(WorldError::OutsideWorld);
            }
            self.spend_reads(1)?;
            let positions = vec![BlockPos::new(pos.x, pos.y, pos.z)];
            let names = env
                .wait(world::read_biomes(env.shared.clone(), dimension, positions))
                .ok_or(WorldError::Unavailable)?;
            names
                .into_iter()
                .next()
                .flatten()
                .map(str::to_owned)
                .ok_or(WorldError::OutsideWorld)
        })())
    }

    pub(in crate::sandbox) fn host_biome_set(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
        biome: &str,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            let (env, dimension) = self.dimension(dimension)?;
            let biome = env
                .shared
                .dimensions
                .biomes()
                .id(biome)
                .ok_or(WorldError::InvalidState)?;
            let (min, max) = (
                [a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)],
                [a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)],
            );
            if [min[1], max[1]].iter().any(|&y| !env.inside(dimension, y)) {
                return Err(WorldError::OutsideWorld);
            }
            let write = BiomeWrite {
                dimension,
                min: BlockPos::new(min[0], min[1], min[2]),
                max: BlockPos::new(max[0], max[1], max[2]),
                biome,
            };
            let queued: u64 = self.effects.biomes.iter().map(BiomeWrite::cells).sum();
            if write.cells() > (self.limits.world_writes as u64).saturating_sub(queued) {
                return Err(WorldError::OverBudget);
            }
            self.effects.biomes.push(write);
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_entities_in_box(
        &mut self,
        dimension: &str,
        a: Vec3,
        b: Vec3,
    ) -> value::Result<Result<Vec<Entity>, WorldError>> {
        let key = dimension;
        Ok((|| {
            let (env, dimension) = self.dimension(key)?;
            let corners = [a.x, a.y, a.z, b.x, b.y, b.z];
            if corners.iter().any(|v| !v.is_finite()) {
                return Err(WorldError::OverBudget);
            }
            let min = [a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)];
            let max = [a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)];
            if (0..3).any(|axis| max[axis] - min[axis] > abi::MAX_ENTITY_BOX) {
                return Err(WorldError::OverBudget);
            }
            self.spend_reads(64)?;
            let found = env
                .wait(world::entities_in(
                    env.shared.clone(),
                    dimension,
                    min,
                    max,
                    abi::MAX_ENTITIES,
                ))
                .unwrap_or_default();
            let _ = self.spend_reads(found.len() as u64);
            let key = env.shared.dimensions.get(dimension).key.clone();
            Ok(found
                .into_iter()
                .map(|entity| {
                    let [x, y, z] = entity.at;
                    Entity {
                        uuid: uuid(entity.uuid),
                        kind: entity.kind.to_owned(),
                        pos: Vec3 { x, y, z },
                        dimension: key.clone(),
                    }
                })
                .collect())
        })())
    }

    pub(in crate::sandbox) fn host_block_normalize(
        &mut self,
        state: &str,
    ) -> value::Result<Option<String>> {
        Ok(crate::registry::normalize(state))
    }

    pub(in crate::sandbox) fn host_block_properties(
        &mut self,
        state: &str,
    ) -> value::Result<Option<Vec<Property>>> {
        Ok(crate::registry::properties(state).map(|lines| {
            lines
                .lines()
                .filter_map(|line| {
                    let (name, values) = line.split_once('=')?;
                    Some(Property {
                        name: name.to_owned(),
                        values: values.split(',').map(str::to_owned).collect(),
                    })
                })
                .collect()
        }))
    }

    pub(in crate::sandbox) fn host_block_tag(
        &mut self,
        tag: &str,
    ) -> value::Result<Option<Vec<String>>> {
        Ok(crate::registry::tag_members(tag)
            .map(|members| members.lines().map(str::to_owned).collect()))
    }

    pub(in crate::sandbox) fn host_item_tag(
        &mut self,
        tag: &str,
    ) -> value::Result<Option<Vec<String>>> {
        Ok(crate::registry::item_tag_members(tag)
            .map(|members| members.lines().map(str::to_owned).collect()))
    }

    pub(in crate::sandbox) fn host_block_transform(
        &mut self,
        state: &str,
        op: Transform,
    ) -> value::Result<Option<String>> {
        let op = match op {
            Transform::Rotate90 => crate::registry::OP_ROTATE_90,
            Transform::Rotate180 => crate::registry::OP_ROTATE_180,
            Transform::Rotate270 => crate::registry::OP_ROTATE_270,
            Transform::SwapNorthSouth => crate::registry::OP_SWAP_NORTH_SOUTH,
            Transform::SwapEastWest => crate::registry::OP_SWAP_EAST_WEST,
        };
        Ok(crate::registry::transform(state, op).ok())
    }

    pub(in crate::sandbox) fn host_item_info(
        &mut self,
        item: &str,
    ) -> value::Result<Option<ItemDetails>> {
        Ok(crate::registry::item(item))
    }

    pub(in crate::sandbox) fn host_block_traits(
        &mut self,
        state: &str,
    ) -> value::Result<Option<Traits>> {
        Ok(crate::registry::traits(state)
            .map(|bits| Traits::from_bits_truncate(u64::from(bits as u32))))
    }

    pub(in crate::sandbox) fn host_add_block_rule(
        &mut self,
        rule: wit::BlockRule,
    ) -> value::Result<Result<u32, HostError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(HostError::NotGranted));
        }
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        Ok(self.guard.add_rule(&mut self.effects.guard, rule))
    }

    pub(in crate::sandbox) fn host_add_change_rule(
        &mut self,
        rule: wit::ChangeRule,
    ) -> value::Result<Result<u32, HostError>> {
        if !self.group_granted(Kind::WorldWrite) {
            return Ok(Err(HostError::NotGranted));
        }
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        Ok(self.guard.add_change(&mut self.effects.guard, rule))
    }

    pub(in crate::sandbox) fn host_remove_block_rule(&mut self, id: u32) -> value::Result<bool> {
        Ok(self.running() && self.guard.remove_rule(&mut self.effects.guard, id))
    }

    pub(in crate::sandbox) fn host_set_team(
        &mut self,
        player: Uuid,
        team: Option<String>,
    ) -> value::Result<Result<(), HostError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(HostError::NotGranted));
        }
        if !self.running() {
            return Ok(Err(HostError::WrongPhase));
        }
        Ok(self
            .guard
            .set_team(&mut self.effects.guard, uuid_of(player), team))
    }

    pub(in crate::sandbox) fn host_fake_player_spawn(
        &mut self,
        spec: wit::FakeSpec,
    ) -> value::Result<Result<Uuid, wit::FakeError>> {
        let (Some(env), true) = (self.env.clone(), self.running()) else {
            return Ok(Err(wit::FakeError::Unavailable));
        };
        Ok(self.fakes.spawn(&env, &self.plugin, spec))
    }

    pub(in crate::sandbox) fn host_fake_player_remove(
        &mut self,
        fake: Uuid,
    ) -> value::Result<Result<(), wit::FakeError>> {
        let Some(env) = self.env.clone() else {
            return Ok(Err(wit::FakeError::Unavailable));
        };
        Ok(self.fakes.remove(&env, &self.plugin, fake))
    }

    pub(in crate::sandbox) fn host_fake_player_input(
        &mut self,
        fake: Uuid,
        input: wit::FakeInput,
    ) -> value::Result<Result<(), wit::FakeError>> {
        Ok(self.fakes.input(fake, input))
    }

    pub(in crate::sandbox) fn host_fake_player_act(
        &mut self,
        fake: Uuid,
        action: wit::FakeAction,
    ) -> value::Result<Result<(), wit::FakeError>> {
        let Some(env) = self.env.clone() else {
            return Ok(Err(wit::FakeError::Unavailable));
        };
        Ok(self.fakes.act(&env, fake, action))
    }

    pub(in crate::sandbox) fn host_fake_player_state(
        &mut self,
        fake: Uuid,
    ) -> value::Result<Option<wit::FakeState>> {
        let Some(env) = self.env.clone() else {
            return Ok(None);
        };
        Ok(self.fakes.state(&env, fake))
    }

    pub(in crate::sandbox) fn host_fake_player_inventory(
        &mut self,
        fake: Uuid,
    ) -> value::Result<Option<wit::Inventory>> {
        Ok(self.fakes.inventory(fake))
    }

    pub(in crate::sandbox) fn host_fake_player_break_ticks(
        &mut self,
        fake: Uuid,
        state: &str,
    ) -> value::Result<Option<wit::BreakTool>> {
        if self.env.is_none() {
            return Ok(None);
        }
        Ok(self.fakes.break_ticks(fake, state))
    }

    pub(in crate::sandbox) fn host_fake_players(&mut self) -> value::Result<Vec<Uuid>> {
        Ok(self.fakes.uuids())
    }

    pub(in crate::sandbox) fn host_acting_damage(
        &mut self,
    ) -> value::Result<Option<wit::DamageSource>> {
        Ok(match &self.guard.detail {
            Some(crate::guard::Detail::Damage(source)) => Some(source.clone()),
            _ => None,
        })
    }

    pub(in crate::sandbox) fn host_acting_hit(&mut self) -> value::Result<Option<wit::Hit>> {
        Ok(match &self.guard.detail {
            Some(crate::guard::Detail::Hit(hit)) => Some(*hit),
            _ => None,
        })
    }
}

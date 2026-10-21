use super::*;

impl RegionEntity for Minecart {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert(FLIPPED, self.flipped);
        out.insert(HAS_TICKED, self.first_tick);
        if self.is_tnt() {
            out.insert(FUSE, self.fuse);
            if self.power != TNT_POWER {
                out.insert(EXPLOSION_POWER, self.power);
            }
            if self.speed_factor != TNT_SPEED_FACTOR {
                out.insert(EXPLOSION_SPEED_FACTOR, self.speed_factor);
            }
        }
        if self.is_furnace() {
            out.insert(PUSH_X, self.push[0]);
            out.insert(PUSH_Z, self.push[1]);
            out.insert(FUEL, self.fuel.clamp(0, i32::from(i16::MAX)) as i16);
        }
        if self.base.kind == EntityType::HopperMinecart {
            out.insert(ENABLED, self.enabled);
        }
        if !self.has_container() {
            return;
        }
        if let Some((table, seed)) = &self.loot_table {
            out.insert(LOOT_TABLE, table.clone());
            if *seed != 0 {
                out.insert(LOOT_TABLE_SEED, *seed);
            }
        } else {
            out.insert(
                ITEMS,
                Tag::List(
                    encode_items(&self.items)
                        .into_iter()
                        .map(Tag::Compound)
                        .collect(),
                ),
            );
        }
    }

    fn container(&self) -> Option<tuff_core::world::entity_slots::ContainerSlots<'_>> {
        self.has_container().then(|| self.items.as_slice().into())
    }

    fn take_items(&mut self) -> Vec<ItemStack> {
        self.items.iter_mut().filter_map(Option::take).collect()
    }

    fn container_mut(&mut self) -> Option<tuff_core::world::entity_slots::ContainerSlotsMut<'_>> {
        self.has_container()
            .then(|| self.items.as_mut_slice().into())
    }

    fn container_menu(&self) -> MenuKind {
        match self.base.kind {
            EntityType::HopperMinecart => MenuKind::Hopper,
            _ => MenuKind::Generic {
                rows: (self.items.len() / 9) as u8,
            },
        }
    }

    fn take_loot_table(&mut self) -> Option<(String, i64)> {
        self.loot_table.take()
    }

    fn loot_pending(&self) -> bool {
        self.loot_table.is_some()
    }

    fn collects_items(&self) -> bool {
        self.base.kind == EntityType::HopperMinecart && self.enabled && !self.broken
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        [
            DATA_HURT,
            DATA_HURT_DIR,
            DATA_DAMAGE,
            DATA_FUEL,
            DATA_LAST_OUTPUT,
        ]
        .into_iter()
        .filter_map(|index| Some((index, self.value(index)?)))
        .collect()
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn rotates(&self) -> bool {
        true
    }

    fn rides_rails(&self) -> bool {
        true
    }

    fn track_steps(&mut self) -> Option<&mut Vec<play::TrackStep>> {
        self.improved.then_some(&mut self.steps)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        if self.broken {
            return Tick::Discard;
        }
        if self.hurt_time > 0 {
            self.hurt_time -= 1;
            self.changed.push(DATA_HURT);
        }
        if self.damage > 0.0 {
            self.damage -= 1.0;
            self.changed.push(DATA_DAMAGE);
        }
        if self.is_command_block() {
            self.show_command();
        }
        match self.pending.take() {
            Some(Pending::Destroy { ignites }) => {
                if self.broken_by(ignites, cx) {
                    return Tick::Discard;
                }
            }
            Some(Pending::Explode { speed_sq }) => {
                if let Some(outcome) = self.explode(speed_sq, cx) {
                    return outcome;
                }
            }
            None => {}
        }
        tuff_core::world::block_contact::pricked_by_cactus(self, cx.block);
        self.improved = track::enabled(cx);
        let reach = if self.improved {
            MOVING_BLOCK_REACH + self.track_max_speed(cx)
        } else {
            MOVING_BLOCK_REACH
        };
        let reach = physics::inflate(self.base.bounds(), [reach; 3]);
        self.moving_blocks.clear();
        self.moving_blocks.extend(
            cx.pistons
                .iter()
                .copied()
                .filter(|b| physics::intersects(*b, reach)),
        );
        let moved = if self.improved {
            self.tick_on_track(cx)
        } else {
            self.tick_on_rails(cx)
        };
        self.first_tick = false;
        self.pushed_by_players(cx);
        self.collide_with_others(cx);
        self.feel_fluids(cx);
        if self.is_furnace() {
            let lit = self.fuel > 0;
            if lit {
                self.fuel -= 1;
            }
            if self.fuel <= 0 {
                self.push = [0.0; 2];
            }
            if lit != (self.fuel > 0) {
                self.changed.push(DATA_FUEL);
            }
        }
        if self.is_tnt() {
            if let Some(fall) = moved.landed
                && fall >= 3.0
            {
                let power = fall / 10.0;
                if let Some(outcome) = self.explode(power * power, cx) {
                    return outcome;
                }
            }
            if self.fuse > 0 {
                self.fuse -= 1;
            } else if self.fuse == 0 {
                let speed_sq = horizontal_sq(self.base.velocity);
                if let Some(outcome) = self.explode(speed_sq, cx) {
                    return outcome;
                }
            }
            let speed_sq = horizontal_sq(self.base.velocity);
            if moved.horizontal_collision
                && speed_sq >= TNT_VOLATILE_SPEED_SQ
                && let Some(outcome) = self.explode(speed_sq, cx)
            {
                return outcome;
            }
        }
        Tick::Alive
    }

    fn hurt(&mut self, amount: f32, damage_type: &'static DamageType) -> bool {
        self.mark_hurt(amount);
        let ignites = tuff_datapack::tags::has(
            tuff_vanilla_data::tag::damage_type::key::IS_FIRE,
            damage_type.name,
        ) || tuff_datapack::tags::has(
            tuff_vanilla_data::tag::damage_type::key::IS_EXPLOSION,
            damage_type.name,
        );
        if self.damage > BREAK_DAMAGE || (self.is_tnt() && ignites) {
            self.pending = Some(Pending::Destroy { ignites });
        }
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        let attacker = cx.players.iter().find(|p| p.id == attack.attacker);
        let creative = attacker.is_some_and(|p| p.creative);
        if self.is_tnt() && self.igniter.is_none() {
            self.igniter = attacker.map(|p| Igniter {
                id: p.id,
                uuid: p.uuid,
                name: p.name.to_owned(),
            });
        }
        self.breaker = attacker.map(|p| p.id);
        self.broken |= self.damage(attack.damage, creative, false, cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn hit(&mut self, hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        let burning_projectile = hit.projectile.is_some() && hit.ignite > 0.0;
        if self.is_tnt() && burning_projectile {
            let speed_sq = hit
                .projectile_velocity
                .map_or(0.0, |v| v.iter().map(|c| c * c).sum());
            self.pending = Some(Pending::Explode { speed_sq });
        }
        let ignites = if hit.projectile.is_some() {
            burning_projectile
        } else {
            tuff_datapack::tags::has(
                tuff_vanilla_data::tag::damage_type::key::IS_FIRE,
                hit.kind.name,
            ) || tuff_datapack::tags::has(
                tuff_vanilla_data::tag::damage_type::key::IS_EXPLOSION,
                hit.kind.name,
            )
        };
        self.broken |= self.damage(hit.amount, false, ignites, cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn pickable(&self) -> bool {
        true
    }

    fn seats(&self) -> usize {
        usize::from(self.rideable())
    }

    fn takes_rider_input(&self) -> bool {
        self.rideable()
    }

    fn steer_with_input(&mut self, input: RiderInput) {
        self.input = input;
    }

    fn interact(
        &mut self,
        interaction: &EntityInteraction,
        _context: &mut TickContext,
    ) -> InteractOutcome {
        if !self.is_furnace() {
            return None;
        }
        let fuel = interaction.stack.as_deref().is_some_and(|stack| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::item::key::FURNACE_MINECART_FUEL,
                stack.item,
            )
        });
        if !fuel || self.fuel + FUEL_TICKS_PER_ITEM > MAX_FUEL_TICKS {
            return Some(HandEffect::None);
        }
        let was_lit = self.fuel > 0;
        self.fuel += FUEL_TICKS_PER_ITEM;
        self.push = [
            self.base.position[0] - interaction.position[0],
            self.base.position[2] - interaction.position[2],
        ];
        if !was_lit {
            self.changed.push(DATA_FUEL);
        }
        Some(HandEffect::Consume(1))
    }

    fn on_mounted(&mut self) {
        self.input = RiderInput::default();
    }

    fn seat(&self, _index: usize, _riders: usize) -> [f64; 3] {
        [0.0, f64::from(self.base.kind.size().height) * 0.5, 0.0]
    }

    fn dismount_location(
        &self,
        width: f64,
        yaw: f32,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> [f64; 3] {
        let _ = yaw;
        let [x, _, z] = self.base.position;
        let forward = tuff_vanilla_common::placement::from_yaw(self.base.yaw);
        let at = BlockPos::new(
            x.floor() as i32,
            self.base.position[1].floor() as i32,
            z.floor() as i32,
        );
        tuff_vanilla_common::dismount::ring_dismount(at, forward, width, block).unwrap_or([
            self.base.position[0],
            self.bounds()[4],
            self.base.position[2],
        ])
    }
}

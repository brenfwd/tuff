use super::*;

impl RegionActor {
    /// The container block at `pos` as a hopper sees it: its kind and the
    /// block entities behind it in slot order.
    pub(crate) fn container_at(&self, pos: BlockPos) -> Option<(ContainerKind, Vec<BlockPos>)> {
        let state = self.block(pos)?;
        let kind = ContainerKind::of_block(state.block()).filter(|k| k.is_container())?;
        let parts = self.joined_parts(pos, state);
        if !parts.iter().all(|part| {
            !self.menus.container_locks.contains_key(part)
                && if self.owns(*part) {
                    self.block_entity(*part)
                        .is_some_and(|entity| entity.kind == kind)
                } else {
                    self.part_items(*part).is_some()
                }
        }) {
            return None;
        }
        Some((kind, parts))
    }

    pub(crate) fn tick_block_entities(&mut self) {
        let _span = tuff_timings::span!("region.block_entities");
        let mut positions: Vec<BlockPos> = self.menus.ticking.iter().copied().collect();
        positions.sort_by_key(|p| (p.y, p.z, p.x));
        let border = self.dimension().border();
        for pos in positions {
            if !border.contains_block(pos)
                || self
                    .columns
                    .get(&ChunkPos::of_block(pos))
                    .is_none_or(|c| c.unloading)
            {
                continue;
            }
            let Some(state) = self.block(pos) else {
                continue;
            };
            let _span = tuff_timings::span!(
                "block_entity",
                tuff_timings::Tag::Name(tuff_vanilla_data::Registry::name(state.block()))
            );
            let kind = self.block_entity(pos).map(|entity| entity.kind);
            if let Some(kind) = kind
                && let Some(ticker) = self.shared.block_entity_tickers.get(kind).cloned()
            {
                self.tick_with(pos, kind, state, &*ticker);
                continue;
            }
            match kind {
                Some(ContainerKind::Hopper) => {}
                _ => continue,
            }
            let Some(facing) = state.facing() else {
                continue;
            };
            let cooldown = {
                let Some(entity) = self.block_entity_mut(pos) else {
                    continue;
                };
                entity.transfer_cooldown -= 1;
                entity.transfer_cooldown
            };
            self.menus.hopper_ticks.insert(pos, self.sched.tick);
            if cooldown > 0 {
                continue;
            }
            self.block_entity_mut(pos).unwrap().transfer_cooldown = 0;
            if state.get("enabled") == Some("false") {
                continue;
            }
            let (empty, full) = {
                let entity = self.block_entity(pos).unwrap();
                (entity.is_empty(), entity.is_full())
            };
            let changed = self.with_cause(BlockCause::Block(Block::Hopper), |region| {
                let mut changed = false;
                if !empty {
                    changed = region.hopper_push(pos, facing);
                }
                if !full {
                    changed |= region.hopper_pull(pos);
                }
                changed
            });
            if changed && let Some(entity) = self.block_entity_mut(pos) {
                entity.transfer_cooldown = block_entity::HOPPER_COOLDOWN;
            }
        }
    }

    pub(super) fn tick_with(
        &mut self,
        pos: BlockPos,
        kind: ContainerKind,
        state: BlockState,
        ticker: &dyn block_entity::BlockEntityTicker,
    ) {
        let recipes = self.shared.recipes();
        let game_time = self.sched.tick as i64;
        let viewed = self.menus.viewers.contains_key(&pos);
        let Some(entity) = self.block_entity_mut(pos) else {
            return;
        };
        let mut items = std::mem::take(&mut entity.items);
        let before: Option<Vec<Option<ItemSnapshot>>> = viewed.then(|| {
            items
                .iter()
                .map(|stack| stack.as_ref().map(ItemSnapshot::of))
                .collect()
        });
        let data_before = entity.data();
        let mut data = std::mem::replace(&mut entity.state, block_entity::State::Plain);
        let mut view = block_entity::TickView {
            pos,
            state,
            kind,
            items: &mut items,
            data: &mut data,
            recipes: &recipes,
            rng: &mut self.rng,
            game_time,
        };
        let outcome = ticker.tick(&mut view);
        let Some(entity) = self.block_entity_mut(pos) else {
            return;
        };
        entity.items = items;
        entity.state = data;
        let changed_slots: Vec<usize> = before.map_or_else(Vec::new, |before| {
            (0..entity.items.len())
                .filter(|&slot| match (&before[slot], &entity.items[slot]) {
                    (None, None) => false,
                    (Some(old), Some(new)) => old != new,
                    _ => true,
                })
                .collect()
        });
        let data_after = entity.data();
        let changed_data: Vec<(usize, i32)> = data_after
            .iter()
            .enumerate()
            .filter(|(i, v)| data_before.get(*i) != Some(*v))
            .map(|(i, v)| (i, *v))
            .collect();
        if let Some(next) = outcome.set_state {
            self.put_block(pos, next);
        }
        self.broadcast_slots(pos, &changed_slots);
        self.broadcast_data(pos, changed_data);
        self.drop_items(pos, outcome.drops);
        if let Some((event, data)) = outcome.level_event {
            self.level_event(event, pos, data, false);
        }
        if outcome.send_data {
            self.send_block_entity_data(pos);
        }
    }

    /// Whether a hopper that just received its first item should skip a
    /// tick of cooldown because it was fed by a hopper it ticks after.
    pub(crate) fn hopper_cooldown_after_insert(
        &mut self,
        target: BlockPos,
        from: Option<BlockPos>,
        was_empty: bool,
    ) {
        if !was_empty {
            return;
        }
        let skip = from
            .and_then(|from| {
                Some((
                    self.menus.hopper_ticks.get(&target)?,
                    self.menus.hopper_ticks.get(&from)?,
                ))
            })
            .is_some_and(|(target_tick, from_tick)| target_tick >= from_tick);
        if let Some(entity) = self.block_entity_mut(target)
            && entity.kind == ContainerKind::Hopper
            && entity.transfer_cooldown <= block_entity::HOPPER_COOLDOWN
        {
            entity.transfer_cooldown = block_entity::HOPPER_COOLDOWN - skip as i32;
        }
    }

    pub(crate) fn insert_item(
        &mut self,
        target: BlockPos,
        face: Option<Facing>,
        stack: ItemStack,
        by: crate::world::item_port::Porter,
    ) -> crate::world::region_api::Insert {
        use crate::world::region_api::Insert;
        if let Some((kind, parts)) = self.container_at(target) {
            let from = match by {
                crate::world::item_port::Porter::Hopper(pos)
                | crate::world::item_port::Porter::Dropper(pos) => Some(pos),
                _ => None,
            };
            return match self.insert_into(kind, &parts, face, stack, from) {
                None => Insert::Stored,
                Some(rest) => Insert::Refused(rest),
            };
        }
        let Some(state) = self.block(target) else {
            return Insert::Nothing(stack);
        };
        let Some(port) = self.shared.item_ports.get(state.block()).cloned() else {
            return Insert::Nothing(stack);
        };
        let at = crate::world::item_port::PortAccess {
            pos: target,
            state,
            face,
            by,
        };
        let (one, rest) = stack.split(1);
        match (port.offer(self, &at, one), rest) {
            (Ok(()), None) => Insert::Stored,
            (Ok(()), Some(rest)) => Insert::Refused(rest),
            (Err(one), None) => Insert::Refused(one),
            (Err(one), Some(mut rest)) => match rest.join(one) {
                Ok(()) => Insert::Refused(rest),
                Err(one) => {
                    debug_assert!(false, "a refused item no longer fits its stack: {one:?}");
                    self.spawn_item_at(one, target.center(), [0.0; 3], 0);
                    Insert::Refused(rest)
                }
            },
        }
    }

    /// Puts `stack` into the container `parts` (a `kind` container reached
    /// through `face`), broadcasting the changed slots; returns what is left.
    pub(crate) fn insert_into(
        &mut self,
        kind: ContainerKind,
        parts: &[BlockPos],
        face: Option<Facing>,
        stack: ItemStack,
        from: Option<BlockPos>,
    ) -> Option<ItemStack> {
        let mut remaining = Some(stack);
        for part in parts {
            let Some(stack) = remaining.take() else {
                break;
            };
            if !self.owns(*part) {
                remaining = self.insert_far(kind, *part, face, stack);
                continue;
            }
            let mut changed = Vec::new();
            let recipes = self.shared.recipes();
            let observed = self.observes_containers();
            let (rest, was_empty, input_was_empty, before) = {
                let Some(entity) = self.block_entity_mut(*part) else {
                    remaining = Some(stack);
                    continue;
                };
                let was_empty = entity.is_empty();
                let input_was_empty = kind.is_furnace()
                    && entity
                        .items
                        .get(furnace::INPUT)
                        .is_some_and(Option::is_none);
                let before = observed.then(|| {
                    entity
                        .items
                        .iter()
                        .map(|stack| stack.as_ref().map(ItemSnapshot::of))
                        .collect::<Vec<_>>()
                });
                let disabled = entity.crafter().map_or(0, |crafter| crafter.disabled);
                let mut items = std::mem::take(&mut entity.items);
                let rest = block_entity::insert(
                    &recipes,
                    kind,
                    &mut items,
                    face,
                    stack,
                    &mut changed,
                    disabled,
                );
                entity.items = items;
                if let Some(crafter) = entity.crafter_mut() {
                    for slot in &changed {
                        crafter.disabled &= !(1 << slot);
                    }
                }
                (rest, was_empty, input_was_empty, before)
            };
            if !changed.is_empty() {
                self.report_block_slots(*part, before, &changed);
                if input_was_empty && changed.contains(&furnace::INPUT) {
                    self.furnace_input_changed(*part);
                }
                self.hopper_cooldown_after_insert(*part, from, was_empty);
                self.broadcast_slots(*part, &changed);
            }
            remaining = rest;
        }
        remaining
    }

    /// A copper golem `golem` takes from the container at `pos`; its hand
    /// is filled only with what the container gives up.
    pub(crate) fn copper_golem_take(&mut self, golem: i32, pos: BlockPos, stack: &ItemStack) {
        let Some(state) = self.block(pos) else {
            return;
        };
        let parts = self.joined_parts(pos, state);
        self.take_joined(&parts, stack, golem, pos);
    }

    /// Removes up to `stack.count` from the first slot holding `stack`
    /// (same item and components); returns the slot and what was removed.
    pub(crate) fn drain_local(
        &mut self,
        pos: BlockPos,
        stack: &ItemStack,
    ) -> Option<(usize, ItemStack)> {
        if self.menus.container_locks.contains_key(&pos) {
            return None;
        }
        let observed = self.observes_containers();
        let entity = self.block_entity_mut(pos)?;
        let slot = entity
            .items
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.stacks_with(stack)))?;
        let held = &mut entity.items[slot];
        let old = observed
            .then(|| held.as_ref().map(ItemSnapshot::of))
            .flatten();
        let taken = ItemStack::take_from(held, stack.count())?;
        if observed {
            self.report_slot(pos, slot, old);
        }
        self.broadcast_slots(pos, &[slot]);
        Some((slot, taken))
    }

    /// Hands a copper golem what a container gave it from a far half; with
    /// the golem gone
    /// it drops at the container instead.
    pub(crate) fn golem_received(&mut self, golem: i32, at: BlockPos, stack: ItemStack) {
        match self.ents.entities.get_mut(&golem) {
            Some(tracked) => tracked.entity.took_item(stack),
            None => self.spill_stack(at, stack),
        }
    }

    /// Places `stack` into the container at `pos` front to back as a
    /// copper golem does; a part another region owns takes it through its
    /// snapshot like a hopper would, and what finds no room drops on top.
    pub(crate) fn copper_golem_deposit(&mut self, pos: BlockPos, stack: ItemStack) {
        let Some((kind, parts)) = self.container_at(pos) else {
            return;
        };
        let mut rest = Some(stack);
        for part in parts {
            let Some(stack) = rest.take() else {
                break;
            };
            if !self.owns(part) {
                rest = self.insert_far(kind, part, None, stack);
                continue;
            }
            let observed = self.observes_containers();
            let Some(entity) = self.block_entity_mut(part) else {
                rest = Some(stack);
                continue;
            };
            let before = observed.then(|| {
                entity
                    .items
                    .iter()
                    .map(|stack| stack.as_ref().map(ItemSnapshot::of))
                    .collect()
            });
            let mut changed = Vec::new();
            let (left, ended) = block_entity::golem_place(&mut entity.items, stack, &mut changed);
            self.report_block_slots(part, before, &changed);
            self.broadcast_slots(part, &changed);
            rest = left;
            if ended {
                break;
            }
        }
        if let Some(rest) = rest {
            self.drop_items(BlockPos::new(pos.x, pos.y + 1, pos.z), vec![rest]);
        }
    }

    /// Moves one item from the hopper at `pos` into the container it faces.
    pub(crate) fn hopper_push(&mut self, pos: BlockPos, facing: Facing) -> bool {
        let target = placement::step(pos, facing);
        let Some((kind, parts)) = self.container_at(target) else {
            if let Some(pushed) = self.hopper_push_port(pos, target, facing.opposite()) {
                return pushed;
            }
            return self.hopper_push_into_entity(pos, target);
        };
        let face = Some(facing.opposite());
        let full = parts.iter().all(|part| {
            self.part_items(*part)
                .is_some_and(|items| block_entity::is_full(kind, items, face))
        });
        if full {
            return false;
        }
        let size = ContainerKind::Hopper.size();
        for slot in 0..size {
            let Some(one) = self.take_one_out(pos, slot) else {
                continue;
            };
            let shown = self.observes_containers().then(|| ItemSnapshot::of(&one));
            if let Some(back) = self.insert_into(kind, &parts, face, one, Some(pos)) {
                self.return_one(pos, slot, back);
                continue;
            }
            self.took_one(pos, slot, shown.as_deref());
            return true;
        }
        false
    }

    /// Moves one item from the hopper at `pos` into a container entity (a
    /// chest or hopper minecart) in the block `target` it faces.
    pub(super) fn hopper_push_into_entity(&mut self, pos: BlockPos, target: BlockPos) -> bool {
        if let Some(cart) = self.container_entity_at(target) {
            for slot in 0..ContainerKind::Hopper.size() {
                let Some(one) = self.take_one_out(pos, slot) else {
                    continue;
                };
                let shown = self.observes_containers().then(|| ItemSnapshot::of(&one));
                if let Some(back) = self.insert_into_entity(cart, one) {
                    self.return_one(pos, slot, back);
                    continue;
                }
                self.took_one(pos, slot, shown.as_deref());
                return true;
            }
        }
        false
    }

    /// Pulls one item into the hopper at `pos` from the container above it,
    /// or a dropped item lying in its mouth.
    pub(crate) fn hopper_pull(&mut self, pos: BlockPos) -> bool {
        let above = placement::step(pos, Facing::Up);
        if let Some((kind, parts)) = self.container_at(above) {
            let face = Some(Facing::Down);
            for part in parts {
                for slot in kind.slots_for_face(face) {
                    if !self
                        .part_items(part)
                        .and_then(|items| items[slot].as_ref())
                        .is_some_and(|stack| kind.can_extract(slot, stack, face))
                    {
                        continue;
                    }
                    if !self.owns(part) {
                        let accepts = self
                            .part_items(part)
                            .and_then(|items| items[slot].as_ref())
                            .is_some_and(|stack| {
                                self.part_accepts(ContainerKind::Hopper, pos, None, stack, 1)
                            });
                        if !accepts {
                            continue;
                        }
                        self.pull_far(part, slot, pos);
                        return true;
                    }
                    let Some(one) = self.take_one_out(part, slot) else {
                        continue;
                    };
                    let shown = self.observes_containers().then(|| ItemSnapshot::of(&one));
                    if let Some(back) =
                        self.insert_into(ContainerKind::Hopper, &[pos], None, one, None)
                    {
                        self.return_one(part, slot, back);
                        continue;
                    }
                    self.took_one(part, slot, shown.as_deref());
                    return true;
                }
            }
            return false;
        }
        if let Some(pulled) = self.hopper_pull_port(pos, above) {
            return pulled;
        }
        if let Some(cart) = self.container_entity_at(above) {
            return self.extract_from_entity(cart, |region, one| {
                region
                    .insert_into(ContainerKind::Hopper, &[pos], None, one, None)
                    .map_or(Ok(()), Err)
            });
        }
        let blocked = self.block(above).is_some_and(|state| {
            state.is_collision_full_block()
                && !tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::DOES_NOT_BLOCK_HOPPERS,
                    state.block(),
                )
        });
        if blocked {
            return false;
        }
        let suck = [
            pos.x as f64,
            pos.y as f64 + HOPPER_SUCK_MIN_Y,
            pos.z as f64,
            pos.x as f64 + 1.0,
            pos.y as f64 + HOPPER_SUCK_MAX_Y,
            pos.z as f64 + 1.0,
        ];
        for id in self.items_in(suck) {
            let Some(stack) = self.take_item_stack(id) else {
                continue;
            };
            let before = stack.count();
            let rest = self.insert_into(ContainerKind::Hopper, &[pos], None, stack, None);
            let left = rest.as_ref().map_or(0, ItemStack::count);
            self.return_item_stack(id, rest, before);
            if left != before {
                return true;
            }
        }
        false
    }
}

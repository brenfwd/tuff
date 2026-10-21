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
        let Some(entity) = self.block_entity_mut(pos) else {
            return;
        };
        let before = std::mem::take(&mut entity.items);
        let mut items = before.clone();
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
        let changed_slots: Vec<(usize, Option<ItemStack>)> = (0..entity.items.len())
            .filter(|slot| before[*slot] != entity.items[*slot])
            .map(|slot| (slot, entity.items[slot].clone()))
            .collect();
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
        self.broadcast_slots(pos, changed_slots);
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
        if !port.offer(self, &at, &stack.clone().with_count(1)) {
            return Insert::Refused(stack);
        }
        match stack.count {
            0 | 1 => Insert::Stored,
            n => Insert::Refused(stack.with_count(n - 1)),
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
            let (rest, was_empty, slots) = {
                let Some(entity) = self.block_entity_mut(*part) else {
                    remaining = Some(stack);
                    continue;
                };
                let was_empty = entity.is_empty();
                let mut items = entity.items.clone();
                let disabled = entity.crafter().map_or(0, |crafter| crafter.disabled);
                let rest = block_entity::insert(
                    &recipes,
                    kind,
                    &mut items,
                    face,
                    stack,
                    &mut changed,
                    disabled,
                );
                if let Some(crafter) = entity.crafter_mut() {
                    for slot in &changed {
                        crafter.disabled &= !(1 << slot);
                    }
                }
                let slots: Vec<(usize, Option<ItemStack>)> = changed
                    .iter()
                    .map(|slot| (*slot, items[*slot].clone()))
                    .collect();
                (rest, was_empty, slots)
            };
            if !slots.is_empty() {
                for (slot, stack) in &slots {
                    self.set_container_slot(*part, *slot, stack.clone());
                }
                self.hopper_cooldown_after_insert(*part, from, was_empty);
                self.broadcast_slots(*part, slots);
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
        let entity = self.block_entity(pos)?;
        let (slot, mut held) = entity.items.iter().enumerate().find_map(|(slot, s)| {
            Some((slot, s.as_ref().filter(|s| s.stacks_with(stack))?.clone()))
        })?;
        let count = stack.count.min(held.count);
        if count == 0 {
            return None;
        }
        let taken = held.clone().with_count(count);
        held.count -= count;
        let value = (held.count > 0).then_some(held);
        self.write_container_slot(pos, slot, value.clone());
        self.broadcast_slots(pos, vec![(slot, value)]);
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
            let before = observed.then(|| entity.items.clone());
            let mut changed = Vec::new();
            let (left, ended) = block_entity::golem_place(&mut entity.items, stack, &mut changed);
            let slots = changed
                .iter()
                .map(|slot| (*slot, entity.items[*slot].clone()))
                .collect();
            self.report_block_slots(part, before, &changed);
            self.broadcast_slots(part, slots);
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
            let Some(one) = self
                .block_entity(pos)
                .and_then(|entity| entity.items[slot].as_ref())
                .map(|stack| stack.clone().with_count(1))
            else {
                continue;
            };
            if self
                .insert_into(kind, &parts, face, one, Some(pos))
                .is_some()
            {
                continue;
            }
            self.take_one(pos, slot);
            return true;
        }
        false
    }

    /// Moves one item from the hopper at `pos` into a container entity (a
    /// chest or hopper minecart) in the block `target` it faces.
    pub(super) fn hopper_push_into_entity(&mut self, pos: BlockPos, target: BlockPos) -> bool {
        if let Some(cart) = self.container_entity_at(target) {
            for slot in 0..ContainerKind::Hopper.size() {
                let Some(one) = self
                    .block_entity(pos)
                    .and_then(|entity| entity.items[slot].as_ref())
                    .map(|stack| stack.clone().with_count(1))
                else {
                    continue;
                };
                if self.insert_into_entity(cart, one).is_some() {
                    continue;
                }
                self.take_one(pos, slot);
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
                    let Some(one) = self
                        .part_items(part)
                        .and_then(|items| items[slot].as_ref())
                        .filter(|stack| kind.can_extract(slot, stack, face))
                        .map(|stack| stack.clone().with_count(1))
                    else {
                        continue;
                    };
                    if !self.owns(part) {
                        if !self.part_accepts(ContainerKind::Hopper, pos, None, &one) {
                            continue;
                        }
                        self.pull_far(part, slot, pos);
                        return true;
                    }
                    if self
                        .insert_into(ContainerKind::Hopper, &[pos], None, one, None)
                        .is_some()
                    {
                        continue;
                    }
                    self.take_one(part, slot);
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
                    .is_none()
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
        for (id, stack) in self.items_in(suck) {
            let rest = self.insert_into(ContainerKind::Hopper, &[pos], None, stack.clone(), None);
            match rest {
                Some(rest) if rest.count == stack.count => continue,
                Some(rest) => {
                    self.set_item_stack(id, rest);
                    return true;
                }
                None => {
                    self.remove_entity(id, &[]);
                    return true;
                }
            }
        }
        false
    }
}

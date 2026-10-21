//! Container slot writes that features observing containers hear about
//! (`RegionHooks::container_changed`), and the slot edits features make.

use super::RegionActor;
use super::sim::BlockWatchers;
use crate::world::region_api::{ContainerEdit, ContainerHolder, SlotEdit};
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_world_model::BlockPos;

impl RegionActor {
    fn container_watchers(&self) -> &BlockWatchers {
        self.sim.container_watchers.get_or_init(|| {
            self.shared
                .region_hooks
                .iter()
                .filter(|hook| hook.observes_container_changes())
                .cloned()
                .collect()
        })
    }

    /// Whether any feature observes container changes.
    pub(super) fn observes_containers(&self) -> bool {
        !self.container_watchers().is_empty()
    }

    /// Tells the observing features `holder`'s `slots` changed, under the
    /// current cause. Slots that did not change are left out.
    pub(super) fn report_container_edit(&mut self, holder: ContainerHolder, slots: Vec<SlotEdit>) {
        let slots: Vec<SlotEdit> = slots.into_iter().filter(|s| s.old != s.new).collect();
        if slots.is_empty() {
            return;
        }
        let watchers = self.container_watchers().clone();
        let edit = ContainerEdit {
            holder,
            slots,
            cause: self.sim.cause,
        };
        for hook in watchers.iter() {
            hook.container_changed(self, &edit);
        }
    }

    /// Writes one slot of the block container at `pos`, reporting the
    /// change; returns what the slot held, or `Err(stack)` if there is no
    /// container there.
    pub(super) fn write_container_slot(
        &mut self,
        pos: BlockPos,
        slot: usize,
        stack: Option<ItemStack>,
    ) -> Result<Option<ItemStack>, Option<ItemStack>> {
        let observed = self.observes_containers();
        let Some(current) = self
            .block_entity_mut(pos)
            .and_then(|entity| entity.items.get_mut(slot))
        else {
            return Err(stack);
        };
        let old = std::mem::replace(current, stack);
        if observed {
            let edit = SlotEdit::of(slot, old.as_ref(), current.as_ref());
            self.report_container_edit(ContainerHolder::Block(pos), vec![edit]);
        }
        Ok(old)
    }

    /// Takes one item out of `slot` of the block container at `pos` for a
    /// hopper or dropper to move on, telling no one yet: hand it back with
    /// [`return_one`](Self::return_one) when it finds no home, or confirm
    /// the move with [`took_one`](Self::took_one).
    pub(super) fn take_one_out(&mut self, pos: BlockPos, slot: usize) -> Option<ItemStack> {
        let held = self.block_entity_mut(pos)?.items.get_mut(slot)?;
        ItemStack::take_from(held, 1)
    }

    /// Puts back an item [`take_one_out`](Self::take_one_out) took.
    pub(super) fn return_one(&mut self, pos: BlockPos, slot: usize, stack: ItemStack) {
        match self
            .block_entity_mut(pos)
            .and_then(|entity| entity.items.get_mut(slot))
        {
            Some(held) => ItemStack::put_back(held, stack),
            None => self.spill_stack(pos, stack),
        }
    }

    /// Reports slot `slot` of the block container at `pos`, which held
    /// `old`, as changed to what it holds now.
    pub(super) fn report_slot(&mut self, pos: BlockPos, slot: usize, old: Option<ItemSnapshot>) {
        let Some(now) = self
            .block_entity(pos)
            .and_then(|entity| entity.items.get(slot))
        else {
            return;
        };
        let edit = SlotEdit {
            slot,
            old,
            new: now.as_ref().map(ItemSnapshot::of),
        };
        self.report_container_edit(ContainerHolder::Block(pos), vec![edit]);
    }

    /// Reports and shows the item `taken` out of `slot` with
    /// [`take_one_out`](Self::take_one_out) as gone for good.
    /// `taken` is needed only while containers are observed.
    pub(super) fn took_one(&mut self, pos: BlockPos, slot: usize, taken: Option<&ItemStack>) {
        if let Some(taken) = taken.filter(|_| self.observes_containers()) {
            let Some(now) = self
                .block_entity(pos)
                .and_then(|entity| entity.items.get(slot))
            else {
                return;
            };
            let old = ItemSnapshot::of(now.as_ref().unwrap_or(taken))
                .with_count(now.as_ref().map_or(0, ItemStack::count) + taken.count());
            self.report_slot(pos, slot, Some(old));
        }
        self.broadcast_slots(pos, &[slot]);
    }

    /// Reports the slots `changed` of a block container whose items were
    /// `before` and are now at `pos`.
    pub(super) fn report_block_slots(
        &mut self,
        pos: BlockPos,
        before: Option<Vec<Option<ItemSnapshot>>>,
        changed: &[usize],
    ) {
        let Some(before) = before else {
            return;
        };
        let Some(items) = self.block_entity(pos).map(|entity| &entity.items) else {
            return;
        };
        let slots = changed
            .iter()
            .filter_map(|&slot| {
                Some(SlotEdit {
                    slot,
                    old: before.get(slot)?.clone(),
                    new: items.get(slot)?.as_ref().map(ItemSnapshot::of),
                })
            })
            .collect();
        self.report_container_edit(ContainerHolder::Block(pos), slots);
    }

    /// The holder of entity `id`'s container, when containers are observed.
    pub(super) fn entity_holder(&self, id: i32) -> Option<ContainerHolder> {
        if !self.observes_containers() {
            return None;
        }
        let tracked = self.ents.entities.get(&id)?;
        Some(ContainerHolder::entity(tracked.entity.as_ref()))
    }

    /// Changes the slots of a block container in place for a feature; see
    /// `RegionWorld::edit_container`.
    pub(super) fn edit_container_for_feature(
        &mut self,
        pos: BlockPos,
        edit: &mut dyn FnMut(&mut [Option<ItemStack>]),
    ) -> bool {
        if self.menus.container_locks.contains_key(&pos) || !self.owns(pos) {
            return false;
        }
        let observed = self.observes_containers();
        let Some(entity) = self.block_entity_mut(pos) else {
            return false;
        };
        let before: Vec<Option<ItemSnapshot>> = tuff_item::item::snapshot_slots(&entity.items);
        edit(&mut entity.items);
        let changed: Vec<usize> = entity
            .items
            .iter()
            .zip(&before)
            .enumerate()
            .filter(|(_, (now, was))| now.as_ref() != was.as_deref())
            .map(|(slot, _)| slot)
            .collect();
        if changed.is_empty() {
            return true;
        }
        if observed {
            for &slot in &changed {
                self.report_slot(pos, slot, before[slot].clone());
            }
        }
        self.broadcast_slots(pos, &changed);
        true
    }

    /// Changes one slot of a block container in place for a feature; see
    /// `RegionWorld::edit_container_slot`.
    pub(super) fn edit_container_slot_for_feature(
        &mut self,
        pos: BlockPos,
        slot: usize,
        edit: &mut dyn FnMut(&mut Option<ItemStack>),
    ) -> bool {
        if self.menus.container_locks.contains_key(&pos) || !self.owns(pos) {
            return false;
        }
        let observed = self.observes_containers();
        let Some(entity) = self.block_entity_mut(pos) else {
            return false;
        };
        let kind = entity.kind;
        let Some(held) = entity.items.get_mut(slot) else {
            return false;
        };
        let old = held.as_ref().map(ItemSnapshot::of);
        edit(held);
        let same = match (&old, held.as_ref()) {
            (Some(old), Some(new)) => old.stacks_with(new),
            _ => false,
        };
        if observed {
            self.report_slot(pos, slot, old);
        }
        if kind.is_furnace() && slot == crate::world::block_entity::furnace::INPUT && !same {
            self.furnace_input_changed(pos);
        }
        self.broadcast_slots(pos, &[slot]);
        true
    }

    /// Sets slots of a block container for a feature; see
    /// `RegionWorld::set_container_items`.
    pub(super) fn set_container_items_for_plugin(
        &mut self,
        pos: BlockPos,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        if self.menus.container_locks.contains_key(&pos) || !self.owns(pos) {
            return false;
        }
        let Some(size) = self.block_entity(pos).map(|entity| entity.items.len()) else {
            return false;
        };
        let slots: Vec<_> = slots.into_iter().filter(|(slot, _)| *slot < size).collect();
        if slots.is_empty() {
            return false;
        }
        let changed: Vec<usize> = slots.iter().map(|(slot, _)| *slot).collect();
        for (slot, stack) in slots {
            if let Some(displaced) = self.set_container_slot(pos, slot, stack) {
                displaced.burn(tuff_item::item::BurnReason::Plugin);
            }
        }
        self.broadcast_slots(pos, &changed);
        true
    }

    fn entity_by_uuid(&self, uuid: u128) -> Option<i32> {
        self.ents
            .entities
            .iter()
            .find(|(_, tracked)| tracked.entity.base().uuid == uuid)
            .map(|(id, _)| *id)
    }

    pub(super) fn entity_container_items_by_uuid(
        &self,
        uuid: u128,
    ) -> Option<crate::world::entity_slots::ContainerSlots<'_>> {
        let id = self.entity_by_uuid(uuid)?;
        self.ents.entities.get(&id)?.entity.container()
    }

    pub(super) fn entity_held_items_by_uuid(
        &self,
        uuid: u128,
    ) -> Option<Vec<Option<tuff_item::item::ItemView<'_>>>> {
        let id = self.entity_by_uuid(uuid)?;
        self.ents.entities.get(&id)?.entity.held_items()
    }

    /// Sets held slots of entity `uuid` for a feature.
    pub(super) fn set_entity_held_items_for_plugin(
        &mut self,
        uuid: u128,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        let Some(id) = self.entity_by_uuid(uuid) else {
            return false;
        };
        let holder = self.entity_holder(id);
        let Some(entity) = self.ents.entities.get_mut(&id).map(|t| &mut t.entity) else {
            return false;
        };
        let Some(before) = entity.held_items().map(|items| {
            items
                .into_iter()
                .map(|item| item.map(ItemSnapshot::of))
                .collect::<Vec<_>>()
        }) else {
            return false;
        };
        if !entity.set_held_items(slots) {
            return false;
        }
        let after = entity.held_items().unwrap_or_default();
        let edits: Vec<_> = before
            .into_iter()
            .zip(after)
            .enumerate()
            .filter(|(_, (old, new))| old.as_deref() != new.map(|new| new.get()))
            .map(|(slot, (old, new))| SlotEdit {
                slot,
                old,
                new: new.map(ItemSnapshot::of),
            })
            .collect();
        if let Some(holder) = holder {
            self.report_container_edit(holder, edits);
        }
        true
    }

    /// Sets slots of entity `uuid`'s container for a feature.
    pub(super) fn set_entity_container_items_for_plugin(
        &mut self,
        uuid: u128,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        let Some(id) = self.entity_by_uuid(uuid) else {
            return false;
        };
        let holder = self.entity_holder(id);
        let Some(mut items) = self
            .ents
            .entities
            .get_mut(&id)
            .and_then(|tracked| tracked.entity.container_mut())
        else {
            return false;
        };
        let slots: Vec<_> = slots
            .into_iter()
            .filter(|(slot, _)| *slot < items.len())
            .collect();
        if slots.is_empty() {
            return false;
        }
        let mut edits = Vec::with_capacity(slots.len());
        let mut changed = Vec::with_capacity(slots.len());
        for (slot, stack) in slots {
            edits.push(SlotEdit::of(slot, items[slot].as_ref(), stack.as_ref()));
            if let Some(old) = std::mem::replace(&mut items[slot], stack) {
                old.burn(tuff_item::item::BurnReason::Plugin);
            }
            changed.push(slot);
        }
        if let Some(holder) = holder {
            self.report_container_edit(holder, edits);
        }
        self.entity_slots_changed(id, &changed, None);
        true
    }
}

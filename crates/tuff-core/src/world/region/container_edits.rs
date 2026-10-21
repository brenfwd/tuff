//! Container slot writes that features observing containers hear about
//! (`RegionHooks::container_changed`), and the slot edits features make.

use super::RegionActor;
use super::sim::BlockWatchers;
use crate::world::region_api::{ContainerEdit, ContainerHolder, SlotEdit};
use tuff_item::item::ItemStack;
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
    /// change; returns false if there is no container there.
    pub(super) fn write_container_slot(
        &mut self,
        pos: BlockPos,
        slot: usize,
        stack: Option<ItemStack>,
    ) -> bool {
        let observed = self.observes_containers();
        let Some(current) = self
            .block_entity_mut(pos)
            .and_then(|entity| entity.items.get_mut(slot))
        else {
            return false;
        };
        let new = observed.then(|| stack.clone());
        let old = std::mem::replace(current, stack);
        if let Some(new) = new {
            self.report_container_edit(
                ContainerHolder::Block(pos),
                vec![SlotEdit { slot, old, new }],
            );
        }
        true
    }

    /// Takes one item out of `slot` of the block container at `pos` (a
    /// hopper or dropper moving it on) and tells its viewers.
    pub(super) fn take_one(&mut self, pos: BlockPos, slot: usize) {
        let Some(mut rest) = self
            .block_entity(pos)
            .and_then(|entity| entity.items.get(slot)?.clone())
        else {
            return;
        };
        rest.count -= 1;
        let rest = (rest.count > 0).then_some(rest);
        self.write_container_slot(pos, slot, rest.clone());
        self.broadcast_slots(pos, vec![(slot, rest)]);
    }

    /// Reports the slots `changed` of a block container whose items were
    /// `before` and are now at `pos`.
    pub(super) fn report_block_slots(
        &mut self,
        pos: BlockPos,
        before: Option<Vec<Option<ItemStack>>>,
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
                    new: items.get(slot)?.clone(),
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
        for (slot, stack) in &slots {
            self.set_container_slot(pos, *slot, stack.clone());
        }
        self.broadcast_slots(pos, slots);
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
    ) -> Option<&[Option<ItemStack>]> {
        let id = self.entity_by_uuid(uuid)?;
        self.ents.entities.get(&id)?.entity.container()
    }

    pub(super) fn entity_held_items_by_uuid(&self, uuid: u128) -> Option<Vec<Option<ItemStack>>> {
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
        let Some(before) = entity.held_items() else {
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
            .filter(|(_, (old, new))| old != new)
            .map(|(slot, (old, new))| SlotEdit { slot, old, new })
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
        let Some(items) = self
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
        for (slot, stack) in &slots {
            let old = std::mem::replace(&mut items[*slot], stack.clone());
            edits.push(SlotEdit {
                slot: *slot,
                old,
                new: stack.clone(),
            });
        }
        if let Some(holder) = holder {
            self.report_container_edit(holder, edits);
        }
        self.entity_slots_changed(id, slots, None);
        true
    }
}

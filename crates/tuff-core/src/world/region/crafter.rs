//! Crafter block entity plumbing: the result preview for their menu,
//! switched-off slots and the triggered flag. The crafting rule itself is
//! vanilla-redstone's.

use super::RegionActor;
use crate::world::block_entity::{self, CRAFTER_SLOTS, ContainerKind};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

impl RegionActor {
    /// What a crafter's grid would make, for the preview slot of its menu.
    pub(super) fn crafter_preview(&self, pos: BlockPos) -> Option<Option<ItemStack>> {
        let entity = self
            .block_entity(pos)
            .filter(|e| e.kind == ContainerKind::Crafter)?;
        Some(self.shared.recipes().grid_result(&entity.items, 3, 3))
    }

    pub(super) fn crafter_triggered(&mut self, pos: BlockPos, on: bool) {
        let Some(crafter) = self
            .block_entity_mut(pos)
            .and_then(|entity| entity.crafter_mut())
        else {
            return;
        };
        crafter.triggered = on;
        self.broadcast_data(pos, vec![(CRAFTER_SLOTS, i32::from(on))]);
    }

    pub(super) fn set_crafter_slot_state(&mut self, pos: BlockPos, slot: usize, enabled: bool) {
        let Some(entity) = self.block_entity_mut(pos) else {
            return;
        };
        if slot >= CRAFTER_SLOTS || entity.items[slot].is_some() {
            return;
        }
        let Some(crafter) = entity.crafter_mut() else {
            return;
        };
        if enabled {
            crafter.disabled &= !(1 << slot);
        } else {
            crafter.disabled |= 1 << slot;
        }
        self.broadcast_data(pos, vec![(slot, i32::from(!enabled))]);
        self.sim.redstone.container_changes.insert(pos);
    }

    pub(super) fn crafter_signal(&self, pos: BlockPos) -> u8 {
        self.block_entity(pos)
            .map_or(0, block_entity::BlockEntity::crafter_signal)
    }
}

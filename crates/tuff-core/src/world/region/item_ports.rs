//! Hoppers, droppers and hopper minecarts offering items to, and pulling
//! them from, blocks that features register an item port for.

use std::sync::Arc;

use super::RegionActor;
use crate::world::block_entity::ContainerKind;
use crate::world::item_port::{ItemPort, PortAccess, Porter};
use tuff_vanilla_data::block::Facing;
use tuff_world_model::BlockPos;

impl RegionActor {
    fn port_at(
        &self,
        pos: BlockPos,
        face: Facing,
        by: Porter,
    ) -> Option<(Arc<dyn ItemPort>, PortAccess)> {
        let state = self.block(pos)?;
        let port = self.shared.item_ports.get(state.block())?.clone();
        Some((
            port,
            PortAccess {
                pos,
                state,
                face: Some(face),
                by,
            },
        ))
    }

    /// `/item` putting `stack` into the port block at `pos`.
    pub(super) fn replace_port_item(
        &mut self,
        pos: BlockPos,
        stack: Option<tuff_item::item::ItemStack>,
    ) -> bool {
        if !self.owns(pos) {
            return false;
        }
        let Some((port, mut at)) = self.port_at(pos, Facing::Up, Porter::Player(None)) else {
            return false;
        };
        at.face = None;
        port.replace(self, &at, stack)
    }

    /// The hopper at `pos` pushing into a port block at `target`, through
    /// `face` of it: its first accepted item goes in. `None` when `target`
    /// has no port.
    pub(super) fn hopper_push_port(
        &mut self,
        pos: BlockPos,
        target: BlockPos,
        face: Facing,
    ) -> Option<bool> {
        let (port, at) = self.port_at(target, face, Porter::Hopper(pos))?;
        for slot in 0..ContainerKind::Hopper.size() {
            let Some(one) = self.take_one_out(pos, slot) else {
                continue;
            };
            if port.offer(self, &at, &one) {
                self.took_one(pos, slot, Some(&one));
                one.burn(tuff_item::item::BurnReason::Plugin);
                return Some(true);
            }
            self.return_one(pos, slot, one);
        }
        Some(false)
    }

    /// The hopper at `pos` below a port block at `above` takes what the
    /// port shows if it has room. `None` when `above` has no port.
    pub(super) fn hopper_pull_port(&mut self, pos: BlockPos, above: BlockPos) -> Option<bool> {
        let (port, at) = self.port_at(above, Facing::Down, Porter::Hopper(pos))?;
        let Some(record) = port.peek(self, &at) else {
            return Some(false);
        };
        if self
            .insert_into(ContainerKind::Hopper, &[pos], None, record, None)
            .is_some()
        {
            return Some(false);
        }
        port.take(self, &at);
        Some(true)
    }

    /// The hopper minecart `id` pulling from a port block at `above`.
    /// `None` when `above` has no port.
    pub(super) fn cart_pull_port(&mut self, id: i32, above: BlockPos) -> Option<bool> {
        let (port, at) = self.port_at(above, Facing::Down, Porter::Cart(id))?;
        let Some(record) = port.peek(self, &at) else {
            return Some(false);
        };
        if self.insert_into_entity(id, record).is_some() {
            return Some(false);
        }
        port.take(self, &at);
        Some(true)
    }
}

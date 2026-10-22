//! Blocks that take items from, and give items to, hoppers, droppers and
//! hopper minecarts without being modeled containers (jukeboxes,
//! composters, decorated pots). Features register one [`ItemPort`] per
//! block in `Registries::item_ports`.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use crate::world::region_api::RegionWorld;
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_world_model::BlockPos;

/// Who moves the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Porter {
    Hopper(BlockPos),
    Dropper(BlockPos),
    Cart(i32),
    Mob(i32),
    /// A player (by entity id), or a command or dispenser acting as one.
    Player(Option<i32>),
}

pub struct PortAccess {
    pub pos: BlockPos,
    pub state: BlockState,
    /// The face of `pos` the item passes through (`None`: no side, a mob).
    pub face: Option<Facing>,
    pub by: Porter,
}

pub trait ItemPort: Send + Sync {
    /// Keeps `stack` (count 1) if it fits; hands it back otherwise.
    fn offer(
        &self,
        world: &mut dyn RegionWorld,
        at: &PortAccess,
        stack: ItemStack,
    ) -> Result<(), ItemStack>;

    /// What a pull through `at.face` would take, without taking it.
    fn peek(&self, world: &dyn RegionWorld, at: &PortAccess) -> Option<ItemSnapshot>;

    /// Takes what `peek` showed; called only once the puller has room. A
    /// player or mob emptying a block by hand may have the item ejected
    /// into the world by the port instead (`None`).
    fn take(&self, world: &mut dyn RegionWorld, at: &PortAccess) -> Option<ItemStack>;

    /// Puts `stack` in place of whatever the block holds, as `/item` does,
    /// with no check on what fits. False when the port does not support it.
    fn replace(
        &self,
        _world: &mut dyn RegionWorld,
        _at: &PortAccess,
        _stack: Option<ItemStack>,
    ) -> bool {
        false
    }
}

//! Beacons: storing the effects a player picks in the beacon menu.

use crate::world::beacon;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::MobEffect;

use super::RegionActor;
use tuff_world_model::BlockPos;

impl RegionActor {
    pub(super) fn set_beacon_effects(
        &mut self,
        pos: BlockPos,
        primary: Option<MobEffect>,
        secondary: Option<MobEffect>,
    ) {
        if !self.owns(pos) || self.block(pos).is_none_or(|s| s.block() != Block::Beacon) {
            return;
        }
        let Some(nbt) = self.raw_block_entity_mut(pos) else {
            return;
        };
        if !beacon::validate_effects(primary, secondary, beacon::levels(nbt)) {
            return;
        }
        beacon::store_effects(nbt, primary, secondary);
    }
}

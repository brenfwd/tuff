pub mod decor;
pub mod lightning;
pub mod trial;
pub mod vault;
pub mod vehicles;

use tuff_core::world::messages::{ActionOutcome, Place};
use tuff_core::world::region_api::{RegionHooks, RegionWorld, TickPhase};
use tuff_vanilla_data::BlockState;

/// Trial spawners and vaults.
pub struct TrialHooks;

impl RegionHooks for TrialHooks {
    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase == TickPhase::Late {
            trial::tick(world);
            vault::tick(world);
        }
    }

    fn use_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        vault::use_block(world, place, state)
    }
}

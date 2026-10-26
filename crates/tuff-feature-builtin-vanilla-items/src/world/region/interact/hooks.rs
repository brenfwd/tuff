use super::*;

/// Vanilla block and item interactions as region hooks.
pub struct InteractionHooks;

impl tuff_core::world::region_api::RegionHooks for InteractionHooks {
    fn use_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        Interact::use_block(world, place, state)
    }

    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        if place.held == Some(Item::Brush) {
            return Some(ActionOutcome::StartUsing {
                off_hand: place.off_hand,
                item: Item::Brush,
                ticks: tuff_vanilla_common::brushable::BRUSH_USE_TICKS,
            });
        }
        Interact::use_tool_on(world, place, state)
    }

    fn attack_block(&self, world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
        if state.block() == Block::DragonEgg {
            Interact::teleport_dragon_egg(world, pos, state);
        }
    }

    fn play_note(&self, world: &mut dyn RegionWorld, pos: BlockPos) {
        Rest::play_note(world, pos);
    }

    fn use_door(&self, world: &mut dyn RegionWorld, pos: BlockPos, open: bool) {
        Rest::use_door(world, pos, open);
    }

    fn occupy_bed(&self, world: &mut dyn RegionWorld, pos: BlockPos, occupied: bool) {
        Rest::occupy_bed(world, pos, occupied);
    }

    fn leave_bed(
        &self,
        world: &mut dyn RegionWorld,
        player: &PlayerHandle,
        pos: BlockPos,
        yaw: f32,
    ) {
        Rest::leave_bed(world, player.clone(), pos, yaw);
    }

    fn respawn_point(
        &self,
        world: &mut dyn RegionWorld,
        query: &tuff_core::world::region_api::RespawnQuery,
    ) -> Option<Option<RespawnSpot>> {
        Some(Rest::respawn_point(
            world,
            query.pos,
            query.yaw,
            query.pitch,
            query.forced,
            query.consume,
        ))
    }

    fn dispense(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        facing: Facing,
        _origin: [f64; 3],
        stack: &tuff_core::world::region_api::Fired,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        super::super::dispense::dispense(world, pos, facing, stack)
    }

    fn use_bucket(&self, world: &mut dyn RegionWorld, request: &BucketUse) {
        if request.item.spawns_entity().is_some() {
            return;
        }
        Interact::use_bucket(world, request.clone());
    }
}

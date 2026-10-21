//! Vanilla placement, survival and shape rules for core's region, and the
//! scheduled ticks they own: falling blocks, brushable blocks, scaffolding,
//! speleothems, blocks that break once unsupported, and covered paths and
//! farmland.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::world::block_rules::BlockRules;
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_PUSH_ENTITIES};
use tuff_core::world::region_api::RegionWorld;
use tuff_vanilla_common::placement::{Context, Level};
use tuff_vanilla_common::{block_support, brushable, placement, scaffolding, speleothem};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::ChunkShape;

use crate::falling::{self, FallingBlock};

const AIR: BlockState = Block::Air.default_state();

pub struct VanillaBlockRules;

impl BlockRules for VanillaBlockRules {
    fn context(
        &self,
        clicked: BlockPos,
        face: Facing,
        cursor: [f32; 3],
        yaw: f32,
        pitch: f32,
        sneaking: bool,
        item: Item,
        shape: ChunkShape,
        level: Level,
    ) -> Context {
        placement::context(
            clicked, face, cursor, yaw, pitch, sneaking, item, shape, level,
        )
    }

    fn can_place(&self, context: &Context, level: Level) -> bool {
        placement::can_place(context, level)
    }

    fn placement_state(&self, context: &Context, level: Level) -> Option<BlockState> {
        placement::placement_state(context, level)
    }

    fn companions(&self, state: BlockState, pos: BlockPos) -> Option<(BlockPos, BlockState)> {
        placement::companions(state, pos)
    }

    fn partner(&self, state: BlockState, pos: BlockPos) -> Option<BlockPos> {
        placement::partner(state, pos)
    }

    fn can_survive(&self, state: BlockState, pos: BlockPos, level: Level) -> bool {
        placement::can_survive(state, pos, level)
    }

    fn update_shape(
        &self,
        state: BlockState,
        pos: BlockPos,
        direction: Facing,
        neighbour: BlockState,
        level: Level,
    ) -> BlockState {
        placement::update_shape(state, pos, direction, neighbour, level)
    }

    fn shape_tick(
        &self,
        state: BlockState,
        pos: BlockPos,
        direction: Facing,
        level: Level,
    ) -> Option<u32> {
        placement::shape_tick(state, pos, direction, level)
    }

    fn holds_shape_while_ticking(&self, state: BlockState, direction: Facing) -> bool {
        !direction.is_horizontal() && speleothem::is_stalactite(state)
    }

    fn place_tick(&self, block: Block) -> Option<u32> {
        placement::place_tick(block)
    }

    fn ticks_when_covered(&self, block: Block) -> bool {
        placement::ticks_when_covered(block)
    }

    fn reverts_to_base(&self, old: BlockState, new: BlockState) -> bool {
        placement::reverts_to_base(old, new)
    }

    fn places_on_water(&self, item: Item) -> bool {
        placement::places_on_water(item)
    }

    fn scheduled_tick(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        state: BlockState,
    ) -> bool {
        let block = state.block();
        if block.is_falling() {
            falling_block_tick(world, pos, state);
        } else if brushable::is_brushable(block) {
            brushable_tick(world, pos, state);
        } else if scaffolding::is_scaffolding(block) {
            scaffolding_tick(world, pos, state);
        } else if speleothem::is_speleothem(block) {
            speleothem_tick(world, pos, state);
        } else if block_support::breaks_on_tick(block) {
            let unknown = std::cell::Cell::new(false);
            let survives = {
                let level = |p: BlockPos| {
                    world.block(p).unwrap_or_else(|| {
                        unknown.set(true);
                        AIR
                    })
                };
                placement::can_survive(state, pos, &level)
            };
            if !survives && !unknown.get() {
                world.break_by_update(pos, state, true);
            }
        } else if placement::ticks_when_covered(block) {
            let new = {
                let level = |p: BlockPos| world.block(p).unwrap_or(AIR);
                placement::tick_state(state, pos, &level)
            };
            if let Some(new) = new {
                world.set_block_flags(pos, new, UPDATE_ALL | UPDATE_PUSH_ENTITIES);
            }
        } else {
            return false;
        }
        true
    }

    fn anvil_damaged(&self, state: BlockState) -> Option<BlockState> {
        falling::degraded_anvil(state)
    }
}

/// Whether the block under `pos` lets a block there start falling; `None`
/// below the world or where it is not loaded.
fn free_below(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    pos.y >= world.dimension_info().shape.min_y
        && world
            .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
            .is_some_and(falling::is_free)
}

/// A falling block's scheduled check: it starts falling only once the
/// block below is free (air, fire, liquid, or replaceable) and it is not
/// below the world's bottom.
fn falling_block_tick(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    if free_below(world, pos) {
        start_falling(world, pos, state, None, false);
    }
}

/// A brushable block's scheduled tick: brushing decays, then it falls like
/// sand, shattering wherever it lands.
fn brushable_tick(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    brushable::check_reset(world, pos, state);
    let Some(state) = world.block(pos).filter(|s| s.block() == state.block()) else {
        return;
    };
    if !free_below(world, pos) {
        return;
    }
    brushable::forget(world, pos);
    start_falling(world, pos, state, None, true);
}

/// Turns the block at `pos` into a falling block entity, leaving any water
/// it held; `hurt` is the damage per block fallen it deals to what it lands
/// on, and a block that `shatters` leaves no drop wherever it comes down.
fn start_falling(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    hurt: Option<f32>,
    shatters: bool,
) {
    let remain = if state.waterlogged() == Some(true) {
        Block::Water.default_state()
    } else {
        AIR
    };
    world.put_block(pos, remain);
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    let mut entity = FallingBlock::new(id, uuid, pos, state);
    if let Some(amount) = hurt {
        entity.hurts_entities(amount, speleothem::MAX_DAMAGE);
    }
    entity.cancel_drop = shatters;
    world.add_entity(Box::new(entity));
}

/// Scaffolding's stability check: it takes its new distance, breaks when
/// it has just lost support, or falls once already unsupported.
fn scaffolding_tick(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    use scaffolding::{TickOutcome, tick};
    let outcome = {
        let level = |p: BlockPos| world.block(p).unwrap_or(AIR);
        tick(state, pos, &level)
    };
    match outcome {
        TickOutcome::Keep => {}
        TickOutcome::Set(new) => world.set_block_flags(pos, new, UPDATE_ALL),
        TickOutcome::Fall(new) => start_falling(world, pos, new, None, false),
        TickOutcome::Destroy => world.break_by_update(pos, state, true),
    }
}

/// A speleothem that lost its support: a stalagmite breaks, a stalactite
/// falls with the rest of its column below it.
fn speleothem_tick(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    use speleothem::{TickOutcome, tick};
    let outcome = {
        let level = |p: BlockPos| world.block(p).unwrap_or(AIR);
        tick(state, pos, &level)
    };
    match outcome {
        TickOutcome::Break => world.break_by_update(pos, state, true),
        TickOutcome::Fall(column) => {
            for (at, falling, hurt) in column {
                start_falling(world, at, falling, hurt, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_doors_bring_their_upper_half_and_need_ground() {
        let rules = VanillaBlockRules;
        let level = |pos: BlockPos| {
            if pos.y < 0 {
                Block::Stone.default_state()
            } else {
                AIR
            }
        };
        let context = rules.context(
            BlockPos::new(0, -1, 0),
            Facing::Up,
            [0.5, 1.0, 0.5],
            0.0,
            0.0,
            false,
            Item::OakDoor,
            ChunkShape::OVERWORLD,
            &level,
        );
        let door = rules.placement_state(&context, &level).unwrap();
        let (upper, _) = rules.companions(door, context.pos).unwrap();
        assert_eq!(upper, BlockPos::new(0, 1, 0));
        assert!(!rules.can_survive(door, BlockPos::new(0, 100, 0), &level));
        assert!(
            rules.holds_shape_while_ticking(
                Block::PointedDripstone
                    .default_state()
                    .with("vertical_direction", "down")
                    .unwrap(),
                Facing::Up
            )
        );
    }
}

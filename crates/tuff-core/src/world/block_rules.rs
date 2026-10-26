//! The seam block placement and shape rules plug into. Core's region runs
//! the update cascade, scheduled ticks and player placement; which state a
//! block places as, whether it survives, how it reacts to a neighbour and
//! what its scheduled tick does come from the feature that fills
//! [`crate::registry::Registries::block_rules`]. With no feature a placed
//! block takes its default state, survives anywhere and never reacts.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{BlockState, Item};

use crate::world::placement::{Context, Level, step};
use crate::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{ChunkShape, is_air};

pub trait BlockRules: Send + Sync {
    /// A player's click with `item` on `clicked`, with where the block goes
    /// decided.
    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
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
        let mut context =
            Context::at_clicked(clicked, face, cursor, yaw, pitch, sneaking, item, shape);
        if !is_air(level(clicked)) {
            context.replace_clicked = false;
            context.pos = step(clicked, face);
        }
        context
    }

    /// Whether the position `context` fills can take the block at all.
    fn can_place(&self, context: &Context, level: Level) -> bool {
        !context.blocked && (context.replace_clicked || is_air(level(context.pos)))
    }

    /// The state `context` places, or `None` if nothing can be placed.
    fn placement_state(&self, context: &Context, _level: Level) -> Option<BlockState> {
        Some(context.item.block()?.default_state())
    }

    /// The other blocks a placed state brings with it, like a door's
    /// upper half.
    fn companions(&self, _state: BlockState, _pos: BlockPos) -> Option<(BlockPos, BlockState)> {
        None
    }

    /// The part of a two-block structure that goes without drops when
    /// `state` is broken by a player who gets no drops from it.
    fn partner(&self, _state: BlockState, _pos: BlockPos) -> Option<BlockPos> {
        None
    }

    /// Whether `state` can stay at `pos`.
    fn can_survive(&self, _state: BlockState, _pos: BlockPos, _level: Level) -> bool {
        true
    }

    /// How `state` at `pos` reacts to its neighbour in `direction` becoming
    /// `neighbour`: its new state, or what remains if it breaks.
    fn update_shape(
        &self,
        state: BlockState,
        _pos: BlockPos,
        _direction: Facing,
        _neighbour: BlockState,
        _level: Level,
    ) -> BlockState {
        state
    }

    /// The delay of the block tick a shape update of `state` schedules.
    fn shape_tick(
        &self,
        _state: BlockState,
        _pos: BlockPos,
        _direction: Facing,
        _level: Level,
    ) -> Option<u32> {
        None
    }

    /// Whether `state` keeps its shape against a neighbour change from
    /// `direction` while a tick of its own is pending (a stalactite about
    /// to fall).
    fn holds_shape_while_ticking(&self, _state: BlockState, _direction: Facing) -> bool {
        false
    }

    /// The delay of the block tick placing `block` schedules.
    fn place_tick(&self, _block: tuff_vanilla_data::Block) -> Option<u32> {
        None
    }

    /// Whether `block` checks on a tick whether it is still uncovered (a
    /// dirt path, farmland), so loading a chunk reschedules it.
    fn ticks_when_covered(&self, _block: tuff_vanilla_data::Block) -> bool {
        false
    }

    /// Whether `old` becoming `new` is a block turning back into its
    /// full-height base, which lifts whatever stands in it.
    fn reverts_to_base(&self, _old: BlockState, _new: BlockState) -> bool {
        false
    }

    /// Items placed by aiming at a water surface rather than a block.
    fn places_on_water(&self, _item: Item) -> bool {
        false
    }

    /// Runs the scheduled tick of a block these rules tick (falling,
    /// support and cover checks); false when `state` is not one of them.
    fn scheduled_tick(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _state: BlockState,
    ) -> bool {
        false
    }

    /// What an anvil turns into after a damaging use: its next damage
    /// stage, or `None` when it breaks.
    fn anvil_damaged(&self, state: BlockState) -> Option<BlockState> {
        Some(state)
    }
}

/// The rules an inert core uses.
pub struct NoBlockRules;

impl BlockRules for NoBlockRules {}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    #[tuff_core::watchdog]
    #[test]
    fn the_inert_rules_place_default_states_that_never_react() {
        let rules = NoBlockRules;
        let shape = ChunkShape::new(-64, 384);
        let level = |pos: BlockPos| {
            if pos.y < 0 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
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
            shape,
            &level,
        );
        assert_eq!(context.pos, BlockPos::new(0, 0, 0));
        assert!(rules.can_place(&context, &level));
        let door = rules.placement_state(&context, &level).unwrap();
        assert_eq!(door, Block::OakDoor.default_state());
        assert!(rules.companions(door, context.pos).is_none());
        assert!(rules.can_survive(door, BlockPos::new(0, 100, 0), &level));
        assert_eq!(
            rules.update_shape(
                door,
                context.pos,
                Facing::Down,
                Block::Air.default_state(),
                &level
            ),
            door
        );
    }
}

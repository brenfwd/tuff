//! Lightning rods: powered for a moment when lightning strikes them.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{BlockBehavior, Effect, RedstoneLevel, SignalBehavior, TickPriority, UPDATE_ALL};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct LightningRod;

const ACTIVATION_TICKS: u32 = 8;
const LEVEL_EVENT_ELECTRIC_SPARK: i32 = 3002;

fn update_behind(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let back = state.facing().unwrap_or(Facing::Up).opposite();
    level.update_neighbors_at(step(pos, back), state.block());
}

fn powered(state: BlockState) -> bool {
    state.powered() == Some(true)
}

impl BlockBehavior for LightningRod {
    fn struck_by_lightning(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        level.set_block(pos, state.with_powered(true).unwrap_or(state), UPDATE_ALL);
        update_behind(level, pos, state);
        level.schedule_tick(pos, state.block(), ACTIVATION_TICKS, TickPriority::Normal);
        let axis = match state.facing().unwrap_or(Facing::Up) {
            Facing::East | Facing::West => 0,
            Facing::Up | Facing::Down => 1,
            Facing::North | Facing::South => 2,
        };
        level.effect(Effect::LevelEvent {
            event: LEVEL_EVENT_ELECTRIC_SPARK,
            pos,
            data: axis,
        });
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        level.set_block(pos, state.with_powered(false).unwrap_or(state), UPDATE_ALL);
        update_behind(level, pos, state);
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _moved: bool,
    ) {
        if powered(state) {
            update_behind(level, pos, state);
        }
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block()
            && powered(state)
            && !level.has_scheduled_tick(pos, state.block())
        {
            level.schedule_tick(pos, state.block(), ACTIVATION_TICKS, TickPriority::Normal);
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for LightningRod {}

/// Every lightning rod block, bare, weathered or waxed: what the rod
/// behaviour belongs to, whatever data packs put in `#lightning_rods`.
pub const BLOCKS: &[Block] = &[
    Block::LightningRod,
    Block::ExposedLightningRod,
    Block::WeatheredLightningRod,
    Block::OxidizedLightningRod,
    Block::WaxedLightningRod,
    Block::WaxedExposedLightningRod,
    Block::WaxedWeatheredLightningRod,
    Block::WaxedOxidizedLightningRod,
];

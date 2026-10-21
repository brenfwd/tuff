//! Vanilla explosion block rules: wind charge rays, wither skull and TNT
//! minecart sparing, soul fire and what a wind burst toggles.

use rand::Rng;
use tuff_core::world::explosion::{BlockInteraction, Explosion, ExplosionRules, resistance};
use tuff_core::world::game_event::{Context, GameEvent};
use tuff_core::world::region_api::RegionWorld;
use tuff_vanilla_common::placement;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::tag::block::key;
use tuff_vanilla_data::{Block, BlockKind, BlockState};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const WIND_PROOF_RESISTANCE: f32 = 3_600_000.0;
const WITHER_SKULL_CAP: f32 = 0.8;

pub struct VanillaExplosionRules;

/// A wind ray's resistance: only blocks tagged to stop wind charge bursts
/// resist, and they stop it outright.
pub fn wind_resistance(state: BlockState) -> Option<f32> {
    tuff_datapack::tags::is(key::BLOCKS_WIND_CHARGE_EXPLOSIONS, state.block())
        .then_some(WIND_PROOF_RESISTANCE)
}

/// The resistance a dangerous wither skull's blast meets, capping blocks
/// the wither can break.
pub fn soften_for_wither_skull(state: BlockState, resistance: f32) -> f32 {
    let breakable = !is_air(state) && !tuff_datapack::tags::is(key::WITHER_IMMUNE, state.block());
    if breakable {
        resistance.min(WITHER_SKULL_CAP)
    } else {
        resistance
    }
}

fn portal_spared(
    explosion: &Explosion,
    pos: BlockPos,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    explosion.spares_nether_portals
        && block(pos).is_some_and(|state| state.block() == Block::NetherPortal)
}

impl ExplosionRules for VanillaExplosionRules {
    fn ray_resistance(
        &self,
        explosion: &Explosion,
        pos: BlockPos,
        state: BlockState,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> Option<f32> {
        if explosion.blocks.wind().is_some() {
            return wind_resistance(state);
        }
        if portal_spared(explosion, pos, block) {
            return None;
        }
        resistance(state).map(|r| {
            if self.spares(explosion, pos, block) {
                0.0
            } else if explosion.dangerous_wither_skull {
                soften_for_wither_skull(state, r)
            } else {
                r
            }
        })
    }

    fn spares(
        &self,
        explosion: &Explosion,
        pos: BlockPos,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> bool {
        portal_spared(explosion, pos, block)
            || matches!(explosion.blocks, BlockInteraction::SpareRails { .. })
                && [pos, BlockPos::new(pos.x, pos.y + 1, pos.z)]
                    .into_iter()
                    .any(|at| {
                        block(at).is_some_and(|state| state.block().kind() == BlockKind::Rail)
                    })
    }

    fn fire(&self, ground: BlockState) -> Option<BlockState> {
        let fire = if tuff_datapack::tags::is(key::SOUL_FIRE_BASE_BLOCKS, ground.block()) {
            Block::SoulFire
        } else {
            Block::Fire
        };
        Some(fire.default_state())
    }

    fn wind_reached(&self, world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) -> bool {
        if wind_toggle(world, pos, state) {
            return true;
        }
        if state.block() == Block::Bell {
            world.ring_bell(pos, None);
            return true;
        }
        if state.lit() == Some(true)
            && (tuff_datapack::tags::is(key::CANDLES, state.block())
                || tuff_datapack::tags::is(key::CANDLE_CAKES, state.block()))
            && let Some(unlit) = state.with_lit(false)
        {
            world.put_block(pos, unlit);
            world.play_sound(SoundEvent::BlockCandleExtinguish, pos, 1.0, 1.0, None);
            world.game_event(GameEvent::BlockChange, pos.center(), Context::default());
            return true;
        }
        false
    }
}

/// A wind burst opens or shuts an unpowered door (lower half only),
/// trapdoor or fence gate, except the iron ones.
fn wind_toggle(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) -> bool {
    let block = state.block();
    let kind = block.kind();
    if !matches!(
        kind,
        BlockKind::Door | BlockKind::Trapdoor | BlockKind::FenceGate
    ) {
        return false;
    }
    let Some(toggle) = block.toggle() else {
        return false;
    };
    let by_wind =
        kind == BlockKind::FenceGate || !matches!(block, Block::IronDoor | Block::IronTrapdoor);
    let lower = kind != BlockKind::Door || state.get("half") == Some("lower");
    if !by_wind || !lower || state.get("powered") == Some("true") {
        return true;
    }
    let Some(open) = state.open() else {
        return true;
    };
    let Some(next) = state.with_open(!open) else {
        return true;
    };
    world.put_block(pos, next);
    if kind == BlockKind::Door {
        let upper = placement::step(pos, Facing::Up);
        if let Some(top) = world.block(upper)
            && top.block() == block
            && let Some(top_next) = top.with_open(!open)
        {
            world.put_block(upper, top_next);
        }
    }
    let sound = if open {
        toggle.close_sound
    } else {
        toggle.open_sound
    };
    let pitch = world.rng().gen_range(0.9..1.0);
    world.play_sound(sound, pos, 1.0, pitch, None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_dangerous_skull_softens_only_blocks_the_wither_can_break() {
        let stone = Block::Stone.default_state();
        let obsidian = Block::Obsidian.default_state();
        assert_eq!(soften_for_wither_skull(obsidian, 1200.0), 0.8);
        assert_eq!(
            soften_for_wither_skull(Block::Bedrock.default_state(), 3.6e6),
            3.6e6
        );
        assert_eq!(soften_for_wither_skull(stone, 6.0), 0.8);
        assert_eq!(soften_for_wither_skull(stone, 0.5), 0.5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn soul_soil_lights_soul_fire_and_the_inert_rules_light_none() {
        let rules = VanillaExplosionRules;
        assert_eq!(
            rules.fire(Block::SoulSoil.default_state()),
            Some(Block::SoulFire.default_state())
        );
        assert_eq!(
            rules.fire(Block::Stone.default_state()),
            Some(Block::Fire.default_state())
        );
        let inert = tuff_core::world::explosion::NoExplosionRules;
        assert_eq!(inert.fire(Block::Stone.default_state()), None);
    }
}

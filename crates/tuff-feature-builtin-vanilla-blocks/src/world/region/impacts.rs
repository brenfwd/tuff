//! How blocks react to the projectiles that strike them: target blocks,
//! bells, TNT, candles and campfires, amethyst, and the blocks a hard
//! enough hit breaks.

use rand::Rng;
use tuff_core::world::block_ledger::{ClaimReason, Remains, TntOrigin};
use tuff_core::world::entity::ProjectileImpact;
use tuff_core::world::messages::Igniter;
use tuff_core::world::raycast::Hit;
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_NONE};
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_vanilla_common::speleothem;
use tuff_vanilla_data::block::{Axis, Facing};
use tuff_vanilla_data::registry::{CustomStat, EntityType, SoundEvent};
use tuff_vanilla_data::tag::block as block_tag;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

/// A trident must be moving faster than this to snap pointed dripstone.
const SPELEOTHEM_BREAK_SPEED: f64 = 0.6;
/// How high up its side a bell can be struck to ring it.
const BELL_RING_HEIGHT: f64 = 0.812_399_983_406_066_9;

pub struct ProjectileImpacts;

impl RegionHooks for ProjectileImpacts {
    fn projectile_hit_block(
        &self,
        world: &mut dyn RegionWorld,
        hit: Hit,
        impact: &ProjectileImpact,
        state: BlockState,
    ) -> bool {
        let pos = hit.pos;
        let block = state.block();
        let shared = world.shared().clone();
        let interact = impact.may_interact(&shared);
        match block {
            Block::Target => world.target_hit(hit, impact),
            Block::Bell => {
                if bell_struck(state, hit) {
                    world.ring_bell(pos, Some(hit.face));
                    if let Some(owner) = impact.owner.as_ref().filter(|o| o.is_player()) {
                        world.award_stat(owner.id, CustomStat::BellRing, 1);
                    }
                }
            }
            Block::Tnt => {
                if impact.on_fire && interact && shared.rule_bool("tnt_explodes") {
                    let igniter =
                        impact
                            .owner
                            .as_ref()
                            .filter(|o| o.is_player())
                            .map(|o| Igniter {
                                id: o.id,
                                uuid: o.uuid,
                                name: o.name.plain(),
                            });
                    if let Some(broken) = world.claim_block(pos, Remains::Air, ClaimReason::Entity)
                    {
                        world.spawn_primed_tnt(TntOrigin::Block(broken), pos, None, igniter);
                    }
                }
            }
            Block::Campfire | Block::SoulCampfire => {
                if impact.on_fire
                    && interact
                    && state.get("lit") == Some("false")
                    && state.get("waterlogged") == Some("false")
                {
                    light(world, pos, state);
                }
            }
            Block::ChorusFlower => {
                if interact && impact.may_break(&shared) {
                    world.destroy(pos, true);
                }
            }
            Block::DecoratedPot => {
                if interact && impact.may_break(&shared) {
                    if let Some(cracked) = state.with("cracked", "true") {
                        world.set(pos, cracked, UPDATE_NONE);
                    }
                    world.destroy(pos, true);
                }
            }
            Block::AmethystBlock
            | Block::BuddingAmethyst
            | Block::AmethystCluster
            | Block::LargeAmethystBud
            | Block::MediumAmethystBud
            | Block::SmallAmethystBud => {
                let pitch = 0.5 + world.rng().r#gen::<f32>() * 1.2;
                world.play_sound(SoundEvent::BlockAmethystBlockChime, pos, 1.0, pitch, None);
            }
            _ if speleothem::is_speleothem(block) => {
                if interact
                    && impact.may_break(&shared)
                    && impact.kind == EntityType::Trident
                    && impact.speed() > SPELEOTHEM_BREAK_SPEED
                {
                    world.destroy(pos, true);
                }
            }
            _ if tuff_datapack::tags::is(block_tag::key::CANDLES, block)
                || tuff_datapack::tags::is(block_tag::key::CANDLE_CAKES, block) =>
            {
                let lit =
                    state.get("lit") == Some("false") && state.get("waterlogged") != Some("true");
                if impact.on_fire && lit {
                    light(world, pos, state);
                }
            }
            _ => return false,
        }
        true
    }
}

fn light(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    if let Some(lit) = state.with("lit", "true") {
        world.set(pos, lit, UPDATE_ALL);
    }
}

/// Whether a hit on a bell's `face` at `hit` swings it: a side, below the
/// rim, across the way its attachment lets it swing.
fn bell_struck(state: BlockState, hit: Hit) -> bool {
    let height = hit.location[1] - f64::from(hit.pos.y);
    if hit.face.axis() == Axis::Y || height > BELL_RING_HEIGHT {
        return false;
    }
    let facing = state.facing().unwrap_or(Facing::North);
    match state.get("attachment") {
        Some("floor") => facing.axis() == hit.face.axis(),
        Some("single_wall" | "double_wall") => facing.axis() != hit.face.axis(),
        Some("ceiling") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(face: Facing, y: f64) -> Hit {
        Hit {
            pos: BlockPos::new(0, 0, 0),
            face,
            location: [0.5, y, 0.5],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn bells_ring_only_from_the_sides_they_swing_to() {
        let floor: BlockState = "minecraft:bell[attachment=floor,facing=north]"
            .parse()
            .unwrap();
        assert!(bell_struck(floor, hit(Facing::South, 0.5)));
        assert!(!bell_struck(floor, hit(Facing::East, 0.5)));
        assert!(
            !bell_struck(floor, hit(Facing::South, 0.9)),
            "above the rim"
        );
        assert!(!bell_struck(floor, hit(Facing::Up, 0.5)));
        let wall: BlockState = "minecraft:bell[attachment=single_wall,facing=north]"
            .parse()
            .unwrap();
        assert!(bell_struck(wall, hit(Facing::East, 0.5)));
        assert!(!bell_struck(wall, hit(Facing::North, 0.5)));
        let ceiling: BlockState = "minecraft:bell[attachment=ceiling,facing=north]"
            .parse()
            .unwrap();
        assert!(bell_struck(ceiling, hit(Facing::West, 0.2)));
    }
}

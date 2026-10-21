//! What the block a thing lands on does to its fall, and which blocks it
//! can be inside to forget how far it has fallen. Pure functions of block
//! states, shared by players (their home region reports the blocks), mobs
//! and falling blocks.

use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::physics::Aabb;
use tuff_world_model::BlockPos;

/// The multiplier landing on hay or honey applies to fall damage.
const SOFT_LANDING: f32 = 0.2;
/// What landing on an upward pointed dripstone tip adds to the fall.
const STALAGMITE_EXTRA_DISTANCE: f64 = 2.5;
const STALAGMITE_MULTIPLIER: f32 = 2.0;
/// A box counts as inside the blocks it overlaps once shrunk by this much.
const INSIDE_INSET: f64 = 1.0e-5;

/// How a landing after a fall hurts: the effective distance fallen, the
/// multiplier on the damage and the damage type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impact {
    pub distance: f64,
    pub multiplier: f32,
    pub kind: &'static DamageType,
}

impl Impact {
    /// Whole points of damage for a thing with this safe fall distance and
    /// fall damage multiplier attribute.
    pub fn damage(&self, safe_fall_distance: f64, attribute_multiplier: f64) -> i32 {
        let power = self.distance + 1.0e-6 - safe_fall_distance;
        (power * self.multiplier as f64 * attribute_multiplier).floor() as i32
    }
}

/// How landing on `on` (the block under the feet, `None` if unknown) after
/// falling `distance` hurts, or `None` when the block takes the landing
/// entirely (powder snow; slime unless the thing suppresses the bounce).
pub fn impact(on: Option<BlockState>, distance: f64, suppressing_bounce: bool) -> Option<Impact> {
    let plain = |distance, multiplier| Impact {
        distance,
        multiplier,
        kind: &damage_type::FALL,
    };
    let Some(state) = on else {
        return Some(plain(distance, 1.0));
    };
    match state.block() {
        Block::HayBlock | Block::HoneyBlock => Some(plain(distance, SOFT_LANDING)),
        Block::SlimeBlock if suppressing_bounce => Some(plain(distance, 1.0)),
        Block::SlimeBlock | Block::PowderSnow => None,
        Block::PointedDripstone if is_stalagmite_tip(state) => Some(Impact {
            distance: distance + STALAGMITE_EXTRA_DISTANCE,
            multiplier: STALAGMITE_MULTIPLIER,
            kind: &damage_type::STALAGMITE,
        }),
        block => {
            let reduction = block.movement().fall_distance_reduction;
            Some(plain(distance * (1.0 - reduction) as f64, 1.0))
        }
    }
}

/// Whether landing after falling `distance` could hurt on the harshest
/// block there is (a stalagmite tip), for a thing with this safe fall
/// distance: anything shorter is harmless wherever it lands.
pub fn could_hurt(distance: f64, safe_fall_distance: f64) -> bool {
    let worst = Impact {
        distance: distance + STALAGMITE_EXTRA_DISTANCE,
        multiplier: STALAGMITE_MULTIPLIER,
        kind: &damage_type::STALAGMITE,
    };
    distance > 0.0 && worst.damage(safe_fall_distance, 1.0) > 0
}

fn is_stalagmite_tip(state: BlockState) -> bool {
    state.get("vertical_direction") == Some("up") && state.get("thickness") == Some("tip")
}

/// Whether a body whose box is `bounds` and whose feet are in `feet` is
/// inside something that makes it forget its fall: cobwebs, sweet berry
/// bushes and the rest of the fall-resetting tag except climbables (those
/// count only when actually climbing, which the caller checks), or powder
/// snow at the feet.
pub fn resets_fall(bounds: Aabb, block: &dyn Fn(BlockPos) -> Option<BlockState>) -> bool {
    let feet = BlockPos::new(
        ((bounds[0] + bounds[3]) / 2.0).floor() as i32,
        bounds[1].floor() as i32,
        ((bounds[2] + bounds[5]) / 2.0).floor() as i32,
    );
    if block(feet).is_some_and(|s| s.block() == Block::PowderSnow) {
        return true;
    }
    let min = [0, 1, 2].map(|i| (bounds[i] + INSIDE_INSET).floor() as i32);
    let max = [0, 1, 2].map(|i| (bounds[i + 3] - INSIDE_INSET).floor() as i32);
    (min[0]..=max[0]).any(|x| {
        (min[1]..=max[1]).any(|y| {
            (min[2]..=max[2]).any(|z| {
                block(BlockPos::new(x, y, z)).is_some_and(|s| {
                    let b = s.block();
                    tuff_datapack::tags::is(tags::key::FALL_DAMAGE_RESETTING, b)
                        && !tuff_datapack::tags::is(tags::key::CLIMBABLE, b)
                })
            })
        })
    })
}

/// Moves of at least a block in one tick are traced this far for blocks
/// that reset a fall, which a box test at either end would miss.
const RESET_TRACE_MIN: f64 = 1.0;
const RESET_TRACE_MAX: f64 = 8.0;

/// Whether a move of `movement` from the feet at `from` passes through
/// something that resets a fall: a fall-resetting block (any part of its
/// cell), water, or for a player an end portal or gateway (and a nether
/// portal when `instant_nether_portal`).
pub fn move_resets_fall(
    from: [f64; 3],
    movement: [f64; 3],
    player: bool,
    instant_nether_portal: bool,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let length = movement.iter().map(|v| v * v).sum::<f64>().sqrt();
    if length < RESET_TRACE_MIN {
        return false;
    }
    let reach = length.min(RESET_TRACE_MAX) / length;
    let to = [0, 1, 2].map(|i| from[i] + movement[i] * reach);
    let air = Block::Air.default_state();
    let level = |pos: BlockPos| {
        let Some(state) = block(pos) else {
            return air;
        };
        let b = state.block();
        let portal = player
            && (matches!(b, Block::EndPortal | Block::EndGateway)
                || (instant_nether_portal && b == Block::NetherPortal));
        if portal || tuff_datapack::tags::is(tags::key::FALL_DAMAGE_RESETTING, b) {
            return Block::Stone.default_state();
        }
        match crate::world::raycast::fluid(state) {
            Some((false, _, _)) if matches!(b, Block::Water | Block::BubbleColumn) => state,
            Some((false, _, _)) => Block::Water.default_state(),
            _ => air,
        }
    };
    crate::world::raycast::cast(from, to, crate::world::raycast::FluidMode::Any, &level).is_some()
}

/// Whether a body is sliding down the side of the honey block at `pos`:
/// airborne, no higher than the block's top, falling faster than
/// 0.08 a tick (`fall_speed`, the tick's downward movement) and pressed
/// against a side rather than standing over the block.
pub fn slides_on_honey(
    pos: BlockPos,
    position: [f64; 3],
    width: f64,
    fall_speed: f64,
    on_ground: bool,
) -> bool {
    const TOP: f64 = 0.9375;
    const MIN_SPEED: f64 = 0.08;
    const HALF_COLUMN: f64 = 0.4375;
    if on_ground || position[1] > f64::from(pos.y) + TOP - 1.0e-7 || fall_speed <= MIN_SPEED {
        return false;
    }
    let overlap = HALF_COLUMN + width / 2.0;
    let dx = (f64::from(pos.x) + 0.5 - position[0]).abs();
    let dz = (f64::from(pos.z) + 0.5 - position[2]).abs();
    dx + 1.0e-7 > overlap || dz + 1.0e-7 > overlap
}

/// Whether a body with box `bounds` slides down a honey block side it is
/// inside, which resets its fall.
pub fn honey_slide(
    bounds: Aabb,
    position: [f64; 3],
    fall_speed: f64,
    on_ground: bool,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let width = bounds[3] - bounds[0];
    let min = [0, 1, 2].map(|i| (bounds[i] + INSIDE_INSET).floor() as i32);
    let max = [0, 1, 2].map(|i| (bounds[i + 3] - INSIDE_INSET).floor() as i32);
    (min[0]..=max[0]).any(|x| {
        (min[1]..=max[1]).any(|y| {
            (min[2]..=max[2]).any(|z| {
                let pos = BlockPos::new(x, y, z);
                block(pos).is_some_and(|s| s.block() == Block::HoneyBlock)
                    && slides_on_honey(pos, position, width, fall_speed, on_ground)
            })
        })
    })
}

/// How much of its vertical speed a thing landing on `on` keeps, reversed,
/// given its own bounciness; zero when it suppresses bounces, falls slower
/// than one tick of `gravity`, or lands on a bounce-suppressing block.
pub fn bounce_restitution(
    on: Option<BlockState>,
    falling_speed: f64,
    gravity: f64,
    suppressing_bounce: bool,
    bounciness: f64,
) -> f64 {
    let Some(state) = on else {
        return 0.0;
    };
    if falling_speed <= gravity
        || suppressing_bounce
        || tuff_datapack::tags::is(tags::key::SUPPRESSES_BOUNCE, state.block())
    {
        return 0.0;
    }
    bounciness.max(state.block().movement().bounce_restitution as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dripstone(direction: &str, thickness: &str) -> BlockState {
        Block::PointedDripstone
            .default_state()
            .with("vertical_direction", direction)
            .and_then(|s| s.with("thickness", thickness))
            .unwrap()
    }

    fn damage(on: Block, distance: f64) -> Option<i32> {
        impact(Some(on.default_state()), distance, false).map(|i| i.damage(3.0, 1.0))
    }

    #[tuff_core::watchdog]
    #[test]
    fn landing_blocks_scale_the_damage() {
        assert_eq!(damage(Block::Stone, 13.0), Some(10));
        assert_eq!(damage(Block::HayBlock, 13.0), Some(2));
        assert_eq!(damage(Block::HoneyBlock, 13.0), Some(2));
        assert_eq!(damage(Block::RedBed, 13.0), Some(3));
        assert_eq!(damage(Block::SlimeBlock, 13.0), None);
        assert_eq!(damage(Block::PowderSnow, 13.0), None);
        let sneaking = impact(Some(Block::SlimeBlock.default_state()), 13.0, true).unwrap();
        assert_eq!(sneaking.damage(3.0, 1.0), 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stalagmite_tip_adds_to_the_fall() {
        let tip = impact(Some(dripstone("up", "tip")), 2.0, false).unwrap();
        assert_eq!(tip.kind.message_id, damage_type::STALAGMITE.message_id);
        assert_eq!(tip.damage(3.0, 1.0), 3);
        let hanging = impact(Some(dripstone("down", "tip")), 2.0, false).unwrap();
        assert_eq!(hanging.damage(3.0, 1.0), -1);
        let base = impact(Some(dripstone("up", "base")), 2.0, false).unwrap();
        assert_eq!(base.kind.message_id, damage_type::FALL.message_id);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cobwebs_and_powder_snow_reset_but_a_nearby_ladder_does_not() {
        let at = |target: Block| {
            move |pos: BlockPos| {
                Some(if pos == BlockPos::new(0, 65, 0) {
                    target.default_state()
                } else {
                    Block::Air.default_state()
                })
            }
        };
        let body = [0.2, 64.5, 0.2, 0.8, 66.3, 0.8];
        assert!(resets_fall(body, &at(Block::Cobweb)));
        assert!(resets_fall(body, &at(Block::SweetBerryBush)));
        assert!(!resets_fall(body, &at(Block::Ladder)));
        assert!(!resets_fall(body, &at(Block::Stone)));
        let in_snow = [0.2, 65.0, 0.2, 0.8, 66.8, 0.8];
        assert!(resets_fall(in_snow, &at(Block::PowderSnow)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fast_move_through_a_cobweb_or_water_resets_the_fall() {
        let web = BlockPos::new(0, 70, 0);
        let only = |at: BlockPos, b: Block| {
            move |pos: BlockPos| {
                Some(if pos == at {
                    b.default_state()
                } else {
                    Block::Air.default_state()
                })
            }
        };
        let down = [0.0, -3.0, 0.0];
        let from = [0.5, 72.0, 0.5];
        assert!(move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::Cobweb)
        ));
        assert!(move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::Water)
        ));
        assert!(move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::Ladder)
        ));
        assert!(!move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::Stone)
        ));
        assert!(
            !move_resets_fall(
                from,
                [0.0, -0.9, 0.0],
                true,
                false,
                &only(web, Block::Cobweb)
            ),
            "short moves are not traced"
        );
        assert!(move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::EndPortal)
        ));
        assert!(!move_resets_fall(
            from,
            down,
            false,
            false,
            &only(web, Block::EndPortal)
        ));
        assert!(!move_resets_fall(
            from,
            down,
            true,
            false,
            &only(web, Block::NetherPortal)
        ));
        assert!(move_resets_fall(
            from,
            down,
            true,
            true,
            &only(web, Block::NetherPortal)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn honey_slides_only_falling_bodies_pressed_against_a_side() {
        let honey = BlockPos::new(0, 64, 0);
        assert!(slides_on_honey(honey, [1.29, 64.5, 0.5], 0.6, 0.12, false));
        assert!(
            !slides_on_honey(honey, [1.29, 64.5, 0.5], 0.6, 0.05, false),
            "too slow"
        );
        assert!(
            !slides_on_honey(honey, [1.29, 64.5, 0.5], 0.6, 0.12, true),
            "standing"
        );
        assert!(
            !slides_on_honey(honey, [0.5, 64.94, 0.5], 0.6, 0.12, false),
            "over it"
        );
        let at = |pos: BlockPos| {
            Some(if pos == honey {
                Block::HoneyBlock.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let body = [0.99, 64.2, 0.2, 1.59, 66.0, 0.8];
        assert!(honey_slide(body, [1.29, 64.2, 0.5], 0.12, false, &at));
        let away = [1.01, 64.2, 0.2, 1.61, 66.0, 0.8];
        assert!(!honey_slide(away, [1.31, 64.2, 0.5], 0.12, false, &at));
    }

    #[tuff_core::watchdog]
    #[test]
    fn slime_and_beds_bounce_but_honey_and_sneaking_do_not() {
        let on = |b: Block| Some(b.default_state());
        assert_eq!(
            bounce_restitution(on(Block::SlimeBlock), 1.0, 0.08, false, 0.0),
            1.0
        );
        assert_eq!(
            bounce_restitution(on(Block::RedBed), 1.0, 0.08, false, 0.0),
            0.75
        );
        assert_eq!(
            bounce_restitution(on(Block::SlimeBlock), 1.0, 0.08, true, 0.0),
            0.0
        );
        assert_eq!(
            bounce_restitution(on(Block::SlimeBlock), 0.05, 0.08, false, 0.0),
            0.0
        );
        assert_eq!(
            bounce_restitution(on(Block::HoneyBlock), 1.0, 0.08, false, 0.0),
            0.0
        );
        assert_eq!(
            bounce_restitution(on(Block::Stone), 1.0, 0.08, false, 0.0),
            0.0
        );
    }
}

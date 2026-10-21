//! Where a player gets off a bed, vehicle or respawn anchor: a safe spot
//! to stand around it. Pure functions of a block lookup.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{BlockKind, BlockState};

use crate::placement::{self, Level};
use tuff_core::world::physics::{self, BlockLookup};
use tuff_world_model::BlockPos;

/// Whether a player looking along `yaw` faces more toward `direction`
/// than away from it.
pub fn faces_angle(direction: Facing, yaw: f32) -> bool {
    let angle = yaw.to_radians();
    let (x, _, z) = direction.step();
    x as f32 * -angle.sin() + z as f32 * angle.cos() > 0.0
}

/// A block's collision shape, except that ladders, vines and open
/// trapdoors are not stood on when getting up.
pub fn standing_boxes(state: BlockState) -> &'static [[f32; 6]] {
    let climbable = tuff_datapack::tags::is(tags::key::CLIMBABLE, state.block());
    let open_trapdoor = state.block().kind() == BlockKind::Trapdoor && state.open() == Some(true);
    if climbable || open_trapdoor {
        &[]
    } else {
        state.collision_boxes()
    }
}

pub fn top(boxes: &[[f32; 6]]) -> Option<f64> {
    boxes.iter().map(|b| b[4] as f64).reduce(f64::max)
}

/// Whether any block collision box overlaps `bounds`.
pub fn collides(bounds: [f64; 6], level: Level) -> bool {
    let min = [0, 1, 2].map(|i| bounds[i].floor() as i32);
    let max = [0, 1, 2].map(|i| (bounds[i + 3] - 1.0e-7).floor() as i32);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                let pos = BlockPos::new(x, y, z);
                if placement::obstructs(level(pos), pos, bounds) {
                    return true;
                }
            }
        }
    }
    false
}

/// Where a player can get off at the block `pos`, as when getting out of a
/// bed or vehicle: standing on the floor there with room overhead and,
/// with `careful`, nothing that hurts.
pub fn safe_dismount(pos: BlockPos, careful: bool, level: Level) -> Option<[f64; 3]> {
    const HALF_WIDTH: f64 = 0.3;
    const HEIGHT: f64 = 1.8;
    let here = level(pos);
    let below_pos = placement::step(pos, Facing::Down);
    if careful && here.is_dangerous() {
        return None;
    }
    let floor = match top(standing_boxes(here)) {
        Some(top) => top,
        None => {
            let below = top(standing_boxes(level(below_pos)))?;
            if below < 1.0 {
                return None;
            }
            below - 1.0
        }
    };
    if floor.is_nan() || floor >= 1.0 {
        return None;
    }
    if careful && floor <= 0.0 && level(below_pos).is_dangerous() {
        return None;
    }
    let position = [pos.x as f64 + 0.5, pos.y as f64 + floor, pos.z as f64 + 0.5];
    let bounds = [
        position[0] - HALF_WIDTH,
        position[1],
        position[2] - HALF_WIDTH,
        position[0] + HALF_WIDTH,
        position[1] + HEIGHT,
        position[2] + HALF_WIDTH,
    ];
    let above = placement::step(pos, Facing::Up);
    let invalid =
        |p: BlockPos| tuff_datapack::tags::is(tags::key::INVALID_SPAWN_INSIDE, level(p).block());
    if collides(bounds, level) || invalid(pos) || invalid(above) {
        return None;
    }
    Some(position)
}

pub fn offsets_around(forward: Facing, side: Facing) -> Vec<(i32, i32)> {
    let (fx, _, fz) = forward.step();
    let (sx, _, sz) = side.step();
    vec![
        (sx, sz),
        (sx - fx, sz - fz),
        (sx - fx * 2, sz - fz * 2),
        (-fx * 2, -fz * 2),
        (-sx - fx * 2, -sz - fz * 2),
        (-sx - fx, -sz - fz),
        (-sx, -sz),
        (-sx + fx, -sz + fz),
        (fx, fz),
        (sx + fx, sz + fz),
    ]
}

pub fn offsets_above(forward: Facing) -> Vec<(i32, i32)> {
    let (fx, _, fz) = forward.step();
    vec![(0, 0), (-fx, -fz)]
}

pub fn first_dismount(
    pos: BlockPos,
    offsets: &[(i32, i32)],
    careful: bool,
    level: Level,
) -> Option<[f64; 3]> {
    offsets.iter().find_map(|(dx, dz)| {
        safe_dismount(BlockPos::new(pos.x + dx, pos.y, pos.z + dz), careful, level)
    })
}

/// Where a player getting out of the bed whose head is at `pos` stands up,
/// trying the side it looks toward last.
pub fn stand_up_position(
    pos: BlockPos,
    forward: Facing,
    yaw: f32,
    level: Level,
) -> Option<[f64; 3]> {
    let right = forward.clockwise();
    let side = if faces_angle(right, yaw) {
        right.opposite()
    } else {
        right
    };
    let below = placement::step(pos, Facing::Down);
    let bunk = level(below).block().bed().is_some();
    let around = offsets_around(forward, side);
    let above = offsets_above(forward);
    if !bunk {
        let all: Vec<(i32, i32)> = around.iter().chain(&above).copied().collect();
        return first_dismount(pos, &all, true, level)
            .or_else(|| first_dismount(pos, &all, false, level));
    }
    [
        (pos, &around, true),
        (below, &around, true),
        (pos, &above, true),
        (pos, &around, false),
        (below, &around, false),
        (pos, &above, false),
    ]
    .into_iter()
    .find_map(|(at, offsets, careful)| first_dismount(at, offsets, careful, level))
}

/// The ring around a respawn anchor searched for a stand-up spot, at its
/// own level, then one below, then one above.
pub const ANCHOR_RING: [(i32, i32); 8] = [
    (0, -1),
    (-1, 0),
    (0, 1),
    (1, 0),
    (-1, -1),
    (1, -1),
    (-1, 1),
    (1, 1),
];

/// Where a player respawning at the anchor at `pos` stands up: the ring
/// around it at its level, below and above, then straight on top; a pass
/// refusing dangerous floors first, then one allowing them.
pub fn anchor_stand_up_position(pos: BlockPos, level: Level) -> Option<[f64; 3]> {
    let offsets: Vec<BlockPos> = [0, -1, 1]
        .into_iter()
        .flat_map(|dy| {
            ANCHOR_RING
                .iter()
                .map(move |&(dx, dz)| BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz))
        })
        .chain(std::iter::once(BlockPos::new(pos.x, pos.y + 1, pos.z)))
        .collect();
    [true, false].into_iter().find_map(|careful| {
        offsets
            .iter()
            .find_map(|&at| safe_dismount(at, careful, level))
    })
}

/// The yaw that looks from `from` toward the bottom center of `pos`.
pub fn yaw_toward(from: [f64; 3], pos: BlockPos) -> f32 {
    let dx = pos.x as f64 + 0.5 - from[0];
    let dz = pos.z as f64 + 0.5 - from[2];
    let length = (dx * dx + dz * dz).sqrt();
    if length < 1.0e-9 {
        return 0.0;
    }
    let yaw = (dz / length).atan2(dx / length).to_degrees() - 90.0;
    (((yaw + 180.0) % 360.0 + 360.0) % 360.0 - 180.0) as f32
}

/// Standing, crouching and swimming heights of a player, in the order a
/// vehicle tries them when letting it off.
pub const PLAYER_POSE_HEIGHTS: [f64; 3] = [1.8, 1.5, 0.6];

fn lookup_top(block: BlockLookup, pos: BlockPos, non_climbable: bool) -> Option<f64> {
    let state = block(pos);
    if non_climbable
        && let Some(state) = state
        && standing_boxes(state).is_empty()
    {
        return None;
    }
    physics::block_boxes(state, pos)
        .map(|b| b[4] - pos.y as f64)
        .reduce(f64::max)
}

/// The height of the floor in the block at `pos` as a rider stands on it:
/// the top of its collision, else the top of the block below less one;
/// `None` when that is not a floor within the block.
pub fn floor_height(block: BlockLookup, pos: BlockPos, non_climbable: bool) -> Option<f64> {
    let height = match lookup_top(block, pos, non_climbable) {
        Some(top) => top,
        None => {
            let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
            let top = lookup_top(block, below, non_climbable)?;
            if top < 1.0 {
                return None;
            }
            top - 1.0
        }
    };
    (height < 1.0).then_some(height)
}

/// Whether a rider `width` wide and `height` tall fits standing at
/// `position`.
pub fn fits_at(block: BlockLookup, position: [f64; 3], width: f64, height: f64) -> bool {
    let reach = width.min(1.0) / 2.0;
    let bounds = [
        position[0] - reach,
        position[1],
        position[2] - reach,
        position[0] + reach,
        position[1] + height,
        position[2] + reach,
    ];
    physics::no_collision(bounds, block)
}

/// The horizontal offset from a vehicle's center, along the rider's look
/// direction, that just clears a vehicle `vehicle_width` wide.
pub fn escape_vector(vehicle_width: f64, width: f64, yaw: f32) -> [f64; 2] {
    let distance = (vehicle_width + width + 1.0e-5) / 2.0;
    let radians = f64::from(yaw).to_radians();
    let (sin, cos) = (-radians.sin(), radians.cos());
    let max = sin.abs().max(cos.abs());
    [sin * distance / max, cos * distance / max]
}

/// Where a rider leaves a boat-like vehicle: on the far side along its
/// look direction, on the floor at the vehicle's top or one block lower,
/// unless that is water.
pub fn side_dismount(
    center: [f64; 3],
    top: f64,
    vehicle_width: f64,
    width: f64,
    yaw: f32,
    block: BlockLookup,
) -> Option<[f64; 3]> {
    let [dx, dz] = escape_vector(vehicle_width, width, yaw);
    let (x, z) = (center[0] + dx, center[2] + dz);
    let target = BlockPos::new(x.floor() as i32, top.floor() as i32, z.floor() as i32);
    let below = BlockPos::new(target.x, target.y - 1, target.z);
    if block(below).is_some_and(|s| !s.fluid_state().is_empty()) {
        return None;
    }
    let candidates: Vec<[f64; 3]> = [target, below]
        .into_iter()
        .filter_map(|pos| floor_height(block, pos, false).map(|h| [x, pos.y as f64 + h, z]))
        .collect();
    PLAYER_POSE_HEIGHTS.iter().find_map(|&height| {
        candidates
            .iter()
            .find(|c| fits_at(block, **c, width, height))
            .copied()
    })
}

/// Where a rider leaves a vehicle that wades through lava (a strider):
/// the first floor out of lava along the rider's look and up to 45
/// degrees either side, from the vehicle's top down to half a block
/// below its feet; on top of it when there is none.
pub fn lava_dismount(
    center: [f64; 3],
    bounds: [f64; 6],
    width: f64,
    yaw: f32,
    block: BlockLookup,
) -> [f64; 3] {
    let vehicle_width = bounds[3] - bounds[0];
    let (top, bottom) = (bounds[4], bounds[1] - 0.5);
    let mut targets: Vec<BlockPos> = Vec::new();
    for turn in [0.0, -22.5, 22.5, -45.0, 45.0] {
        let [dx, dz] = escape_vector(vehicle_width, width, yaw + turn);
        let (x, z) = (
            (center[0] + dx).floor() as i32,
            (center[2] + dz).floor() as i32,
        );
        let mut y = top;
        let mut pos = BlockPos::new(x, top.floor() as i32, z);
        while y > bottom {
            if !targets.contains(&pos) {
                targets.push(pos);
            }
            pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
            y -= 1.0;
        }
    }
    for pos in targets {
        let lava = block(pos).is_some_and(|s| {
            s.fluid_state()
                .fluid()
                .is_same(tuff_vanilla_data::fluid::Fluid::Lava)
        });
        if lava {
            continue;
        }
        let Some(floor) = floor_height(block, pos, false) else {
            continue;
        };
        let location = [pos.x as f64 + 0.5, pos.y as f64 + floor, pos.z as f64 + 0.5];
        if PLAYER_POSE_HEIGHTS
            .iter()
            .any(|&height| fits_at(block, location, width, height))
        {
            return location;
        }
    }
    [center[0], top, center[2]]
}

/// The block offsets around a vehicle travelling along `forward`, sides
/// first, then diagonals behind and ahead, then straight behind and ahead.
pub fn vehicle_offsets(forward: Facing) -> [(i32, i32); 8] {
    let right = forward.clockwise();
    let left = right.opposite();
    let back = forward.opposite();
    let step = |f: Facing| {
        let (x, _, z) = f.step();
        (x, z)
    };
    let sum = |a: Facing, b: Facing| {
        let (ax, az) = step(a);
        let (bx, bz) = step(b);
        (ax + bx, az + bz)
    };
    [
        step(right),
        step(left),
        sum(back, right),
        sum(back, left),
        sum(forward, right),
        sum(forward, left),
        step(back),
        step(forward),
    ]
}

/// Where a rider leaves a vehicle at block `at` travelling along
/// `forward` (a horizontal facing): the first free floor around it, per
/// pose, at the vehicle's level, one above, then one below.
pub fn ring_dismount(
    at: BlockPos,
    forward: Facing,
    width: f64,
    block: BlockLookup,
) -> Option<[f64; 3]> {
    let offsets = vehicle_offsets(forward);
    let poses: [(f64, &[i32]); 3] = [
        (PLAYER_POSE_HEIGHTS[0], &[0, 1, -1]),
        (PLAYER_POSE_HEIGHTS[1], &[0, 1, -1]),
        (PLAYER_POSE_HEIGHTS[2], &[0, 1]),
    ];
    for (height, ys) in poses {
        for &dy in ys {
            for (dx, dz) in offsets {
                let pos = BlockPos::new(at.x + dx, at.y + dy, at.z + dz);
                let Some(floor) = floor_height(block, pos, true) else {
                    continue;
                };
                let location = [pos.x as f64 + 0.5, pos.y as f64 + floor, pos.z as f64 + 0.5];
                if fits_at(block, location, width, height) {
                    return Some(location);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod vehicle_tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn flat(top_stone: i32, wall: Option<BlockPos>) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            if Some(pos) == wall || pos.y <= top_stone {
                Some(Block::Stone.default_state())
            } else {
                Some(Block::Air.default_state())
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_lets_a_rider_off_on_the_side_it_looks_toward() {
        let level = flat(63, None);
        let at = side_dismount([0.5, 64.0, 0.5], 64.5, 1.4, 0.6, 270.0, &level).unwrap();
        assert!(at[0] > 1.0 && (at[1] - 64.0).abs() < 1.0e-9, "{at:?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_refuses_a_blocked_or_watery_side() {
        let wall = BlockPos::new(1, 64, 0);
        let level = flat(63, Some(wall));
        assert!(side_dismount([0.5, 64.0, 0.5], 64.5, 1.4, 0.6, 270.0, &level).is_none());
        let water = |pos: BlockPos| {
            Some(if pos.y <= 62 {
                Block::Stone.default_state()
            } else if pos.y == 63 {
                Block::Water.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert!(side_dismount([0.5, 64.0, 0.5], 64.5, 1.4, 0.6, 270.0, &water).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cart_lets_a_rider_off_to_the_side_then_around() {
        let level = flat(63, None);
        let at = ring_dismount(BlockPos::new(0, 64, 0), Facing::East, 0.6, &level).unwrap();
        assert_eq!(at, [0.5, 64.0, 1.5]);
        let wall = BlockPos::new(0, 64, 1);
        let blocked = flat(63, Some(wall));
        let at = ring_dismount(BlockPos::new(0, 64, 0), Facing::East, 0.6, &blocked).unwrap();
        assert_eq!(at, [0.5, 64.0, -0.5]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_strider_lets_its_rider_off_on_a_shore_not_into_lava() {
        let level = |pos: BlockPos| {
            Some(if pos.y <= 63 {
                Block::Stone.default_state()
            } else if pos.y == 64 && pos.x <= 0 {
                Block::Lava.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let bounds = [0.05, 64.0, 0.05, 0.95, 65.7, 0.95];
        let shore = lava_dismount([0.5, 64.0, 0.5], bounds, 0.6, 270.0, &level);
        assert_eq!(shore, [1.5, 64.0, 0.5]);
        let stuck = lava_dismount([0.5, 64.0, 0.5], bounds, 0.6, 90.0, &level);
        assert_eq!(stuck, [0.5, 65.7, 0.5], "all lava: on top of it");
    }
}

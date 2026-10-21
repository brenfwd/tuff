//! Sweeping a box along a straight move: which blocks the move touches
//! and whether any of them stops it, the way flying mobs check their way
//! is clear.

use std::collections::HashSet;

use super::physics::{self, Aabb};
use tuff_core::world::entity::BlockLookup;
use tuff_world_model::BlockPos;

const CLIP_EPSILON: f64 = 1.0e-7;
const CORNER_MARGIN: f64 = 9.999_999_747_378_752e-6;

fn floor(v: f64) -> i32 {
    v.floor() as i32
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

fn frac(v: f64) -> f64 {
    v - v.floor()
}

/// The blocks between two corner cells, inclusive.
fn between(a: [i32; 3], b: [i32; 3], out: &mut HashSet<[i32; 3]>) {
    for x in a[0].min(b[0])..=a[0].max(b[0]) {
        for y in a[1].min(b[1])..=a[1].max(b[1]) {
            for z in a[2].min(b[2])..=a[2].max(b[2]) {
                out.insert([x, y, z]);
            }
        }
    }
}

fn cells_of(bounds: Aabb) -> ([i32; 3], [i32; 3]) {
    (
        [floor(bounds[0]), floor(bounds[1]), floor(bounds[2])],
        [floor(bounds[3]), floor(bounds[4]), floor(bounds[5])],
    )
}

/// Where the segment `from` to `to` first enters the box through a face,
/// as a fraction of the segment.
fn clip_fraction(bounds: Aabb, from: [f64; 3], to: [f64; 3]) -> Option<f64> {
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut scale = 1.0;
    let mut hit = false;
    for axis in 0..3 {
        let d = delta[axis];
        if d.abs() <= CLIP_EPSILON {
            continue;
        }
        let face = if d > 0.0 {
            bounds[axis]
        } else {
            bounds[axis + 3]
        };
        let s = (face - from[axis]) / d;
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        let pb = from[b] + s * delta[b];
        let pc = from[c] + s * delta[c];
        if 0.0 < s
            && s < scale
            && bounds[b] - CLIP_EPSILON < pb
            && pb < bounds[b + 3] + CLIP_EPSILON
            && bounds[c] - CLIP_EPSILON < pc
            && pc < bounds[c + 3] + CLIP_EPSILON
        {
            scale = s;
            hit = true;
        }
    }
    hit.then_some(scale)
}

fn furthest_corner(direction: [f64; 3]) -> [i32; 3] {
    let [dx, dy, dz] = direction.map(f64::abs);
    let sx = if direction[0] >= 0.0 { 1 } else { -1 };
    let sy = if direction[1] >= 0.0 { 1 } else { -1 };
    let sz = if direction[2] >= 0.0 { 1 } else { -1 };
    if dx <= dy && dx <= dz {
        [-sx, -sz, sy]
    } else if dy <= dz {
        [sz, -sy, -sx]
    } else {
        [-sy, sx, -sz]
    }
}

/// Every block a box touches while it moves from the position it has to
/// `at_target`, which is the box where the move ends.
pub fn blocks_between(at_target: Aabb, travel: [f64; 3]) -> Vec<BlockPos> {
    let mut blocks: HashSet<[i32; 3]> = HashSet::new();
    let (lo, hi) = cells_of(at_target);
    if travel.iter().map(|c| c * c).sum::<f64>() < 1.0e-10 {
        between(lo, hi, &mut blocks);
        return collect(blocks);
    }
    let at_start = physics::offset(at_target, travel.map(|c| -c));
    let (slo, shi) = cells_of(at_start);
    between(slo, shi, &mut blocks);
    along_travel(&mut blocks, travel, at_target);
    between(lo, hi, &mut blocks);
    collect(blocks)
}

fn collect(blocks: HashSet<[i32; 3]>) -> Vec<BlockPos> {
    blocks
        .into_iter()
        .map(|[x, y, z]| BlockPos::new(x, y, z))
        .collect()
}

fn along_travel(blocks: &mut HashSet<[i32; 3]>, travel: [f64; 3], at_target: Aabb) {
    let size = [
        at_target[3] - at_target[0],
        at_target[4] - at_target[1],
        at_target[5] - at_target[2],
    ];
    let corner_dir = furthest_corner(travel);
    let center = [
        (at_target[0] + at_target[3]) / 2.0,
        (at_target[1] + at_target[4]) / 2.0,
        (at_target[2] + at_target[5]) / 2.0,
    ];
    let to_corner: [f64; 3] =
        std::array::from_fn(|a| center[a] + size[a] * 0.5 * f64::from(corner_dir[a]));
    let from_corner: [f64; 3] = std::array::from_fn(|a| to_corner[a] - travel[a]);
    let mut cell: [i32; 3] = from_corner.map(floor);
    let signs = travel.map(sign);
    let t_delta: [f64; 3] = std::array::from_fn(|a| {
        if signs[a] == 0 {
            f64::MAX
        } else {
            f64::from(signs[a]) / travel[a]
        }
    });
    let mut t: [f64; 3] = std::array::from_fn(|a| {
        let f = frac(from_corner[a]);
        t_delta[a] * if signs[a] > 0 { 1.0 - f } else { f }
    });
    loop {
        if t[0] > 1.0 && t[1] > 1.0 && t[2] > 1.0 {
            return;
        }
        let axis = if t[0] < t[1] {
            if t[0] < t[2] { 0 } else { 2 }
        } else if t[1] < t[2] {
            1
        } else {
            2
        };
        cell[axis] += signs[axis];
        t[axis] += t_delta[axis];
        let cell_box: Aabb = [
            f64::from(cell[0]),
            f64::from(cell[1]),
            f64::from(cell[2]),
            f64::from(cell[0] + 1),
            f64::from(cell[1] + 1),
            f64::from(cell[2] + 1),
        ];
        let Some(scale) = clip_fraction(cell_box, from_corner, to_corner) else {
            continue;
        };
        let hit: [f64; 3] = std::array::from_fn(|a| from_corner[a] + scale * travel[a]);
        let clamped: [f64; 3] = std::array::from_fn(|a| {
            hit[a].clamp(
                f64::from(cell[a]) + CORNER_MARGIN,
                f64::from(cell[a]) + 1.0 - CORNER_MARGIN,
            )
        });
        let opposite: [i32; 3] =
            std::array::from_fn(|a| floor(clamped[a] - size[a] * f64::from(corner_dir[a])));
        between(cell, opposite, blocks);
    }
}

fn contains(bounds: Aabb, p: [f64; 3]) -> bool {
    (0..3).all(|a| bounds[a] <= p[a] && p[a] < bounds[a + 3])
}

/// Whether a box at `start` moved along `travel` meets any of `shapes`:
/// its centre's path against each shape grown by the box's half size.
fn collided_along(start: Aabb, travel: [f64; 3], shapes: &[Aabb]) -> bool {
    let from: [f64; 3] = std::array::from_fn(|a| (start[a] + start[a + 3]) / 2.0);
    let to: [f64; 3] = std::array::from_fn(|a| from[a] + travel[a]);
    shapes.iter().any(|shape| {
        let grown: Aabb = std::array::from_fn(|i| {
            let half = (start[i % 3 + 3] - start[i % 3]) * 0.5 - CLIP_EPSILON;
            if i < 3 {
                shape[i] - half
            } else {
                shape[i] + half
            }
        });
        contains(grown, to) || contains(grown, from) || clip_fraction(grown, from, to).is_some()
    })
}

/// Whether a box at `bounds` can fly `travel` without meeting a block it
/// does not already overlap.
pub fn can_fly(bounds: Aabb, travel: [f64; 3], block: BlockLookup) -> bool {
    let at_target = physics::offset(bounds, travel);
    blocks_between(at_target, travel).into_iter().all(|pos| {
        let cell: Aabb = [
            f64::from(pos.x),
            f64::from(pos.y),
            f64::from(pos.z),
            f64::from(pos.x + 1),
            f64::from(pos.y + 1),
            f64::from(pos.z + 1),
        ];
        if physics::intersects(bounds, cell) {
            return true;
        }
        let shapes: Vec<Aabb> = physics::block_boxes(block(pos), pos).collect();
        !collided_along(bounds, travel, &shapes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn wall_at(x: i32) -> impl Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        move |pos| {
            Some(if pos.x == x {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    const GHAST: Aabb = [-2.0, 100.0, -2.0, 2.0, 104.0, 2.0];

    #[tuff_core::watchdog]
    #[test]
    fn the_blocks_a_move_touches_follow_its_path() {
        let at_target = physics::offset(GHAST, [10.0, 0.0, 0.0]);
        let blocks = blocks_between(at_target, [10.0, 0.0, 0.0]);
        for x in -2..12 {
            assert!(blocks.contains(&BlockPos::new(x, 101, 0)), "x {x}");
        }
        assert!(!blocks.contains(&BlockPos::new(13, 101, 0)));
        assert!(!blocks.contains(&BlockPos::new(0, 110, 0)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wall_in_the_way_stops_a_float_and_one_behind_does_not() {
        let block = wall_at(8);
        assert!(!can_fly(GHAST, [10.0, 0.0, 0.0], &block));
        assert!(can_fly(GHAST, [3.0, 0.0, 0.0], &block));
        assert!(can_fly(GHAST, [-10.0, 0.0, 0.0], &block));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_thin_wall_between_steps_is_still_found() {
        let block = |pos: BlockPos| {
            Some(if pos.x == 5 && pos.y == 102 && pos.z == 0 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert!(!can_fly(GHAST, [12.0, 0.0, 0.0], &block));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_block_the_box_already_overlaps_does_not_pin_it() {
        let inside = |pos: BlockPos| {
            Some(if pos.x == 1 && pos.y == 101 && pos.z == 0 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert!(can_fly(GHAST, [6.0, 0.0, 0.0], &inside));
    }
}

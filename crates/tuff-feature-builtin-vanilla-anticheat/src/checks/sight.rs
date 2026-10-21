//! Lines of sight through the client's copy of the world: a ray against
//! block outline shapes, the shapes a cursor hits.

use tuff_core::world::physics::{self, Aabb};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;

const END_MARGIN: f64 = 1e-4;
/// How far a ray may graze into a blocking shape: rounding and the eye
/// moving between the client's raycast and the packet.
const EDGE_MARGIN: f64 = 0.05;
const FACE_INSET: f64 = 0.1;
const MAX_CELLS: usize = 256;

/// The point of `b` nearest to `p`.
pub fn nearest(p: [f64; 3], b: Aabb) -> [f64; 3] {
    [
        p[0].clamp(b[0], b[3]),
        p[1].clamp(b[1], b[4]),
        p[2].clamp(b[2], b[5]),
    ]
}

pub fn distance(p: [f64; 3], b: Aabb) -> f64 {
    let n = nearest(p, b);
    ((p[0] - n[0]).powi(2) + (p[1] - n[1]).powi(2) + (p[2] - n[2]).powi(2)).sqrt()
}

/// A block's outline boxes in world space; a block without an outline
/// counts as a full cube.
pub fn outline(state: BlockState, pos: BlockPos) -> Vec<Aabb> {
    let shape = physics::Shape::outline(state, pos);
    if shape.boxes.is_empty() {
        physics::block_boxes(None, pos).collect()
    } else {
        shape.placed(pos).collect()
    }
}

/// `b` less the edge margin, never thinner than half its extent.
fn shrink(b: Aabb) -> Aabb {
    let mut out = b;
    for axis in 0..3 {
        let inset = EDGE_MARGIN.min((b[axis + 3] - b[axis]) / 4.0);
        out[axis] += inset;
        out[axis + 3] -= inset;
    }
    out
}

/// Points on face `face` (down, up, north, south, west, east) of `b`:
/// its centre and four points inset from the corners.
pub fn face_points(b: Aabb, face: i8) -> [[f64; 3]; 5] {
    let (axis, positive) = match face {
        0 => (1, false),
        1 => (1, true),
        2 => (2, false),
        3 => (2, true),
        4 => (0, false),
        _ => (0, true),
    };
    let plane = if positive { b[axis + 3] } else { b[axis] };
    let others: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
    let (u, v) = (others[0], others[1]);
    let at = |fu: f64, fv: f64| {
        let mut p = [0.0; 3];
        p[axis] = plane;
        p[u] = b[u] + (b[u + 3] - b[u]) * fu;
        p[v] = b[v] + (b[v + 3] - b[v]) * fv;
        p
    };
    let (lo, hi) = (FACE_INSET, 1.0 - FACE_INSET);
    [at(0.5, 0.5), at(lo, lo), at(lo, hi), at(hi, lo), at(hi, hi)]
}

/// Where along `from + t * delta` (t in 0..=1) the segment enters `b`.
fn entry(from: [f64; 3], delta: [f64; 3], b: Aabb) -> Option<f64> {
    let mut near = 0.0f64;
    let mut far = 1.0f64;
    for axis in 0..3 {
        let (lo, hi) = (b[axis], b[axis + 3]);
        if delta[axis].abs() < 1e-12 {
            if from[axis] <= lo || from[axis] >= hi {
                return None;
            }
            continue;
        }
        let a = (lo - from[axis]) / delta[axis];
        let c = (hi - from[axis]) / delta[axis];
        near = near.max(a.min(c));
        far = far.min(a.max(c));
        if near >= far {
            return None;
        }
    }
    Some(near)
}

/// Whether a block outline other than `skip` stands between `from` and
/// `to`; `None` when the line crosses an unsent chunk. Shapes the eye is
/// already inside do not block.
pub fn blocked(
    world: &ChunkMirror,
    from: [f64; 3],
    to: [f64; 3],
    skip: Option<BlockPos>,
) -> Option<bool> {
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let length = (delta[0].powi(2) + delta[1].powi(2) + delta[2].powi(2)).sqrt();
    if length < 1e-9 {
        return Some(false);
    }
    let limit = 1.0 - END_MARGIN / length;
    let mut cell = [
        from[0].floor() as i32,
        from[1].floor() as i32,
        from[2].floor() as i32,
    ];
    let end = [
        to[0].floor() as i32,
        to[1].floor() as i32,
        to[2].floor() as i32,
    ];
    let mut step = [0i32; 3];
    let mut next = [f64::INFINITY; 3];
    let mut each = [f64::INFINITY; 3];
    for axis in 0..3 {
        if delta[axis] > 0.0 {
            step[axis] = 1;
            next[axis] = (f64::from(cell[axis]) + 1.0 - from[axis]) / delta[axis];
            each[axis] = 1.0 / delta[axis];
        } else if delta[axis] < 0.0 {
            step[axis] = -1;
            next[axis] = (f64::from(cell[axis]) - from[axis]) / delta[axis];
            each[axis] = -1.0 / delta[axis];
        }
    }
    for _ in 0..MAX_CELLS {
        let pos = BlockPos::new(cell[0], cell[1], cell[2]);
        if skip != Some(pos) {
            if !world.in_loaded_chunk(pos) {
                return None;
            }
            if let Some(state) = world.block_at(pos) {
                let hit = physics::Shape::outline(state, pos).placed(pos).any(|b| {
                    let inside = (0..3).all(|a| from[a] > b[a] && from[a] < b[a + 3]);
                    !inside && entry(from, delta, shrink(b)).is_some_and(|t| t < limit)
                });
                if hit {
                    return Some(true);
                }
            }
        }
        if cell == end {
            return Some(false);
        }
        let axis = (0..3)
            .min_by(|a, b| next[*a].total_cmp(&next[*b]))
            .expect("three axes");
        if next[axis] > 1.0 {
            return Some(false);
        }
        cell[axis] += step[axis];
        next[axis] += each[axis];
    }
    Some(false)
}

/// Whether every line from `from` to a sample is blocked; `None` when none
/// is clear and some cross unsent chunks.
pub fn all_blocked(
    world: &ChunkMirror,
    from: [f64; 3],
    samples: &[[f64; 3]],
    skip: Option<BlockPos>,
) -> Option<bool> {
    let mut unknown = false;
    for sample in samples {
        match blocked(world, from, *sample, skip) {
            Some(false) => return Some(false),
            None => unknown = true,
            Some(true) => {}
        }
    }
    if unknown { None } else { Some(true) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn segment_entry_respects_the_end() {
        let cube = [1.0, 0.0, 0.0, 2.0, 1.0, 1.0];
        assert_eq!(
            entry([0.0, 0.5, 0.5], [3.0, 0.0, 0.0], cube),
            Some(1.0 / 3.0)
        );
        assert_eq!(entry([0.0, 0.5, 0.5], [0.5, 0.0, 0.0], cube), None);
        assert_eq!(entry([0.0, 1.5, 0.5], [3.0, 0.0, 0.0], cube), None);
        assert_eq!(distance([0.0, 0.5, 0.5], cube), 1.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn grazing_an_edge_does_not_block_and_faces_are_sampled_on_their_plane() {
        let cube = [1.0, 0.0, 0.0, 2.0, 1.0, 1.0];
        let graze = [0.0, 0.98, 0.5];
        let delta = [3.0, 0.0, 0.0];
        assert!(entry(graze, delta, cube).is_some());
        assert!(entry(graze, delta, shrink(cube)).is_none());
        let slab = [0.0, 0.0, 0.0, 1.0, 0.1, 1.0];
        let thin = shrink(slab);
        assert!(thin[4] > thin[1]);
        let up = face_points(cube, 1);
        assert!(up.iter().all(|p| p[1] == 1.0));
        assert_eq!(up[0], [1.5, 1.0, 0.5]);
        assert!(face_points(cube, 4).iter().all(|p| p[0] == 1.0));
    }
}

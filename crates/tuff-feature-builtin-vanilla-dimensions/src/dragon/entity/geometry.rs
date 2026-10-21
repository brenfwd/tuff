use super::*;

pub(super) fn part_size(part: usize) -> (f64, f64) {
    let (_, width, height) = tuff_vanilla_data::dragon::PARTS[part];
    (f64::from(width), f64::from(height))
}

pub(super) fn part_box(position: [f64; 3], part: usize) -> [f64; 6] {
    let (width, height) = part_size(part);
    let [x, y, z] = position;
    [
        x - width / 2.0,
        y,
        z - width / 2.0,
        x + width / 2.0,
        y + height,
        z + width / 2.0,
    ]
}

pub(super) fn inflate(b: [f64; 6], x: f64, y: f64, z: f64) -> [f64; 6] {
    [b[0] - x, b[1] - y, b[2] - z, b[3] + x, b[4] + y, b[5] + z]
}

pub(super) fn intersects(a: [f64; 6], b: [f64; 6]) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

pub(super) fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1.0e-4 {
        [0.0; 3]
    } else {
        v.map(|c| c / length)
    }
}

pub(super) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(super) fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    tuff_core::world::distance_sq(a, b)
}

/// Whether a block stops things falling through it (the heightmap's
/// motion-blocking test).
pub(super) fn blocks_motion(state: BlockState) -> bool {
    crate::world::portal::blocks_motion(state)
}

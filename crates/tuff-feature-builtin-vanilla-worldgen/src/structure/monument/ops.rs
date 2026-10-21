//! A tiny drawing language: monument parts are tables of box operations in
//! piece-local coordinates, replayed in order.

use super::*;

/// `[x0, y0, z0, x1, y1, z1]`, inclusive.
pub(super) type Cuboid = [i32; 6];

#[derive(Clone, Copy)]
pub(super) enum Op {
    /// Overwrites every block of the box.
    Solid(BlockState, Cuboid),
    /// Water below sea level, air above, keeping ice and water.
    Flood(Cuboid),
    /// Overwrites only plain water.
    Cap(BlockState, Cuboid),
    /// `count` solid boxes, each corner moved by `step` from the last.
    Series(BlockState, Cuboid, Cuboid, i32),
}

pub(super) const fn solid(state: BlockState, b: Cuboid) -> Op {
    Op::Solid(state, b)
}

pub(super) const fn dot(state: BlockState, x: i32, y: i32, z: i32) -> Op {
    Op::Solid(state, [x, y, z, x, y, z])
}

pub(super) const fn flood(b: Cuboid) -> Op {
    Op::Flood(b)
}

pub(super) const fn series(state: BlockState, first: Cuboid, step: Cuboid, count: i32) -> Op {
    Op::Series(state, first, step, count)
}

/// Where a table lands: shifted along x, and optionally mirrored across a
/// span of the given width.
#[derive(Clone, Copy, Default)]
pub(super) struct Place {
    pub shift_x: i32,
    pub mirror_width: Option<i32>,
}

impl Place {
    fn map(self, b: Cuboid) -> Cuboid {
        match self.mirror_width {
            None => [
                b[0] + self.shift_x,
                b[1],
                b[2],
                b[3] + self.shift_x,
                b[4],
                b[5],
            ],
            Some(w) => [
                self.shift_x + w - b[3],
                b[1],
                b[2],
                self.shift_x + w - b[0],
                b[4],
                b[5],
            ],
        }
    }
}

pub(super) fn replay(d: &mut Draw, ops: &[Op]) {
    replay_at(d, ops, Place::default());
}

pub(super) fn replay_at(d: &mut Draw, ops: &[Op], at: Place) {
    for op in ops {
        match *op {
            Op::Solid(state, b) => d.solid(at.map(b), state),
            Op::Flood(b) => d.flood(at.map(b)),
            Op::Cap(state, b) => d.cap(at.map(b), state),
            Op::Series(state, mut b, step, count) => {
                for _ in 0..count {
                    d.solid(at.map(b), state);
                    for (v, s) in b.iter_mut().zip(step) {
                        *v += s;
                    }
                }
            }
        }
    }
}

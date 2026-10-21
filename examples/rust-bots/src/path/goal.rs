// SPDX-License-Identifier: MIT
use tuff_bots_api::control::{Goal, LeafGoal, Vec3};

use super::{Pos, SPRINT_TICKS};
use crate::core::goal_reached;

pub fn node_center((x, y, z): Pos) -> Vec3 {
    Vec3 {
        x: f64::from(x) + 0.5,
        y: f64::from(y),
        z: f64::from(z) + 0.5,
    }
}

pub fn reached(goal: &Goal, at: Pos) -> bool {
    goal_reached(goal, &node_center(at))
}

fn octile(dx: f64, dz: f64) -> f64 {
    let (a, b) = (dx.abs(), dz.abs());
    a.max(b) + (std::f64::consts::SQRT_2 - 1.0) * a.min(b)
}

fn leaf(goal: &LeafGoal, (x, _, z): Pos) -> f64 {
    let (fx, fz) = (f64::from(x) + 0.5, f64::from(z) + 0.5);
    let blocks = match goal {
        LeafGoal::Block(p) => octile(fx - f64::from(p.x) - 0.5, fz - f64::from(p.z) - 0.5),
        LeafGoal::Near(n) => (octile(fx - f64::from(n.pos.x) - 0.5, fz - f64::from(n.pos.z) - 0.5)
            - n.radius)
            .max(0.0),
        LeafGoal::Xz(g) => octile(fx - f64::from(g.x) - 0.5, fz - f64::from(g.z) - 0.5),
        LeafGoal::GetToBlock(p) => {
            (octile(fx - f64::from(p.x) - 0.5, fz - f64::from(p.z) - 0.5) - 1.0).max(0.0)
        }
        LeafGoal::RunAway(r) => {
            let dx = fx - f64::from(r.origin.x) - 0.5;
            let dz = fz - f64::from(r.origin.z) - 0.5;
            (r.distance - dx.hypot(dz)).max(0.0)
        }
        LeafGoal::YLevel(_) => 0.0,
    };
    blocks * SPRINT_TICKS
}

/// A lower bound on the ticks from `at` to the goal: horizontal distance at
/// sprint speed. Vertical distance is not counted, which keeps it admissible.
pub fn heuristic(goal: &Goal, at: Pos) -> f64 {
    match goal {
        Goal::Leaf(l) => leaf(l, at),
        Goal::AnyOf(ls) => ls.iter().map(|l| leaf(l, at)).fold(f64::INFINITY, f64::min),
    }
}

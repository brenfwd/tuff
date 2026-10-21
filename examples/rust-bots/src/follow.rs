// SPDX-License-Identifier: MIT
//! Target tracking for `follow` and chunk ordering for `explore`, free of
//! host calls: both turn the world into the next goal for the executor.

use std::collections::BTreeSet;

use tuff_bots_api::control::{BlockPos, Goal, LeafGoal, NearGoal, Vec3, XzGoal};

/// Longest leg of a long walk; farther destinations are pathed in
/// segments with xz goals.
pub const SEGMENT_BLOCKS: f64 = 48.0;
const REPLAN_MIN_BLOCKS: f64 = 2.0;

pub type Chunk = (i32, i32);

pub fn distance(a: &Vec3, b: &Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tracking {
    InRange,
    Chase(Goal),
}

/// Keeps a bot within range of a moving target: the goal is re-planned
/// only once the target has moved well away from where it was aimed at.
#[derive(Default)]
pub struct Follower {
    planned: Option<BlockPos>,
}

fn block_of(pos: &Vec3) -> BlockPos {
    BlockPos {
        x: pos.x.floor() as i32,
        y: pos.y.floor() as i32,
        z: pos.z.floor() as i32,
    }
}

impl Follower {
    pub fn track(&mut self, me: &Vec3, target: &Vec3, range: f64) -> Tracking {
        if distance(me, target) <= range {
            self.planned = None;
            return Tracking::InRange;
        }
        let at = block_of(target);
        let slack = REPLAN_MIN_BLOCKS.max(range / 2.0);
        let stale = self.planned.as_ref().is_none_or(|p| {
            let (dx, dy, dz) = (p.x - at.x, p.y - at.y, p.z - at.z);
            f64::from(dx * dx + dy * dy + dz * dz).sqrt() > slack
        });
        let pos = match &self.planned {
            Some(p) if !stale => p.clone(),
            _ => at,
        };
        self.planned = Some(pos.clone());
        Tracking::Chase(Goal::Leaf(LeafGoal::Near(NearGoal {
            pos,
            radius: (range * 0.5).max(1.0),
        })))
    }
}

pub fn chunk_of(pos: &Vec3) -> Chunk {
    (
        (pos.x.floor() as i32).div_euclid(16),
        (pos.z.floor() as i32).div_euclid(16),
    )
}

/// Chunk offsets within `radius` chunks of the centre, ring by ring.
pub fn spiral(radius: u32) -> Vec<Chunk> {
    let max = i64::from(radius);
    let mut out = vec![(0, 0)];
    for r in 1..=radius as i32 {
        let mut ring = Vec::new();
        ring.extend((-r..=r).map(|x| (x, -r)));
        ring.extend((-r + 1..=r).map(|z| (r, z)));
        ring.extend((-r..r).rev().map(|x| (x, r)));
        ring.extend((-r + 1..r).rev().map(|z| (-r, z)));
        out.extend(ring.into_iter().filter(|(x, z)| {
            i64::from(*x) * i64::from(*x) + i64::from(*z) * i64::from(*z) <= max * max
        }));
    }
    out
}

/// A goal at most [`SEGMENT_BLOCKS`] from `me` toward the block column
/// `(x, z)`.
pub fn segment_goal(me: &Vec3, x: i32, z: i32) -> Goal {
    let (dx, dz) = (f64::from(x) + 0.5 - me.x, f64::from(z) + 0.5 - me.z);
    let far = dx.hypot(dz);
    let (gx, gz) = if far <= SEGMENT_BLOCKS {
        (x, z)
    } else {
        let k = SEGMENT_BLOCKS / far;
        (
            (me.x + dx * k).floor() as i32,
            (me.z + dz * k).floor() as i32,
        )
    };
    Goal::Leaf(LeafGoal::Xz(XzGoal { x: gx, z: gz }))
}

/// The chunks a bot has been through, remembered across explore runs.
#[derive(Default)]
pub struct Explorer {
    visited: BTreeSet<Chunk>,
    current: Option<Chunk>,
}

impl Explorer {
    pub fn visited(&self) -> usize {
        self.visited.len()
    }

    pub fn visit(&mut self, chunk: Chunk) {
        self.visited.insert(chunk);
    }

    /// Gives up on the chunk being walked to (unreachable).
    pub fn skip_current(&mut self) {
        if let Some(chunk) = self.current.take() {
            self.visited.insert(chunk);
        }
    }

    pub fn forget_target(&mut self) {
        self.current = None;
    }

    /// The next leg toward the nearest unvisited chunk around `origin`,
    /// or `None` once every chunk in the disc has been visited.
    pub fn next(&mut self, me: &Vec3, origin: &BlockPos, radius_blocks: u32) -> Option<Goal> {
        self.visited.insert(chunk_of(me));
        if self.current.is_some_and(|c| self.visited.contains(&c)) {
            self.current = None;
        }
        if self.current.is_none() {
            let centre = chunk_of(&Vec3 {
                x: f64::from(origin.x),
                y: 0.0,
                z: f64::from(origin.z),
            });
            self.current = spiral(radius_blocks.div_ceil(16))
                .into_iter()
                .map(|(dx, dz)| (centre.0 + dx, centre.1 + dz))
                .find(|c| !self.visited.contains(c));
        }
        let (cx, cz) = self.current?;
        Some(segment_goal(me, cx * 16 + 8, cz * 16 + 8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, z: f64) -> Vec3 {
        Vec3 { x, y: 64.0, z }
    }

    #[test]
    fn spiral_visits_the_centre_then_each_ring_without_repeats() {
        let order = spiral(2);
        assert_eq!(order[0], (0, 0));
        assert_eq!(order[1], (-1, -1));
        assert_eq!(spiral(1)[1], (0, -1));
        let ring_of = |c: &Chunk| c.0.abs().max(c.1.abs());
        assert!(order.windows(2).all(|w| ring_of(&w[0]) <= ring_of(&w[1])));
        let unique: BTreeSet<_> = order.iter().collect();
        assert_eq!(unique.len(), order.len());
        assert_eq!(spiral(1).len(), 5);
    }

    #[test]
    fn spiral_is_clipped_to_a_disc() {
        assert!(!spiral(3).contains(&(3, 3)));
        assert!(spiral(3).contains(&(3, 0)));
    }

    #[test]
    fn follow_is_idle_in_range_and_chases_beyond_it() {
        let mut f = Follower::default();
        assert_eq!(
            f.track(&at(0.0, 0.0), &at(2.0, 0.0), 4.0),
            Tracking::InRange
        );
        let Tracking::Chase(Goal::Leaf(LeafGoal::Near(n))) =
            f.track(&at(0.0, 0.0), &at(10.5, 0.5), 4.0)
        else {
            panic!("expected a chase");
        };
        assert_eq!((n.pos.x, n.pos.z), (10, 0));
        assert_eq!(n.radius, 2.0);
    }

    #[test]
    fn follow_keeps_its_goal_until_the_target_moves_away() {
        let mut f = Follower::default();
        let first = f.track(&at(0.0, 0.0), &at(10.5, 0.5), 4.0);
        assert_eq!(f.track(&at(1.0, 0.0), &at(11.5, 0.5), 4.0), first);
        assert_ne!(f.track(&at(1.0, 0.0), &at(14.5, 0.5), 4.0), first);
    }

    #[test]
    fn follow_forgets_the_plan_when_back_in_range() {
        let mut f = Follower::default();
        f.track(&at(0.0, 0.0), &at(10.5, 0.5), 4.0);
        assert_eq!(
            f.track(&at(9.0, 0.0), &at(10.5, 0.5), 4.0),
            Tracking::InRange
        );
        assert!(f.planned.is_none());
    }

    #[test]
    fn far_destinations_are_pathed_in_segments() {
        let Goal::Leaf(LeafGoal::Xz(g)) = segment_goal(&at(0.5, 0.5), 1000, 0) else {
            panic!()
        };
        assert!((47..=49).contains(&g.x) && g.z == 0);
        let Goal::Leaf(LeafGoal::Xz(near)) = segment_goal(&at(0.5, 0.5), 20, 5) else {
            panic!()
        };
        assert_eq!((near.x, near.z), (20, 5));
    }

    #[test]
    fn explore_walks_the_nearest_unvisited_chunk_and_remembers_visits() {
        let mut e = Explorer::default();
        let origin = BlockPos { x: 8, y: 64, z: 8 };
        let Some(Goal::Leaf(LeafGoal::Xz(g))) = e.next(&at(40.0, 40.0), &origin, 16) else {
            panic!()
        };
        assert_eq!((g.x, g.z), (8, 8));
        e.next(&at(8.5, 8.5), &origin, 16);
        let Some(Goal::Leaf(LeafGoal::Xz(g))) = e.next(&at(8.5, 8.5), &origin, 16) else {
            panic!()
        };
        assert_eq!((g.x, g.z), (8, -8));
    }

    #[test]
    fn explore_finishes_when_every_chunk_is_visited() {
        let mut e = Explorer::default();
        let origin = BlockPos { x: 0, y: 64, z: 0 };
        for (dx, dz) in spiral(1) {
            e.visit((dx, dz));
        }
        assert!(e.next(&at(500.0, 500.0), &origin, 16).is_none());
    }

    #[test]
    fn a_skipped_chunk_is_not_tried_again() {
        let mut e = Explorer::default();
        let origin = BlockPos { x: 0, y: 64, z: 0 };
        e.next(&at(100.0, 100.0), &origin, 0);
        e.skip_current();
        assert!(e.next(&at(100.0, 100.0), &origin, 0).is_none());
        assert_eq!(e.visited(), 2);
    }
}

//! Small detours to pick up item drops lying near a moving bot. Free of
//! host calls so the decisions are plain functions.

use tuff_bots_api::control::{BlockPos, Goal, LeafGoal, NearGoal, Vec3};

/// How far from the bot a drop may lie to be worth a detour.
pub const PICKUP_RADIUS: f64 = 4.0;
const MAX_HEIGHT_GAP: f64 = 3.0;
const ARRIVED: f64 = 0.8;
const MAX_TICKS: u32 = 60;
const IGNORE_TICKS: u64 = 200;
/// Occupied main and hotbar slots at which a bot counts as full.
pub const FULL_SLOTS: usize = 36;

type Cell = (i32, i32, i32);

fn cell(p: &Vec3) -> Cell {
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}

fn dist(a: &Vec3, b: &Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}

#[derive(Debug)]
struct Chase {
    at: Vec3,
    ticks: u32,
}

#[derive(Debug, Default)]
pub struct Pickup {
    chase: Option<Chase>,
    ignored: Vec<(Cell, u64)>,
}

impl Pickup {
    pub fn chasing(&self) -> bool {
        self.chase.is_some()
    }

    /// The detour cannot be walked: leave that drop alone for a while.
    pub fn give_up(&mut self, now: u64) {
        if let Some(chase) = self.chase.take() {
            self.ignored.push((cell(&chase.at), now + IGNORE_TICKS));
        }
    }

    pub fn reset(&mut self) {
        self.chase = None;
    }

    /// The goal that detours to a drop this tick, if any. `now` is the
    /// plugin tick; a drop that was reached, or not reached in time, is left
    /// alone for a while.
    pub fn tick(&mut self, now: u64, me: &Vec3, drops: &[Vec3], room: bool) -> Option<Goal> {
        self.ignored.retain(|(_, until)| *until > now);
        if let Some(chase) = &mut self.chase {
            let still = drops.iter().any(|d| dist(d, &chase.at) < 1.0);
            let arrived = dist(me, &chase.at) <= ARRIVED && (me.y - chase.at.y).abs() < 2.0;
            chase.ticks += 1;
            if !still || !room || arrived || chase.ticks > MAX_TICKS {
                if still {
                    self.ignored.push((cell(&chase.at), now + IGNORE_TICKS));
                }
                self.chase = None;
                return None;
            }
            return Some(near(&chase.at));
        }
        if !room {
            return None;
        }
        let best = drops
            .iter()
            .filter(|d| dist(d, me) <= PICKUP_RADIUS && (d.y - me.y).abs() <= MAX_HEIGHT_GAP)
            .filter(|d| !self.ignored.iter().any(|(c, _)| *c == cell(d)))
            .min_by(|a, b| dist(a, me).total_cmp(&dist(b, me)))?;
        let goal = near(best);
        self.chase = Some(Chase {
            at: best.clone(),
            ticks: 0,
        });
        Some(goal)
    }
}

fn near(at: &Vec3) -> Goal {
    let c = cell(at);
    Goal::Leaf(LeafGoal::Near(NearGoal {
        pos: BlockPos {
            x: c.0,
            y: c.1,
            z: c.2,
        },
        radius: 0.5,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, z: f64) -> Vec3 {
        Vec3 { x, y: 64.0, z }
    }

    #[test]
    fn a_near_drop_is_chased_and_a_far_one_is_not() {
        let mut p = Pickup::default();
        assert!(p.tick(0, &v(0.0, 0.0), &[v(9.0, 0.0)], true).is_none());
        assert!(p.tick(1, &v(0.0, 0.0), &[v(3.0, 0.0)], true).is_some());
        assert!(p.chasing());
    }

    #[test]
    fn nothing_is_chased_with_a_full_inventory() {
        let mut p = Pickup::default();
        assert!(p.tick(0, &v(0.0, 0.0), &[v(2.0, 0.0)], false).is_none());
    }

    #[test]
    fn the_nearest_drop_wins() {
        let mut p = Pickup::default();
        let goal = p.tick(0, &v(0.0, 0.0), &[v(3.5, 0.0), v(1.5, 0.0)], true);
        let Some(Goal::Leaf(LeafGoal::Near(n))) = goal else {
            panic!("a near goal");
        };
        assert_eq!(n.pos.x, 1);
    }

    #[test]
    fn an_unreachable_drop_is_given_up_and_then_ignored() {
        let mut p = Pickup::default();
        let drop = [v(3.0, 0.0)];
        assert!(p.tick(0, &v(0.0, 0.0), &drop, true).is_some());
        for t in 1..=MAX_TICKS as u64 {
            assert!(p.tick(t, &v(0.0, 0.0), &drop, true).is_some());
        }
        assert!(p.tick(100, &v(0.0, 0.0), &drop, true).is_none());
        assert!(p.tick(101, &v(0.0, 0.0), &drop, true).is_none());
        assert!(p.tick(400, &v(0.0, 0.0), &drop, true).is_some());
    }

    #[test]
    fn a_picked_up_drop_ends_the_detour() {
        let mut p = Pickup::default();
        assert!(p.tick(0, &v(0.0, 0.0), &[v(3.0, 0.0)], true).is_some());
        assert!(p.tick(1, &v(2.5, 0.0), &[], true).is_none());
        assert!(!p.chasing());
    }
}

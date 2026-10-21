//! The zombie's walk through villages at night, hunting for doors.

use tuff_vanilla_data::registry::PointOfInterestType;
use tuff_vanilla_data::tag::point_of_interest_type::key::VILLAGE;

use super::goal::{Flags, Goal};
use super::physics::block_pos;
use super::raider::is_village;
use super::{MobCore, random_pos};
use tuff_core::world::entity::TickContext;
use tuff_core::world::poi::Occupancy;
use tuff_world_model::BlockPos;

const CLOSE_SECTIONS: i32 = 6;
const POI_SEARCH: i32 = 10;
const VISITED_LIMIT: usize = 15;

#[derive(Debug)]
pub struct MoveThroughVillage {
    speed: f64,
    only_at_night: bool,
    distance_to_poi: f64,
    poi: Option<BlockPos>,
    visited: Vec<BlockPos>,
}

impl MoveThroughVillage {
    pub fn new(speed: f64, only_at_night: bool, distance_to_poi: i32) -> Self {
        Self {
            speed,
            only_at_night,
            distance_to_poi: f64::from(distance_to_poi),
            poi: None,
            visited: Vec::new(),
        }
    }

    fn nearest_unvisited(&self, around: BlockPos, cx: &TickContext) -> Option<BlockPos> {
        cx.pois
            .closest_first(around, POI_SEARCH, Occupancy::IsOccupied, |kind| {
                tuff_datapack::tags::is(VILLAGE, kind)
            })
            .into_iter()
            .map(|(pos, _): (BlockPos, PointOfInterestType)| pos)
            .find(|pos| !self.visited.contains(pos))
    }

    fn within(&self, mob: &MobCore, distance: f64) -> bool {
        self.poi.is_some_and(|p| {
            let center = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ];
            mob.distance_sq(center) < distance * distance
        })
    }
}

impl Goal for MoveThroughVillage {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.visited.len() > VISITED_LIMIT {
            self.visited.remove(0);
        }
        if self.only_at_night && super::goals::bright_outside(cx) {
            return false;
        }
        let here = block_pos(mob.base.position);
        if cx.pois.sections_to_village(here) > CLOSE_SECTIONS {
            return false;
        }
        let Some(land) = random_pos::land_pos_scored(
            mob,
            15,
            7,
            |_, pos, cx| {
                if !is_village(cx.pois, pos) {
                    return f32::NEG_INFINITY;
                }
                self.nearest_unvisited(pos, cx)
                    .map_or(f32::NEG_INFINITY, |poi| {
                        let (dx, dy, dz) = (poi.x - here.x, poi.y - here.y, poi.z - here.z);
                        -((dx * dx + dy * dy + dz * dz) as f32)
                    })
            },
            cx,
        ) else {
            return false;
        };
        let Some(target) = self.nearest_unvisited(block_pos(land), cx) else {
            return false;
        };
        self.poi = Some(target);
        true
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !(mob.navigation.is_done()
            || self.within(mob, f64::from(mob.size.width) + self.distance_to_poi))
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(p) = self.poi else {
            return;
        };
        let target = [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
        if !mob.move_to_position(target, self.speed, cx) {
            let towards = [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5];
            if let Some(step) =
                random_pos::pos_towards_within(mob, 10, 7, towards, std::f64::consts::FRAC_PI_2, cx)
            {
                mob.move_to_position(step, self.speed, cx);
            }
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if (mob.navigation.is_done() || self.within(mob, self.distance_to_poi))
            && let Some(p) = self.poi
        {
            self.visited.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn it_does_nothing_with_no_village_around() {
        with_context(|core, cx| {
            let mut goal = MoveThroughVillage::new(1.0, false, 4);
            assert!(!goal.can_use(core, cx));
            assert!(goal.poi.is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_visited_list_is_capped() {
        with_context(|core, cx| {
            let mut goal = MoveThroughVillage::new(1.0, false, 4);
            goal.visited = (0..20).map(|i| BlockPos::new(i, 0, 0)).collect();
            goal.can_use(core, cx);
            assert_eq!(goal.visited.len(), 20 - 1);
        });
    }
}

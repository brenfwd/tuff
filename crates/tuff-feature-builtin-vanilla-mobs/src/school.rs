//! Schooling fish: a fish with no school joins a nearby leader with room
//! (or leads the loners around it) and swims after it. Who follows whom is
//! region state, since joining a school changes the other fish too.

use std::collections::HashMap;

use rand::Rng;

use super::MobCore;
use super::goal::{Flags, Goal, reduced_delay};
use super::physics;
use tuff_core::world::entity::{EntitySummary, TickContext};

/// Each follower's leader, by entity id.
#[derive(Debug, Default)]
pub struct Schools {
    leaders: HashMap<i32, i32>,
}

impl Schools {
    pub fn leader_of(&self, follower: i32) -> Option<i32> {
        self.leaders.get(&follower).copied()
    }

    pub fn follow(&mut self, follower: i32, leader: i32) {
        self.leaders.insert(follower, leader);
    }

    pub fn stop_following(&mut self, follower: i32) {
        self.leaders.remove(&follower);
    }

    /// Breaks up `leader`'s school.
    pub fn disband(&mut self, leader: i32) {
        self.leaders.retain(|_, l| *l != leader);
    }

    /// The leader and its followers that are still around.
    pub fn size(&self, leader: i32, present: impl Fn(i32) -> bool) -> u32 {
        1 + self
            .leaders
            .iter()
            .filter(|(follower, l)| **l == leader && present(**follower))
            .count() as u32
    }
}

fn alive_fish(cx: &TickContext, id: i32) -> Option<EntitySummary> {
    cx.entities
        .iter()
        .find(|e| e.id == id && e.living.is_some_and(|l| l.alive))
        .copied()
}

/// Whether the fish `id` is in the region now (or is `me`, which ticks
/// outside the entity list).
fn present(cx: &TickContext, me: i32, id: i32) -> bool {
    id == me || alive_fish(cx, id).is_some()
}

/// The leader `mob` follows, if it is still alive nearby.
pub fn leader(mob: &MobCore, cx: &mut TickContext) -> Option<EntitySummary> {
    let leader = cx.state.get::<Schools>().leader_of(mob.base.id)?;
    alive_fish(cx, leader)
}

pub fn is_follower(mob: &MobCore, cx: &mut TickContext) -> bool {
    leader(mob, cx).is_some()
}

fn school_size(id: i32, me: i32, cx: &mut TickContext) -> u32 {
    let ids: Vec<i32> = cx.entities.iter().map(|e| e.id).collect();
    let state = cx.state.get::<Schools>();
    state.size(id, |f| f == me || ids.contains(&f))
}

/// A leader's school falls apart once no fish of its kind is around (a
/// 1 in 200 check each tick, as the game does).
pub fn tick_leader(mob: &MobCore, cx: &mut TickContext) {
    let me = mob.base.id;
    if school_size(me, me, cx) <= 1 || cx.rng.gen_range(0..200) != 1 {
        return;
    }
    let area = physics::inflate(mob.bounds(), [8.0; 3]);
    let neighbours = cx
        .entities
        .iter()
        .filter(|e| e.kind == mob.base.kind && physics::intersects(e.bounds, area))
        .count();
    if neighbours == 0 {
        cx.state.get::<Schools>().disband(me);
    }
}

/// Swims after its school's leader, forming a school first if it has
/// none.
#[derive(Debug)]
pub struct FollowFlockLeader {
    max_school: u32,
    next_start: Option<i32>,
    recalculate: i32,
}

impl FollowFlockLeader {
    pub fn new(max_school: u32) -> Self {
        Self {
            max_school,
            next_start: None,
            recalculate: 0,
        }
    }

    fn next_start_tick(cx: &mut TickContext) -> i32 {
        reduced_delay(200 + cx.rng.gen_range(0..200) % 20)
    }
}

impl Goal for FollowFlockLeader {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let me = mob.base.id;
        let next_start = *self
            .next_start
            .get_or_insert_with(|| Self::next_start_tick(cx));
        if school_size(me, me, cx) > 1 {
            return false;
        }
        if is_follower(mob, cx) {
            return true;
        }
        if next_start > 0 {
            self.next_start = Some(next_start - 1);
            return false;
        }
        self.next_start = Some(Self::next_start_tick(cx));
        let area = physics::inflate(mob.bounds(), [8.0; 3]);
        let mut nearby: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                e.kind == mob.base.kind
                    && e.living.is_some_and(|l| l.alive)
                    && physics::intersects(e.bounds, area)
            })
            .map(|e| e.id)
            .collect();
        nearby.push(me);
        nearby.sort_unstable();
        let max = self.max_school;
        let follows = |id: i32, cx: &mut TickContext| {
            let leader = cx.state.get::<Schools>().leader_of(id);
            leader.is_some_and(|l| present(cx, me, l))
        };
        let can_be_followed = |id: i32, cx: &mut TickContext| {
            let size = school_size(id, me, cx);
            size > 1 && size < max
        };
        let mut candidates = Vec::new();
        for id in nearby {
            if can_be_followed(id, cx) || !follows(id, cx) {
                candidates.push(id);
            }
        }
        let leader = candidates
            .iter()
            .copied()
            .find(|id| can_be_followed(*id, cx))
            .unwrap_or(me);
        let room = max.saturating_sub(school_size(leader, me, cx)) as usize;
        let loners: Vec<i32> = candidates
            .into_iter()
            .filter(|id| !follows(*id, cx))
            .take(room)
            .filter(|id| *id != leader)
            .collect();
        let mut schools = cx.state.get::<Schools>();
        for id in loners {
            schools.follow(id, leader);
        }
        drop(schools);
        is_follower(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        leader(mob, cx).is_some_and(|l| mob.distance_sq(l.position) <= 121.0)
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.recalculate = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        cx.state.get::<Schools>().stop_following(mob.base.id);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.recalculate -= 1;
        if self.recalculate > 0 {
            return;
        }
        self.recalculate = self.adjusted_delay(10);
        if let Some(leader) = leader(mob, cx) {
            mob.move_to_entity(leader.position, 1.0, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_entities;
    use tuff_core::world::entity::LivingSummary;
    use tuff_vanilla_data::registry::EntityType;

    fn cod(id: i32, x: f64) -> EntitySummary {
        EntitySummary {
            id,
            velocity: [0.0; 3],
            yaw: 0.0,
            uuid: id as u128,
            kind: EntityType::Cod,
            position: [x, 64.0, 0.5],
            bounds: [x - 0.25, 64.0, 0.25, x + 0.25, 64.3, 0.75],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 0.2,
                in_water: true,
                seen_as_enemy: true,
                enemy: false,
                health: 20.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_loner_gathers_the_fish_around_it_into_its_school() {
        let others = [cod(10, 2.5), cod(11, 3.5), cod(12, 30.5)];
        with_entities(&others, |core, cx| {
            core.base.kind = EntityType::Cod;
            let mut goal = FollowFlockLeader::new(8);
            goal.next_start = Some(0);
            assert!(!goal.can_use(core, cx), "it leads rather than follows");
            let schools = cx.state.get::<Schools>();
            assert_eq!(schools.leader_of(10), Some(core.base.id));
            assert_eq!(schools.leader_of(11), Some(core.base.id));
            assert_eq!(schools.leader_of(12), None, "too far to join");
            drop(schools);
            assert_eq!(school_size(core.base.id, core.base.id, cx), 3);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fish_joins_a_nearby_school_with_room_and_follows_its_leader() {
        let others = [cod(10, 2.5), cod(11, 3.5)];
        with_entities(&others, |core, cx| {
            core.base.kind = EntityType::Cod;
            cx.state.get::<Schools>().follow(11, 10);
            let mut goal = FollowFlockLeader::new(8);
            goal.next_start = Some(0);
            assert!(goal.can_use(core, cx));
            assert_eq!(leader(core, cx).map(|l| l.id), Some(10));
            goal.stop(core, cx);
            assert!(!is_follower(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_school_counts_its_leader_and_the_followers_still_around() {
        let mut schools = Schools::default();
        schools.follow(2, 1);
        schools.follow(3, 1);
        schools.follow(4, 9);
        assert_eq!(schools.size(1, |_| true), 3);
        assert_eq!(schools.size(1, |id| id != 3), 2, "a gone fish leaves");
        schools.disband(1);
        assert_eq!(schools.size(1, |_| true), 1);
        assert_eq!(schools.leader_of(4), Some(9));
    }
}

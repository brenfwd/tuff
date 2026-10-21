//! Wandering that keeps a villager to its village: strolls lean back
//! toward a village from outside one, and a villager without a home spends
//! the night walking to someone's bed, shuffling about indoors or heading
//! for the nearest village.

use std::collections::HashMap;

use rand::Rng;
use rand::seq::SliceRandom;
use tuff_vanilla_data::registry::PointOfInterestType;

use super::{SPEED, Villager};
use crate::physics::block_pos;
use crate::world::poi::Occupancy;
use crate::{MobCore, raider, random_pos};
use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

const VILLAGE_SECTION_RADIUS: i32 = 2;
const CLOSEST_VILLAGE_TRIES: u32 = 5;
const CLOSEST_VILLAGE_CLOSE: i32 = 4;
const HOME_RANGE: i32 = 48;
const HOME_SEARCH_INTERVAL: u64 = 20;
const HOME_BATCH: u32 = 5;
const HOME_CACHE_TICKS: u64 = 40;
const HOME_OK_DISTANCE_SQ: i64 = 4;

/// What a homeless villager may do at night, with its weight.
#[derive(Debug, Clone, Copy)]
enum Night {
    ClosestHome,
    BrownianIndoors,
    ClosestVillage,
    Nothing,
}

const NIGHT: [(Night, u32); 4] = [
    (Night::ClosestHome, 1),
    (Night::BrownianIndoors, 4),
    (Night::ClosestVillage, 2),
    (Night::Nothing, 2),
];

#[derive(Debug, Default)]
pub struct State {
    /// Beds already tried as a walk target, until when.
    tried_homes: HashMap<BlockPos, u64>,
    last_home_search: u64,
    pub(super) idle_until: u64,
}

/// A random stroll: toward the nearby section closest to a village when
/// outside one, anywhere on land inside one.
pub(super) fn village_stroll(
    core: &mut MobCore,
    cx: &mut TickContext,
    speed: f64,
    horizontal: i32,
    vertical: i32,
) {
    let here = block_pos(core.base.position);
    let toward = if raider::is_village(cx.pois, here) {
        None
    } else {
        raider::toward_village(cx.pois, here, VILLAGE_SECTION_RADIUS)
    };
    let target = match toward {
        Some(village) => random_pos::pos_towards_within(
            core,
            horizontal,
            vertical,
            village,
            std::f64::consts::FRAC_PI_2,
            cx,
        ),
        None => random_pos::land_pos(core, horizontal, vertical, cx),
    };
    if let Some(target) = target {
        core.move_to_position(target, speed, cx);
    }
}

/// Outside a village, walks to a spot nearer one; false inside one.
fn go_to_closest_village(core: &mut MobCore, cx: &mut TickContext, speed: f64) -> bool {
    let here = block_pos(core.base.position);
    if raider::is_village(cx.pois, here) {
        return false;
    }
    let distance = cx.pois.sections_to_village(here);
    let mut target = None;
    for _ in 0..CLOSEST_VILLAGE_TRIES {
        let Some(pos) = random_pos::land_pos_scored(
            core,
            15,
            7,
            |_, pos, cx| -(cx.pois.sections_to_village(pos) as f32),
            cx,
        ) else {
            continue;
        };
        let landed = cx.pois.sections_to_village(block_pos(pos));
        if landed < distance {
            target = Some(pos);
            break;
        }
        if landed == distance {
            target = Some(pos);
        }
    }
    if let Some(target) = target {
        core.move_to_block(block_pos(target), CLOSEST_VILLAGE_CLOSE, speed, cx);
    }
    true
}

/// Under a roof, steps to a neighbouring block that is also covered;
/// false under the open sky.
fn brownian_indoors(core: &mut MobCore, cx: &mut TickContext, speed: f64) -> bool {
    let here = block_pos(core.base.position);
    if core.can_see_sky(here, cx) {
        return false;
    }
    let mut around: Vec<BlockPos> = (-1..=1)
        .flat_map(|x| (-1..=1).flat_map(move |y| (-1..=1).map(move |z| [x, y, z])))
        .map(|[x, y, z]| BlockPos::new(here.x + x, here.y + y, here.z + z))
        .collect();
    around.shuffle(cx.rng);
    let floor = around
        .into_iter()
        .find(|&pos| !core.can_see_sky(pos, cx) && (cx.block)(pos).is_some_and(|s| s.is_solid()));
    if let Some(floor) = floor {
        let above = [
            f64::from(floor.x) + 0.5,
            f64::from(floor.y) + 1.0,
            f64::from(floor.z) + 0.5,
        ];
        core.move_to_position(above, speed, cx);
    }
    true
}

impl Villager {
    /// Walks to the nearest bed it can reach, whoever sleeps there; false
    /// when searched too recently or already beside the closest one.
    fn go_to_closest_home(&mut self, core: &mut MobCore, cx: &mut TickContext, speed: f64) -> bool {
        let state = &mut self.brain.wander;
        if cx.tick < state.last_home_search + HOME_SEARCH_INTERVAL {
            return false;
        }
        let here = block_pos(core.base.position);
        let homes = cx
            .pois
            .closest_first(here, HOME_RANGE, Occupancy::Any, |kind| {
                kind == PointOfInterestType::Home
            });
        let Some(&(closest, _)) = homes.first() else {
            return false;
        };
        let offset = [closest.x - here.x, closest.y - here.y, closest.z - here.z];
        if offset.iter().map(|d| i64::from(*d).pow(2)).sum::<i64>() <= HOME_OK_DISTANCE_SQ {
            return false;
        }
        let searched = cx.tick + cx.rng.gen_range(0..HOME_SEARCH_INTERVAL);
        state.last_home_search = searched;
        let mut tried = 0;
        let batch: Vec<BlockPos> = homes
            .into_iter()
            .map(|(pos, _)| pos)
            .filter(|pos| {
                if state.tried_homes.contains_key(pos) {
                    return false;
                }
                tried += 1;
                if tried >= HOME_BATCH {
                    return false;
                }
                state.tried_homes.insert(*pos, searched + HOME_CACHE_TICKS);
                true
            })
            .collect();
        match core.path_to_any(&batch, 1, cx) {
            Some(path) => {
                core.move_along(Some(path), speed, cx);
            }
            None if tried < HOME_BATCH => {
                state.tried_homes.retain(|_, until| *until >= searched);
            }
            None => {}
        }
        true
    }

    /// A night without a home: tries, in a weighted random order, walking
    /// to the nearest bed, moving about under a roof, heading for a
    /// village, or standing still a moment.
    pub(super) fn homeless_night(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.tick < self.brain.wander.idle_until || !core.navigation.is_done() {
            return;
        }
        let mut order: Vec<(f32, Night)> = NIGHT
            .iter()
            .map(|&(night, weight)| (cx.rng.r#gen::<f32>().powf(1.0 / weight as f32), night))
            .collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, night) in order {
            let started = match night {
                Night::ClosestHome => self.go_to_closest_home(core, cx, SPEED),
                Night::BrownianIndoors => brownian_indoors(core, cx, SPEED),
                Night::ClosestVillage => go_to_closest_village(core, cx, SPEED),
                Night::Nothing => {
                    self.brain.wander.idle_until = cx.tick + cx.rng.gen_range(20..=40);
                    true
                }
            };
            if started {
                return;
            }
        }
    }

    /// Daytime idle: a weighted shuffle of greeting a villager or cat, a
    /// village-bound stroll, walking to the look target, a baby bouncing
    /// on a bed, or standing still 30-60 ticks, tried until one starts.
    pub(super) fn idle_run_one(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.tick < self.brain.wander.idle_until || !core.navigation.is_done() {
            return;
        }
        let mut order =
            IDLE.map(|(idle, weight)| (cx.rng.r#gen::<f32>().powf(1.0 / weight as f32), idle));
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, idle) in order {
            match idle {
                Idle::Villager => {
                    if self.start_villager_interaction(core, cx) {
                        return;
                    }
                }
                Idle::Cat => {
                    if self.start_cat_interaction(core, cx) {
                        return;
                    }
                }
                Idle::Bed => {
                    if core.is_baby() && self.start_bounce(core, cx) {
                        return;
                    }
                }
                Idle::Stroll => {
                    village_stroll(core, cx, SPEED, 10, 7);
                    return;
                }
                Idle::LookWalk => {
                    if core.look.is_looking() {
                        let target = core.look.wanted;
                        core.move_to_entity_reach(target, 2, SPEED, cx);
                        return;
                    }
                }
                Idle::Nothing => {
                    self.brain.wander.idle_until = cx.tick + cx.rng.gen_range(30..=60);
                    return;
                }
            }
        }
    }
}

const IDLE: [(Idle, u32); 6] = [
    (Idle::Villager, 2),
    (Idle::Cat, 1),
    (Idle::Stroll, 1),
    (Idle::LookWalk, 1),
    (Idle::Bed, 1),
    (Idle::Nothing, 1),
];

#[derive(Clone, Copy)]
enum Idle {
    Villager,
    Cat,
    Bed,
    Stroll,
    LookWalk,
    Nothing,
}

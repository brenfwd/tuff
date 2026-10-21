//! Bells: a ring shakes the bell for 50 ticks; raiders within hearing
//! make it resonate, and 40 ticks later every raider nearby glows.

use std::collections::HashMap;

use tuff_core::world::area;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::region_api::RegionWorld;
use tuff_vanilla_data::registry::{MobEffect, SoundEvent};
use tuff_vanilla_data::tag;
use tuff_world_model::BlockPos;

const SHAKE_TICKS: u32 = 50;
const TICKS_BEFORE_RESONATION: u32 = 5;
const RESONATION_TICKS: u32 = 40;
const SEARCH_RADIUS: f64 = 48.0;
const HEAR_RADIUS: f64 = 32.0;
const GLOW_TICKS: i64 = 60;
const MIN_TICKS_BETWEEN_SEARCHES: u64 = 60;

#[derive(Default)]
struct Bell {
    ticks: u32,
    shaking: bool,
    resonating: bool,
    resonation_ticks: u32,
    nearby: Vec<i32>,
    searched_at: Option<u64>,
}

#[derive(Default)]
pub struct Bells {
    ringing: HashMap<BlockPos, Bell>,
}

fn closer_to_center(pos: BlockPos, at: [f64; 3], range: f64) -> bool {
    let dx = f64::from(pos.x) + 0.5 - at[0];
    let dy = f64::from(pos.y) + 0.5 - at[1];
    let dz = f64::from(pos.z) + 0.5 - at[2];
    dx * dx + dy * dy + dz * dz < range * range
}

fn raider(world: &dyn RegionWorld, id: i32, pos: BlockPos, range: f64) -> bool {
    let Some(tracked) = world.entity(id) else {
        return false;
    };
    let base = tracked.entity.base();
    tracked.entity.living().is_some_and(|l| l.alive)
        && closer_to_center(pos, base.position, range)
        && tuff_datapack::tags::is(tag::entity_type::key::RAIDERS, base.kind)
}

fn search(world: &dyn RegionWorld, pos: BlockPos) -> Vec<i32> {
    let inside = |at: [f64; 3]| {
        [pos.x, pos.y, pos.z].iter().zip(at).all(|(&block, at)| {
            at >= f64::from(block) - SEARCH_RADIUS && at <= f64::from(block) + 1.0 + SEARCH_RADIUS
        })
    };
    world
        .entity_ids()
        .into_iter()
        .filter(|&id| {
            world
                .entity(id)
                .is_some_and(|t| t.entity.living().is_some() && inside(t.entity.base().position))
        })
        .collect()
}

fn glow_raiders_near(world: &mut dyn RegionWorld, pos: BlockPos) {
    for id in world.entity_ids() {
        if raider(world, id, pos, SEARCH_RADIUS)
            && let Some(tracked) = world.entity_mut(id)
        {
            tracked
                .entity
                .add_effects(&[ActiveEffect::timed(MobEffect::Glowing, GLOW_TICKS, 0)]);
        }
    }
}

pub fn rung(world: &mut dyn RegionWorld, pos: BlockPos) {
    let now = world.tick();
    let mut bells = std::mem::take(&mut world.state().get::<Bells>().ringing);
    let bell = bells.entry(pos).or_default();
    if bell
        .searched_at
        .is_none_or(|t| now > t + MIN_TICKS_BETWEEN_SEARCHES)
    {
        bell.searched_at = Some(now);
        bell.nearby = search(world, pos);
    }
    bell.resonation_ticks = 0;
    bell.ticks = 0;
    bell.shaking = true;
    world.state().get::<Bells>().ringing = bells;
}

pub fn tick(world: &mut dyn RegionWorld) {
    if world.state().get::<Bells>().ringing.is_empty() {
        return;
    }
    let mut bells = std::mem::take(&mut world.state().get::<Bells>().ringing);
    for (&pos, bell) in bells.iter_mut() {
        if bell.shaking {
            bell.ticks += 1;
        }
        if bell.ticks >= SHAKE_TICKS {
            bell.shaking = false;
            bell.ticks = 0;
        }
        if bell.ticks >= TICKS_BEFORE_RESONATION
            && bell.resonation_ticks == 0
            && bell
                .nearby
                .iter()
                .any(|&id| raider(world, id, pos, HEAR_RADIUS))
        {
            bell.resonating = true;
            world.play_sound(SoundEvent::BlockBellResonate, pos, 1.0, 1.0, None);
        }
        if bell.resonating {
            if bell.resonation_ticks < RESONATION_TICKS {
                bell.resonation_ticks += 1;
            } else {
                for &id in &bell.nearby {
                    if raider(world, id, pos, SEARCH_RADIUS)
                        && let Some(tracked) = world.entity_mut(id)
                    {
                        tracked.entity.add_effects(&[ActiveEffect::timed(
                            MobEffect::Glowing,
                            GLOW_TICKS,
                            0,
                        )]);
                    }
                }
                bell.resonating = false;
                let at = [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                ];
                area::broadcast(world, area::around(at, SEARCH_RADIUS), move |other| {
                    glow_raiders_near(other, pos);
                });
            }
        }
    }
    bells.retain(|_, bell| bell.shaking || bell.resonating);
    let state = &mut world.state().get::<Bells>().ringing;
    for (pos, bell) in bells {
        state.entry(pos).or_insert(bell);
    }
}

//! Crowd mode: an opt-in that runs mobs in a region holding at least
//! `tuff.mobs.crowd.threshold` entities with cheaper pushing and goal
//! selection. Off by default, so vanilla behaviour is the default.
//!
//! The threshold counts entities per region, the unit that runs on one
//! thread and whose tick time is what players feel as lag; a per-chunk
//! count would miss a farm spread over a few chunks that still overloads
//! its region. Above it:
//! - pushing runs every other tick (staggered by id) at twice the
//!   strength; its broadphase stops after [`MAX_PUSHERS`] overlapping
//!   neighbours (or the cramming limit, if larger), starting each scan at
//!   a rotating offset, and pushes come from the nearest of those; the
//!   cramming roll is 1 in 2 so damage keeps vanilla's average rate;
//! - full goal selection runs every [`GOAL_PERIOD`] ticks instead of 2
//!   (running goals still tick every tick), and every
//!   [`IDLE_GOAL_PERIOD`] ticks for mobs with no player within
//!   [`IDLE_RANGE`] blocks.
//!
//! Tempting, breeding, wandering and targeting still run, only with up to
//! a few ticks more latency. Mobs read the flag the region hook set at the
//! end of the previous tick, so a mob that changes region never keeps a
//! stale one.

use tuff_core::world::entity::TickContext;
use tuff_core::world::region_api::RegionWorld;

use crate::MobCore;

pub const ENABLED_KEY: &str = "tuff.mobs.crowd.enabled";
pub const THRESHOLD_KEY: &str = "tuff.mobs.crowd.threshold";
pub const DEFAULT_THRESHOLD: i64 = 1500;
pub const MAX_PUSHERS: usize = 8;
pub const COLLISION_CAP_KEY: &str = "tuff.mobs.collisionCap";
pub const GOAL_PERIOD: u64 = 4;
pub const IDLE_GOAL_PERIOD: u64 = 8;
pub const IDLE_RANGE: f64 = 32.0;

pub fn config_keys() -> Vec<tuff_core::config::Key> {
    use tuff_core::config::{Key, Value};
    vec![
        Key::new(
            ENABLED_KEY,
            Value::Bool(false),
            "Simplify pushing and goal selection for mobs in regions crowded past tuff.mobs.crowd.threshold",
        ),
        Key::new(
            THRESHOLD_KEY,
            Value::Int(DEFAULT_THRESHOLD),
            "Entities in one region (32x32 chunks) at which crowd mode starts",
        )
        .validated(|value| match value {
            Value::Int(n) if (1..=i64::from(i32::MAX)).contains(n) => Ok(()),
            _ => Err("must be between 1 and 2147483647".to_owned()),
        }),
        Key::new(
            COLLISION_CAP_KEY,
            Value::Int(0),
            "Most entity pushes one mob processes per tick (0 = unlimited, as vanilla); never below max_entity_cramming",
        )
        .validated(|value| match value {
            Value::Int(n) if (0..=i64::from(i32::MAX)).contains(n) => Ok(()),
            _ => Err("must be between 0 and 2147483647".to_owned()),
        }),
    ]
}

#[derive(Default)]
struct CrowdState {
    crowded: bool,
}

/// Whether a region holding `count` entities runs in crowd mode.
pub fn crowded(config: &tuff_core::config::ConfigStore, count: usize) -> bool {
    config.get_bool(ENABLED_KEY) == Some(true)
        && count as i64 >= config.get_int(THRESHOLD_KEY).unwrap_or(DEFAULT_THRESHOLD)
}

/// Decides the region's mode for the next tick, logs a change and marks
/// every entity while crowded.
pub fn tick(world: &mut dyn RegionWorld) {
    let ids = world.entity_ids();
    let now = crowded(&world.shared().config, ids.len());
    let region = world.region_pos();
    let state = world.state().get::<CrowdState>();
    if state.crowded != now {
        state.crowded = now;
        if now {
            tuff_core::log::info!(
                "region {region:?} entered crowd mode with {} entities",
                ids.len()
            );
        } else {
            tuff_core::log::info!(
                "region {region:?} left crowd mode with {} entities",
                ids.len()
            );
        }
    }
    if !now {
        return;
    }
    for id in ids {
        if let Some(tracked) = world.entity_mut(id) {
            tracked.entity.crowd();
        }
    }
}

pub fn pushes_this_tick(core: &MobCore) -> bool {
    !core.crowded || (core.tick_count + core.base.id as u64).is_multiple_of(2)
}

pub fn push_scale(core: &MobCore) -> f64 {
    if core.crowded { 2.0 } else { 1.0 }
}

/// The per-mob push limit from `tuff.mobs.collisionCap`: `usize::MAX` when
/// unlimited, otherwise at least `max_entity_cramming` so cramming
/// still sees enough neighbours.
pub fn collision_cap(config: &tuff_core::config::ConfigStore, max_cramming: i32) -> usize {
    match config.get_int(COLLISION_CAP_KEY).unwrap_or(0) {
        n if n <= 0 => usize::MAX,
        n => (n as usize).max(max_cramming.max(0) as usize),
    }
}

/// Keeps only the [`MAX_PUSHERS`] nearest of `pushers`.
pub fn keep_nearest<T>(pushers: &mut Vec<T>, own: [f64; 3], position: impl Fn(&T) -> [f64; 3]) {
    keep_nearest_n(pushers, MAX_PUSHERS, own, position);
}

/// Keeps only the `limit` nearest of `pushers`.
pub fn keep_nearest_n<T>(
    pushers: &mut Vec<T>,
    limit: usize,
    own: [f64; 3],
    position: impl Fn(&T) -> [f64; 3],
) {
    if pushers.len() <= limit {
        return;
    }
    let distance = |p: &T| {
        let [x, _, z] = position(p);
        (x - own[0]).powi(2) + (z - own[2]).powi(2)
    };
    pushers.select_nth_unstable_by(limit, |a, b| distance(a).total_cmp(&distance(b)));
    pushers.truncate(limit);
}

/// How often full goal selection runs for this mob.
pub fn goal_period(core: &MobCore, cx: &TickContext) -> u64 {
    if !core.crowded {
        return 2;
    }
    let near = cx
        .players
        .iter()
        .any(|p| core.distance_sq(p.position) < IDLE_RANGE * IDLE_RANGE);
    if near { GOAL_PERIOD } else { IDLE_GOAL_PERIOD }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn crowd_mode_is_off_unless_enabled_and_past_the_threshold() {
        let config = tuff_core::config::ConfigStore::default();
        for key in config_keys() {
            config.register(key).unwrap();
        }
        assert!(!crowded(&config, 1_000_000), "off by default");
        config
            .set(ENABLED_KEY, tuff_core::config::Value::Bool(true))
            .unwrap();
        assert!(!crowded(&config, DEFAULT_THRESHOLD as usize - 1));
        assert!(crowded(&config, DEFAULT_THRESHOLD as usize));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_crowded_mob_keeps_its_nearest_pushers() {
        let mut pushers: Vec<[f64; 3]> = (0..20).map(|i| [f64::from(i) * 0.01, 0.0, 0.0]).collect();
        pushers.reverse();
        keep_nearest(&mut pushers, [0.0; 3], |p| *p);
        pushers.sort_by(|a, b| a[0].total_cmp(&b[0]));
        assert_eq!(pushers.len(), MAX_PUSHERS);
        assert!(pushers.last().unwrap()[0] < 0.075);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_collision_cap_is_unlimited_by_default_and_floored_at_cramming() {
        let config = tuff_core::config::ConfigStore::default();
        for key in config_keys() {
            config.register(key).unwrap();
        }
        assert_eq!(collision_cap(&config, 24), usize::MAX);
        config
            .set(COLLISION_CAP_KEY, tuff_core::config::Value::Int(2))
            .unwrap();
        assert_eq!(collision_cap(&config, 24), 24);
        assert_eq!(collision_cap(&config, 0), 2);
        config
            .set(COLLISION_CAP_KEY, tuff_core::config::Value::Int(40))
            .unwrap();
        assert_eq!(collision_cap(&config, 24), 40);
        assert!(
            config
                .set(COLLISION_CAP_KEY, tuff_core::config::Value::Int(-1))
                .is_err()
        );
    }
}

//! How long dropped items and experience orbs live and how close they must
//! be to merge.

use crate::config::{ConfigStore, Key, Value};

pub const ITEM_DESPAWN_TICKS: &str = "tuff.world.item.despawnTicks";
pub const ITEM_MERGE_RADIUS: &str = "tuff.world.item.mergeRadius";
pub const ORB_DESPAWN_TICKS: &str = "tuff.world.experience.despawnTicks";
pub const ORB_MERGE_RADIUS: &str = "tuff.world.experience.mergeRadius";

pub const DEFAULT_DESPAWN_TICKS: i64 = 6000;
pub const DEFAULT_MERGE_RADIUS: f64 = 0.5;
const MAX_DESPAWN_TICKS: i64 = 1_000_000_000;
const MAX_MERGE_RADIUS: f64 = 64.0;

fn ticks_key(name: &str, what: &str) -> Key {
    Key::new(
        name,
        Value::Int(DEFAULT_DESPAWN_TICKS),
        &format!("Ticks a {what} lives before it despawns"),
    )
    .validated(|value| match value {
        Value::Int(n) if (1..=MAX_DESPAWN_TICKS).contains(n) => Ok(()),
        _ => Err(format!("must be between 1 and {MAX_DESPAWN_TICKS}")),
    })
}

fn radius_key(name: &str, what: &str) -> Key {
    Key::new(
        name,
        Value::String(DEFAULT_MERGE_RADIUS.to_string()),
        &format!("Blocks around a {what} within which others of its kind merge into it"),
    )
    .validated(|value| match value {
        Value::String(text) if parse_radius(text).is_some() => Ok(()),
        _ => Err(format!("must be a number from 0 to {MAX_MERGE_RADIUS}")),
    })
}

fn parse_radius(text: &str) -> Option<f64> {
    let radius: f64 = text.trim().parse().ok()?;
    (radius.is_finite() && (0.0..=MAX_MERGE_RADIUS).contains(&radius)).then_some(radius)
}

pub fn config_keys() -> Vec<Key> {
    vec![
        ticks_key(ITEM_DESPAWN_TICKS, "dropped item"),
        radius_key(ITEM_MERGE_RADIUS, "dropped item"),
        ticks_key(ORB_DESPAWN_TICKS, "experience orb"),
        radius_key(ORB_MERGE_RADIUS, "experience orb"),
    ]
}

/// The lifetimes and merge radii in force; read from config when needed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DespawnSettings {
    pub item_age: i32,
    pub item_merge: f64,
    pub orb_age: u32,
    pub orb_merge: f64,
}

impl Default for DespawnSettings {
    fn default() -> DespawnSettings {
        DespawnSettings {
            item_age: DEFAULT_DESPAWN_TICKS as i32,
            item_merge: DEFAULT_MERGE_RADIUS,
            orb_age: DEFAULT_DESPAWN_TICKS as u32,
            orb_merge: DEFAULT_MERGE_RADIUS,
        }
    }
}

impl DespawnSettings {
    pub fn from_config(config: &ConfigStore) -> DespawnSettings {
        let default = DespawnSettings::default();
        let ticks = |name: &str, fallback: i64| {
            config
                .get_int(name)
                .filter(|n| (1..=MAX_DESPAWN_TICKS).contains(n))
                .unwrap_or(fallback)
        };
        let radius = |name: &str, fallback: f64| {
            config
                .get_string(name)
                .and_then(|text| parse_radius(&text))
                .unwrap_or(fallback)
        };
        DespawnSettings {
            item_age: ticks(ITEM_DESPAWN_TICKS, default.item_age as i64) as i32,
            item_merge: radius(ITEM_MERGE_RADIUS, default.item_merge),
            orb_age: ticks(ORB_DESPAWN_TICKS, default.orb_age as i64) as u32,
            orb_merge: radius(ORB_MERGE_RADIUS, default.orb_merge),
        }
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn settings_defaults_equal_the_vanilla_constants() {
        let settings = DespawnSettings::default();
        assert_eq!(settings.item_age, crate::world::items::DESPAWN_AGE);
        assert_eq!(settings.orb_age, crate::world::experience::LIFETIME);
        assert_eq!(settings.item_merge, crate::world::items::MERGE_REACH);
        assert_eq!(settings.orb_merge, crate::world::experience::MERGE_DISTANCE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn settings_reject_negative_nan_and_out_of_range_values() {
        let config = ConfigStore::new();
        assert_eq!(
            DespawnSettings::from_config(&config),
            DespawnSettings::default()
        );
        for bad in [Value::Int(0), Value::Int(-5)] {
            assert!(config.set(ITEM_DESPAWN_TICKS, bad).is_err());
        }
        for bad in ["-1", "NaN", "inf", "65", "abc"] {
            assert!(
                config
                    .set(ITEM_MERGE_RADIUS, Value::String(bad.into()))
                    .is_err()
            );
        }
        config.set(ORB_DESPAWN_TICKS, Value::Int(100)).unwrap();
        config
            .set(ORB_MERGE_RADIUS, Value::String("2".into()))
            .unwrap();
        let settings = DespawnSettings::from_config(&config);
        assert_eq!((settings.orb_age, settings.orb_merge), (100, 2.0));
    }
}

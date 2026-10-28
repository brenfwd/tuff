//! Per-category mob despawn distances: the range beyond which a mob always
//! despawns and the range inside which it never does. Both default to the
//! game's values.

use tuff_core::config::{ConfigStore, Key, Value};
use tuff_vanilla_data::entity::MobCategory;

const MAX_DISTANCE: i64 = 1024;

fn far_key(category: MobCategory) -> String {
    format!("tuff.world.despawn.{}.distance", category.name())
}

fn near_key(category: MobCategory) -> String {
    format!("tuff.world.despawn.{}.minDistance", category.name())
}

fn distance_key(name: &str, default: u32, description: &str) -> Key {
    Key::new(name, Value::Int(i64::from(default)), description).validated(|value| match value {
        Value::Int(n) if (0..=MAX_DISTANCE).contains(n) => Ok(()),
        _ => Err(format!("must be between 0 and {MAX_DISTANCE}")),
    })
}

pub fn config_keys() -> Vec<Key> {
    MobCategory::ALL
        .iter()
        .flat_map(|&category| {
            let info = category.info();
            [
                distance_key(
                    &far_key(category),
                    info.despawn_distance,
                    "Blocks from every player beyond which a mob of this category always despawns",
                ),
                distance_key(
                    &near_key(category),
                    info.no_despawn_distance,
                    "Blocks from the nearest player within which a mob of this category never despawns (capped at the category's distance)",
                ),
            ]
        })
        .collect()
}

/// The despawn distances of `category` in blocks, `(far, near)`; near is
/// never beyond far.
pub fn distances(config: &ConfigStore, category: MobCategory) -> (u32, u32) {
    let info = category.info();
    let read = |name: String, default: u32| {
        config
            .get_int(&name)
            .filter(|n| (0..=MAX_DISTANCE).contains(n))
            .map_or(default, |n| n as u32)
    };
    let far = read(far_key(category), info.despawn_distance);
    let near = read(near_key(category), info.no_despawn_distance);
    (far, near.min(far))
}

#[cfg(test)]
mod despawn_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn despawn_defaults_equal_the_vanilla_distances() {
        let config = ConfigStore::new();
        for key in config_keys() {
            config.register(key).unwrap();
        }
        for category in MobCategory::ALL {
            let info = category.info();
            assert_eq!(
                distances(&config, category),
                (info.despawn_distance, info.no_despawn_distance)
            );
        }
        assert_eq!(distances(&config, MobCategory::WaterAmbient).0, 64);
        assert_eq!(distances(&config, MobCategory::Monster), (128, 32));
    }

    #[tuff_core::watchdog]
    #[test]
    fn despawn_distances_reject_negative_and_cap_near_at_far() {
        let config = ConfigStore::new();
        for key in config_keys() {
            config.register(key).unwrap();
        }
        let far = "tuff.world.despawn.monster.distance";
        assert!(config.set(far, Value::Int(-1)).is_err());
        assert!(config.set(far, Value::Int(5000)).is_err());
        config.set(far, Value::Int(10)).unwrap();
        assert_eq!(distances(&config, MobCategory::Monster), (10, 10));
    }
}

use crate::config::{ConfigStore, Key, Value};

pub const HEALTH_KEY: &str = "tuff.hidePlayerDetails.health";
pub const ITEMS_KEY: &str = "tuff.hidePlayerDetails.items";
const HEALTH_MODES: &[&str] = &["real", "full", "alive"];

pub fn config_keys() -> Vec<Key> {
    vec![
        Key::new(
            HEALTH_KEY,
            Value::String("real".to_owned()),
            "Health other players see: real, full (always 20) or alive (1 while alive, 0 when dead); applies to players spawned or respawned after a change",
        )
        .hinted(HEALTH_MODES)
        .validated(|value| match value {
            Value::String(mode) if HEALTH_MODES.contains(&mode.as_str()) => Ok(()),
            _ => Err("must be real, full or alive".to_owned()),
        }),
        Key::new(
            ITEMS_KEY,
            Value::Bool(false),
            "Hide durability, enchantments and custom data of the equipment other players see (the enchantment glint stays); applies to equipment sent after a change",
        ),
    ]
}

pub fn shown_health(config: &ConfigStore, real: f32) -> f32 {
    match config.get_string(HEALTH_KEY).as_deref() {
        Some("full") => 20.0,
        Some("alive") => {
            if real > 0.0 {
                1.0
            } else {
                0.0
            }
        }
        _ => real,
    }
}

pub fn hide_items(config: &ConfigStore) -> bool {
    config.get_bool(ITEMS_KEY).unwrap_or(false)
}

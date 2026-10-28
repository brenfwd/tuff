//! Per-chunk limits on how many entities of one type are saved and loaded.

use crate::config::{ConfigStore, Key, Value};
use tuff_nbt::Compound;

const DEFAULTS: [(&str, &str, i64); 4] = [
    ("minecraft:item", "tuff.world.entityCaps.item", 1024),
    (
        "minecraft:experience_orb",
        "tuff.world.entityCaps.experience_orb",
        512,
    ),
    ("minecraft:arrow", "tuff.world.entityCaps.arrow", 256),
    ("minecraft:snowball", "tuff.world.entityCaps.snowball", 128),
];

pub fn config_keys() -> Vec<Key> {
    DEFAULTS
        .iter()
        .map(|(id, key, default)| {
            Key::new(
                key,
                Value::Int(*default),
                &format!(
                    "Most {id} entities one chunk saves or loads; the excess is dropped and logged. 0 = unlimited"
                ),
            )
            .validated(|value| match value {
                Value::Int(n) if *n >= 0 => Ok(()),
                _ => Err("must be zero or more".to_owned()),
            })
        })
        .collect()
}

/// The caps in force; read from config at each save and load.
#[derive(Clone, Debug, Default)]
pub struct EntityCaps {
    limits: Vec<(&'static str, usize)>,
}

impl EntityCaps {
    pub fn new(limits: Vec<(&'static str, usize)>) -> EntityCaps {
        EntityCaps { limits }
    }

    pub fn from_config(config: &ConfigStore) -> EntityCaps {
        let limits = DEFAULTS
            .iter()
            .map(|(id, key, default)| {
                let cap = config.get_int(key).unwrap_or(*default).max(0) as usize;
                (*id, cap)
            })
            .collect();
        EntityCaps { limits }
    }

    /// The cap on `id`, `None` when it is unlimited.
    pub fn limit(&self, id: &str) -> Option<usize> {
        self.limits
            .iter()
            .find(|(name, cap)| *name == id && *cap > 0)
            .map(|(_, cap)| *cap)
    }

    /// Keeps the first `cap` of each capped type, in stored order. Returns
    /// the kept entries and the `(id, kept, dropped)` of each type that
    /// lost some.
    pub fn trim_saved(&self, saved: Vec<Compound>) -> (Vec<Compound>, Vec<(String, usize, usize)>) {
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        let mut out = Vec::with_capacity(saved.len());
        for entry in saved {
            let Some(id) = entry.get_str("id").map(str::to_owned) else {
                out.push(entry);
                continue;
            };
            let Some(cap) = self.limit(&id) else {
                out.push(entry);
                continue;
            };
            let slot = match seen.iter().position(|(name, ..)| *name == id) {
                Some(slot) => slot,
                None => {
                    seen.push((id, 0, 0));
                    seen.len() - 1
                }
            };
            if seen[slot].1 < cap {
                seen[slot].1 += 1;
                out.push(entry);
            } else {
                seen[slot].2 += 1;
            }
        }
        seen.retain(|(_, _, dropped)| *dropped > 0);
        (out, seen)
    }
}

#[cfg(test)]
mod entity_caps_tests {
    use super::*;

    fn entry(id: &str) -> Compound {
        Compound::new().with("id", id)
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_caps_keep_first_n_per_type() {
        let caps = EntityCaps::new(vec![("minecraft:item", 2)]);
        let saved = vec![
            entry("minecraft:item"),
            entry("minecraft:pig"),
            entry("minecraft:item"),
            entry("minecraft:item"),
            Compound::new(),
        ];
        let (kept, dropped) = caps.trim_saved(saved);
        assert_eq!(kept.len(), 4);
        assert_eq!(dropped, vec![("minecraft:item".to_owned(), 2, 1)]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_caps_zero_means_unlimited() {
        let caps = EntityCaps::new(vec![("minecraft:item", 0)]);
        let saved = vec![entry("minecraft:item"); 5];
        let (kept, dropped) = caps.trim_saved(saved);
        assert_eq!(kept.len(), 5);
        assert!(dropped.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_caps_keys_are_declared_with_defaults() {
        let keys = config_keys();
        assert_eq!(keys.len(), 4);
        let caps = EntityCaps::from_config(&ConfigStore::new());
        assert_eq!(caps.limit("minecraft:item"), Some(1024));
        assert_eq!(caps.limit("minecraft:snowball"), Some(128));
        assert_eq!(caps.limit("minecraft:pig"), None);
    }
}

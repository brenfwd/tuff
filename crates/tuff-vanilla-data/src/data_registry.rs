//! Data-driven registries defined by the built-in data pack. Their network IDs
//! are the positions of entries in the order the server sends them.

#[derive(Clone, Copy, Debug)]
pub struct DataRegistry {
    pub key: &'static str,
    pub synchronized: bool,
    /// Resource locations, sorted.
    pub entries: &'static [&'static str],
    /// Per entry, the first 8 bytes (big-endian) of the SHA-1 of its file in
    /// the built-in pack.
    pub hashes: &'static [u64],
}

/// The JSON kind of a required registry entry field. Strings and structures
/// accept each other's forms: ids may be inline holders and text components
/// may be plain strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Bool,
    Number,
    String,
    Structure,
    Any,
}

impl FieldKind {
    pub fn matches(self, value: &serde_json::Value) -> bool {
        use serde_json::Value;
        match self {
            Self::Bool => value.is_boolean(),
            Self::Number => value.is_number(),
            Self::String => value.is_string() || value.is_object(),
            Self::Structure => {
                matches!(value, Value::Object(_) | Value::Array(_) | Value::String(_))
            }
            Self::Any => !value.is_null(),
        }
    }
}

impl DataRegistry {
    /// Whether this registry's entries are JSON lists rather than objects.
    pub fn list_entries(&self) -> bool {
        LIST_ENTRIES.contains(&self.key)
    }

    /// Fields every entry of synchronized registry `key` must have.
    pub fn required_fields(key: &str) -> &'static [(&'static str, FieldKind)] {
        REQUIRED_FIELDS
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(&[], |(_, fields)| fields)
    }

    pub fn get(key: &str) -> Option<&'static DataRegistry> {
        ALL.iter().find(|registry| registry.key == key)
    }
}

include!(concat!(env!("OUT_DIR"), "/data_registries.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_registries_are_the_ones_clients_sync() {
        let sent: Vec<&str> = SYNCHRONIZED.iter().map(|r| r.key).collect();
        assert_eq!(
            sent,
            [
                "minecraft:worldgen/biome",
                "minecraft:chat_type",
                "minecraft:trim_pattern",
                "minecraft:trim_material",
                "minecraft:wolf_variant",
                "minecraft:wolf_sound_variant",
                "minecraft:pig_variant",
                "minecraft:pig_sound_variant",
                "minecraft:frog_variant",
                "minecraft:cat_variant",
                "minecraft:cat_sound_variant",
                "minecraft:cow_sound_variant",
                "minecraft:cow_variant",
                "minecraft:chicken_sound_variant",
                "minecraft:chicken_variant",
                "minecraft:zombie_nautilus_variant",
                "minecraft:painting_variant",
                "minecraft:sulfur_cube_archetype",
                "minecraft:dimension_type",
                "minecraft:damage_type",
                "minecraft:banner_pattern",
                "minecraft:enchantment",
                "minecraft:jukebox_song",
                "minecraft:instrument",
                "minecraft:test_environment",
                "minecraft:test_instance",
                "minecraft:dialog",
                "minecraft:world_clock",
                "minecraft:timeline",
                "minecraft:decorated_pot_pattern",
                "minecraft:block_transformer",
                "minecraft:worldgen/block_state_provider",
            ]
        );
        let flagged: Vec<&str> = ALL
            .iter()
            .filter(|r| r.synchronized)
            .map(|r| r.key)
            .collect();
        let mut sorted = sent.clone();
        sorted.sort_unstable();
        assert_eq!(flagged, sorted);
        for key in [
            "minecraft:dimension",
            "minecraft:enchantment_provider",
            "minecraft:trade_set",
            "minecraft:trial_spawner",
            "minecraft:villager_trade",
            "minecraft:worldgen/feature",
            "minecraft:worldgen/noise",
            "minecraft:worldgen/structure",
        ] {
            assert!(!DataRegistry::get(key).unwrap().synchronized, "{key}");
        }
    }

    #[test]
    fn variant_registries_are_never_empty() {
        for key in [
            "minecraft:wolf_variant",
            "minecraft:pig_variant",
            "minecraft:frog_variant",
            "minecraft:cat_variant",
            "minecraft:cow_variant",
            "minecraft:chicken_variant",
            "minecraft:zombie_nautilus_variant",
            "minecraft:painting_variant",
        ] {
            assert!(!DataRegistry::get(key).unwrap().entries.is_empty(), "{key}");
        }
    }
}

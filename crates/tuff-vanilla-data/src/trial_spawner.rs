//! The data pack's `minecraft:trial_spawner` entries: the waves a trial
//! spawner sends and the rewards it ejects. Spawn data is entity NBT in
//! JSON form, an open-ended shape kept as compact JSON text.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrialSpawnerConfig {
    pub name: &'static str,
    pub spawn_range: i64,
    pub total_mobs: f32,
    pub simultaneous_mobs: f32,
    pub total_mobs_added_per_player: f32,
    pub simultaneous_mobs_added_per_player: f32,
    pub ticks_between_spawn: i64,
    pub spawn_potentials: &'static [SpawnPotential],
    /// `(weight, loot table)` pairs; one is picked per reward ejection run.
    pub loot_tables_to_eject: &'static [(u64, &'static str)],
    pub items_to_drop_when_ominous: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnPotential {
    pub weight: u64,
    /// The entity to spawn, as JSON (an `id` and any saved fields).
    pub entity: &'static str,
    /// `{loot_table, slot_drop_chances}` as JSON.
    pub equipment: Option<&'static str>,
    /// `{block_light_limit, sky_light_limit}` as JSON.
    pub custom_spawn_rules: Option<&'static str>,
}

pub const DEFAULT_LOOT_TABLES_TO_EJECT: &[(u64, &str)] = &[
    (1, "minecraft:spawners/trial_chamber/consumables"),
    (1, "minecraft:spawners/trial_chamber/key"),
];
pub const DEFAULT_ITEMS_TO_DROP_WHEN_OMINOUS: &str =
    "minecraft:spawners/trial_chamber/items_to_drop_when_ominous";

impl TrialSpawnerConfig {
    /// The game's defaults, used where a spawner names no config.
    pub const DEFAULT: TrialSpawnerConfig = TrialSpawnerConfig {
        name: "",
        spawn_range: 4,
        total_mobs: 6.0,
        simultaneous_mobs: 2.0,
        total_mobs_added_per_player: 2.0,
        simultaneous_mobs_added_per_player: 1.0,
        ticks_between_spawn: 40,
        spawn_potentials: &[],
        loot_tables_to_eject: DEFAULT_LOOT_TABLES_TO_EJECT,
        items_to_drop_when_ominous: DEFAULT_ITEMS_TO_DROP_WHEN_OMINOUS,
    };

    pub fn get(name: &str) -> Option<&'static TrialSpawnerConfig> {
        crate::lookup(ALL, name)
    }

    /// How many mobs a trial sends in all, with `additional` players
    /// beyond the first.
    pub fn target_total_mobs(&self, additional: usize) -> usize {
        (self.total_mobs + self.total_mobs_added_per_player * additional as f32).floor() as usize
    }

    /// How many of its mobs may be alive at once.
    pub fn target_simultaneous_mobs(&self, additional: usize) -> usize {
        (self.simultaneous_mobs + self.simultaneous_mobs_added_per_player * additional as f32)
            .floor() as usize
    }
}

include!(concat!(env!("OUT_DIR"), "/trial_spawners.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_chamber_configs_carry_their_waves() {
        let breeze = TrialSpawnerConfig::get("minecraft:trial_chamber/breeze/normal").unwrap();
        assert_eq!(breeze.total_mobs, 2.0);
        assert_eq!(breeze.simultaneous_mobs, 1.0);
        assert_eq!(breeze.ticks_between_spawn, 20);
        assert_eq!(breeze.loot_tables_to_eject, DEFAULT_LOOT_TABLES_TO_EJECT);
        assert_eq!(breeze.spawn_potentials.len(), 1);
        assert!(
            breeze.spawn_potentials[0]
                .entity
                .contains("minecraft:breeze")
        );
        assert_eq!(breeze.target_total_mobs(2), 4);
        assert_eq!(breeze.target_simultaneous_mobs(1), 1);

        let skeleton =
            TrialSpawnerConfig::get("minecraft:trial_chamber/ranged/skeleton/ominous").unwrap();
        assert_eq!(skeleton.total_mobs, 6.0);
        assert_eq!(
            skeleton.loot_tables_to_eject,
            &[
                (3, "minecraft:spawners/ominous/trial_chamber/key"),
                (7, "minecraft:spawners/ominous/trial_chamber/consumables"),
            ]
        );
        assert!(
            skeleton.spawn_potentials[0]
                .equipment
                .unwrap()
                .contains("minecraft:equipment/trial_chamber_ranged")
        );
        assert!(ALL.windows(2).all(|w| w[0].0 < w[1].0));
    }
}

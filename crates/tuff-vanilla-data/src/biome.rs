//! The built-in `minecraft:worldgen/biome` entries. This is a data-driven
//! registry (see [`crate::data_registry`]): its network IDs are assigned by
//! send order, not fixed by the game version.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biome {
    pub name: &'static str,
    pub has_precipitation: bool,
    pub temperature: f32,
    pub temperature_modifier: Option<TemperatureModifier>,
    pub downfall: f32,
    pub effects: BiomeEffects,
    /// Worldgen carver names, in the order they run.
    pub carvers: &'static [&'static str],
    /// Placed-feature names, one slice per generation step, in the order
    /// features are placed within that step.
    pub features: &'static [&'static [&'static str]],
    /// `(attribute key, JSON value)` pairs. Attribute values are either a bare
    /// scalar or `{"argument": ..., "modifier": ...}`; parse with
    /// [`serde_json`] if you need one.
    pub attributes: &'static [(&'static str, &'static str)],
    /// The natural mob spawn settings attribute, typed; taken out of
    /// `attributes`.
    pub natural_spawns: NaturalSpawns,
}

/// What spawns naturally in a biome, per mob category.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NaturalSpawns {
    pub categories: &'static [(crate::entity::MobCategory, &'static [Spawner])],
    /// Spawn potential costs of types that have one.
    pub costs: &'static [(crate::registry::EntityType, SpawnCost)],
    /// The chance of each creature group spawning with a new chunk; `None`
    /// for the attribute's default.
    pub creature_probability: Option<f32>,
}

impl NaturalSpawns {
    pub fn spawners(&self, category: crate::entity::MobCategory) -> &'static [Spawner] {
        self.categories
            .iter()
            .find(|(c, _)| *c == category)
            .map_or(&[], |(_, spawners)| spawners)
    }

    pub fn cost(&self, kind: crate::registry::EntityType) -> Option<SpawnCost> {
        self.costs
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, cost)| *cost)
    }

    /// The spawn list of `category` with `overlay` laid over this one: a
    /// category the overlay defines (even empty) replaces this one's.
    pub fn spawners_under(
        &self,
        overlay: Option<&NaturalSpawns>,
        category: crate::entity::MobCategory,
    ) -> &'static [Spawner] {
        match overlay {
            Some(over) if over.defines(category) => over.spawners(category),
            _ => self.spawners(category),
        }
    }

    /// Whether the settings list `category`, even with no entries.
    pub fn defines(&self, category: crate::entity::MobCategory) -> bool {
        self.categories.iter().any(|(c, _)| *c == category)
    }

    /// The spawn cost of `kind` with `overlay` laid over this one.
    pub fn cost_under(
        &self,
        overlay: Option<&NaturalSpawns>,
        kind: crate::registry::EntityType,
    ) -> Option<SpawnCost> {
        overlay
            .and_then(|over| over.cost(kind))
            .or_else(|| self.cost(kind))
    }

    pub fn creature_probability(&self) -> f32 {
        self.creature_probability
            .unwrap_or(crate::entity::CREATURE_SPAWN_PROBABILITY)
    }
}

/// One weighted entry of a spawn list: a group of `min_count..=max_count`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spawner {
    pub kind: crate::registry::EntityType,
    pub weight: u32,
    pub min_count: u32,
    pub max_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnCost {
    pub energy_budget: f64,
    pub charge: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiomeEffects {
    /// Packed `0xRRGGBB`.
    pub water_color: u32,
    pub foliage_color: Option<u32>,
    pub grass_color: Option<u32>,
    pub dry_foliage_color: Option<u32>,
    pub grass_color_modifier: Option<GrassColorModifier>,
}

impl Biome {
    pub fn get(name: &str) -> Option<&'static Biome> {
        crate::lookup(ALL, name)
    }

    /// The vanilla biome with the given `WORLDGEN_BIOME` id (ids past the
    /// registry read as plains), through a table built on first use.
    pub fn by_id(id: u16) -> Option<&'static Biome> {
        static TABLE: std::sync::OnceLock<Vec<Option<&'static Biome>>> = std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| {
            crate::data_registry::WORLDGEN_BIOME
                .entries
                .iter()
                .map(|name| Biome::get(name))
                .collect()
        });
        match table.get(id as usize) {
            Some(biome) => *biome,
            None => Biome::get("minecraft:plains"),
        }
    }

    /// Parses an attribute value looked up by key in [`Biome::attributes`].
    pub fn attribute(&self, key: &str) -> Option<Value> {
        self.attributes
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, raw)| serde_json::from_str(raw).expect("generated JSON is valid"))
    }
}

include!(concat!(env!("OUT_DIR"), "/biomes.rs"));

#[cfg(test)]
mod by_id_tests {
    use super::*;

    #[test]
    fn by_id_matches_the_name_lookup() {
        let entries = crate::data_registry::WORLDGEN_BIOME.entries;
        for id in 0..entries.len() as u16 + 4 {
            let name = entries
                .get(id as usize)
                .copied()
                .unwrap_or("minecraft:plains");
            assert!(std::ptr::eq(
                Biome::by_id(id).unwrap(),
                Biome::get(name).unwrap()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::MobCategory;
    use crate::registry::EntityType;

    #[test]
    fn natural_spawns_are_typed() {
        let plains = Biome::get("minecraft:plains").unwrap();
        let creatures = plains.natural_spawns.spawners(MobCategory::Creature);
        let sheep = creatures
            .iter()
            .find(|s| s.kind == EntityType::Sheep)
            .unwrap();
        assert_eq!((sheep.weight, sheep.min_count, sheep.max_count), (12, 4, 4));
        let horse = creatures
            .iter()
            .find(|s| s.kind == EntityType::Horse)
            .unwrap();
        assert_eq!((horse.min_count, horse.max_count), (2, 6));
        assert_eq!(plains.natural_spawns.creature_probability(), 0.1);
        assert!(
            plains
                .attribute("minecraft:gameplay/natural_mob_spawns")
                .is_none()
        );
        let valley = Biome::get("minecraft:soul_sand_valley").unwrap();
        assert!(valley.natural_spawns.cost(EntityType::Ghast).is_some());
    }
}

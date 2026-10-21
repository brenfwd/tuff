//! Dimension types and biomes data packs change or add, resolved once at
//! world start.

use serde_json::Value;
use tuff_vanilla_data::data_registry::{DIMENSION_TYPE, WORLDGEN_BIOME};
use tuff_vanilla_data::dimension_type::DimensionType;

use super::{BiomeDefinition, add_biome, dimension_type, leak};
use tuff_item::registries::{Entry, Registries};
use tuff_world_model::biomes::{BiomeClimate, Biomes, CustomBiome};

/// The world's dimension types as the data packs leave them, with their
/// network ids: the built-in ones keep theirs, pack-added ones follow.
pub struct PackTypes {
    list: Vec<(&'static DimensionType, i32)>,
}

impl PackTypes {
    /// The vanilla dimension types alone.
    pub fn vanilla() -> Self {
        let list = DIMENSION_TYPE
            .entries
            .iter()
            .enumerate()
            .filter_map(|(id, name)| Some((DimensionType::get(name)?, id as i32)))
            .collect();
        Self { list }
    }

    /// Decodes every dimension type a pack changed or added. One that
    /// fails (the registry freeze checks them, so only a bug gets here)
    /// keeps its built-in data, or is left out when it has none.
    pub fn from_registries(registries: &Registries) -> Self {
        let Some(registry) = registries.get(DIMENSION_TYPE.key).filter(|r| r.modified()) else {
            return Self::vanilla();
        };
        let list = registry
            .iter()
            .enumerate()
            .filter_map(|(id, (name, entry))| {
                let decoded = match entry {
                    Entry::Builtin(_) => None,
                    Entry::Loaded(raw) => match dimension_type(name, raw.json.as_deref()?) {
                        Ok(kind) => Some(&*Box::leak(Box::new(kind))),
                        Err(err) => {
                            log::error!("{err}");
                            None
                        }
                    },
                };
                Some((decoded.or_else(|| DimensionType::get(name))?, id as i32))
            })
            .collect();
        Self { list }
    }

    /// The type called `name` and its network id.
    pub fn get(&self, name: &str) -> Option<(&'static DimensionType, i32)> {
        self.list
            .iter()
            .copied()
            .find(|(kind, _)| kind.name == name)
    }

    /// How many ids the types take; feature types follow.
    pub fn len(&self) -> usize {
        self.list
            .iter()
            .map(|(_, id)| *id as usize + 1)
            .max()
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Adds the biomes data packs added, in their registry order, so each id
/// is its index in the frozen registry. Returns how many.
pub(super) fn add_pack_biomes(biomes: &mut Biomes, registries: &Registries) -> usize {
    let Some(registry) = registries.get(WORLDGEN_BIOME.key).filter(|r| r.modified()) else {
        return 0;
    };
    let mut added = 0;
    for (key, entry, _) in registry.network_iter().skip(WORLDGEN_BIOME.entries.len()) {
        let json = match entry {
            Entry::Loaded(raw) => raw.json.as_deref(),
            Entry::Builtin(_) => None,
        };
        let definition = BiomeDefinition {
            key: key.to_owned(),
            json: json.map(Value::to_string).unwrap_or_default(),
        };
        if let Err(err) = add_biome(biomes, &definition) {
            log::error!("data pack biome {key} gets default climate and no spawns: {err}");
            biomes.push(CustomBiome {
                key: leak(key),
                json: definition.json,
                nbt: Vec::new(),
                climate: BiomeClimate::vanilla(u16::MAX),
                spawns: Vec::new(),
            });
        }
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::world::custom_dimension::tests::{BIOME, Provider, SKY_TYPE, definition};
    use crate::world::custom_dimension::{CustomDimensions, DimensionProvider};

    #[tuff_core::watchdog]
    #[test]
    fn plugin_biomes_and_types_follow_the_ones_data_packs_add() {
        let mut loader = tuff_item::registries::Loader::default();
        loader.insert(
            &WORLDGEN_BIOME,
            "a:glade".into(),
            "p",
            br##"{"carvers":[],"downfall":0.5,"effects":{"water_color":"#3f76e4"},"features":[],"has_precipitation":true,"temperature":0.25}"##.to_vec(),
        );
        loader.insert(
            &DIMENSION_TYPE,
            "a:aaa".into(),
            "p",
            SKY_TYPE.as_bytes().to_vec(),
        );
        let registries = loader.freeze().unwrap();
        let pack_types = PackTypes::from_registries(&registries);
        let (aaa, aaa_id) = pack_types.get("a:aaa").unwrap();
        assert_eq!(aaa_id, DIMENSION_TYPE.entries.len() as i32);
        assert_eq!(aaa.height, 128);
        let provider: Arc<dyn DimensionProvider> = Arc::new(Provider(vec![
            definition("ex:sky", "ex:sky_type", SKY_TYPE, &[("ex:cloud", BIOME)]),
            definition("ex:low", "a:aaa", "", &[]),
        ]));
        let custom = CustomDimensions::collect_in(&[provider], &[], &registries, &pack_types);
        assert!(custom.rejected().is_empty(), "{:?}", custom.rejected());
        assert_eq!(custom.dimensions()[0].type_id, aaa_id + 1);
        assert_eq!(custom.dimensions()[1].type_id, aaa_id);
        let biomes = custom.biomes();
        let glade = biomes.id("a:glade").unwrap();
        assert_eq!(glade as usize, WORLDGEN_BIOME.entries.len());
        assert_eq!(biomes.id("ex:cloud"), Some(glade + 1));
        assert_eq!(biomes.climate(glade).unwrap().temperature, 0.25);
        let sent: Vec<&str> = custom
            .registry_entries(WORLDGEN_BIOME.key)
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(sent, ["ex:cloud"], "pack biomes go with the registry");
    }
}

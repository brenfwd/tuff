use tuff_vanilla_data::biome::Spawner;
use tuff_vanilla_data::data_registry::WORLDGEN_BIOME;
use tuff_vanilla_data::entity::MobCategory;

use crate::chunk::ceil_log2;

/// What the weather, snow, ice and fire read from a biome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiomeClimate {
    pub has_precipitation: bool,
    pub temperature: f32,
    pub frozen: bool,
    pub increased_fire_burnout: Option<bool>,
}

impl BiomeClimate {
    pub fn vanilla(biome: u16) -> Self {
        use tuff_vanilla_data::biome::{Biome, TemperatureModifier};
        match Biome::by_id(biome) {
            Some(data) => Self {
                has_precipitation: data.has_precipitation,
                temperature: data.temperature,
                frozen: data.temperature_modifier == Some(TemperatureModifier::Frozen),
                increased_fire_burnout: data
                    .attribute("minecraft:gameplay/increased_fire_burnout")
                    .and_then(|value| value.as_bool()),
            },
            None => Self {
                has_precipitation: false,
                temperature: 0.8,
                frozen: false,
                increased_fire_burnout: None,
            },
        }
    }
}

/// A custom biome, resolved.
#[derive(Debug)]
pub struct CustomBiome {
    pub key: &'static str,
    pub json: String,
    pub nbt: Vec<u8>,
    pub climate: BiomeClimate,
    pub spawns: Vec<(MobCategory, &'static [Spawner])>,
}

/// Every biome by id: the vanilla ones in registry order, then the custom
/// ones in the order they were first defined. Built once at startup and
/// never changed.
#[derive(Debug, Default)]
pub struct Biomes {
    custom: Vec<CustomBiome>,
    /// Names nothing defines, kept under ids after the custom ones so an
    /// offline conversion can round-trip biomes it has no definition for.
    preserved: Option<std::sync::RwLock<Vec<&'static str>>>,
}

static VANILLA_BIOMES: Biomes = Biomes {
    custom: Vec::new(),
    preserved: None,
};

/// The most unknown biome names a preserving registry keeps; later ones
/// fall back to plains.
const MAX_PRESERVED: usize = 4096;

/// The most biomes clients may know: custom biomes must not widen the
/// global biome palette chunk sections are sent with.
pub fn max_biomes() -> usize {
    1 << ceil_log2(WORLDGEN_BIOME.entries.len())
}

impl Biomes {
    /// The vanilla biomes alone.
    pub fn vanilla() -> &'static Biomes {
        &VANILLA_BIOMES
    }

    /// The vanilla biomes, plus an id for every unknown name looked up, so
    /// that decoding and re-encoding keeps biomes no definition is loaded
    /// for (offline world conversion).
    pub fn preserving() -> Self {
        Self {
            custom: Vec::new(),
            preserved: Some(Default::default()),
        }
    }

    fn preserved_id(&self, name: &str) -> Option<u16> {
        let preserved = self.preserved.as_ref()?;
        let base = self.len();
        let lookup = |names: &[&'static str]| {
            names
                .iter()
                .position(|known| *known == name)
                .map(|index| (base + index) as u16)
        };
        if let Some(id) = lookup(&preserved.read().unwrap_or_else(|e| e.into_inner())) {
            return Some(id);
        }
        let mut names = preserved.write().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = lookup(&names) {
            return Some(id);
        }
        if names.len() >= MAX_PRESERVED || base + names.len() >= u16::MAX as usize {
            return None;
        }
        names.push(Box::leak(name.to_owned().into_boxed_str()));
        Some((base + names.len() - 1) as u16)
    }

    pub fn len(&self) -> usize {
        WORLDGEN_BIOME.entries.len() + self.custom.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn id(&self, name: &str) -> Option<u16> {
        crate::chunk::biome_id(name).or_else(|| {
            self.custom
                .iter()
                .position(|entry| entry.key == name)
                .map(|index| (WORLDGEN_BIOME.entries.len() + index) as u16)
                .or_else(|| self.preserved_id(name))
        })
    }

    /// The biome's name; plains for an id nothing defines.
    pub fn name(&self, id: u16) -> &'static str {
        match (id as usize).checked_sub(WORLDGEN_BIOME.entries.len()) {
            None => crate::chunk::biome_name(id),
            Some(index) => match self.custom.get(index) {
                Some(entry) => entry.key,
                None => self
                    .preserved
                    .as_ref()
                    .and_then(|names| {
                        names
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(index - self.custom.len())
                            .copied()
                    })
                    .unwrap_or("minecraft:plains"),
            },
        }
    }

    /// The climate of a custom biome; `None` for vanilla ids.
    pub fn climate(&self, id: u16) -> Option<BiomeClimate> {
        self.custom_biome(id).map(|entry| entry.climate)
    }

    /// The custom biome with `id`; `None` for vanilla ids.
    pub fn custom_biome(&self, id: u16) -> Option<&CustomBiome> {
        let index = (id as usize).checked_sub(WORLDGEN_BIOME.entries.len())?;
        self.custom.get(index)
    }

    /// The natural spawn list of `category` in the custom biome `name`;
    /// `None` for a name that is not a custom biome.
    pub fn spawners(&self, name: &str, category: MobCategory) -> Option<&'static [Spawner]> {
        let entry = self.custom.iter().find(|entry| entry.key == name)?;
        Some(
            entry
                .spawns
                .iter()
                .find(|(c, _)| *c == category)
                .map_or(&[], |(_, list)| *list),
        )
    }

    /// Whether `id` is a custom biome.
    pub fn is_custom(&self, id: u16) -> bool {
        (id as usize) >= WORLDGEN_BIOME.entries.len() && (id as usize) < self.len()
    }

    /// The custom biomes' keys and network NBT, in id order.
    pub fn custom_entries(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        self.custom
            .iter()
            .map(|entry| (entry.key, entry.nbt.as_slice()))
    }

    /// Appends a custom biome and returns its id.
    pub fn push(&mut self, biome: CustomBiome) -> u16 {
        self.custom.push(biome);
        (self.len() - 1) as u16
    }

    /// Drops the custom biomes added after the first `len` biomes.
    pub fn truncate(&mut self, len: usize) {
        self.custom
            .truncate(len.saturating_sub(WORLDGEN_BIOME.entries.len()));
    }
}

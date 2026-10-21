//! Biomes a data pack adds, and multi-noise parameter lists from packs, as
//! a generator sees them: names resolve to the ids the frozen biome
//! registry gives them (after every built-in biome), and each added biome
//! carries the climate, carvers and features generation reads.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tuff_vanilla_data::biome::{Biome, NaturalSpawns, TemperatureModifier};

use crate::carver::CarverKind;
use crate::json::{Result, qualify};

pub const BIOME: &str = "minecraft:worldgen/biome";
pub const PARAMETER_LIST: &str = "minecraft:worldgen/multi_noise_biome_source_parameter_list";

/// One pack-added biome.
pub struct AddedBiome {
    pub id: u16,
    pub name: String,
    pub data: &'static Biome,
    pub carvers: Box<[(usize, Arc<dyn CarverKind>)]>,
}

/// The pack-added biomes and pack parameter lists of one world.
#[derive(Default)]
pub struct PackBiomes {
    added: Vec<AddedBiome>,
    /// Parameter list id -> the built-in preset it names.
    parameter_lists: HashMap<String, String>,
    /// Unknown names resolve to plains (checks made before data packs
    /// load).
    lenient: bool,
}

impl PackBiomes {
    /// No pack biomes: names resolve through the generated table only.
    pub fn none() -> &'static PackBiomes {
        static NONE: OnceLock<PackBiomes> = OnceLock::new();
        NONE.get_or_init(PackBiomes::default)
    }

    /// Accepts any name, for checks made before data packs load.
    pub fn lenient() -> &'static PackBiomes {
        static LENIENT: OnceLock<PackBiomes> = OnceLock::new();
        LENIENT.get_or_init(|| PackBiomes {
            lenient: true,
            ..PackBiomes::default()
        })
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.parameter_lists.is_empty()
    }

    pub fn id(&self, name: &str) -> Option<u16> {
        let name = qualify(name);
        tuff_world_model::chunk::biome_id(&name)
            .or_else(|| {
                self.added
                    .iter()
                    .find(|biome| biome.name == name)
                    .map(|biome| biome.id)
            })
            .or_else(|| self.lenient.then(tuff_world_model::chunk::plains))
    }

    pub fn name(&self, id: u16) -> &str {
        match self.added(id) {
            Some(biome) => &biome.name,
            None => tuff_world_model::chunk::biome_name(id),
        }
    }

    pub fn added(&self, id: u16) -> Option<&AddedBiome> {
        self.added.iter().find(|biome| biome.id == id)
    }

    pub fn added_biomes(&self) -> &[AddedBiome] {
        &self.added
    }

    /// The biome data of `id`; plains for an id nothing defines.
    pub fn data(&self, id: u16) -> &'static Biome {
        if let Some(biome) = self.added(id) {
            return biome.data;
        }
        Biome::by_id(id)
            .or_else(|| Biome::get("minecraft:plains"))
            .expect("plains biome")
    }

    /// The built-in climate points a parameter list `id` stands for.
    pub fn parameter_list(
        &self,
        id: &str,
    ) -> Option<&'static [tuff_vanilla_data::worldgen::ClimatePoint]> {
        let id = qualify(id);
        let preset = self
            .parameter_lists
            .get(&id)
            .map_or(id.as_str(), String::as_str);
        tuff_vanilla_data::worldgen::parameter_list(preset).or_else(|| {
            self.lenient
                .then(|| tuff_vanilla_data::worldgen::parameter_list("minecraft:overworld"))
                .flatten()
        })
    }

    /// The pack entries of `registries`; `Err` is the registry, id, pack
    /// and message of an unreadable entry.
    pub fn from_registries(
        registries: &tuff_item::registries::Registries,
    ) -> std::result::Result<Self, super::features::FeatureError> {
        let mut out = Self::default();
        if let Some(lists) = registries.get(PARAMETER_LIST).filter(|r| r.modified()) {
            for (id, entry) in lists.iter() {
                let raw = entry.value();
                let Some(json) = &raw.json else {
                    continue;
                };
                let preset = parameter_list_preset(json)
                    .map_err(|e| (PARAMETER_LIST, id.to_owned(), raw.pack.clone(), e))?;
                out.parameter_lists.insert(qualify(id), preset);
            }
        }
        if let Some(biomes) = registries.get(BIOME).filter(|r| r.modified()) {
            for (index, (id, entry, _)) in biomes.network_iter().enumerate() {
                let name = qualify(id);
                if tuff_world_model::chunk::biome_id(&name).is_some() {
                    continue;
                }
                let raw = entry.value();
                let Some(json) = &raw.json else {
                    continue;
                };
                let added = decode(index as u16, &name, json)
                    .map_err(|e| (BIOME, id.to_owned(), raw.pack.clone(), e))?;
                out.added.push(added);
            }
        }
        Ok(out)
    }
}

fn parameter_list_preset(json: &Value) -> Result<String> {
    let preset = json
        .get("preset")
        .and_then(Value::as_str)
        .ok_or("a parameter list needs a preset")?;
    let preset = qualify(preset);
    if tuff_vanilla_data::worldgen::parameter_list(&preset).is_none() {
        return Err(format!("unknown parameter list preset {preset}"));
    }
    Ok(preset)
}

fn number(json: &Value, key: &str) -> Result<f32> {
    json.get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .ok_or_else(|| format!("{key} must be a number"))
}

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn carver_names(json: &Value) -> Result<Vec<String>> {
    let names = match json.get("carvers") {
        None => return Ok(Vec::new()),
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| name.as_str().ok_or_else(|| format!("bad carver {name}")))
            .collect::<Result<Vec<_>>>()?,
        Some(other) => return Err(format!("bad carvers {other}")),
    };
    names
        .into_iter()
        .map(|name| {
            if name.starts_with('#') {
                return Err(format!("carver tags are not supported in biomes: {name}"));
            }
            Ok(qualify(name))
        })
        .collect()
}

fn decode(id: u16, name: &str, json: &Value) -> Result<AddedBiome> {
    let has_precipitation = json
        .get("has_precipitation")
        .and_then(Value::as_bool)
        .ok_or("has_precipitation must be a boolean")?;
    let temperature = number(json, "temperature")?;
    let downfall = number(json, "downfall")?;
    let temperature_modifier = match json.get("temperature_modifier").and_then(Value::as_str) {
        None | Some("none") => None,
        Some("frozen") => Some(TemperatureModifier::Frozen),
        Some(other) => return Err(format!("unknown temperature_modifier {other}")),
    };
    let names = carver_names(json)?;
    let mut carvers = Vec::with_capacity(names.len());
    for (index, carver) in names.iter().enumerate() {
        let kind = crate::carver::configured(carver)
            .ok_or_else(|| format!("unknown configured carver {carver}"))?;
        carvers.push((index, kind));
    }
    let features: Vec<&'static [&'static str]> = super::features::biome_features(json, id)?
        .0
        .into_iter()
        .map(|step| {
            let step: Vec<&'static str> = step
                .into_iter()
                .map(|feature| &*Box::leak(feature.into_boxed_str()))
                .collect();
            &*Box::leak(step.into_boxed_slice())
        })
        .collect();
    let carver_names: Vec<&'static str> = names
        .into_iter()
        .map(|carver| &*Box::leak(carver.into_boxed_str()))
        .collect();
    let plains = Biome::get("minecraft:plains").expect("plains biome");
    let data = leak(Biome {
        name: Box::leak(name.to_owned().into_boxed_str()),
        has_precipitation,
        temperature,
        temperature_modifier,
        downfall,
        effects: plains.effects,
        carvers: Box::leak(carver_names.into_boxed_slice()),
        features: Box::leak(features.into_boxed_slice()),
        attributes: &[],
        natural_spawns: NaturalSpawns {
            categories: &[],
            costs: &[],
            creature_probability: None,
        },
    });
    Ok(AddedBiome {
        id,
        name: name.to_owned(),
        data,
        carvers: carvers.into_boxed_slice(),
    })
}

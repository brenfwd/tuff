//! Configured features, placed features and biome feature lists from data
//! pack JSON. Features are parsed from JSON at generator build for the
//! built-in data too, so a pack entry only replaces the text a name
//! resolves to; entries no pack touched keep the generated tables.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::json::{Result, qualify};

pub const CONFIGURED_FEATURE: &str = "minecraft:worldgen/feature";
pub const PLACED_FEATURE: &str = "minecraft:worldgen/placed_feature";
pub const BIOME: &str = "minecraft:worldgen/biome";

/// A bad entry: registry, id, pack and message.
pub type FeatureError = (&'static str, String, Option<Arc<str>>, String);

/// Pack configured and placed features by id, and pack biome feature
/// lists (per step, placed feature ids) by biome id.
#[derive(Default)]
pub struct FeatureSources {
    pub configured: HashMap<String, Arc<Value>>,
    pub placed: HashMap<String, Arc<Value>>,
    pub biomes: HashMap<u16, Vec<Vec<String>>>,
}

impl FeatureSources {
    pub fn is_empty(&self) -> bool {
        self.configured.is_empty() && self.placed.is_empty() && self.biomes.is_empty()
    }

    /// Adds a biome's JSON; fails on a feature list this server cannot
    /// read.
    pub fn insert_biome(&mut self, id: &str, json: &Value) -> Result<()> {
        let id = qualify(id);
        let biome =
            tuff_world_model::chunk::biome_id(&id).ok_or_else(|| format!("unknown biome {id}"))?;
        self.insert_biome_id(biome, json)
    }

    /// Adds the JSON of the biome with network id `biome`.
    pub fn insert_biome_id(&mut self, biome: u16, json: &Value) -> Result<()> {
        let (steps, inline) = biome_features(json, biome)?;
        for (name, feature) in inline {
            self.placed.insert(name, Arc::new(feature));
        }
        self.biomes.insert(biome, steps);
        Ok(())
    }

    /// The pack entries of `registries`; `Err` names the registry and id
    /// of an unreadable biome.
    pub fn from_registries(
        registries: &tuff_item::registries::Registries,
    ) -> std::result::Result<Self, FeatureError> {
        let mut out = Self::default();
        for key in [CONFIGURED_FEATURE, PLACED_FEATURE, BIOME] {
            let Some(registry) = registries.get(key) else {
                continue;
            };
            if !registry.modified() {
                continue;
            }
            for (index, (id, entry, _)) in registry.network_iter().enumerate() {
                let raw = entry.value();
                let Some(json) = &raw.json else {
                    continue;
                };
                match key {
                    CONFIGURED_FEATURE => {
                        out.configured.insert(qualify(id), json.clone());
                    }
                    PLACED_FEATURE => {
                        out.placed.insert(qualify(id), json.clone());
                    }
                    _ => out
                        .insert_biome_id(index as u16, json)
                        .map_err(|e| (key, id.to_owned(), raw.pack.clone(), e))?,
                }
            }
        }
        Ok(out)
    }
}

/// Per step placed feature ids, and the inline placed features named in
/// them.
pub type BiomeFeatures = (Vec<Vec<String>>, Vec<(String, Value)>);

/// A biome's `features`: per generation step, a placed feature holder
/// set (an id, a `#tag`, or a list of ids and inline placed features).
/// Tags stay `#`-prefixed until the decorator expands them with the tags
/// in effect; inline features get names of their own, returned with
/// their JSON.
pub fn biome_features(json: &Value, biome: u16) -> Result<BiomeFeatures> {
    let Some(steps) = json.get("features") else {
        return Ok((Vec::new(), Vec::new()));
    };
    let steps = steps.as_array().ok_or("features must be a list")?;
    let mut out = Vec::with_capacity(steps.len());
    let mut inline = Vec::new();
    for (step_index, step) in steps.iter().enumerate() {
        let ids = match step {
            Value::String(id) => vec![step_id(id)],
            Value::Array(ids) => ids
                .iter()
                .enumerate()
                .map(|(i, id)| match id {
                    Value::String(id) => Ok(step_id(id)),
                    Value::Object(_) => {
                        let name = format!("tuff:inline/biome_{biome}/{step_index}/{i}");
                        inline.push((name.clone(), id.clone()));
                        Ok(name)
                    }
                    _ => Err(format!("bad placed feature {id}")),
                })
                .collect::<Result<Vec<_>>>()?,
            _ => return Err(format!("bad feature step {step}")),
        };
        out.push(ids);
    }
    Ok((out, inline))
}

fn step_id(id: &str) -> String {
    match id.strip_prefix('#') {
        Some(tag) => format!("#{}", qualify(tag)),
        None => qualify(id),
    }
}

/// The placed feature ids a biome step entry stands for: a `#tag`'s
/// members in the tags in effect, else the id itself.
pub fn expand(name: &str) -> Vec<String> {
    match name.strip_prefix('#') {
        Some(tag) => tuff_datapack::tags::with(|tags| {
            tags.get("minecraft:worldgen/placed_feature", tag)
                .map(<[String]>::to_vec)
                .unwrap_or_default()
        }),
        None => vec![name.to_owned()],
    }
}

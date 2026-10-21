//! Structures, structure sets, template pools and processor lists from
//! data pack JSON. Pack structures and sets decode into the generated
//! shapes (leaked once, registries are start-only) and replace or join the
//! built-in ones in each generator's structure list; pack pools and
//! processor lists shadow the names the jigsaw and template code look up
//! while a generator's structures run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tuff_vanilla_data::structure::{
    ExclusionZone, FrequencyReduction, Spread, SpreadType, Structure, StructurePlacement,
    StructureSet, TerrainAdaptation,
};

use crate::json::{Result, qualify};
use crate::structure::pool::Pool;
use crate::structure::processor::ProcessorList;

pub const STRUCTURE: &str = "minecraft:worldgen/structure";
pub const STRUCTURE_SET: &str = "minecraft:worldgen/structure_set";
pub const TEMPLATE_POOL: &str = "minecraft:worldgen/template_pool";
pub const PROCESSOR_LIST: &str = "minecraft:worldgen/processor_list";

/// The fields every structure has beside its type's own.
const SHARED_FIELDS: &[&str] = &[
    "type",
    "biomes",
    "step",
    "spawn_overrides",
    "terrain_adaptation",
];

/// A bad entry: registry, id, pack and message.
pub type StructureError = (&'static str, String, Option<Arc<str>>, String);

/// The pack entries of the structure registries.
#[derive(Default)]
pub struct StructureSources {
    pub structures: Vec<&'static Structure>,
    pub sets: Vec<&'static StructureSet>,
    pub pools: HashMap<String, Arc<Value>>,
    pub processors: HashMap<String, Arc<Value>>,
    parsed_pools: Mutex<HashMap<String, Option<Arc<Pool>>>>,
    parsed_processors: Mutex<HashMap<String, ProcessorList>>,
}

impl StructureSources {
    pub fn is_empty(&self) -> bool {
        self.structures.is_empty()
            && self.sets.is_empty()
            && self.pools.is_empty()
            && self.processors.is_empty()
    }

    pub fn structure(&self, name: &str) -> Option<&'static Structure> {
        self.structures.iter().copied().find(|s| s.name == name)
    }

    pub fn set(&self, name: &str) -> Option<&'static StructureSet> {
        self.sets.iter().copied().find(|s| s.name == name)
    }

    /// The pack entries of `registries`.
    pub fn from_registries(
        registries: &tuff_item::registries::Registries,
    ) -> std::result::Result<Self, StructureError> {
        let mut out = Self::default();
        let entries = |key: &'static str| {
            registries
                .get(key)
                .filter(|r| r.modified())
                .into_iter()
                .flat_map(|r| r.iter())
                .filter_map(move |(id, entry)| {
                    let raw = entry.value();
                    Some((key, qualify(id), raw.pack.clone(), raw.json.clone()?))
                })
        };
        for (_, id, _, json) in entries(TEMPLATE_POOL) {
            out.pools.insert(id, json);
        }
        for (_, id, _, json) in entries(PROCESSOR_LIST) {
            out.processors.insert(id, json);
        }
        for (key, id, pack, json) in entries(STRUCTURE) {
            let structure = decode_structure(&id, &json).map_err(|e| (key, id.clone(), pack, e))?;
            out.structures.push(structure);
        }
        let exists = |name: &str| {
            Structure::get(name).is_some() || out.structures.iter().any(|s| s.name == name)
        };
        let set_exists = |name: &str| {
            StructureSet::get(name).is_some()
                || registries.exists(STRUCTURE_SET, name).unwrap_or(false)
        };
        let mut sets = Vec::new();
        for (key, id, pack, json) in entries(STRUCTURE_SET) {
            let set = decode_set(&id, &json, &exists, &set_exists)
                .map_err(|e| (key, id.clone(), pack, e))?;
            sets.push(leak(set));
        }
        out.sets = sets;
        Ok(out)
    }
}

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn leak_str(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn object<'v>(value: &'v Value, what: &str) -> Result<&'v Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))
}

fn int(map: &Map<String, Value>, key: &str) -> Result<i32> {
    map.get(key)
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| format!("{key} is not an integer"))
}

/// A biome holder set: `#tag`, one id or a list of ids. Returns the ids
/// (the tag's built-in members for a tag) and the tag.
fn biomes(value: &Value) -> Result<(Vec<&'static str>, Option<&'static str>)> {
    match value {
        Value::String(name) => match name.strip_prefix('#') {
            Some(tag) => {
                let tag = qualify(tag);
                let members = tuff_datapack::tags::with(|tags| {
                    tags.get("minecraft:worldgen/biome", &tag)
                        .map(<[String]>::to_vec)
                        .unwrap_or_default()
                });
                Ok((
                    members.iter().map(|m| leak_str(m)).collect(),
                    Some(leak_str(&tag)),
                ))
            }
            None => Ok((vec![leak_str(&qualify(name))], None)),
        },
        Value::Array(ids) => Ok((
            ids.iter()
                .map(|id| {
                    id.as_str()
                        .map(|id| leak_str(&qualify(id)))
                        .ok_or_else(|| "biome ids are strings".to_owned())
                })
                .collect::<Result<_>>()?,
            None,
        )),
        _ => Err("biomes must be a tag, an id or a list".to_owned()),
    }
}

/// A pack structure as the generator's structure list holds it.
pub fn decode_structure(name: &str, json: &Value) -> Result<&'static Structure> {
    let map = object(json, "structure")?;
    let kind = qualify(
        map.get("type")
            .and_then(Value::as_str)
            .ok_or("structure has no type")?,
    );
    if !crate::structure::kinds().contains_key(kind.as_str()) {
        return Err(format!("unknown structure type {kind}"));
    }
    let step_name = map
        .get("step")
        .and_then(Value::as_str)
        .ok_or("structure has no step")?;
    let step = tuff_vanilla_data::world::DECORATION_STEPS
        .iter()
        .position(|s| *s == step_name)
        .ok_or_else(|| format!("unknown decoration step {step_name}"))?;
    let terrain_adaptation = match map.get("terrain_adaptation").map(Value::as_str) {
        None | Some(Some("none")) => TerrainAdaptation::None,
        Some(Some("beard_thin")) => TerrainAdaptation::BeardThin,
        Some(Some("beard_box")) => TerrainAdaptation::BeardBox,
        Some(Some("bury")) => TerrainAdaptation::Bury,
        Some(Some("encapsulate")) => TerrainAdaptation::Encapsulate,
        Some(other) => return Err(format!("unknown terrain adaptation {other:?}")),
    };
    let (biome_list, biome_tag) = biomes(map.get("biomes").ok_or("structure has no biomes")?)?;
    let spawn_overrides = map.get("spawn_overrides").cloned().unwrap_or_default();
    if !spawn_overrides.is_object() && !spawn_overrides.is_null() {
        return Err("spawn_overrides is not an object".to_owned());
    }
    let config: Map<String, Value> = map
        .iter()
        .filter(|(key, _)| !SHARED_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let config = Value::Object(config);
    let structure = leak(Structure {
        name: leak_str(name),
        kind: leak_str(&kind),
        biomes: Box::leak(biome_list.into_boxed_slice()),
        biome_tag,
        step,
        terrain_adaptation,
        spawn_overrides: leak_str(&if spawn_overrides.is_null() {
            "{}".to_owned()
        } else {
            spawn_overrides.to_string()
        }),
        config: leak_str(&config.to_string()),
    });
    let factory = crate::structure::kinds()[structure.kind];
    factory(structure, &config)?;
    Ok(structure)
}

fn spread(map: &Map<String, Value>, set_exists: &dyn Fn(&str) -> bool) -> Result<Spread> {
    let locate_offset = match map.get("locate_offset") {
        None => [0; 3],
        Some(value) => {
            let list = value.as_array().ok_or("locate_offset is a list")?;
            if list.len() != 3 {
                return Err("locate_offset has three values".to_owned());
            }
            let mut out = [0; 3];
            for (o, v) in out.iter_mut().zip(list) {
                *o = v.as_i64().ok_or("locate_offset values are integers")? as i32;
            }
            out
        }
    };
    let frequency_reduction = match map.get("frequency_reduction_method").map(Value::as_str) {
        None | Some(Some("default")) => FrequencyReduction::Default,
        Some(Some("legacy_type_1")) => FrequencyReduction::LegacyType1,
        Some(Some("legacy_type_2")) => FrequencyReduction::LegacyType2,
        Some(Some("legacy_type_3")) => FrequencyReduction::LegacyType3,
        Some(other) => return Err(format!("unknown frequency reduction {other:?}")),
    };
    let frequency = match map.get("frequency") {
        None => 1.0,
        Some(v) => v.as_f64().ok_or("frequency is a number")? as f32,
    };
    let exclusion_zone = match map.get("exclusion_zone") {
        None => None,
        Some(zone) => {
            let zone = object(zone, "exclusion_zone")?;
            let other = qualify(
                zone.get("other_set")
                    .and_then(Value::as_str)
                    .ok_or("exclusion zone has no other_set")?,
            );
            if !set_exists(&other) {
                return Err(format!("unknown structure set {other}"));
            }
            Some(ExclusionZone {
                other_set: leak_str(&other),
                chunk_count: int(zone, "chunk_count")?,
            })
        }
    };
    Ok(Spread {
        locate_offset,
        frequency_reduction,
        frequency,
        salt: int(map, "salt")?,
        exclusion_zone,
    })
}

/// A pack structure set.
pub fn decode_set(
    name: &str,
    json: &Value,
    structure_exists: &dyn Fn(&str) -> bool,
    set_exists: &dyn Fn(&str) -> bool,
) -> Result<StructureSet> {
    let map = object(json, "structure set")?;
    let mut structures = Vec::new();
    for entry in map
        .get("structures")
        .and_then(Value::as_array)
        .ok_or("structures is not a list")?
    {
        let entry = object(entry, "structure set entry")?;
        let structure = qualify(
            entry
                .get("structure")
                .and_then(Value::as_str)
                .ok_or("entry has no structure")?,
        );
        if !structure_exists(&structure) {
            return Err(format!("unknown structure {structure}"));
        }
        let weight = int(entry, "weight")?;
        if weight <= 0 {
            return Err("structure weights are positive".to_owned());
        }
        structures.push((leak_str(&structure), weight));
    }
    if structures.is_empty() {
        return Err("a structure set needs a structure".to_owned());
    }
    let placement = object(map.get("placement").ok_or("no placement")?, "placement")?;
    let kind = placement
        .get("type")
        .and_then(Value::as_str)
        .ok_or("placement has no type")?;
    let placement = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "random_spread" => {
            let spacing = int(placement, "spacing")?;
            let separation = int(placement, "separation")?;
            if spacing <= separation {
                return Err("spacing must exceed separation".to_owned());
            }
            let spread_type = match placement.get("spread_type").map(Value::as_str) {
                None | Some(Some("linear")) => SpreadType::Linear,
                Some(Some("triangular")) => SpreadType::Triangular,
                Some(other) => return Err(format!("unknown spread type {other:?}")),
            };
            StructurePlacement::RandomSpread {
                spread: spread(placement, set_exists)?,
                spacing,
                separation,
                spread_type,
            }
        }
        "concentric_rings" => {
            let (preferred, tag) = biomes(
                placement
                    .get("preferred_biomes")
                    .ok_or("no preferred_biomes")?,
            )?;
            StructurePlacement::ConcentricRings {
                spread: spread(placement, set_exists)?,
                distance: int(placement, "distance")?,
                ring_spread: int(placement, "spread")?,
                count: int(placement, "count")?,
                preferred_biomes: Box::leak(preferred.into_boxed_slice()),
                preferred_tag: tag,
            }
        }
        "dimension_origin" => StructurePlacement::DimensionOrigin {
            spread: spread(placement, set_exists)?,
        },
        other => return Err(format!("unknown structure placement {other}")),
    };
    Ok(StructureSet {
        name: leak_str(name),
        structures: Box::leak(structures.into_boxed_slice()),
        placement,
    })
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<StructureSources>>> = const { RefCell::new(None) };
}

/// Runs `f` with `sources` shadowing the template pools and processor
/// lists the structure code looks up.
pub fn scope<R>(sources: Option<&Arc<StructureSources>>, f: impl FnOnce() -> R) -> R {
    let Some(sources) = sources else {
        return f();
    };
    let previous = CURRENT.with(|current| current.replace(Some(sources.clone())));
    let result = f();
    CURRENT.with(|current| *current.borrow_mut() = previous);
    result
}

fn current() -> Option<Arc<StructureSources>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// The template pool `name` of the sources in scope; `None` when there are
/// none (or they change nothing structural), so the shared cache serves.
pub(crate) fn pool(name: &str) -> Option<Option<Arc<Pool>>> {
    let sources = current()?;
    if sources.pools.is_empty() && sources.processors.is_empty() {
        return None;
    }
    if let Some(found) = sources.parsed_pools.lock().ok()?.get(name) {
        return Some(found.clone());
    }
    let pool = match sources.pools.get(name) {
        Some(json) => Some(Arc::new(Pool::parse_named(name, json).unwrap_or_else(
            |e| {
                log::error!("data pack template pool {name}: {e}");
                Pool::empty(name)
            },
        ))),
        None => tuff_vanilla_data::structure::template_pool(name).map(|text| {
            let value: Value = serde_json::from_str(text).expect("template pool is JSON");
            Arc::new(
                Pool::parse_named(name, &value)
                    .unwrap_or_else(|e| panic!("template pool {name}: {e}")),
            )
        }),
    };
    sources
        .parsed_pools
        .lock()
        .ok()?
        .insert(name.to_owned(), pool.clone());
    Some(pool)
}

/// The processor list `name` of the sources in scope, when they have
/// their own.
pub(crate) fn processors(name: &str) -> Option<Result<ProcessorList>> {
    let sources = current()?;
    let json = sources.processors.get(name)?;
    if let Some(found) = sources.parsed_processors.lock().ok()?.get(name) {
        return Some(Ok(found.clone()));
    }
    let list = ProcessorList::parse(json);
    if let Ok(list) = &list {
        sources
            .parsed_processors
            .lock()
            .ok()?
            .insert(name.to_owned(), list.clone());
    }
    Some(list)
}

/// Parses every pack pool and processor list, for validation.
pub fn check(
    sources: &Arc<StructureSources>,
) -> std::result::Result<(), (&'static str, String, String)> {
    scope(Some(sources), || {
        let mut names: Vec<&String> = sources.processors.keys().collect();
        names.sort();
        for name in names {
            ProcessorList::parse(&sources.processors[name])
                .map_err(|e| (PROCESSOR_LIST, name.clone(), e))?;
        }
        let mut names: Vec<&String> = sources.pools.keys().collect();
        names.sort();
        for name in names {
            Pool::parse_named(name, &sources.pools[name])
                .map_err(|e| (TEMPLATE_POOL, name.clone(), e))?;
        }
        Ok(())
    })
}

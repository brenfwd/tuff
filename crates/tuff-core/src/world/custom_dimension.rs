//! Dimensions features add to every world at startup, beside the ones the
//! level data lists. A [`DimensionProvider`] describes each dimension with
//! data pack JSON for its dimension type and biomes, and makes its chunk
//! generator. Everything is resolved once, before the world's dimensions
//! are built, into an immutable [`CustomDimensions`]: custom dimension
//! types and biomes get network ids after the vanilla entries and are sent
//! to clients with their data, so a vanilla client needs no resource pack.
//! Plugin dimensions are never written to the level data.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::biome::{NaturalSpawns, SpawnCost, Spawner};
use tuff_vanilla_data::data_registry::{DIMENSION_TYPE, WORLDGEN_BIOME};
use tuff_vanilla_data::dimension_type::DimensionType;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::{Activity, EntityType};
use tuff_vanilla_data::timeline::Timeline;
use tuff_vanilla_data::world::{BedCondition, BedRule, DEFAULT_ENVIRONMENT, Environment};

use crate::world::generator::ChunkGenerator;
use tuff_item::registries::codecs::dimension_type;
use tuff_world_model::biomes::{BiomeClimate, Biomes, CustomBiome, max_biomes};

mod pack;

pub use pack::PackTypes;
use pack::add_pack_biomes;

/// Adds dimensions to every world. Asked once per world start, before the
/// world's dimensions exist.
pub trait DimensionProvider: Send + Sync {
    /// Called once per world start, before [`Self::dimensions`], with what
    /// the server resolved at startup.
    fn attach(&self, _ctx: &ProviderContext) {}

    /// The dimensions this provider adds.
    fn dimensions(&self) -> Vec<DimensionDefinition>;

    /// The generator of `dimension`, one of the keys [`Self::dimensions`]
    /// returned. `biomes` names the ids of every biome, custom ones
    /// included.
    fn generator(
        &self,
        dimension: &str,
        seed: i64,
        biomes: &Arc<Biomes>,
    ) -> Arc<dyn ChunkGenerator>;
}

/// What the server resolved at startup, handed to every provider before
/// it is asked for dimensions.
#[derive(Clone)]
pub struct ProviderContext {
    pub config_store: Arc<dyn tuff_storage::config::ConfigStore>,
    /// The `tuff.*` and feature settings, saved values and `--config`
    /// overrides applied.
    pub settings: crate::config::ConfigStore,
    pub properties: tuff_storage::admin::ServerProperties,
}

/// One dimension a provider adds.
#[derive(Clone, Debug)]
pub struct DimensionDefinition {
    /// The dimension's key, e.g. `example:sky`.
    pub key: String,
    /// The key of its dimension type.
    pub dimension_type: String,
    /// The dimension type as data pack JSON; empty to use the vanilla
    /// dimension type `dimension_type` names.
    pub type_json: String,
    /// Biomes the dimension's generator uses that are not vanilla ones.
    pub biomes: Vec<BiomeDefinition>,
    /// Whether mobs spawn on their own here: in chunks near players, with
    /// new chunks, and from the overworld-only spawners (phantoms, patrols,
    /// wandering traders).
    pub natural_spawning: bool,
    /// Whether the provider wants this to be the spawn dimension: where new
    /// players appear and respawns fall back to. The operator's
    /// `tuff.world.spawnDimension` overrides it; of several claims the
    /// first wins.
    pub claims_spawn: bool,
}

/// A biome as data pack JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BiomeDefinition {
    pub key: String,
    pub json: String,
}

fn add_biome(biomes: &mut Biomes, definition: &BiomeDefinition) -> Result<u16, String> {
    check_key(&definition.key)?;
    if tuff_world_model::chunk::biome_id(&definition.key).is_some() {
        return Err(format!("biome {} is a vanilla biome", definition.key));
    }
    let value = parse(&definition.json, &definition.key)?;
    if let Some(id) = biomes.id(&definition.key) {
        let existing = biomes.custom_biome(id).expect("custom id");
        if parse(&existing.json, existing.key)? != value {
            return Err(format!(
                "biome {} is defined twice, differently",
                definition.key
            ));
        }
        return Ok(id);
    }
    if biomes.len() >= max_biomes() {
        return Err(format!(
            "biome {} is one too many: at most {} biomes fit",
            definition.key,
            max_biomes()
        ));
    }
    check_biome(&value, &definition.key)?;
    let nbt = network_nbt(&value, &definition.key)?;
    let climate = BiomeClimate {
        has_precipitation: value["has_precipitation"].as_bool().unwrap_or(false),
        temperature: value["temperature"].as_f64().unwrap_or(0.8) as f32,
        frozen: value["temperature_modifier"].as_str() == Some("frozen"),
        increased_fire_burnout: value["attributes"]["minecraft:gameplay/increased_fire_burnout"]
            .as_bool(),
    };
    let spawns = parse_spawners(&value, &definition.key)?;
    Ok(biomes.push(CustomBiome {
        climate,
        spawns,
        key: leak(&definition.key),
        json: definition.json.clone(),
        nbt,
    }))
}

/// A custom dimension type, resolved.
#[derive(Debug)]
pub struct CustomType {
    pub kind: &'static DimensionType,
    pub environment: &'static Environment,
    /// The `minecraft:dimension_type` network id.
    pub id: i32,
    json: String,
    nbt: Vec<u8>,
}

impl CustomType {
    /// The type's definition.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.json).ok()
    }
}

/// A custom dimension, resolved.
pub struct Resolved {
    pub key: String,
    pub kind: &'static DimensionType,
    pub environment: &'static Environment,
    pub type_id: i32,
    pub natural_spawning: bool,
    pub claims_spawn: bool,
    provider: Arc<dyn DimensionProvider>,
}

impl Resolved {
    pub fn generator(&self, seed: i64, biomes: &Arc<Biomes>) -> Arc<dyn ChunkGenerator> {
        self.provider.generator(&self.key, seed, biomes)
    }
}

/// Every plugin dimension of a world, with the custom dimension types and
/// biomes they need. Immutable once built.
#[derive(Default)]
pub struct CustomDimensions {
    biomes: Arc<Biomes>,
    /// How many of the custom biomes data packs added; clients get those
    /// with the frozen registry, not as feature entries.
    pack_biomes: usize,
    types: Vec<CustomType>,
    dimensions: Vec<Resolved>,
    rejected: Vec<String>,
}

impl CustomDimensions {
    /// Asks every provider for its dimensions and resolves them. A
    /// dimension whose key is in `taken`, or that another provider already
    /// added, or whose data is invalid is left out, with its reason logged
    /// and kept in [`Self::rejected`].
    pub fn collect(providers: &[Arc<dyn DimensionProvider>], taken: &[&str]) -> Self {
        Self::collect_in(
            providers,
            taken,
            &tuff_item::registries::Registries::vanilla(),
            &PackTypes::vanilla(),
        )
    }

    /// Like [`Self::collect`] in a world whose data packs left
    /// `registries` and `pack_types`: biomes and dimension types the packs
    /// add come first, so feature ones get ids after them.
    pub fn collect_in(
        providers: &[Arc<dyn DimensionProvider>],
        taken: &[&str],
        registries: &tuff_item::registries::Registries,
        pack_types: &PackTypes,
    ) -> Self {
        let mut biomes = Biomes::default();
        let pack_biomes = add_pack_biomes(&mut biomes, registries);
        let mut types: Vec<CustomType> = Vec::new();
        let mut dimensions: Vec<Resolved> = Vec::new();
        let mut rejected = Vec::new();
        for provider in providers {
            for definition in provider.dimensions() {
                let taken = taken.contains(&definition.key.as_str())
                    || dimensions.iter().any(|d| d.key == definition.key);
                let resolved = if taken {
                    Err(format!("dimension {} already exists", definition.key))
                } else {
                    resolve(&definition, &mut biomes, &mut types, pack_types)
                };
                match resolved {
                    Ok((kind, environment, type_id)) => dimensions.push(Resolved {
                        key: definition.key,
                        kind,
                        environment,
                        type_id,
                        natural_spawning: definition.natural_spawning,
                        claims_spawn: definition.claims_spawn,
                        provider: provider.clone(),
                    }),
                    Err(err) => {
                        log::error!("plugin dimension {} is left out: {err}", definition.key);
                        rejected.push(err);
                    }
                }
            }
        }
        Self {
            biomes: Arc::new(biomes),
            pack_biomes,
            types,
            dimensions,
            rejected,
        }
    }

    pub fn biomes(&self) -> &Arc<Biomes> {
        &self.biomes
    }

    pub fn dimensions(&self) -> &[Resolved] {
        &self.dimensions
    }

    pub fn types(&self) -> &[CustomType] {
        &self.types
    }

    /// Why each left-out dimension was left out.
    pub fn rejected(&self) -> &[String] {
        &self.rejected
    }

    /// The entries to append, with their data, to the data registry `key`
    /// sent to clients while configuring.
    pub fn registry_entries(&self, key: &str) -> Vec<(&str, &[u8])> {
        if key == DIMENSION_TYPE.key {
            self.types
                .iter()
                .map(|t| (t.kind.name, t.nbt.as_slice()))
                .collect()
        } else if key == WORLDGEN_BIOME.key {
            self.biomes
                .custom_entries()
                .skip(self.pack_biomes)
                .collect()
        } else {
            Vec::new()
        }
    }
}

type ResolvedType = (&'static DimensionType, &'static Environment, i32);

fn resolve(
    definition: &DimensionDefinition,
    biomes: &mut Biomes,
    types: &mut Vec<CustomType>,
    pack_types: &PackTypes,
) -> Result<ResolvedType, String> {
    check_key(&definition.key)?;
    let before = biomes.len();
    let resolved = definition
        .biomes
        .iter()
        .try_for_each(|biome| add_biome(biomes, biome).map(drop))
        .and_then(|()| resolve_type(definition, types, pack_types));
    if resolved.is_err() {
        biomes.truncate(before);
    }
    resolved
}

fn resolve_type(
    definition: &DimensionDefinition,
    types: &mut Vec<CustomType>,
    pack_types: &PackTypes,
) -> Result<ResolvedType, String> {
    let name = definition.dimension_type.as_str();
    if definition.type_json.trim().is_empty() {
        let (kind, id) = pack_types.get(name).ok_or_else(|| {
            format!("dimension type {name} has no data and is not vanilla or from a data pack")
        })?;
        return Ok((kind, Environment::of(kind.name), id));
    }
    check_key(name)?;
    if pack_types.get(name).is_some() {
        return Err(format!(
            "dimension type {name} is a vanilla or data pack one"
        ));
    }
    let value = parse(&definition.type_json, name)?;
    if let Some(existing) = types.iter().find(|t| t.kind.name == name) {
        if parse(&existing.json, name)? != value {
            return Err(format!(
                "dimension type {name} is defined twice, differently"
            ));
        }
        return Ok((existing.kind, existing.environment, existing.id));
    }
    let kind: &'static DimensionType = Box::leak(Box::new(dimension_type(name, &value)?));
    let environment: &'static Environment = Box::leak(Box::new(environment(&value)?));
    let id = (pack_types.len() + types.len()) as i32;
    types.push(CustomType {
        kind,
        environment,
        id,
        json: definition.type_json.clone(),
        nbt: network_nbt(&with_argb_colors(value), name)?,
    });
    Ok((kind, environment, id))
}

const MAX_SPAWNERS: usize = 64;
const MAX_GROUP: u64 = 64;

fn parse_spawners(
    value: &Value,
    key: &str,
) -> Result<Vec<(MobCategory, &'static [Spawner])>, String> {
    let Some(spawners) = value.get("spawners") else {
        return Ok(Vec::new());
    };
    let spawners = spawners
        .as_object()
        .ok_or_else(|| format!("biome {key}: spawners is not an object"))?;
    let mut out = Vec::new();
    for (name, list) in spawners {
        let category = MobCategory::from_name(name)
            .or_else(|| MobCategory::from_name(&format!("minecraft:{name}")))
            .ok_or_else(|| format!("biome {key}: unknown mob category {name}"))?;
        let entries = list
            .as_array()
            .filter(|entries| entries.len() <= MAX_SPAWNERS)
            .ok_or_else(|| format!("biome {key}: bad spawn list for {name}"))?;
        let mut parsed = Vec::with_capacity(entries.len());
        for entry in entries {
            let kind = entry["type"]
                .as_str()
                .and_then(EntityType::from_name)
                .ok_or_else(|| format!("biome {key}: unknown spawner type in {name}"))?;
            let count = |field: &str| {
                entry[field]
                    .as_u64()
                    .filter(|n| *n <= MAX_GROUP)
                    .map(|n| n as u32)
                    .ok_or_else(|| format!("biome {key}: bad {field} in {name}"))
            };
            let weight = entry["weight"]
                .as_u64()
                .filter(|n| *n <= 1_000_000)
                .ok_or_else(|| format!("biome {key}: bad weight in {name}"))?
                as u32;
            let (min_count, max_count) = (count("minCount")?, count("maxCount")?);
            if min_count > max_count {
                return Err(format!("biome {key}: minCount above maxCount in {name}"));
            }
            parsed.push(Spawner {
                kind,
                weight,
                min_count,
                max_count,
            });
        }
        out.push((category, &*Box::leak(parsed.into_boxed_slice())));
    }
    Ok(out)
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn parse(json: &str, what: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(json).map_err(|err| format!("{what}: {err}"))?;
    if !value.is_object() {
        return Err(format!("{what}: not a JSON object"));
    }
    Ok(value)
}

const ARGB_ATTRIBUTES: [&str; 2] = [
    "minecraft:visual/cloud_color",
    "minecraft:visual/sunrise_sunset_color",
];

fn widen_color(value: &mut Value) {
    match value {
        Value::String(text) => {
            let digits = text.strip_prefix('#').unwrap_or(text);
            if digits.len() == 6 && text.starts_with('#') {
                *text = format!("#ff{digits}");
            }
        }
        Value::Object(map) => {
            if let Some(argument) = map.get_mut("argument") {
                widen_color(argument);
            }
        }
        _ => {}
    }
}

/// Colours that the client reads as ARGB need eight digits; `#rrggbb` gets full alpha.
fn with_argb_colors(mut value: Value) -> Value {
    if let Some(Value::Object(attributes)) = value.get_mut("attributes") {
        for key in ARGB_ATTRIBUTES {
            if let Some(color) = attributes.get_mut(key) {
                widen_color(color);
            }
        }
    }
    value
}

fn network_nbt(value: &Value, what: &str) -> Result<Vec<u8>, String> {
    let tag = tuff_item::registries::codecs::json_to_nbt(value)
        .ok_or_else(|| format!("{what}: holds a null"))?;
    tag.to_network_bytes()
        .map_err(|err| format!("{what}: {err}"))
}

/// A resource location: `namespace:path` of lower-case letters, digits and
/// `_-.`, with `/` also allowed in the path.
fn check_key(key: &str) -> Result<(), String> {
    let valid = key.split_once(':').is_some_and(|(namespace, path)| {
        let allowed =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.');
        !namespace.is_empty()
            && !path.is_empty()
            && namespace.chars().all(allowed)
            && path.chars().all(|c| allowed(c) || c == '/')
    });
    if valid {
        Ok(())
    } else {
        Err(format!("{key:?} is not a valid resource location"))
    }
}

fn check_biome(value: &Value, key: &str) -> Result<(), String> {
    for field in ["has_precipitation", "temperature", "downfall", "effects"] {
        if value.get(field).is_none() {
            return Err(format!("biome {key}: no {field}"));
        }
    }
    if !value["effects"].is_object() {
        return Err(format!("biome {key}: effects is not an object"));
    }
    Ok(())
}

const BED_RULE: &str = "minecraft:gameplay/bed_rule";
const STRAW_BED_RULE: &str = "minecraft:gameplay/straw_bed_rule";

/// The gameplay environment of a dimension type: its `attributes` over the
/// game's defaults, following the built-in timelines its `timelines` field
/// names (a feature type is read before data packs load; data pack
/// timelines are applied by [`data_pack_environment`]).
pub fn environment(json: &Value) -> Result<Environment, String> {
    let registries = tuff_item::registries::Registries::vanilla();
    let tags = tuff_datapack::tags::TagSet::vanilla();
    let timelines = resolve_timelines(json.get("timelines"), &registries, &tags, false)?;
    environment_with(json, timelines)
}

const TIMELINE: &str = "minecraft:timeline";

/// The timelines a dimension type's `timelines` field names: a tag, one
/// id or a list of ids. With `strict`, an id the registries lack is an
/// error; otherwise it is skipped.
pub fn resolve_timelines(
    spec: Option<&Value>,
    registries: &tuff_item::registries::Registries,
    tags: &tuff_datapack::tags::TagSet,
    strict: bool,
) -> Result<&'static [&'static Timeline], String> {
    let ids: Vec<String> = match spec {
        None => Vec::new(),
        Some(Value::String(tag)) if tag.starts_with('#') => {
            let name = qualify(&tag[1..]);
            match tags.get(TIMELINE, &name) {
                Some(ids) => ids.to_vec(),
                None if strict => return Err(format!("unknown timeline tag {tag}")),
                None => Vec::new(),
            }
        }
        Some(Value::String(id)) => vec![qualify(id)],
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| {
                id.as_str()
                    .map(qualify)
                    .ok_or("timelines holds a non-string")
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("timelines is not a tag, id or list".into()),
    };
    let registry = registries.get(TIMELINE);
    let mut out = Vec::with_capacity(ids.len());
    for id in &ids {
        let entry = registry.and_then(|registry| registry.entry(id));
        let timeline = match entry {
            Some(tuff_item::registries::Entry::Loaded(raw)) => match raw.json.as_deref() {
                Some(json) => Some(Timeline::from_json(id, json)?),
                None => Timeline::get(id),
            },
            Some(tuff_item::registries::Entry::Builtin(_)) => Timeline::get(id),
            None => None,
        };
        match timeline {
            Some(timeline) => out.push(timeline),
            None if strict => return Err(format!("unknown timeline {id}")),
            None => {}
        }
    }
    Ok(if out.is_empty() {
        &[]
    } else {
        Box::leak(out.into_boxed_slice())
    })
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// The timeline tag a built-in dimension type follows.
fn builtin_timeline_tag(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "minecraft:overworld" | "minecraft:overworld_caves" => "#minecraft:in_overworld",
        "minecraft:the_nether" => "#minecraft:in_nether",
        "minecraft:the_end" => "#minecraft:in_end",
        _ => return None,
    })
}

/// The environment of the dimension type `kind` once data packs have
/// loaded, or `None` when they leave `current` as it is. `type_json` is
/// the type's own definition when a pack or feature provides one.
pub fn data_pack_environment(
    kind: &str,
    type_json: Option<&Value>,
    current: &'static Environment,
    registries: &tuff_item::registries::Registries,
    tags: &tuff_datapack::tags::TagSet,
) -> Result<Option<Environment>, String> {
    let builtin_spec = builtin_timeline_tag(kind).map(|tag| Value::String(tag.to_owned()));
    let spec = match type_json {
        Some(json) => json.get("timelines"),
        None => builtin_spec.as_ref(),
    };
    let timelines = resolve_timelines(spec, registries, tags, true)?;
    let unchanged = type_json.is_none()
        && timelines.len() == current.timelines.len()
        && timelines
            .iter()
            .zip(current.timelines)
            .all(|(a, b)| std::ptr::eq(*a, *b));
    if unchanged {
        return Ok(None);
    }
    match type_json {
        Some(json) => environment_with(json, timelines).map(Some),
        None => current.with_timelines(timelines).map(Some),
    }
}

fn environment_with(
    json: &Value,
    timelines: &'static [&'static Timeline],
) -> Result<Environment, String> {
    let empty = serde_json::Map::new();
    let attributes = match json.get("attributes") {
        None => &empty,
        Some(Value::Object(map)) => map,
        Some(_) => return Err("attributes is not an object".into()),
    };
    let attribute = |key: &str| attributes.get(&format!("minecraft:gameplay/{key}"));
    let flag = |key: &str, default: bool| match attribute(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} is not a boolean")),
    };
    let number = |key: &str, default: f32| match attribute(key) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .map(|n| n as f32)
            .ok_or_else(|| format!("{key} is not a number")),
    };
    let activity = |key: &str, default: Activity| match attribute(key) {
        None => Ok(default),
        Some(value) => {
            let name = value
                .as_str()
                .ok_or_else(|| format!("{key} is not a string"))?;
            let full = if name.contains(':') {
                name.to_owned()
            } else {
                format!("minecraft:{name}")
            };
            Activity::from_name(&full).ok_or_else(|| format!("{key}: unknown activity {name}"))
        }
    };
    let bed = |key: &str, default: BedRule| match attributes.get(key) {
        None => Ok(default),
        Some(value) => bed_rule(value).map_err(|err| format!("{key}: {err}")),
    };
    let base = &DEFAULT_ENVIRONMENT;
    let tracks = &base.with_timelines(timelines)?;
    let ranged = |key: &str, default: f32| {
        let value = number(key, default)?;
        match tuff_vanilla_data::world::attribute_range(&format!("minecraft:gameplay/{key}")) {
            Some((min, max)) if !(min..=max).contains(&value) => {
                Err(format!("{key}: {value} is not in range [{min}; {max}]"))
            }
            _ => Ok(value),
        }
    };
    let sky_light_level = ranged("sky_light_level", base.sky_light_level)?;
    let sun_angle = match attributes.get("minecraft:visual/sun_angle") {
        None => base.sun_angle,
        Some(value) => value.as_f64().ok_or("sun_angle is not a number")? as f32,
    };
    Ok(Environment {
        bed_rule: bed(BED_RULE, base.bed_rule)?,
        straw_bed_rule: bed(STRAW_BED_RULE, base.straw_bed_rule)?,
        water_evaporates: flag("water_evaporates", base.water_evaporates)?,
        sky_light_level,
        has_fixed_time: json
            .get("has_fixed_time")
            .map(|v| v.as_bool().ok_or("has_fixed_time is not a boolean"))
            .transpose()?
            .unwrap_or(false),
        sky_light_tracks: tracks.sky_light_tracks,
        monsters_burn: flag("monsters_burn", base.monsters_burn)?,
        monsters_burn_tracks: tracks.monsters_burn_tracks,
        surface_slime_spawn_chance: ranged(
            "surface_slime_spawn_chance",
            base.surface_slime_spawn_chance,
        )?,
        surface_slime_tracks: tracks.surface_slime_tracks,
        turtle_egg_hatch_chance: ranged("turtle_egg_hatch_chance", base.turtle_egg_hatch_chance)?,
        turtle_egg_tracks: tracks.turtle_egg_tracks,
        piglins_zombify: flag("piglins_zombify", base.piglins_zombify)?,
        respawn_anchor_works: flag("respawn_anchor_works", base.respawn_anchor_works)?,
        can_start_raid: flag("can_start_raid", base.can_start_raid)?,
        villager_activity: activity("villager_activity", base.villager_activity)?,
        villager_activity_tracks: tracks.villager_activity_tracks,
        baby_villager_activity: activity("baby_villager_activity", base.baby_villager_activity)?,
        baby_villager_activity_tracks: tracks.baby_villager_activity_tracks,
        sun_angle,
        sun_angle_tracks: tracks.sun_angle_tracks,
        nether_portal_spawns_piglin: flag(
            "nether_portal_spawns_piglin",
            base.nether_portal_spawns_piglin,
        )?,
        eyeblossom_open_tracks: tracks.eyeblossom_open_tracks,
        increased_fire_burnout: flag("increased_fire_burnout", base.increased_fire_burnout)?,
        creaking_active: flag("creaking_active", base.creaking_active)?,
        creaking_active_tracks: tracks.creaking_active_tracks,
        bees_stay_in_hive: flag("bees_stay_in_hive", base.bees_stay_in_hive)?,
        bees_stay_in_hive_tracks: tracks.bees_stay_in_hive_tracks,
        natural_spawns: natural_spawns(attributes.get(NATURAL_SPAWNS))?,
        timelines: tracks.timelines,
    })
}

const NATURAL_SPAWNS: &str = "minecraft:gameplay/natural_mob_spawns";

/// The `natural_mob_spawns` attribute of a dimension type: the spawn
/// settings (or an `{argument, modifier}` pair using the overlay modifier)
/// laid over each biome's.
fn natural_spawns(value: Option<&Value>) -> Result<Option<&'static NaturalSpawns>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let settings = match value.get("modifier") {
        None => value,
        Some(modifier) => {
            let name = modifier.as_str().unwrap_or_default();
            if name != "overlay" && name != "minecraft:overlay" {
                return Err(format!(
                    "natural_mob_spawns: unsupported modifier {modifier}"
                ));
            }
            value
                .get("argument")
                .ok_or("natural_mob_spawns: no argument")?
        }
    };
    let mut categories = Vec::new();
    if let Some(by_category) = settings.get("spawns_by_category") {
        let by_category = by_category
            .as_object()
            .ok_or("natural_mob_spawns: spawns_by_category is not an object")?;
        for (name, list) in by_category {
            let category = MobCategory::from_name(name)
                .or_else(|| MobCategory::from_name(&format!("minecraft:{name}")))
                .ok_or_else(|| format!("natural_mob_spawns: unknown mob category {name}"))?;
            let entries = list
                .as_array()
                .filter(|entries| entries.len() <= MAX_SPAWNERS)
                .ok_or_else(|| format!("natural_mob_spawns: bad spawn list for {name}"))?;
            let mut parsed = Vec::with_capacity(entries.len());
            for entry in entries {
                parsed.push(attribute_spawner(entry, name)?);
            }
            categories.push((category, &*Box::leak(parsed.into_boxed_slice())));
        }
    }
    let mut costs = Vec::new();
    if let Some(spawn_costs) = settings.get("spawn_costs") {
        let spawn_costs = spawn_costs
            .as_object()
            .filter(|costs| costs.len() <= MAX_SPAWNERS)
            .ok_or("natural_mob_spawns: bad spawn_costs")?;
        for (name, cost) in spawn_costs {
            let kind = EntityType::from_name(name)
                .ok_or_else(|| format!("natural_mob_spawns: unknown entity {name}"))?;
            let number = |field: &str| {
                cost[field]
                    .as_f64()
                    .ok_or_else(|| format!("natural_mob_spawns: bad {field} for {name}"))
            };
            costs.push((
                kind,
                SpawnCost {
                    energy_budget: number("energy_budget")?,
                    charge: number("charge")?,
                },
            ));
        }
    }
    let spawns = NaturalSpawns {
        categories: Box::leak(categories.into_boxed_slice()),
        costs: Box::leak(costs.into_boxed_slice()),
        creature_probability: None,
    };
    Ok(Some(Box::leak(Box::new(spawns))))
}

fn attribute_spawner(entry: &Value, category: &str) -> Result<Spawner, String> {
    let bad = |field: &str| format!("natural_mob_spawns: bad {field} in {category}");
    let kind = entry["type"]
        .as_str()
        .and_then(EntityType::from_name)
        .ok_or_else(|| bad("type"))?;
    let weight = entry["weight"]
        .as_u64()
        .filter(|n| *n <= 1_000_000)
        .ok_or_else(|| bad("weight"))? as u32;
    let group = |value: &Value| value.as_u64().filter(|n| *n <= MAX_GROUP).map(|n| n as u32);
    let (min_count, max_count) = match &entry["count"] {
        count if count.is_number() => {
            let n = group(count).ok_or_else(|| bad("count"))?;
            (n, n)
        }
        count => (
            group(&count["min_inclusive"]).ok_or_else(|| bad("count"))?,
            group(&count["max_inclusive"]).ok_or_else(|| bad("count"))?,
        ),
    };
    if min_count > max_count {
        return Err(bad("count"));
    }
    Ok(Spawner {
        kind,
        weight,
        min_count,
        max_count,
    })
}

fn bed_rule(value: &Value) -> Result<BedRule, String> {
    let map = value.as_object().ok_or("not an object")?;
    let condition = |key: &str| -> Result<BedCondition, String> {
        match map.get(key).and_then(Value::as_str) {
            Some("always") => Ok(BedCondition::Always),
            Some("when_dark") => Ok(BedCondition::WhenDark),
            Some("never") => Ok(BedCondition::Never),
            _ => Err(format!("{key} must be always, when_dark or never")),
        }
    };
    let flag = |key: &str| match map.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} is not a boolean")),
    };
    Ok(BedRule {
        can_sleep: condition("can_sleep")?,
        can_set_spawn: condition("can_set_spawn")?,
        destroy_on_use: flag("destroy_on_use")?,
        destroy_on_leave: flag("destroy_on_leave")?,
        error_message: map.get("error_message").map(|m| leak(&m.to_string())),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::world::dimension::{OVERWORLD, THE_END, THE_NETHER};
    use tuff_world_model::chunk::{Chunk, ChunkShape, ceil_log2};
    use tuff_world_model::pos::ChunkPos;

    pub(crate) const SKY_TYPE: &str = r##"{
        "ambient_light": 0.5,
        "attributes": {
            "minecraft:gameplay/bed_rule": {
                "can_set_spawn": "never",
                "can_sleep": "never",
                "error_message": {"translate": "sky.no_sleep"}
            },
            "minecraft:gameplay/water_evaporates": true,
            "minecraft:gameplay/villager_activity": "minecraft:rest",
            "minecraft:gameplay/sky_light_level": 7.0,
            "minecraft:visual/sky_color": "#000000"
        },
        "coordinate_scale": 2.0,
        "has_ceiling": false,
        "has_ender_dragon_fight": false,
        "has_fixed_time": true,
        "has_skylight": true,
        "height": 128,
        "infiniburn": "#minecraft:infiniburn_overworld",
        "logical_height": 96,
        "min_y": -32,
        "monster_spawn_block_light_limit": 0,
        "monster_spawn_light_level": {"type": "minecraft:uniform", "min_inclusive": 0, "max_inclusive": 7},
        "timelines": "#minecraft:in_overworld"
    }"##;

    pub(crate) const BIOME: &str = r#"{"has_precipitation": false, "temperature": 0.5,
        "downfall": 0.0, "effects": {"water_color": 4159204}}"#;

    struct Void;

    impl ChunkGenerator for Void {
        fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
            Chunk::empty(shape)
        }
        fn sea_level(&self) -> i32 {
            0
        }
        fn spawn(&self) -> tuff_world_model::BlockPos {
            tuff_world_model::BlockPos::new(0, 0, 0)
        }
    }

    pub(crate) struct Provider(pub(crate) Vec<DimensionDefinition>);

    impl DimensionProvider for Provider {
        fn dimensions(&self) -> Vec<DimensionDefinition> {
            self.0.clone()
        }
        fn generator(&self, _: &str, _: i64, _: &Arc<Biomes>) -> Arc<dyn ChunkGenerator> {
            Arc::new(Void)
        }
    }

    pub(crate) fn definition(
        key: &str,
        kind: &str,
        json: &str,
        biomes: &[(&str, &str)],
    ) -> DimensionDefinition {
        DimensionDefinition {
            key: key.into(),
            dimension_type: kind.into(),
            type_json: json.into(),
            biomes: biomes
                .iter()
                .map(|(key, json)| BiomeDefinition {
                    key: (*key).into(),
                    json: (*json).into(),
                })
                .collect(),
            natural_spawning: false,
            claims_spawn: false,
        }
    }

    pub(crate) fn collect(definitions: Vec<DimensionDefinition>) -> CustomDimensions {
        let provider: Arc<dyn DimensionProvider> = Arc::new(Provider(definitions));
        CustomDimensions::collect(&[provider], &[OVERWORLD, THE_NETHER, THE_END])
    }

    #[tuff_core::watchdog]
    #[test]
    fn argb_attributes_are_widened_for_the_client() {
        let value: Value = serde_json::from_str(
            r##"{"attributes": {"minecraft:visual/cloud_color": "#e9eeff",
                "minecraft:visual/sunrise_sunset_color": {"argument": "#102030", "modifier": "add"},
                "minecraft:visual/sky_color": "#8fa4dc"}}"##,
        )
        .unwrap();
        let out = with_argb_colors(value);
        let a = &out["attributes"];
        assert_eq!(a["minecraft:visual/cloud_color"], "#ffe9eeff");
        assert_eq!(
            a["minecraft:visual/sunrise_sunset_color"]["argument"],
            "#ff102030"
        );
        assert_eq!(a["minecraft:visual/sky_color"], "#8fa4dc");
    }

    #[tuff_core::watchdog]
    #[test]
    fn dimension_types_and_environments_come_from_json() {
        let custom = collect(vec![definition(
            "ex:sky",
            "ex:sky_type",
            SKY_TYPE,
            &[("ex:cloud", BIOME)],
        )]);
        assert!(custom.rejected().is_empty(), "{:?}", custom.rejected());
        let sky = &custom.dimensions()[0];
        assert_eq!(sky.type_id, DIMENSION_TYPE.entries.len() as i32);
        let kind = sky.kind;
        assert_eq!(
            (kind.min_y, kind.height, kind.logical_height),
            (-32, 128, 96)
        );
        assert_eq!(kind.coordinate_scale, 2.0);
        assert_eq!(kind.monster_spawn_light_level, (0, 7));
        assert_eq!(kind.default_clock, None);
        let environment = sky.environment;
        assert_eq!(environment.bed_rule.can_sleep, BedCondition::Never);
        assert_eq!(environment.bed_rule.can_set_spawn, BedCondition::Never);
        assert!(!environment.bed_rule.destroy_on_use);
        assert!(
            environment
                .bed_rule
                .error_message
                .is_some_and(|m| m.contains("sky.no_sleep"))
        );
        assert_eq!(
            environment.straw_bed_rule,
            DEFAULT_ENVIRONMENT.straw_bed_rule
        );
        assert!(environment.water_evaporates);
        assert!(environment.has_fixed_time);
        assert_eq!(environment.sky_light_level, 7.0);
        assert_eq!(environment.villager_activity, Activity::Rest);
        assert_eq!(
            environment.sky_light_tracks,
            Environment::of(OVERWORLD).sky_light_tracks,
            "the overworld's timelines"
        );
        let biomes = custom.biomes();
        let cloud = biomes.id("ex:cloud").unwrap();
        assert_eq!(cloud as usize, WORLDGEN_BIOME.entries.len());
        assert_eq!(biomes.name(cloud), "ex:cloud");
        assert!(biomes.is_custom(cloud));
        assert_eq!(biomes.name(cloud + 1), "minecraft:plains");
        assert_eq!(
            biomes.id("minecraft:desert"),
            tuff_world_model::chunk::biome_id("minecraft:desert")
        );
        assert_eq!(Biomes::vanilla().id("ex:cloud"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_biomes_carry_their_climate() {
        let snowy = r##"{"has_precipitation": true, "temperature": -0.5, "downfall": 0.4,
            "temperature_modifier": "frozen",
            "attributes": {"minecraft:gameplay/increased_fire_burnout": true},
            "effects": {"water_color": "#3d57d6"}}"##;
        let custom = collect(vec![definition(
            "ex:sky",
            "minecraft:overworld",
            "",
            &[("ex:snowy", snowy)],
        )]);
        let biomes = custom.biomes();
        let climate = biomes.climate(biomes.id("ex:snowy").unwrap()).unwrap();
        assert!(climate.has_precipitation && climate.frozen);
        assert_eq!(climate.increased_fire_burnout, Some(true));
        assert_eq!(climate.temperature, -0.5);
        assert_eq!(biomes.climate(0), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_type_overlays_natural_mob_spawns_on_the_biomes() {
        let typed = r##"{"ambient_light": 0.0, "coordinate_scale": 1, "has_ceiling": true,
            "has_ender_dragon_fight": false, "has_skylight": false, "height": 256,
            "infiniburn": "#minecraft:infiniburn_nether", "logical_height": 128, "min_y": 0,
            "monster_spawn_block_light_limit": 15, "monster_spawn_light_level": 7,
            "attributes": {"minecraft:gameplay/natural_mob_spawns": {"modifier": "overlay",
              "argument": {"spawn_costs": {"minecraft:ghast": {"charge": 0.5, "energy_budget": 2.0}},
                "spawns_by_category": {"monster": [
                  {"type": "minecraft:zombie", "weight": 3, "count": {"type": "minecraft:uniform", "min_inclusive": 2, "max_inclusive": 4}}],
                  "creature": []}}}}}"##;
        let environment = environment(&serde_json::from_str(typed).unwrap()).unwrap();
        let overlay = environment.natural_spawns.unwrap();
        let plains = tuff_vanilla_data::biome::Biome::get("minecraft:plains").unwrap();
        let monsters = plains
            .natural_spawns
            .spawners_under(Some(overlay), MobCategory::Monster);
        assert_eq!(monsters.len(), 1);
        assert_eq!(monsters[0].kind, EntityType::Zombie);
        assert_eq!((monsters[0].min_count, monsters[0].max_count), (2, 4));
        assert!(
            plains
                .natural_spawns
                .spawners_under(Some(overlay), MobCategory::Creature)
                .is_empty()
        );
        assert_eq!(
            plains
                .natural_spawns
                .spawners_under(Some(overlay), MobCategory::WaterCreature),
            plains.natural_spawns.spawners(MobCategory::WaterCreature)
        );
        assert_eq!(
            plains
                .natural_spawns
                .cost_under(Some(overlay), EntityType::Ghast)
                .map(|cost| cost.charge),
            Some(0.5)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_type_without_attributes_takes_the_defaults() {
        let bare = r##"{"ambient_light": 0.0, "coordinate_scale": 1, "has_ceiling": true,
            "has_ender_dragon_fight": false, "has_skylight": false, "height": 256,
            "infiniburn": "#minecraft:infiniburn_nether", "logical_height": 128, "min_y": 0,
            "monster_spawn_block_light_limit": 15, "monster_spawn_light_level": 7}"##;
        let environment = environment(&serde_json::from_str(bare).unwrap()).unwrap();
        assert_eq!(
            Environment {
                sky_light_tracks: &[],
                ..environment
            },
            DEFAULT_ENVIRONMENT
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn shared_types_and_biomes_are_resolved_once() {
        let custom = collect(vec![
            definition("ex:a", "ex:sky_type", SKY_TYPE, &[("ex:cloud", BIOME)]),
            definition("ex:b", "ex:sky_type", SKY_TYPE, &[("ex:cloud", BIOME)]),
            definition("ex:c", THE_NETHER, "", &[]),
        ]);
        assert!(custom.rejected().is_empty(), "{:?}", custom.rejected());
        assert_eq!(custom.types().len(), 1);
        assert_eq!(custom.biomes().len(), WORLDGEN_BIOME.entries.len() + 1);
        let [a, b, c] = custom.dimensions() else {
            panic!("three dimensions");
        };
        assert_eq!(a.type_id, b.type_id);
        assert_eq!(c.kind.name, THE_NETHER);
        assert!(c.environment.water_evaporates);
        assert_eq!(
            c.type_id,
            crate::world::registry_id(&DIMENSION_TYPE, THE_NETHER)
        );
        assert_eq!(custom.registry_entries(DIMENSION_TYPE.key).len(), 1);
        assert_eq!(custom.registry_entries(WORLDGEN_BIOME.key).len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn invalid_dimensions_are_left_out_without_their_biomes() {
        let other = r#"{"has_precipitation": true, "temperature": 1.0,
            "downfall": 0.5, "effects": {"water_color": 1}}"#;
        let tall = SKY_TYPE.replace("\"height\": 128", "\"height\": 100");
        let custom = collect(vec![
            definition(OVERWORLD, "ex:sky_type", SKY_TYPE, &[]),
            definition("Ex:Bad", "ex:sky_type", SKY_TYPE, &[]),
            definition(
                "ex:plains",
                "ex:sky_type",
                SKY_TYPE,
                &[("minecraft:plains", BIOME)],
            ),
            definition("ex:tall", "ex:tall_type", &tall, &[("ex:lost", BIOME)]),
            definition("ex:vanilla", OVERWORLD, SKY_TYPE, &[]),
            definition("ex:unknown", "ex:nothing", "", &[]),
            definition("ex:broken", "ex:broken_type", "{", &[]),
            definition(
                "ex:no_effects",
                "ex:sky_type",
                SKY_TYPE,
                &[("ex:flat", r#"{"temperature": 1}"#)],
            ),
            definition("ex:ok", "ex:sky_type", SKY_TYPE, &[("ex:cloud", BIOME)]),
            definition("ex:ok", "ex:sky_type", SKY_TYPE, &[]),
            definition("ex:clash", "ex:sky_type", SKY_TYPE, &[("ex:cloud", other)]),
        ]);
        assert_eq!(custom.rejected().len(), 10, "{:?}", custom.rejected());
        let keys: Vec<&str> = custom.dimensions().iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["ex:ok"]);
        assert_eq!(custom.biomes().id("ex:lost"), None, "rolled back");
        assert_eq!(custom.biomes().len(), WORLDGEN_BIOME.entries.len() + 1);
        assert_eq!(custom.types().len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_biomes_never_widen_the_global_palette() {
        let free = max_biomes() - WORLDGEN_BIOME.entries.len();
        let keys: Vec<String> = (0..=free).map(|i| format!("ex:biome_{i}")).collect();
        let biomes: Vec<(&str, &str)> = keys.iter().map(|k| (k.as_str(), BIOME)).collect();
        let custom = collect(vec![
            definition("ex:many", "ex:sky_type", SKY_TYPE, &biomes),
            definition("ex:fits", "ex:sky_type", SKY_TYPE, &biomes[..free]),
        ]);
        assert_eq!(custom.rejected().len(), 1);
        assert_eq!(custom.biomes().len(), max_biomes());
        assert_eq!(
            ceil_log2(custom.biomes().len()),
            ceil_log2(WORLDGEN_BIOME.entries.len())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn network_nbt_round_trips_the_json() {
        let custom = collect(vec![definition(
            "ex:sky",
            "ex:sky_type",
            SKY_TYPE,
            &[("ex:cloud", BIOME)],
        )]);
        let (name, mut nbt) = custom.registry_entries(DIMENSION_TYPE.key)[0];
        assert_eq!(name, "ex:sky_type");
        let tag = tuff_nbt::Tag::read_network(&mut nbt, tuff_nbt::NbtLimits::NETWORK)
            .unwrap()
            .unwrap();
        let root = tag.as_compound().unwrap();
        assert_eq!(root.get_bool("has_fixed_time"), Some(true));
        assert_eq!(root.get_double("coordinate_scale"), Some(2.0));
        assert_eq!(root.get_int("min_y"), Some(-32));
        assert_eq!(
            root.get_compound("monster_spawn_light_level")
                .and_then(|l| l.get_int("max_inclusive")),
            Some(7)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn custom_biomes_carry_spawn_lists() {
        let sky = r##"{"has_precipitation": false, "temperature": 0.5, "downfall": 0.0,
            "effects": {},
            "spawners": {"monster": [{"type": "minecraft:zombie", "weight": 5,
                "minCount": 1, "maxCount": 3}]}}"##;
        let custom = collect(vec![definition(
            "ex:sky",
            "minecraft:overworld",
            "",
            &[("ex:sky_biome", sky)],
        )]);
        let biomes = custom.biomes();
        let monsters = biomes
            .spawners("ex:sky_biome", MobCategory::Monster)
            .unwrap();
        assert_eq!(monsters.len(), 1);
        assert_eq!(monsters[0].kind, EntityType::Zombie);
        assert_eq!((monsters[0].weight, monsters[0].max_count), (5, 3));
        assert!(
            biomes
                .spawners("ex:sky_biome", MobCategory::Creature)
                .unwrap()
                .is_empty()
        );
        assert!(
            biomes
                .spawners("minecraft:plains", MobCategory::Creature)
                .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_spawn_lists_reject_the_biome() {
        for spawners in [
            r#"{"monster": [{"type": "minecraft:nope", "weight": 1, "minCount": 1, "maxCount": 1}]}"#,
            r#"{"monster": [{"type": "minecraft:zombie", "weight": 1, "minCount": 3, "maxCount": 1}]}"#,
            r#"{"monster": [{"type": "minecraft:zombie", "weight": 1, "minCount": 1, "maxCount": 99999}]}"#,
            r#"{"bogus": []}"#,
            "[]",
        ] {
            let json = format!(
                r#"{{"has_precipitation": false, "temperature": 0.5, "downfall": 0.0,
                "effects": {{}}, "spawners": {spawners}}}"#
            );
            let custom = collect(vec![definition(
                "ex:sky",
                "minecraft:overworld",
                "",
                &[("ex:bad", &json)],
            )]);
            assert!(custom.dimensions().is_empty(), "{spawners}");
        }
    }

    fn pack_timeline(json: &str) -> Result<Arc<tuff_item::registries::Registries>, String> {
        let mut loader = tuff_item::registries::Loader::default();
        loader.insert(
            tuff_vanilla_data::data_registry::DataRegistry::get(TIMELINE).unwrap(),
            "ex:dusk".to_owned(),
            "file/ex",
            json.as_bytes().to_vec(),
        );
        loader.freeze().map_err(|err| err.to_string())
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_timeline_tag_change_reaches_vanilla_dimensions() {
        let registries = tuff_item::registries::Registries::vanilla();
        let nether = Environment::of(THE_NETHER);
        let vanilla = tuff_datapack::tags::TagSet::vanilla();
        assert_eq!(
            data_pack_environment(THE_NETHER, None, nether, &registries, &vanilla),
            Ok(None)
        );
        let tags = tuff_datapack::tags::vanilla_with(&[(
            TIMELINE,
            "minecraft:in_nether",
            "minecraft:day",
        )]);
        let changed = data_pack_environment(THE_NETHER, None, nether, &registries, &tags)
            .unwrap()
            .unwrap();
        assert!(nether.sun_angle_tracks.is_empty());
        assert_eq!(
            changed.sun_angle_tracks,
            Environment::of(OVERWORLD).sun_angle_tracks
        );
        assert!(changed.water_evaporates);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pack_timeline_drives_a_dimension_type_naming_it() {
        let registries = pack_timeline(
            r#"{"clock": "minecraft:overworld", "period_ticks": 100,
                "tracks": {"minecraft:gameplay/sky_light_level": {"modifier": "alpha_blend",
                    "keyframes": [{"ticks": 0, "value": {"value": 0.0, "alpha": 0.5}}]},
                    "minecraft:gameplay/monsters_burn": {"modifier": "nor",
                    "keyframes": [{"ticks": 0, "value": false}]}}}"#,
        )
        .unwrap();
        let tags = tuff_datapack::tags::TagSet::vanilla();
        let json = serde_json::json!({
            "timelines": ["ex:dusk"],
            "attributes": {"minecraft:gameplay/monsters_burn": true}
        });
        let environment = data_pack_environment(
            "ex:t",
            Some(&json),
            &DEFAULT_ENVIRONMENT,
            &registries,
            &tags,
        )
        .unwrap()
        .unwrap();
        assert_eq!(environment.timelines[0].name, "ex:dusk");
        assert_eq!(environment.sky_light(|_| 0, 0.0, 0.0), 7.5);
        assert!(!environment.monsters_burn(|_| 0));
        let missing = serde_json::json!({"timelines": ["ex:nowhere"]});
        assert!(
            data_pack_environment(
                "ex:t",
                Some(&missing),
                &DEFAULT_ENVIRONMENT,
                &registries,
                &tags
            )
            .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn invalid_pack_timelines_reject_the_load() {
        assert!(pack_timeline(r#"{"clock": "ex:no_such_clock"}"#).is_err());
        assert!(
            pack_timeline(
                r#"{"clock": "minecraft:overworld", "tracks": {"minecraft:gameplay/turtle_egg_hatch_chance":
                    {"keyframes": [{"ticks": 0, "value": 2.0}]}}}"#
            )
            .is_err()
        );
        assert!(pack_timeline(r#"{"clock": "minecraft:overworld"}"#).is_ok());
        let out_of_range = serde_json::json!({
            "attributes": {"minecraft:gameplay/surface_slime_spawn_chance": 1.5}
        });
        assert!(environment(&out_of_range).is_err());
    }
}

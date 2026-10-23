//! The enchantment effect model: effect components decoded once from JSON
//! (built-in tables or pack files) into enums, and the arithmetic to
//! evaluate them for an enchantment level.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::registries::{DecodeCtx, Registries, RegistryCodec, RegistryError, RuntimeRegistry};
use rand::Rng;
use serde_json::{Map, Value};
use tuff_vanilla_data::enchantment::{Enchantment, EnchantmentCost};
use tuff_vanilla_data::registry::Item;

#[cfg(test)]
mod tests;

type Object = Map<String, Value>;

/// A number computed from the enchantment level.
#[derive(Clone, Debug, PartialEq)]
pub enum LevelValue {
    Constant(f32),
    Linear {
        base: f32,
        per_level_above_first: f32,
    },
    Clamped {
        value: Box<LevelValue>,
        min: f32,
        max: f32,
    },
    Fraction {
        numerator: Box<LevelValue>,
        denominator: Box<LevelValue>,
    },
    LevelsSquared {
        added: f32,
    },
    Exponent {
        base: Box<LevelValue>,
        power: Box<LevelValue>,
    },
    Lookup {
        values: Vec<f32>,
        fallback: Box<LevelValue>,
    },
}

impl LevelValue {
    pub fn at(&self, level: u32) -> f32 {
        let level = level as i32;
        match self {
            Self::Constant(value) => *value,
            Self::Linear {
                base,
                per_level_above_first,
            } => base + per_level_above_first * (level - 1) as f32,
            Self::Clamped { value, min, max } => value.at(level as u32).clamp(*min, *max),
            Self::Fraction {
                numerator,
                denominator,
            } => {
                let denominator = denominator.at(level as u32);
                if denominator == 0.0 {
                    0.0
                } else {
                    numerator.at(level as u32) / denominator
                }
            }
            Self::LevelsSquared { added } => (level * level) as f32 + added,
            Self::Exponent { base, power } => {
                (base.at(level as u32) as f64).powf(power.at(level as u32) as f64) as f32
            }
            Self::Lookup { values, fallback } => {
                if level >= 1 && level as usize <= values.len() {
                    values[level as usize - 1]
                } else {
                    fallback.at(level as u32)
                }
            }
        }
    }
}

/// A float drawn per use (sound volume and pitch, particle speed).
#[derive(Clone, Debug, PartialEq)]
pub enum FloatProvider {
    Constant(f32),
    Uniform {
        min_inclusive: f32,
        max_exclusive: f32,
    },
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: f32,
        max: f32,
    },
    Trapezoid {
        min: f32,
        max: f32,
        plateau: f32,
    },
}

impl FloatProvider {
    pub fn sample(&self, rng: &mut impl Rng) -> f32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform {
                min_inclusive,
                max_exclusive,
            } => min_inclusive + rng.r#gen::<f32>() * (max_exclusive - min_inclusive),
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => (mean + gaussian(rng) as f32 * deviation).clamp(min, max),
            Self::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let plateau_start = (range - plateau) / 2.0;
                let plateau_end = range - plateau_start;
                min + rng.r#gen::<f32>() * plateau_end + rng.r#gen::<f32>() * plateau_start
            }
        }
    }
}

/// Changes a number (damage, protection, experience, ...) for a level.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueEffect {
    Add(LevelValue),
    Multiply(LevelValue),
    Set(LevelValue),
    RemoveBinomial(LevelValue),
    Exponential {
        base: LevelValue,
        exponent: LevelValue,
    },
    AllOf(Vec<ValueEffect>),
}

impl ValueEffect {
    pub fn process(&self, level: u32, rng: &mut impl Rng, input: f32) -> f32 {
        match self {
            Self::Add(value) => input + value.at(level),
            Self::Multiply(factor) => input * factor.at(level),
            Self::Set(value) => value.at(level),
            Self::RemoveBinomial(chance) => remove_binomial(chance.at(level), rng, input),
            Self::Exponential { base, exponent } => {
                (input as f64 * (base.at(level) as f64).powf(exponent.at(level) as f64)) as f32
            }
            Self::AllOf(effects) => effects
                .iter()
                .fold(input, |value, effect| effect.process(level, rng, value)),
        }
    }
}

fn gaussian(rng: &mut impl Rng) -> f64 {
    loop {
        let u = rng.r#gen::<f64>() * 2.0 - 1.0;
        let v = rng.r#gen::<f64>() * 2.0 - 1.0;
        let s = u * u + v * v;
        if s > 0.0 && s < 1.0 {
            return u * (-2.0 * s.ln() / s).sqrt();
        }
    }
}

fn remove_binomial(p: f32, rng: &mut impl Rng, n: f32) -> f32 {
    let points = n.max(0.0) as u32;
    n - crate::item::dropped_points(points, p, rng) as f32
}

/// Who an effect's enchanted item belongs to, or who it affects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Attacker,
    DamagingEntity,
    Victim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeOperation {
    AddValue,
    AddMultipliedBase,
    AddMultipliedTotal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttributeEffect {
    pub id: String,
    pub attribute: String,
    pub amount: LevelValue,
    pub operation: AttributeOperation,
}

/// Ids, or one tag (without the `#`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HolderSet {
    Ids(Vec<String>),
    Tag(String),
}

/// Items an enchantment applies to: resolved, or a tag resolved later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemSet {
    Items(Vec<Item>),
    Tag(String),
}

impl ItemSet {
    pub fn contains(&self, item: Item) -> bool {
        match self {
            Self::Items(items) => items.contains(&item),
            Self::Tag(tag) => {
                tuff_datapack::tags::with(|tags| tags.contains("minecraft:item", tag, item.name()))
            }
        }
    }
}

/// Whether two enchantments can coexist on one item: different, and in
/// neither one's exclusive set.
pub fn compatible(a: (&str, &EnchantmentDef), b: (&str, &EnchantmentDef)) -> bool {
    a.0 != b.0 && !a.1.excludes(b.0) && !b.1.excludes(a.0)
}

/// A sound event by id, or inline with a fixed range.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundRef {
    pub id: String,
    pub range: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PositionSource {
    pub kind: String,
    pub offset: f32,
    pub scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VelocitySource {
    pub movement_scale: f32,
    pub base: FloatProvider,
}

/// Something done to an entity or at a position. `Attribute` only occurs in
/// location-based effects.
#[derive(Clone, Debug, PartialEq)]
pub enum EntityEffect {
    AllOf(Vec<EntityEffect>),
    ApplyMobEffect {
        to_apply: HolderSet,
        min_duration: LevelValue,
        max_duration: LevelValue,
        min_amplifier: LevelValue,
        max_amplifier: LevelValue,
    },
    Attribute(AttributeEffect),
    ChangeItemDamage(LevelValue),
    DamageEntity {
        damage_type: String,
        min_damage: LevelValue,
        max_damage: LevelValue,
    },
    Explode {
        attribute_to_user: bool,
        damage_type: Option<String>,
        knockback_multiplier: Option<LevelValue>,
        immune_blocks: Option<HolderSet>,
        offset: [f64; 3],
        radius: LevelValue,
        create_fire: bool,
        block_interaction: String,
        small_particle: Value,
        large_particle: Value,
        block_particles: Value,
        sound: SoundRef,
    },
    Ignite(LevelValue),
    ApplyImpulse {
        direction: [f64; 3],
        coordinate_scale: [f64; 3],
        magnitude: LevelValue,
    },
    ApplyExhaustion(LevelValue),
    PlaySound {
        sounds: Vec<SoundRef>,
        volume: FloatProvider,
        pitch: FloatProvider,
    },
    ReplaceBlock {
        offset: [i32; 3],
        predicate: Option<Value>,
        block_state: Value,
        trigger_game_event: Option<String>,
    },
    ReplaceDisk {
        radius: LevelValue,
        height: LevelValue,
        offset: [i32; 3],
        predicate: Option<Value>,
        block_state: Value,
        trigger_game_event: Option<String>,
    },
    RunFunction(String),
    SetBlockProperties {
        properties: Value,
        offset: [i32; 3],
        trigger_game_event: Option<String>,
    },
    SpawnParticles {
        particle: Value,
        horizontal_position: PositionSource,
        vertical_position: PositionSource,
        horizontal_velocity: VelocitySource,
        vertical_velocity: VelocitySource,
        speed: FloatProvider,
    },
    SummonEntity {
        entity: HolderSet,
        join_team: bool,
    },
}

/// Evaluates a requirement about an item alone (its enchantment `level`
/// and the tool), for effects that have no entity to look at.
pub type ItemCondition = fn(&Value, crate::item::AnyItem<'_>, u32, &mut dyn rand::RngCore) -> bool;

static ITEM_CONDITION: OnceLock<ItemCondition> = OnceLock::new();

/// Installs the full loot-condition evaluator for item requirements; until
/// then only `match_tool` and the logical conditions are understood.
pub fn install_item_condition(condition: ItemCondition) {
    let _ = ITEM_CONDITION.set(condition);
}

/// Whether `requirement` holds for an effect of an enchantment at `level`
/// on `tool`.
pub fn item_requirement<M: crate::item::Mode>(
    requirement: Option<&Requirement>,
    tool: &crate::item::ItemStack<M>,
    level: u32,
    rng: &mut dyn rand::RngCore,
) -> bool {
    requirement.is_none_or(|requirement| match ITEM_CONDITION.get() {
        Some(condition) => condition(&requirement.condition, tool.any(), level, rng),
        None => crate::item::tool_requirement(&requirement.condition, tool.item()),
    })
}

/// A loot predicate gating an effect: its type, and the whole condition
/// object (a bare id becomes a `minecraft:reference`).
#[derive(Clone, Debug)]
pub struct Requirement {
    pub kind: Box<str>,
    pub condition: Value,
}

/// An effect applied when its loot-predicate requirements hold.
#[derive(Clone, Debug)]
pub struct Conditional<T> {
    pub effect: T,
    pub requirements: Option<Requirement>,
}

#[derive(Clone, Debug)]
pub struct Targeted<T> {
    pub enchanted: Target,
    pub affected: Target,
    pub effect: T,
    pub requirements: Option<Requirement>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChargingSounds {
    pub start: Option<SoundRef>,
    pub mid: Option<SoundRef>,
    pub end: Option<SoundRef>,
}

/// Every effect component an enchantment can carry; absent ones are empty.
#[derive(Clone, Debug, Default)]
pub struct EnchantmentEffects {
    pub damage_protection: Vec<Conditional<ValueEffect>>,
    pub damage_immunity: Vec<Conditional<()>>,
    pub damage: Vec<Conditional<ValueEffect>>,
    pub smash_damage_per_fallen_block: Vec<Conditional<ValueEffect>>,
    pub knockback: Vec<Conditional<ValueEffect>>,
    pub armor_effectiveness: Vec<Conditional<ValueEffect>>,
    pub post_attack: Vec<Targeted<EntityEffect>>,
    pub post_piercing_attack: Vec<Conditional<EntityEffect>>,
    pub hit_block: Vec<Conditional<EntityEffect>>,
    pub item_damage: Vec<Conditional<ValueEffect>>,
    pub equipment_drops: Vec<Targeted<ValueEffect>>,
    pub location_changed: Vec<Conditional<EntityEffect>>,
    pub tick: Vec<Conditional<EntityEffect>>,
    pub ammo_use: Vec<Conditional<ValueEffect>>,
    pub projectile_piercing: Vec<Conditional<ValueEffect>>,
    pub projectile_spawned: Vec<Conditional<EntityEffect>>,
    pub projectile_spread: Vec<Conditional<ValueEffect>>,
    pub projectile_count: Vec<Conditional<ValueEffect>>,
    pub trident_return_acceleration: Vec<Conditional<ValueEffect>>,
    pub fishing_time_reduction: Vec<Conditional<ValueEffect>>,
    pub fishing_luck_bonus: Vec<Conditional<ValueEffect>>,
    pub block_experience: Vec<Conditional<ValueEffect>>,
    pub mob_experience: Vec<Conditional<ValueEffect>>,
    pub repair_with_xp: Vec<Conditional<ValueEffect>>,
    pub attributes: Vec<AttributeEffect>,
    pub crossbow_charge_time: Option<ValueEffect>,
    pub crossbow_charging_sounds: Vec<ChargingSounds>,
    pub trident_sound: Vec<SoundRef>,
    pub prevent_equipment_drop: bool,
    pub prevent_armor_change: bool,
    pub trident_spin_attack_strength: Option<ValueEffect>,
}

/// Runs each value effect whose requirements `matches` accepts over `input`,
/// in order.
pub fn modify_value(
    effects: &[Conditional<ValueEffect>],
    level: u32,
    rng: &mut impl Rng,
    input: f32,
    mut matches: impl FnMut(&Requirement) -> bool,
) -> f32 {
    effects.iter().fold(input, |value, conditional| {
        if conditional.requirements.as_ref().is_none_or(&mut matches) {
            conditional.effect.process(level, rng, value)
        } else {
            value
        }
    })
}

/// One enchantment as the registry holds it.
#[derive(Clone, Debug)]
pub struct EnchantmentDef {
    /// Raw JSON text component.
    pub description: String,
    pub exclusive_set: HolderSet,
    pub supported_items: ItemSet,
    pub primary_items: Option<ItemSet>,
    pub weight: u32,
    pub max_level: u32,
    pub min_cost: EnchantmentCost,
    pub max_cost: EnchantmentCost,
    pub anvil_cost: u32,
    pub slots: Vec<String>,
    pub effects: EnchantmentEffects,
}

impl EnchantmentDef {
    fn from_builtin(enchantment: &Enchantment) -> Result<Self, String> {
        let mut effects = EnchantmentEffects::default();
        for (component, raw) in enchantment.effects {
            let json: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            decode_component(&mut effects, component, &json)
                .map_err(|e| format!("{component}: {e}"))?;
        }
        Ok(Self {
            description: enchantment.description.to_owned(),
            exclusive_set: HolderSet::Ids(
                enchantment
                    .exclusive_set
                    .iter()
                    .map(|id| normalize(id))
                    .collect(),
            ),
            supported_items: ItemSet::Items(enchantment.supported_items.to_vec()),
            primary_items: enchantment
                .primary_items
                .map(|items| ItemSet::Items(items.to_vec())),
            weight: enchantment.weight,
            max_level: enchantment.max_level,
            min_cost: enchantment.min_cost,
            max_cost: enchantment.max_cost,
            anvil_cost: enchantment.anvil_cost,
            slots: enchantment.slots.iter().map(|s| (*s).to_owned()).collect(),
            effects,
        })
    }

    /// Whether the enchantment can be put on `item`.
    pub fn supports(&self, item: Item) -> bool {
        self.supported_items.contains(item)
    }

    /// Whether the enchanting table offers this enchantment for `item`.
    pub fn is_primary(&self, item: Item) -> bool {
        self.primary_items
            .as_ref()
            .unwrap_or(&self.supported_items)
            .contains(item)
    }

    /// Whether `id` is in this enchantment's exclusive set.
    pub fn excludes(&self, id: &str) -> bool {
        match &self.exclusive_set {
            HolderSet::Ids(ids) => ids.iter().any(|other| other == id),
            HolderSet::Tag(tag) => {
                tuff_datapack::tags::with(|tags| tags.contains("minecraft:enchantment", tag, id))
            }
        }
    }

    pub fn decode(json: &Value) -> Result<Self, String> {
        let object = object(json)?;
        let mut effects = EnchantmentEffects::default();
        if let Some(map) = object.get("effects") {
            for (component, value) in self::object(map)? {
                decode_component(&mut effects, component, value)
                    .map_err(|e| format!("effects.{component}: {e}"))?;
            }
        }
        let weight = bounded(object, "weight", 1, 1024)?;
        let max_level = bounded(object, "max_level", 1, 255)?;
        Ok(Self {
            description: field(object, "description")?.to_string(),
            exclusive_set: match object.get("exclusive_set") {
                Some(value) => holder_set(value)?,
                None => HolderSet::Ids(Vec::new()),
            },
            supported_items: item_set(field(object, "supported_items")?)?,
            primary_items: object.get("primary_items").map(item_set).transpose()?,
            weight,
            max_level,
            min_cost: cost(field(object, "min_cost")?)?,
            max_cost: cost(field(object, "max_cost")?)?,
            anvil_cost: bounded(object, "anvil_cost", 0, u32::MAX as i64)?,
            slots: list(field(object, "slots")?)
                .iter()
                .map(|slot| string(slot).map(str::to_owned))
                .collect::<Result<_, _>>()?,
            effects,
        })
    }
}

/// Every built-in enchantment, decoded once, keyed by namespaced id.
pub fn builtin() -> &'static HashMap<String, EnchantmentDef> {
    static BUILTIN: OnceLock<HashMap<String, EnchantmentDef>> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        tuff_vanilla_data::enchantment::ALL
            .iter()
            .map(|(name, enchantment)| {
                let def = EnchantmentDef::from_builtin(enchantment)
                    .unwrap_or_else(|e| panic!("built-in enchantment {name}: {e}"));
                (normalize(name), def)
            })
            .collect()
    })
}

/// The `minecraft:enchantment` registry codec.
pub struct EnchantmentCodec;

impl RegistryCodec for EnchantmentCodec {
    const KEY: &'static str = "minecraft:enchantment";
    type Value = EnchantmentDef;

    fn builtin(id: &str) -> Option<&'static EnchantmentDef> {
        builtin().get(&normalize(id))
    }

    fn decode(json: &Value, _ctx: &DecodeCtx<'_>) -> Result<EnchantmentDef, String> {
        EnchantmentDef::decode(json)
    }
}

/// A pack enchantment as clients receive it: validated, then its JSON as
/// network NBT (numbers in a list share one type).
pub fn network_nbt(json: &Value) -> Result<Vec<u8>, String> {
    EnchantmentDef::decode(json)?;
    json_tag(json)
        .ok_or_else(|| "null value".to_owned())?
        .to_network_bytes()
        .map_err(|err| err.to_string())
}

pub(crate) fn json_tag(json: &Value) -> Option<tuff_nbt::Tag> {
    use tuff_nbt::Tag;
    Some(match json {
        Value::Null => return None,
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Tag::Int(i),
            None => Tag::Double(n.as_f64()?),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(values) => {
            let mut tags: Vec<Tag> = values.iter().map(json_tag).collect::<Option<_>>()?;
            if tags.iter().any(|t| matches!(t, Tag::Double(_)))
                && tags
                    .iter()
                    .all(|t| matches!(t, Tag::Int(_) | Tag::Double(_)))
            {
                for tag in &mut tags {
                    if let Tag::Int(i) = *tag {
                        *tag = Tag::Double(f64::from(i));
                    }
                }
            }
            Tag::List(tags)
        }
        Value::Object(map) => Tag::Compound(
            map.iter()
                .map(|(k, v)| Some((k.clone(), json_tag(v)?)))
                .collect::<Option<tuff_nbt::Compound>>()?,
        ),
    })
}

fn decode_component(
    effects: &mut EnchantmentEffects,
    component: &str,
    json: &Value,
) -> Result<(), String> {
    let values = |json| conditional_list(json, value_effect);
    let entities = |json| conditional_list(json, entity_effect);
    match component.strip_prefix("minecraft:").unwrap_or(component) {
        "damage_protection" => effects.damage_protection = values(json)?,
        "damage_immunity" => {
            effects.damage_immunity = conditional_list(json, |effect| {
                object(effect)?;
                Ok(())
            })?
        }
        "damage" => effects.damage = values(json)?,
        "smash_damage_per_fallen_block" => effects.smash_damage_per_fallen_block = values(json)?,
        "knockback" => effects.knockback = values(json)?,
        "armor_effectiveness" => effects.armor_effectiveness = values(json)?,
        "post_attack" => {
            effects.post_attack = list(json)
                .iter()
                .map(|entry| targeted(entry, false, entity_effect))
                .collect::<Result<_, _>>()?
        }
        "post_piercing_attack" => effects.post_piercing_attack = entities(json)?,
        "hit_block" => effects.hit_block = entities(json)?,
        "item_damage" => effects.item_damage = values(json)?,
        "equipment_drops" => {
            effects.equipment_drops = list(json)
                .iter()
                .map(|entry| targeted(entry, true, value_effect))
                .collect::<Result<_, _>>()?
        }
        "location_changed" => effects.location_changed = conditional_list(json, location_effect)?,
        "tick" => effects.tick = entities(json)?,
        "ammo_use" => effects.ammo_use = values(json)?,
        "projectile_piercing" => effects.projectile_piercing = values(json)?,
        "projectile_spawned" => effects.projectile_spawned = entities(json)?,
        "projectile_spread" => effects.projectile_spread = values(json)?,
        "projectile_count" => effects.projectile_count = values(json)?,
        "trident_return_acceleration" => effects.trident_return_acceleration = values(json)?,
        "fishing_time_reduction" => effects.fishing_time_reduction = values(json)?,
        "fishing_luck_bonus" => effects.fishing_luck_bonus = values(json)?,
        "block_experience" => effects.block_experience = values(json)?,
        "mob_experience" => effects.mob_experience = values(json)?,
        "repair_with_xp" => effects.repair_with_xp = values(json)?,
        "attributes" => {
            effects.attributes = list(json)
                .iter()
                .map(|entry| attribute_effect(object(entry)?))
                .collect::<Result<_, _>>()?
        }
        "crossbow_charge_time" => effects.crossbow_charge_time = Some(value_effect(json)?),
        "crossbow_charging_sounds" => {
            effects.crossbow_charging_sounds = list(json)
                .iter()
                .map(|entry| {
                    let object = object(entry)?;
                    let sound = |key| object.get(key).map(sound_ref).transpose();
                    Ok(ChargingSounds {
                        start: sound("start")?,
                        mid: sound("mid")?,
                        end: sound("end")?,
                    })
                })
                .collect::<Result<_, String>>()?
        }
        "trident_sound" => {
            effects.trident_sound = list(json).iter().map(sound_ref).collect::<Result<_, _>>()?
        }
        "prevent_equipment_drop" => {
            object(json)?;
            effects.prevent_equipment_drop = true;
        }
        "prevent_armor_change" => {
            object(json)?;
            effects.prevent_armor_change = true;
        }
        "trident_spin_attack_strength" => {
            effects.trident_spin_attack_strength = Some(value_effect(json)?)
        }
        other => return Err(format!("unknown effect component {other}")),
    }
    Ok(())
}

fn conditional_list<T>(
    json: &Value,
    effect: impl Fn(&Value) -> Result<T, String>,
) -> Result<Vec<Conditional<T>>, String> {
    list(json)
        .iter()
        .map(|entry| {
            let object = object(entry)?;
            Ok(Conditional {
                effect: effect(field(object, "effect")?)?,
                requirements: requirements(object)?,
            })
        })
        .collect()
}

fn targeted<T>(
    json: &Value,
    equipment_drops: bool,
    effect: impl Fn(&Value) -> Result<T, String>,
) -> Result<Targeted<T>, String> {
    let object = object(json)?;
    let enchanted = target(field(object, "enchanted")?)?;
    let affected = if equipment_drops {
        if enchanted == Target::DamagingEntity {
            return Err("enchanted must be attacker or victim".to_owned());
        }
        Target::Victim
    } else {
        target(field(object, "affected")?)?
    };
    Ok(Targeted {
        enchanted,
        affected,
        effect: effect(field(object, "effect")?)?,
        requirements: requirements(object)?,
    })
}

fn target(json: &Value) -> Result<Target, String> {
    match string(json)? {
        "attacker" => Ok(Target::Attacker),
        "damaging_entity" => Ok(Target::DamagingEntity),
        "victim" => Ok(Target::Victim),
        other => Err(format!("unknown target {other}")),
    }
}

fn requirements(object: &Object) -> Result<Option<Requirement>, String> {
    let Some(value) = object.get("requirements") else {
        return Ok(None);
    };
    let condition = match value {
        Value::String(id) => serde_json::json!({
            "type": "minecraft:reference",
            "name": normalize(id),
        }),
        Value::Object(condition) => {
            let mut condition = condition.clone();
            let kind = normalize(string(field(&condition, "type")?)?);
            condition.insert("type".to_owned(), Value::String(kind));
            Value::Object(condition)
        }
        _ => return Err("requirements must be an object or id".to_owned()),
    };
    Ok(Some(Requirement {
        kind: condition["type"].as_str().unwrap_or_default().into(),
        condition,
    }))
}

pub fn level_value(json: &Value) -> Result<LevelValue, String> {
    if let Some(number) = json.as_f64() {
        return Ok(LevelValue::Constant(number as f32));
    }
    let object = object(json)?;
    let boxed = |key| level_value(field(object, key)?).map(Box::new);
    Ok(match kind(object)? {
        "clamped" => {
            let min = float(object, "min")?;
            let max = float(object, "max")?;
            if max <= min {
                return Err(format!(
                    "max must be larger than min, min: {min}, max: {max}"
                ));
            }
            LevelValue::Clamped {
                value: boxed("value")?,
                min,
                max,
            }
        }
        "fraction" => LevelValue::Fraction {
            numerator: boxed("numerator")?,
            denominator: boxed("denominator")?,
        },
        "levels_squared" => LevelValue::LevelsSquared {
            added: float(object, "added")?,
        },
        "linear" => LevelValue::Linear {
            base: float(object, "base")?,
            per_level_above_first: float(object, "per_level_above_first")?,
        },
        "exponent" => LevelValue::Exponent {
            base: boxed("base")?,
            power: boxed("power")?,
        },
        "lookup" => LevelValue::Lookup {
            values: list(field(object, "values")?)
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|v| v as f32)
                        .ok_or("lookup value not a number")
                })
                .collect::<Result<_, _>>()?,
            fallback: boxed("fallback")?,
        },
        other => return Err(format!("unknown level-based value {other}")),
    })
}

pub fn value_effect(json: &Value) -> Result<ValueEffect, String> {
    let object = object(json)?;
    let level = |key| level_value(field(object, key)?);
    Ok(match kind(object)? {
        "add" => ValueEffect::Add(level("value")?),
        "all_of" => ValueEffect::AllOf(
            list(field(object, "effects")?)
                .iter()
                .map(value_effect)
                .collect::<Result<_, _>>()?,
        ),
        "multiply" => ValueEffect::Multiply(level("factor")?),
        "remove_binomial" => ValueEffect::RemoveBinomial(level("chance")?),
        "exponential" => ValueEffect::Exponential {
            base: level("base")?,
            exponent: level("exponent")?,
        },
        "set" => ValueEffect::Set(level("value")?),
        other => return Err(format!("unknown value effect {other}")),
    })
}

pub fn entity_effect(json: &Value) -> Result<EntityEffect, String> {
    effect(json, false)
}

pub fn location_effect(json: &Value) -> Result<EntityEffect, String> {
    effect(json, true)
}

fn effect(json: &Value, location: bool) -> Result<EntityEffect, String> {
    let object = object(json)?;
    let level = |key| level_value(field(object, key)?);
    let flag = |key| object.get(key).map_or(Ok(false), boolean);
    let opt_string = |key| {
        object
            .get(key)
            .map(|v| string(v).map(normalize))
            .transpose()
    };
    Ok(match kind(object)? {
        "all_of" => EntityEffect::AllOf(
            list(field(object, "effects")?)
                .iter()
                .map(|effect| self::effect(effect, location))
                .collect::<Result<_, _>>()?,
        ),
        "apply_mob_effect" => EntityEffect::ApplyMobEffect {
            to_apply: holder_set(field(object, "to_apply")?)?,
            min_duration: level("min_duration")?,
            max_duration: level("max_duration")?,
            min_amplifier: level("min_amplifier")?,
            max_amplifier: level("max_amplifier")?,
        },
        "attribute" if location => EntityEffect::Attribute(attribute_effect(object)?),
        "change_item_damage" => EntityEffect::ChangeItemDamage(level("amount")?),
        "damage_entity" => EntityEffect::DamageEntity {
            damage_type: normalize(string(field(object, "damage_type")?)?),
            min_damage: level("min_damage")?,
            max_damage: level("max_damage")?,
        },
        "explode" => EntityEffect::Explode {
            attribute_to_user: flag("attribute_to_user")?,
            damage_type: opt_string("damage_type")?,
            knockback_multiplier: object
                .get("knockback_multiplier")
                .map(level_value)
                .transpose()?,
            immune_blocks: object.get("immune_blocks").map(holder_set).transpose()?,
            offset: match object.get("offset") {
                Some(offset) => vec3(offset)?,
                None => [0.0; 3],
            },
            radius: level("radius")?,
            create_fire: flag("create_fire")?,
            block_interaction: string(field(object, "block_interaction")?)?.to_owned(),
            small_particle: field(object, "small_particle")?.clone(),
            large_particle: field(object, "large_particle")?.clone(),
            block_particles: object
                .get("block_particles")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
            sound: sound_ref(field(object, "sound")?)?,
        },
        "ignite" => EntityEffect::Ignite(level("duration")?),
        "apply_impulse" => EntityEffect::ApplyImpulse {
            direction: vec3(field(object, "direction")?)?,
            coordinate_scale: vec3(field(object, "coordinate_scale")?)?,
            magnitude: level("magnitude")?,
        },
        "apply_exhaustion" => EntityEffect::ApplyExhaustion(level("amount")?),
        "play_sound" => EntityEffect::PlaySound {
            sounds: match field(object, "sound")? {
                Value::Array(sounds) => sounds.iter().map(sound_ref).collect::<Result<_, _>>()?,
                sound => vec![sound_ref(sound)?],
            },
            volume: float_provider(field(object, "volume")?)?,
            pitch: float_provider(field(object, "pitch")?)?,
        },
        "replace_block" => EntityEffect::ReplaceBlock {
            offset: offset(object)?,
            predicate: object.get("predicate").cloned(),
            block_state: field(object, "block_state")?.clone(),
            trigger_game_event: opt_string("trigger_game_event")?,
        },
        "replace_disk" => EntityEffect::ReplaceDisk {
            radius: level("radius")?,
            height: level("height")?,
            offset: offset(object)?,
            predicate: object.get("predicate").cloned(),
            block_state: field(object, "block_state")?.clone(),
            trigger_game_event: opt_string("trigger_game_event")?,
        },
        "run_function" => EntityEffect::RunFunction(normalize(string(field(object, "function")?)?)),
        "set_block_properties" => EntityEffect::SetBlockProperties {
            properties: field(object, "properties")?.clone(),
            offset: offset(object)?,
            trigger_game_event: opt_string("trigger_game_event")?,
        },
        "spawn_particles" => EntityEffect::SpawnParticles {
            particle: field(object, "particle")?.clone(),
            horizontal_position: position_source(field(object, "horizontal_position")?)?,
            vertical_position: position_source(field(object, "vertical_position")?)?,
            horizontal_velocity: velocity_source(field(object, "horizontal_velocity")?)?,
            vertical_velocity: velocity_source(field(object, "vertical_velocity")?)?,
            speed: match object.get("speed") {
                Some(speed) => float_provider(speed)?,
                None => FloatProvider::Constant(0.0),
            },
        },
        "summon_entity" => EntityEffect::SummonEntity {
            entity: holder_set(field(object, "entity")?)?,
            join_team: flag("join_team")?,
        },
        other => return Err(format!("unknown entity effect {other}")),
    })
}

fn attribute_effect(object: &Object) -> Result<AttributeEffect, String> {
    Ok(AttributeEffect {
        id: normalize(string(field(object, "id")?)?),
        attribute: normalize(string(field(object, "attribute")?)?),
        amount: level_value(field(object, "amount")?)?,
        operation: match string(field(object, "operation")?)? {
            "add_value" => AttributeOperation::AddValue,
            "add_multiplied_base" => AttributeOperation::AddMultipliedBase,
            "add_multiplied_total" => AttributeOperation::AddMultipliedTotal,
            other => return Err(format!("unknown attribute operation {other}")),
        },
    })
}

fn float_provider(json: &Value) -> Result<FloatProvider, String> {
    if let Some(number) = json.as_f64() {
        return Ok(FloatProvider::Constant(number as f32));
    }
    let object = object(json)?;
    Ok(match kind(object)? {
        "constant" => FloatProvider::Constant(float(object, "value")?),
        "uniform" => FloatProvider::Uniform {
            min_inclusive: float(object, "min_inclusive")?,
            max_exclusive: float(object, "max_exclusive")?,
        },
        "clamped_normal" => FloatProvider::ClampedNormal {
            mean: float(object, "mean")?,
            deviation: float(object, "deviation")?,
            min: float(object, "min")?,
            max: float(object, "max")?,
        },
        "trapezoid" => FloatProvider::Trapezoid {
            min: float(object, "min")?,
            max: float(object, "max")?,
            plateau: float(object, "plateau")?,
        },
        other => return Err(format!("unknown float provider {other}")),
    })
}

fn position_source(json: &Value) -> Result<PositionSource, String> {
    let object = object(json)?;
    Ok(PositionSource {
        kind: kind(object)?.to_owned(),
        offset: optional_float(object, "offset", 0.0)?,
        scale: optional_float(object, "scale", 1.0)?,
    })
}

fn velocity_source(json: &Value) -> Result<VelocitySource, String> {
    let object = object(json)?;
    Ok(VelocitySource {
        movement_scale: optional_float(object, "movement_scale", 0.0)?,
        base: match object.get("base") {
            Some(base) => float_provider(base)?,
            None => FloatProvider::Constant(0.0),
        },
    })
}

fn sound_ref(json: &Value) -> Result<SoundRef, String> {
    match json {
        Value::String(id) => Ok(SoundRef {
            id: normalize(id),
            range: None,
        }),
        Value::Object(object) => Ok(SoundRef {
            id: normalize(string(field(object, "sound_id")?)?),
            range: object
                .get("range")
                .map(|v| v.as_f64().map(|v| v as f32).ok_or("range not a number"))
                .transpose()?,
        }),
        _ => Err("sound must be an id or object".to_owned()),
    }
}

fn holder_set(json: &Value) -> Result<HolderSet, String> {
    match json {
        Value::String(id) => Ok(match id.strip_prefix('#') {
            Some(tag) => HolderSet::Tag(normalize(tag)),
            None => HolderSet::Ids(vec![normalize(id)]),
        }),
        Value::Array(ids) => Ok(HolderSet::Ids(
            ids.iter()
                .map(|id| string(id).map(normalize))
                .collect::<Result<_, _>>()?,
        )),
        _ => Err("expected an id, tag or list".to_owned()),
    }
}

fn item_set(json: &Value) -> Result<ItemSet, String> {
    match holder_set(json)? {
        HolderSet::Tag(tag) => Ok(ItemSet::Tag(tag)),
        HolderSet::Ids(ids) => ids
            .iter()
            .map(|id| {
                Item::from_name(id)
                    .or_else(|| Item::from_name(id.strip_prefix("minecraft:")?))
                    .ok_or_else(|| format!("unknown item {id}"))
            })
            .collect::<Result<_, _>>()
            .map(ItemSet::Items),
    }
}

fn cost(json: &Value) -> Result<EnchantmentCost, String> {
    let object = object(json)?;
    let int = |key| {
        field(object, key)?
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| format!("{key} is not an int"))
    };
    Ok(EnchantmentCost {
        base: int("base")?,
        per_level_above_first: int("per_level_above_first")?,
    })
}

fn offset(object: &Object) -> Result<[i32; 3], String> {
    let Some(value) = object.get("offset") else {
        return Ok([0; 3]);
    };
    let list = list(value);
    let int = |v: &Value| {
        v.as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or("offset is not an int")
    };
    match list {
        [x, y, z] => Ok([int(x)?, int(y)?, int(z)?]),
        _ => Err("offset must have 3 elements".to_owned()),
    }
}

fn vec3(json: &Value) -> Result<[f64; 3], String> {
    let number = |v: &Value| v.as_f64().ok_or("vector element is not a number");
    match list(json) {
        [x, y, z] => Ok([number(x)?, number(y)?, number(z)?]),
        _ => Err("vector must have 3 elements".to_owned()),
    }
}

pub fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn kind(object: &Object) -> Result<&str, String> {
    let kind = string(field(object, "type")?)?;
    Ok(kind.strip_prefix("minecraft:").unwrap_or(kind))
}

fn object(json: &Value) -> Result<&Object, String> {
    json.as_object()
        .ok_or_else(|| format!("expected an object, got {json}"))
}

fn list(json: &Value) -> &[Value] {
    match json {
        Value::Array(list) => list,
        one => std::slice::from_ref(one),
    }
}

fn field<'a>(object: &'a Object, key: &str) -> Result<&'a Value, String> {
    object
        .get(key)
        .ok_or_else(|| format!("missing field {key}"))
}

fn string(json: &Value) -> Result<&str, String> {
    json.as_str()
        .ok_or_else(|| format!("expected a string, got {json}"))
}

fn boolean(json: &Value) -> Result<bool, String> {
    json.as_bool()
        .ok_or_else(|| format!("expected a boolean, got {json}"))
}

fn float(object: &Object, key: &str) -> Result<f32, String> {
    field(object, key)?
        .as_f64()
        .map(|v| v as f32)
        .ok_or_else(|| format!("{key} is not a number"))
}

fn optional_float(object: &Object, key: &str, default: f32) -> Result<f32, String> {
    match object.get(key) {
        Some(_) => float(object, key),
        None => Ok(default),
    }
}

fn bounded(object: &Object, key: &str, min: i64, max: i64) -> Result<u32, String> {
    let value = field(object, key)?
        .as_i64()
        .ok_or_else(|| format!("{key} is not an int"))?;
    if !(min..=max).contains(&value) {
        return Err(format!("{key} {value} outside [{min}, {max}]"));
    }
    Ok(value as u32)
}

/// The enchantment registry the server runs with.
pub type EnchantmentRegistry = RuntimeRegistry<EnchantmentDef>;

/// The enchantment and enchantment provider registries the server runs
/// with; dereferences to the enchantments.
pub struct EnchantmentData {
    pub enchantments: EnchantmentRegistry,
    pub providers: RuntimeRegistry<EnchantmentProvider>,
}

impl std::ops::Deref for EnchantmentData {
    type Target = EnchantmentRegistry;

    fn deref(&self) -> &EnchantmentRegistry {
        &self.enchantments
    }
}

/// The built-in enchantments and providers, shared.
pub fn vanilla() -> Arc<EnchantmentData> {
    static VANILLA: OnceLock<Arc<EnchantmentData>> = OnceLock::new();
    VANILLA
        .get_or_init(|| {
            let registries = Registries::vanilla();
            Arc::new(EnchantmentData {
                enchantments: registries
                    .decode::<EnchantmentCodec>()
                    .unwrap_or_else(|e| panic!("built-in enchantments: {e}")),
                providers: registries
                    .decode::<EnchantmentProviderCodec>()
                    .unwrap_or_else(|e| panic!("built-in enchantment providers: {e}")),
            })
        })
        .clone()
}

/// The enchantments and providers `registries` describe; the shared
/// built-in ones when no pack touched them. Provider references to
/// enchantments must resolve.
pub fn from_registries(registries: &Registries) -> Result<Arc<EnchantmentData>, RegistryError> {
    let modified = |key| registries.get(key).is_some_and(|raw| raw.modified());
    if !modified(EnchantmentCodec::KEY) && !modified(EnchantmentProviderCodec::KEY) {
        return Ok(vanilla());
    }
    let data = EnchantmentData {
        enchantments: registries.decode::<EnchantmentCodec>()?,
        providers: registries.decode::<EnchantmentProviderCodec>()?,
    };
    for (id, entry) in data.providers.iter() {
        if let EnchantmentProvider::Single { enchantment, .. } = entry.value()
            && !data.enchantments.contains(enchantment)
        {
            return Err(RegistryError {
                pack: String::new(),
                registry: EnchantmentProviderCodec::KEY,
                id: id.to_owned(),
                message: format!("unknown enchantment {enchantment}"),
            });
        }
    }
    Ok(Arc::new(data))
}

tokio::task_local! {
    static CURRENT: Arc<EnchantmentData>;
}

/// Runs `future` with `data` as the enchantments in effect.
pub fn scope<F: std::future::Future>(
    data: Arc<EnchantmentData>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    CURRENT.scope(data, future)
}

/// [`scope`] for synchronous code.
pub fn sync_scope<R>(data: Arc<EnchantmentData>, f: impl FnOnce() -> R) -> R {
    CURRENT.sync_scope(data, f)
}

/// The enchantments in effect: the enclosing [`scope`]'s, else the
/// built-in ones.
pub fn current() -> Arc<EnchantmentData> {
    CURRENT.try_with(Arc::clone).unwrap_or_else(|_| vanilla())
}

/// An integer drawn from a distribution (`int_provider` in data files).
#[derive(Clone, Debug, PartialEq)]
pub enum IntProvider {
    Constant(i32),
    Uniform {
        min: i32,
        max: i32,
    },
    BiasedToBottom {
        min: i32,
        max: i32,
    },
    Clamped {
        source: Box<IntProvider>,
        min: i32,
        max: i32,
    },
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: i32,
        max: i32,
    },
    WeightedList(Vec<(IntProvider, u32)>),
}

impl IntProvider {
    pub fn sample(&self, rng: &mut impl Rng) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::Uniform { min, max } => rng.gen_range(*min..=(*max).max(*min)),
            Self::BiasedToBottom { min, max } => {
                let span = (max - min + 1).max(1);
                let bound = rng.gen_range(0..span) + 1;
                min + rng.gen_range(0..bound)
            }
            Self::Clamped { source, min, max } => source.sample(rng).clamp(*min, *max),
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => ((mean + gaussian(rng) as f32 * deviation) as i32).clamp(*min, *max),
            Self::WeightedList(entries) => {
                let total: u32 = entries.iter().map(|(_, weight)| weight).sum();
                if total == 0 {
                    return 0;
                }
                let mut roll = rng.gen_range(0..total);
                for (provider, weight) in entries {
                    if roll < *weight {
                        return provider.sample(rng);
                    }
                    roll -= weight;
                }
                0
            }
        }
    }
}

pub fn int_provider(json: &Value) -> Result<IntProvider, String> {
    if let Some(value) = json.as_i64() {
        return i32::try_from(value)
            .map(IntProvider::Constant)
            .map_err(|_| format!("{value} out of range"));
    }
    let object = object(json)?;
    let int = |key: &str| {
        field(object, key)?
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| format!("{key} is not an int"))
    };
    Ok(match kind(object)? {
        "constant" => IntProvider::Constant(int("value")?),
        "uniform" => IntProvider::Uniform {
            min: int("min_inclusive")?,
            max: int("max_inclusive")?,
        },
        "biased_to_bottom" => IntProvider::BiasedToBottom {
            min: int("min_inclusive")?,
            max: int("max_inclusive")?,
        },
        "clamped" => IntProvider::Clamped {
            source: Box::new(int_provider(field(object, "source")?)?),
            min: int("min_inclusive")?,
            max: int("max_inclusive")?,
        },
        "clamped_normal" => IntProvider::ClampedNormal {
            mean: float(object, "mean")?,
            deviation: float(object, "deviation")?,
            min: int("min_inclusive")?,
            max: int("max_inclusive")?,
        },
        "weighted_list" => IntProvider::WeightedList(
            list(field(object, "distribution")?)
                .iter()
                .map(|entry| {
                    let entry = self::object(entry)?;
                    Ok((
                        int_provider(field(entry, "data")?)?,
                        bounded(entry, "weight", 0, i32::MAX as i64)?,
                    ))
                })
                .collect::<Result<_, String>>()?,
        ),
        other => return Err(format!("unknown int provider {other}")),
    })
}

/// How mob gear and some loot get enchanted (`enchantment_provider`).
#[derive(Clone, Debug, PartialEq)]
pub enum EnchantmentProvider {
    /// Enchanting-table style selection at a cost scaled by the local
    /// difficulty's special multiplier.
    ByCostWithDifficulty {
        enchantments: HolderSet,
        min_cost: i32,
        max_cost_span: i32,
    },
    ByCost {
        enchantments: HolderSet,
        cost: IntProvider,
    },
    Single {
        enchantment: String,
        level: IntProvider,
    },
}

impl EnchantmentProvider {
    pub fn decode(json: &Value) -> Result<Self, String> {
        let object = object(json)?;
        Ok(match kind(object)? {
            "by_cost_with_difficulty" => Self::ByCostWithDifficulty {
                enchantments: holder_set(field(object, "enchantments")?)?,
                min_cost: bounded(object, "min_cost", 1, 10000)? as i32,
                max_cost_span: bounded(object, "max_cost_span", 0, 10000)? as i32,
            },
            "by_cost" => Self::ByCost {
                enchantments: holder_set(field(object, "enchantments")?)?,
                cost: int_provider(field(object, "cost")?)?,
            },
            "single" => Self::Single {
                enchantment: normalize(string(field(object, "enchantment")?)?),
                level: int_provider(field(object, "level")?)?,
            },
            other => return Err(format!("unknown enchantment provider {other}")),
        })
    }
}

/// The built-in enchantment providers, keyed by namespaced id.
pub fn builtin_providers() -> &'static HashMap<String, EnchantmentProvider> {
    static BUILTIN: OnceLock<HashMap<String, EnchantmentProvider>> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let single = |enchantment: &str, level| EnchantmentProvider::Single {
            enchantment: normalize(enchantment),
            level: IntProvider::Constant(level),
        };
        [
            (
                "mob_spawn_equipment",
                EnchantmentProvider::ByCostWithDifficulty {
                    enchantments: HolderSet::Tag("minecraft:on_mob_spawn_equipment".to_owned()),
                    min_cost: 5,
                    max_cost_span: 17,
                },
            ),
            ("pillager_spawn_crossbow", single("piercing", 1)),
            ("raid/pillager_post_wave_3", single("quick_charge", 1)),
            ("raid/pillager_post_wave_5", single("quick_charge", 2)),
            ("raid/vindicator", single("sharpness", 1)),
            ("raid/vindicator_post_wave_5", single("sharpness", 2)),
            ("enderman_loot_drop", single("silk_touch", 1)),
        ]
        .into_iter()
        .map(|(id, provider)| (normalize(id), provider))
        .collect()
    })
}

/// The `minecraft:enchantment_provider` registry codec.
pub struct EnchantmentProviderCodec;

impl RegistryCodec for EnchantmentProviderCodec {
    const KEY: &'static str = "minecraft:enchantment_provider";
    type Value = EnchantmentProvider;

    fn builtin(id: &str) -> Option<&'static EnchantmentProvider> {
        builtin_providers().get(&normalize(id))
    }

    fn decode(json: &Value, _ctx: &DecodeCtx<'_>) -> Result<EnchantmentProvider, String> {
        EnchantmentProvider::decode(json)
    }
}

/// Each enchantment of `on` that `registry` knows, with its definition and
/// level (unknown ids are skipped, as the game drops them).
pub fn leveled<'a>(
    registry: &'a EnchantmentRegistry,
    on: Option<&'a crate::item::Enchantments>,
) -> impl Iterator<Item = (&'a str, &'a EnchantmentDef, u32)> + 'a {
    on.into_iter()
        .flat_map(|enchantments| enchantments.entries())
        .filter_map(move |(id, level)| {
            let def = registry.get(id).or_else(|| registry.get(&normalize(id)))?;
            Some((id.as_str(), def, (*level).max(0) as u32))
        })
}

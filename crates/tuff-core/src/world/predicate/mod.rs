//! The predicate language advancement criteria and data pack predicates
//! use, evaluated over facts the reporting actor gathered: item, entity,
//! location, damage and distance predicates, number bounds, and loot
//! conditions. Conditions that read the world, the scoreboard or a random
//! source need a [`PredicateWorld`] (commands have one, advancement
//! criteria do not). A field this server cannot evaluate never matches, so
//! a criterion that needs it simply stays unmet.

use serde_json::Value;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, MobEffect};
use tuff_vanilla_data::{Block, BlockState, Item};

use crate::protocol::play::GameMode;
use tuff_item::item::{Enchantments, ItemStack};
use tuff_nbt::{Compound, Tag};

mod numbers;
mod sub;

pub use sub::slot_range;

/// An entity as a predicate sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityFacts {
    pub kind: Option<EntityType>,
    /// The entity id, when the source of the facts knows it.
    pub entity_id: Option<i32>,
    pub position: [f64; 3],
    /// The dimension key, e.g. `minecraft:overworld`.
    pub dimension: String,
    pub biome: Option<String>,
    pub flags: Flags,
    pub equipment: Vec<(EquipmentSlot, tuff_item::item::ItemSnapshot)>,
    pub vehicle: Option<Box<EntityFacts>>,
    pub passengers: Vec<EntityFacts>,
    /// Entity data components such as `minecraft:cat/variant`, by id.
    pub components: Vec<(String, String)>,
    /// The block under the entity's feet.
    pub stepping_on: Option<BlockState>,
    /// Players: the entity in the crosshair within 100 blocks and line of
    /// sight.
    pub looking_at: Option<Box<EntityFacts>>,
    /// Players: where the eyes are and which way they face, for
    /// resolving `looking_at` against a region.
    pub view: Option<ViewRay>,
    /// `(effect, amplifier, duration)`.
    pub effects: Vec<EffectFact>,
    /// The name its scores are kept under: a player's name, else its UUID.
    pub holder: Option<String>,
    /// Its scoreboard tags.
    pub tags: Vec<String>,
    /// Its saved form, for `nbt` checks.
    pub nbt: Option<Compound>,
    pub player: Option<PlayerFacts>,
    /// Ticks since it was loaded or created.
    pub tick_count: Option<i64>,
    /// Its movement over the last tick.
    pub movement: Option<[f64; 3]>,
    pub fall_distance: Option<f64>,
    /// What it attacks (mobs).
    pub target: Option<Box<EntityFacts>>,
    /// Fishing bobbers: whether the hook floats in open water.
    pub open_water: Option<bool>,
    /// Lightning bolts: how many fires the strike lit.
    pub blocks_set_on_fire: Option<i32>,
    /// Lightning bolts: the entities the strike hit.
    pub entity_struck: Vec<EntityFacts>,
}

/// What a player's session publishes for predicates beyond its saved
/// form and published state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveFacts {
    pub sprinting: bool,
    pub swimming: bool,
    /// The last Player Input flags.
    pub input: u8,
    pub tick_count: i64,
    pub movement: [f64; 3],
    pub fall_distance: f64,
    pub stats: std::sync::Arc<crate::world::stats::Stats>,
}

impl EntityFacts {
    /// Adds what a player's session published live.
    pub fn with_live(mut self, live: &LiveFacts) -> Self {
        self.flags.sprinting = live.sprinting;
        self.flags.swimming = live.swimming;
        self.tick_count = Some(live.tick_count);
        self.movement = Some(live.movement);
        self.fall_distance = Some(live.fall_distance);
        if let Some(player) = &mut self.player {
            player.input = Some(live.input);
            player.stats = Some(live.stats.clone());
        }
        self
    }
}

/// An eye position and facing in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRay {
    pub eye: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
}

/// What `type_specific/player` reads of a player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerFacts {
    pub game_mode: GameMode,
    pub level: i32,
    pub advancements: Option<crate::world::advancement::View>,
    pub stats: Option<std::sync::Arc<crate::world::stats::Stats>>,
    /// The last Player Input flags.
    pub input: Option<u8>,
    /// Food level and saturation, when known without the saved form.
    pub food: Option<(i32, f32)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub on_fire: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub swimming: bool,
    pub on_ground: bool,
    pub flying: bool,
    pub baby: bool,
    pub in_water: bool,
    pub fall_flying: bool,
}

/// Where a location predicate is tested.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocationFacts {
    pub position: [f64; 3],
    pub dimension: String,
    pub biome: Option<String>,
    pub block: Option<BlockState>,
    /// The block's saved block entity, for block `nbt` checks.
    pub block_entity: Option<Compound>,
    /// The brightness there: sky light less the sky's darkening, or block
    /// light if brighter.
    pub light: Option<u8>,
    pub can_see_sky: Option<bool>,
    /// Whether a lit campfire smokes up through it.
    pub smokey: Option<bool>,
    /// The structures with a piece containing it.
    pub structures: Option<Vec<String>>,
}

/// What a condition reads beyond the facts it is handed: the world around
/// the origin, the scoreboard and a random source.
pub trait PredicateWorld {
    /// The block containing `position` as location predicates see it,
    /// with only position and dimension known where it is not loaded.
    fn location(&self, position: [f64; 3]) -> LocationFacts;
    fn raining(&self) -> bool;
    fn thundering(&self) -> bool;
    /// The total ticks of the world clock `id`.
    fn clock(&self, id: &str) -> Option<i64>;
    fn score(&self, holder: &str, objective: &str) -> Option<i32>;
    fn team(&self, holder: &str) -> Option<&str>;
    /// A uniform draw from `[0, 1)`.
    fn random(&self) -> f32;
    /// A uniform draw from `min..=max`; `min` when the range is empty.
    fn random_int(&self, min: i64, max: i64) -> i64;
    /// The command storage `id` (`minecraft:...`), if it was read.
    fn storage(&self, _id: &str) -> Option<Compound> {
        None
    }
    /// The environment attribute `key` (`minecraft:gameplay/...`) at
    /// `position`, as a bool, number or string; `None` where it is not
    /// modelled.
    fn attribute(&self, _key: &str, _position: [f64; 3]) -> Option<Value> {
        None
    }
    /// The entity an `entity_properties` selector other than `this` names
    /// (`attacker`, `direct_attacker`, `attacking_player`,
    /// `target_entity`), where the context has one.
    fn selected(&self, _selector: &str) -> Option<&EntityFacts> {
        None
    }
    /// The damage being dealt, for `damage_source_properties`.
    fn damage(&self) -> Option<&DamageFacts> {
        None
    }
    /// Whether the enchantment effect being tested was active, for
    /// `enchantment_active_check`.
    fn enchantment_active(&self) -> Option<bool> {
        None
    }
}

/// A damage source.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageFacts {
    pub kind: &'static DamageType,
    /// The entity the damage is blamed on.
    pub source: Option<EntityFacts>,
    /// The entity that dealt it directly (a projectile or the attacker).
    pub direct: Option<EntityFacts>,
}

/// What a loot condition is tested against: `this` is the entity, `origin`
/// the position distances are measured from, and `block`/`tool` the block
/// and item of a block interaction.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub this: Option<&'a EntityFacts>,
    pub target: Option<&'a EntityFacts>,
    pub origin: [f64; 3],
    pub location: Option<&'a LocationFacts>,
    pub tool: Option<&'a ItemStack>,
    /// The level of the enchantment being applied, for level based values.
    pub enchantment_level: Option<i32>,
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

fn same_id(expected: &str, actual: &str) -> bool {
    match expected.split_once(':') {
        Some(_) => expected == actual,
        None => actual.strip_prefix("minecraft:") == Some(expected),
    }
}

fn attribute_equals(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(a, b)| a as f32 == b as f32),
        (Value::Bool(a), Value::Number(b)) => b.as_f64().is_some_and(|b| (b != 0.0) == *a),
        (Value::String(a), Value::String(b)) => qualified(a) == qualified(b),
        _ => actual == expected,
    }
}

/// Whether `name` is in a holder set: one id, a `#tag`, or a list of ids.
pub fn in_set(set: &Value, name: &str, in_tag: impl Fn(&str) -> bool) -> bool {
    match set {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => in_tag(&qualified(tag)),
            None => same_id(entry, name),
        },
        Value::Array(entries) => entries
            .iter()
            .any(|e| e.as_str().is_some_and(|e| same_id(e, name))),
        _ => false,
    }
}

/// Whether `#tag` (namespaced) of `registry` holds `id` in the tags in
/// effect.
fn in_tag(registry: &str, tag: &str, id: &str) -> bool {
    tuff_datapack::tags::with(|tags| tags.contains(registry, tag, id))
}

fn item_in_tag(item: Item, tag: &str) -> bool {
    in_tag("minecraft:item", tag, item.name())
}

fn block_in_tag(block: Block, tag: &str) -> bool {
    in_tag("minecraft:block", tag, block.name())
}

fn entity_in_tag(kind: EntityType, tag: &str) -> bool {
    in_tag("minecraft:entity_type", tag, kind.name())
}

fn biome_in_tag(biome: &str, tag: &str) -> bool {
    in_tag("minecraft:worldgen/biome", tag, biome)
}

fn enchantment_in_tag(id: &str, tag: &str) -> bool {
    in_tag("minecraft:enchantment", tag, id)
}

fn potion_in_tag(potion: tuff_vanilla_data::registry::Potion, tag: &str) -> bool {
    in_tag("minecraft:potion", tag, potion.name())
}

/// Integer or float bounds: a number, or `{min, max}` with either end
/// optional.
pub fn bounds(bound: &Value, value: f64) -> bool {
    if let Some(exact) = bound.as_f64() {
        return value == exact;
    }
    let Some(range) = bound.as_object() else {
        return false;
    };
    range.keys().all(|key| key == "min" || key == "max")
        && range
            .get("min")
            .and_then(Value::as_f64)
            .is_none_or(|min| value >= min)
        && range
            .get("max")
            .and_then(Value::as_f64)
            .is_none_or(|max| value <= max)
}

/// Bounds on a length given its square, as the game tests distances.
pub fn bounds_sqr(bound: &Value, value_sqr: f64) -> bool {
    if let Some(exact) = bound.as_f64() {
        return value_sqr == exact * exact;
    }
    let Some(range) = bound.as_object() else {
        return false;
    };
    let edge = |key: &str| range.get(key).and_then(Value::as_f64);
    range.keys().all(|key| key == "min" || key == "max")
        && edge("min").is_none_or(|min| min < 0.0 || value_sqr >= min * min)
        && edge("max").is_none_or(|max| max >= 0.0 && value_sqr <= max * max)
}

/// The registry a `<kind>/variant` entity component draws from.
fn component_registry(id: &str) -> Option<String> {
    let kind = id.strip_prefix("minecraft:")?.strip_suffix("/variant")?;
    Some(match kind {
        "villager" => "minecraft:villager_type".to_owned(),
        kind => format!("minecraft:{kind}_variant"),
    })
}

/// An entity data component's value as a registry id, from the facts a
/// mob reports or else its saved form.
fn component_value(entity: &EntityFacts, id: &str) -> Option<String> {
    let id = qualified(id);
    if let Some((_, value)) = entity.components.iter().find(|(key, _)| same_id(key, &id)) {
        return Some(qualified(value));
    }
    let saved = entity.nbt.as_ref()?;
    if id == "minecraft:villager/variant" {
        return saved
            .get_compound("VillagerData")
            .and_then(|data| data.get_str("type"))
            .map(qualified);
    }
    component_registry(&id)?;
    saved.get_str("variant").map(qualified)
}

/// An item predicate: `items`, `count`, exact `components` and component
/// `predicates`.
pub fn item(predicate: &Value, stack: &ItemStack) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "items" => in_set(value, stack.item().name(), |tag| {
            item_in_tag(stack.item(), tag)
        }),
        "count" => bounds(value, f64::from(stack.count())),
        "components" => value.as_object().is_some_and(|components| {
            let patch = stack.components.to_nbt();
            components.iter().all(|(id, expected)| {
                let id = qualified(id);
                patch
                    .get(id.as_str())
                    .cloned()
                    .or_else(|| {
                        (!stack.components.is_removed(&id))
                            .then(|| default_component(stack, &id))
                            .flatten()
                    })
                    .is_some_and(|actual| nbt_equals_json(&actual, expected))
            })
        }),
        "predicates" => value.as_object().is_some_and(|predicates| {
            predicates
                .iter()
                .all(|(id, predicate)| item_sub_predicate(id, predicate, stack))
        }),
        _ => false,
    })
}

/// The prototype value of the item component `id` on `stack` for the
/// components the item type fixes.
fn default_component(stack: &ItemStack, id: &str) -> Option<Tag> {
    let item = stack.item();
    let int = |value: u32| Tag::Int(i32::try_from(value).unwrap_or(i32::MAX));
    match id {
        "minecraft:max_stack_size" => Some(Tag::Int(i32::from(item.max_stack_size()))),
        "minecraft:max_damage" => item.max_damage().map(int),
        "minecraft:damage" => item.max_damage().map(|_| Tag::Int(0)),
        "minecraft:repair_cost" => Some(Tag::Int(0)),
        _ => None,
    }
}

fn item_sub_predicate(id: &str, predicate: &Value, stack: &ItemStack) -> bool {
    match qualified(id).as_str() {
        "minecraft:enchantments" => enchantments(predicate, stack.components.enchantments.as_ref()),
        "minecraft:stored_enchantments" => {
            enchantments(predicate, stack.components.stored_enchantments.as_ref())
        }
        "minecraft:damage" => predicate.as_object().is_some_and(|fields| {
            let max = stack.max_damage();
            max.is_some()
                && fields.iter().all(|(key, bound)| match key.as_str() {
                    "damage" => bounds(bound, f64::from(stack.damage())),
                    "durability" => max.is_some_and(|max| {
                        bounds(bound, f64::from(max) - f64::from(stack.damage()))
                    }),
                    _ => false,
                })
        }),
        other => sub::item_component(other, predicate, stack).unwrap_or(false),
    }
}

/// A list of `{enchantments, levels}` entries, each needing a matching
/// enchantment on the item.
fn enchantments(predicate: &Value, on_item: Option<&Enchantments>) -> bool {
    let Some(wanted) = predicate.as_array() else {
        return false;
    };
    let entries = on_item.map_or(&[][..], Enchantments::entries);
    wanted.iter().all(|wanted| {
        entries.iter().any(|(id, level)| {
            wanted
                .get("enchantments")
                .is_none_or(|set| in_set(set, id, |tag| enchantment_in_tag(id, tag)))
                && wanted
                    .get("levels")
                    .is_none_or(|levels| bounds(levels, f64::from(*level)))
        })
    })
}

/// Structural equality of stored NBT and a JSON value, numbers compared
/// by value and booleans as bytes.
fn nbt_equals_json(tag: &Tag, value: &Value) -> bool {
    match (tag, value) {
        (Tag::Compound(compound), Value::Object(map)) => {
            compound.iter().count() == map.len()
                && map.iter().all(|(key, value)| {
                    compound
                        .get(key.as_str())
                        .is_some_and(|tag| nbt_equals_json(tag, value))
                })
        }
        (Tag::List(items), Value::Array(values)) => {
            items.len() == values.len()
                && items
                    .iter()
                    .zip(values)
                    .all(|(tag, value)| nbt_equals_json(tag, value))
        }
        (Tag::String(text), Value::String(expected)) => {
            text == expected || same_id(expected, text) || same_id(text, expected)
        }
        (Tag::Compound(compound), Value::String(text)) => {
            compound.len() == 1
                && compound
                    .get("text")
                    .is_some_and(|t| nbt_equals_json(t, &Value::String(text.clone())))
        }
        (Tag::String(text), Value::Object(map)) => {
            map.len() == 1 && map.get("text").and_then(Value::as_str) == Some(text)
        }
        (_, Value::Bool(b)) => number(tag) == Some(if *b { 1.0 } else { 0.0 }),
        (_, Value::Number(n)) => number(tag).is_some_and(|v| Some(v) == n.as_f64()),
        _ => false,
    }
}

fn number(tag: &Tag) -> Option<f64> {
    Some(match tag {
        Tag::Byte(v) => f64::from(*v),
        Tag::Short(v) => f64::from(*v),
        Tag::Int(v) => f64::from(*v),
        Tag::Long(v) => *v as f64,
        Tag::Float(v) => f64::from(*v),
        Tag::Double(v) => *v,
        _ => return None,
    })
}

fn entity_type_matches(set: &Value, kind: EntityType) -> bool {
    in_set(set, kind.name(), |tag| entity_in_tag(kind, tag))
}

/// Block predicate: `blocks` and `state`.
pub fn block(predicate: &Value, state: BlockState) -> bool {
    block_with(predicate, state, None)
}

/// [`block`], with the block entity for an `nbt` field.
pub fn block_with(predicate: &Value, state: BlockState, block_entity: Option<&Compound>) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "blocks" => in_set(value, state.block().name(), |tag| {
            block_in_tag(state.block(), tag)
        }),
        "state" => state_properties(value, state),
        "nbt" => block_entity.is_some_and(|saved| nbt_matches(value, saved)),
        _ => false,
    })
}

/// Whether an adventure mode predicate component (`can_place_on`,
/// `can_break`: one block predicate or a list of them) matches the block.
pub fn adventure_allows(
    component: &tuff_nbt::Tag,
    state: BlockState,
    block_entity: Option<&Compound>,
) -> bool {
    use tuff_nbt::Tag;
    let matches = |tag: &Tag| {
        let Tag::Compound(fields) = tag else {
            return false;
        };
        let mut rest = fields.clone();
        let nbt_ok = match rest.remove("nbt") {
            None => true,
            Some(Tag::String(text)) => {
                block_entity.is_some_and(|saved| nbt_matches(&Value::String(text), saved))
            }
            Some(Tag::Compound(expected)) => block_entity
                .is_some_and(|saved| crate::command::selector::compound_matches(&expected, saved)),
            Some(_) => false,
        };
        nbt_ok
            && block_with(
                &crate::world::commands::predicates::json_of(&Tag::Compound(rest)),
                state,
                None,
            )
    };
    match component {
        Tag::List(items) => items.iter().any(matches),
        other => matches(other),
    }
}

/// An NBT predicate (SNBT text) over a saved form.
fn nbt_matches(predicate: &Value, actual: &Compound) -> bool {
    let Some(text) = predicate.as_str() else {
        return false;
    };
    let mut reader = crate::command::reader::Reader::new(text);
    crate::command::args::read_nbt_compound(&mut reader)
        .is_ok_and(|expected| crate::command::selector::compound_matches(&expected, actual))
}

/// A boolean field, which NBT-born predicates write as a byte.
fn flag(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        _ => None,
    }
}

/// A fluid predicate: `fluids` and `state` (`level`, `falling`).
fn fluid(predicate: &Value, state: BlockState) -> bool {
    let fluid = state.fluid_state();
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    let kind = fluid.fluid();
    fields.iter().all(|(key, value)| match key.as_str() {
        "fluids" => in_set(value, kind.name(), |tag| {
            in_tag("minecraft:fluid", tag, kind.name())
        }),
        "state" => value.as_object().is_some_and(|properties| {
            properties.iter().all(|(name, expected)| {
                let actual = match name.as_str() {
                    "falling" => fluid.is_falling().to_string(),
                    "level" if !fluid.is_empty() => fluid.amount().to_string(),
                    _ => return false,
                };
                match expected {
                    Value::String(text) => *text == actual,
                    Value::Bool(flag) => flag.to_string() == actual,
                    Value::Number(n) => n.to_string() == actual,
                    Value::Object(_) => actual.parse::<f64>().is_ok_and(|v| bounds(expected, v)),
                    _ => false,
                }
            })
        }),
        _ => false,
    })
}

/// `{property: value}` or `{property: {min, max}}` over a block state.
pub fn state_properties(predicate: &Value, state: BlockState) -> bool {
    let Some(properties) = predicate.as_object() else {
        return false;
    };
    properties.iter().all(|(name, expected)| {
        let Some(actual) = state.get(name) else {
            return false;
        };
        match expected {
            Value::String(value) => actual == value,
            Value::Bool(value) => actual == value.to_string(),
            Value::Number(n) => n.to_string() == actual,
            Value::Object(range) => {
                let values = state.property_values(name).unwrap_or(&[]);
                let rank = |text: &str| match text {
                    "false" if values.contains(&"true") => Some(0),
                    "true" if values.contains(&"false") => Some(1),
                    _ => values.iter().position(|v| *v == text),
                };
                let Some(actual) = rank(actual) else {
                    return false;
                };
                let edge = |key: &str| match range.get(key) {
                    Some(Value::String(s)) => Some(rank(s)),
                    Some(Value::Number(n)) => Some(rank(&n.to_string())),
                    Some(Value::Bool(b)) => Some(rank(&b.to_string())),
                    _ => None,
                };
                range.keys().all(|k| k == "min" || k == "max")
                    && edge("min").is_none_or(|min| min.is_some_and(|min| actual >= min))
                    && edge("max").is_none_or(|max| max.is_some_and(|max| actual <= max))
            }
            _ => false,
        }
    })
}

/// A location predicate.
pub fn location(predicate: &Value, at: &LocationFacts) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "position" => value.as_object().is_some_and(|axes| {
            axes.iter().all(|(axis, bound)| {
                let index = match axis.as_str() {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    _ => return false,
                };
                bounds(bound, at.position[index])
            })
        }),
        "dimension" => value
            .as_str()
            .is_some_and(|dimension| same_id(dimension, &at.dimension)),
        "biomes" => at
            .biome
            .as_deref()
            .is_some_and(|biome| in_set(value, biome, |tag| biome_in_tag(biome, tag))),
        "block" => at
            .block
            .is_some_and(|state| block_with(value, state, at.block_entity.as_ref())),
        "fluid" => at.block.is_some_and(|state| fluid(value, state)),
        "light" => at.light.is_some_and(|light| {
            value
                .get("light")
                .is_none_or(|bound| bounds(bound, f64::from(light)))
        }),
        "smokey" => flag(value).is_some_and(|expected| at.smokey == Some(expected)),
        "structures" => at.structures.as_ref().is_some_and(|found| {
            found.iter().any(|id| {
                in_set(value, id, |tag| {
                    in_tag("minecraft:worldgen/structure", tag, id)
                })
            })
        }),
        "can_see_sky" => flag(value).is_some_and(|expected| at.can_see_sky == Some(expected)),
        _ => false,
    })
}

/// A distance predicate between two positions.
pub fn distance(predicate: &Value, from: [f64; 3], to: [f64; 3]) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    fields.iter().all(|(key, bound)| match key.as_str() {
        "x" => bounds(bound, d[0].abs()),
        "y" => bounds(bound, d[1].abs()),
        "z" => bounds(bound, d[2].abs()),
        "horizontal" => bounds_sqr(bound, d[0] * d[0] + d[2] * d[2]),
        "absolute" => bounds_sqr(bound, d[0] * d[0] + d[1] * d[1] + d[2] * d[2]),
        _ => false,
    })
}

fn slot_name(slot: EquipmentSlot) -> &'static str {
    match slot {
        EquipmentSlot::Mainhand => "mainhand",
        EquipmentSlot::Offhand => "offhand",
        EquipmentSlot::Feet => "feet",
        EquipmentSlot::Legs => "legs",
        EquipmentSlot::Chest => "chest",
        EquipmentSlot::Head => "head",
        EquipmentSlot::Body => "body",
        EquipmentSlot::Saddle => "saddle",
    }
}

/// An entity predicate, with `origin` for its `distance` check.
pub fn entity(predicate: &Value, entity: &EntityFacts, origin: [f64; 3]) -> bool {
    entity_in(predicate, entity, origin, None)
}

/// The middle of the block `drop` below `position`.
fn block_center_below(position: [f64; 3], drop: f64) -> [f64; 3] {
    [
        position[0].floor() + 0.5,
        (position[1] - drop).floor() + 0.5,
        position[2].floor() + 0.5,
    ]
}

/// [`entity`], reading the world around it when there is one.
pub fn entity_in(
    predicate: &Value,
    entity: &EntityFacts,
    origin: [f64; 3],
    world: Option<&dyn PredicateWorld>,
) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields
        .iter()
        .all(|(key, value)| match qualified(key).as_str() {
            "minecraft:entity_type" | "minecraft:type" => entity
                .kind
                .is_some_and(|kind| entity_type_matches(value, kind)),
            "minecraft:distance" => distance(value, origin, entity.position),
            "minecraft:location" => match world {
                Some(world) => location(value, &world.location(entity.position)),
                None => location(
                    value,
                    &LocationFacts {
                        position: entity.position,
                        dimension: entity.dimension.clone(),
                        biome: entity.biome.clone(),
                        ..LocationFacts::default()
                    },
                ),
            },
            "minecraft:stepping_on" => match world {
                Some(world) => {
                    entity.flags.on_ground
                        && location(
                            value,
                            &world.location(block_center_below(entity.position, 0.2)),
                        )
                }
                None => location(
                    value,
                    &LocationFacts {
                        position: entity.position.map(f64::floor),
                        dimension: entity.dimension.clone(),
                        biome: entity.biome.clone(),
                        block: entity.stepping_on,
                        ..LocationFacts::default()
                    },
                ),
            },
            "minecraft:movement_affected_by" => world.is_some_and(|world| {
                location(
                    value,
                    &world.location(block_center_below(entity.position, 0.500001)),
                )
            }),
            "minecraft:nbt" => entity
                .nbt
                .as_ref()
                .is_some_and(|saved| nbt_matches(value, saved)),
            "minecraft:entity_tags" => value.as_object().is_some_and(|lists| {
                let has = |tag: &Value| {
                    tag.as_str()
                        .is_some_and(|tag| entity.tags.iter().any(|t| t == tag))
                };
                lists.iter().all(|(key, list)| {
                    let Some(list) = list.as_array() else {
                        return false;
                    };
                    match key.as_str() {
                        "any_of" => list.iter().any(has),
                        "all_of" => list.iter().all(has),
                        "none_of" => !list.iter().any(has),
                        _ => false,
                    }
                })
            }),
            "minecraft:team" => world.is_some_and(|world| {
                entity
                    .holder
                    .as_deref()
                    .and_then(|holder| world.team(holder))
                    .is_some_and(|team| value.as_str() == Some(team))
            }),
            "minecraft:flags" => value.as_object().is_some_and(|flags| {
                flags.iter().all(|(flag, expected)| {
                    let actual = match flag.as_str() {
                        "is_on_fire" => entity.flags.on_fire,
                        "is_sneaking" => entity.flags.sneaking,
                        "is_sprinting" => entity.flags.sprinting,
                        "is_swimming" => entity.flags.swimming,
                        "is_on_ground" => entity.flags.on_ground,
                        "is_flying" => entity.flags.flying || entity.flags.fall_flying,
                        "is_baby" => entity.flags.baby,
                        "is_in_water" => entity.flags.in_water,
                        "is_fall_flying" => entity.flags.fall_flying,
                        _ => return false,
                    };
                    self::flag(expected) == Some(actual)
                })
            }),
            "minecraft:equipment" => value.as_object().is_some_and(|slots| {
                slots.iter().all(|(slot, predicate)| {
                    entity
                        .equipment
                        .iter()
                        .find(|(s, _)| slot_name(*s) == slot)
                        .is_some_and(|(_, stack)| item(predicate, stack))
                })
            }),
            "minecraft:vehicle" => entity
                .vehicle
                .as_deref()
                .is_some_and(|vehicle| entity_in(value, vehicle, origin, world)),
            "minecraft:passenger" => entity
                .passengers
                .iter()
                .any(|passenger| entity_in(value, passenger, origin, world)),
            "minecraft:targeted_entity" => entity
                .target
                .as_deref()
                .is_some_and(|target| entity_in(value, target, origin, world)),
            "minecraft:slots" => sub::slots(value, entity),
            "minecraft:movement" => sub::movement(value, entity),
            "minecraft:periodic_tick" => sub::periodic_tick(value, entity),
            "minecraft:type_specific/sheep" => sub::type_specific("sheep", value, entity),
            "minecraft:type_specific/cube_mob" => sub::type_specific("cube_mob", value, entity),
            "minecraft:type_specific/raider" => sub::type_specific("raider", value, entity),
            "minecraft:type_specific/fishing_hook" => {
                sub::type_specific("fishing_hook", value, entity)
            }
            "minecraft:type_specific/lightning" => sub::type_specific("lightning", value, entity),
            "minecraft:components" => value.as_object().is_some_and(|components| {
                components.iter().all(|(id, expected)| {
                    component_value(entity, id).is_some_and(|actual| {
                        expected.as_str().is_some_and(|e| same_id(e, &actual))
                    })
                })
            }),
            "minecraft:predicates" => value.as_object().is_some_and(|predicates| {
                predicates.iter().all(|(id, predicate)| {
                    let id = qualified(id);
                    component_registry(&id).is_some_and(|registry| {
                        component_value(entity, &id).is_some_and(|variant| {
                            in_set(predicate, &variant, |tag| in_tag(&registry, tag, &variant))
                        })
                    })
                })
            }),
            "minecraft:effects" => effects(value, &entity.effects),
            "minecraft:type_specific/player" => {
                entity.player.is_some()
                    && value.as_object().is_some_and(|fields| {
                        fields.iter().all(|(field, expected)| match field.as_str() {
                            "looking_at" => entity
                                .looking_at
                                .as_deref()
                                .is_some_and(|seen| self::entity(expected, seen, origin)),
                            "gamemode" => entity.player.as_ref().is_some_and(|player| {
                                let name = player.game_mode.name();
                                match expected {
                                    Value::Array(modes) => {
                                        modes.iter().any(|mode| mode.as_str() == Some(name))
                                    }
                                    Value::String(mode) => mode == name,
                                    _ => false,
                                }
                            }),
                            "level" => entity
                                .player
                                .as_ref()
                                .is_some_and(|player| bounds(expected, f64::from(player.level))),
                            "advancements" => entity
                                .player
                                .as_ref()
                                .and_then(|player| player.advancements.as_ref())
                                .is_some_and(|view| advancements(expected, view)),
                            other => sub::player_field(other, expected, entity).unwrap_or(false),
                        })
                    })
            }
            _ => false,
        })
}

/// `{advancement: done}` or `{advancement: {criterion: met}}` over a
/// player's progress.
fn advancements(predicate: &Value, view: &crate::world::advancement::View) -> bool {
    let Some(wanted) = predicate.as_object() else {
        return false;
    };
    wanted.iter().all(|(name, expected)| {
        let name = qualified(name);
        match expected {
            Value::Object(criteria) => {
                view.done(&name).is_some()
                    && criteria.iter().all(|(criterion, met)| {
                        flag(met).is_some_and(|met| view.criterion(&name, criterion) == Some(met))
                    })
            }
            other => flag(other).is_some_and(|done| view.done(&name) == Some(done)),
        }
    })
}

/// An active effect as predicates see it: effect, amplifier, remaining
/// ticks, ambient and visible.
pub type EffectFact = (MobEffect, i32, i64, bool, bool);

/// A mob effects predicate: every listed effect active, within bounds.
pub fn effects(predicate: &Value, active: &[EffectFact]) -> bool {
    let Some(wanted) = predicate.as_object() else {
        return false;
    };
    wanted.iter().all(|(id, conditions)| {
        active
            .iter()
            .find(|(effect, ..)| same_id(id, effect.name()))
            .is_some_and(|(_, amplifier, duration, ambient, visible)| {
                conditions.as_object().is_some_and(|fields| {
                    fields.iter().all(|(key, bound)| match key.as_str() {
                        "amplifier" => bounds(bound, f64::from(*amplifier)),
                        "duration" => bounds(bound, *duration as f64),
                        "ambient" => flag(bound) == Some(*ambient),
                        "visible" => flag(bound) == Some(*visible),
                        _ => false,
                    })
                })
            })
    })
}

/// A damage source predicate.
pub fn damage_source(predicate: &Value, damage: &DamageFacts, origin: [f64; 3]) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "tags" => value.as_array().is_some_and(|tags| {
            tags.iter().all(|tag| {
                let id = tag["id"].as_str().unwrap_or_default();
                let expected = tag["expected"].as_bool().unwrap_or(true);
                crate::world::damage_type_in_tag(damage.kind, id) == expected
            })
        }),
        "direct_entity" => damage
            .direct
            .as_ref()
            .is_some_and(|direct| entity(value, direct, origin)),
        "source_entity" => damage
            .source
            .as_ref()
            .is_some_and(|source| entity(value, source, origin)),
        "is_direct" => {
            let direct = match (&damage.source, &damage.direct) {
                (Some(source), Some(direct)) => {
                    source.position == direct.position && source.kind == direct.kind
                }
                (None, None) => true,
                _ => false,
            };
            value.as_bool() == Some(direct)
        }
        _ => false,
    })
}

/// A damage predicate (hurt triggers).
pub fn damage(
    predicate: &Value,
    source: &DamageFacts,
    dealt: f32,
    taken: f32,
    blocked: bool,
    origin: [f64; 3],
) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "dealt" => bounds(value, f64::from(dealt)),
        "taken" => bounds(value, f64::from(taken)),
        "blocked" => value.as_bool() == Some(blocked),
        "source_entity" => source
            .source
            .as_ref()
            .is_some_and(|source| entity(value, source, origin)),
        "type" => damage_source(value, source, origin),
        _ => false,
    })
}

/// A loot condition in a context, without a world: `entity_properties`,
/// `location_check`, `match_block`/`block_state_property`, `match_tool`
/// and the logical combinators. Predicate file references and anything
/// needing the world, the scoreboard or randomness never match.
pub fn condition(condition: &Value, context: &Context<'_>) -> bool {
    condition_in(condition, context, None)
}

/// A number provider's value (a float, or an int when `float` is false)
/// for `context`; `None` when it cannot be evaluated here.
pub fn number_in(
    provider: &Value,
    float: bool,
    context: &Context<'_>,
    world: Option<&dyn PredicateWorld>,
) -> Option<f64> {
    let numbers = numbers::Numbers { context, world };
    if float {
        numbers.float(provider)
    } else {
        numbers.int(provider).map(|value| value as f64)
    }
}

/// A world that only draws random numbers, for conditions about an item.
struct RandomOnly<'a>(std::cell::RefCell<&'a mut dyn rand::RngCore>);

impl PredicateWorld for RandomOnly<'_> {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        LocationFacts {
            position,
            ..LocationFacts::default()
        }
    }
    fn raining(&self) -> bool {
        false
    }
    fn thundering(&self) -> bool {
        false
    }
    fn clock(&self, _id: &str) -> Option<i64> {
        None
    }
    fn score(&self, _holder: &str, _objective: &str) -> Option<i32> {
        None
    }
    fn team(&self, _holder: &str) -> Option<&str> {
        None
    }
    fn random(&self) -> f32 {
        rand::Rng::r#gen(&mut *self.0.borrow_mut())
    }
    fn random_int(&self, min: i64, max: i64) -> i64 {
        if max <= min {
            min
        } else {
            rand::Rng::gen_range(&mut *self.0.borrow_mut(), min..=max)
        }
    }
}

/// Makes enchantment requirements about an item alone go through
/// [`condition_in`].
pub fn install_item_conditions() {
    tuff_item::enchantment::install_item_condition(|condition, tool, level, rng| {
        let context = Context {
            this: None,
            target: None,
            origin: [0.0; 3],
            location: None,
            tool: Some(tool),
            enchantment_level: Some(level as i32),
        };
        let world = RandomOnly(std::cell::RefCell::new(rng));
        condition_in(condition, &context, Some(&world))
    });
}

/// [`condition`], reading the world through `world` when there is one.
pub fn condition_in(
    condition: &Value,
    context: &Context<'_>,
    world: Option<&dyn PredicateWorld>,
) -> bool {
    let test = |c: &Value| condition_in(c, context, world);
    if let Some(list) = condition.as_array() {
        return list.iter().all(test);
    }
    let terms = || condition["terms"].as_array().map_or(&[][..], Vec::as_slice);
    let chance = |chance: Option<f64>| {
        world.is_some_and(|world| chance.is_some_and(|c| f64::from(world.random()) < c))
    };
    let this = || match condition["entity"].as_str() {
        Some("this") => context.this,
        Some(selector) => world.and_then(|world| world.selected(selector)),
        None => None,
    };
    let numbers = numbers::Numbers { context, world };
    match condition["type"].as_str().map(qualified).as_deref() {
        Some("minecraft:entity_properties") => match condition.get("predicate") {
            None => true,
            Some(predicate) => {
                this().is_some_and(|this| entity_in(predicate, this, context.origin, world))
            }
        },
        Some("minecraft:entity_scores") => {
            let holder = this().and_then(|this| this.holder.as_deref());
            match (world, holder, condition["scores"].as_object()) {
                (Some(world), Some(holder), Some(scores)) => {
                    scores.iter().all(|(objective, range)| {
                        world
                            .score(holder, objective)
                            .is_some_and(|value| numbers.int_range(range, i64::from(value)))
                    })
                }
                _ => false,
            }
        }
        Some("minecraft:location_check") => {
            let offset = ["offsetX", "offsetY", "offsetZ"]
                .map(|key| condition.get(key).and_then(Value::as_i64).unwrap_or(0));
            match world {
                Some(world) => {
                    let at = [0, 1, 2].map(|i| context.origin[i] + offset[i] as f64);
                    condition
                        .get("predicate")
                        .is_none_or(|p| location(p, &world.location(at)))
                }
                None => {
                    offset == [0, 0, 0]
                        && context.location.is_some_and(|at| {
                            condition.get("predicate").is_none_or(|p| location(p, at))
                        })
                }
            }
        }
        Some("minecraft:match_block") | Some("minecraft:block_state_property") => context
            .location
            .and_then(|at| at.block)
            .is_some_and(|state| {
                condition.get("blocks").is_none_or(|set| {
                    in_set(set, state.block().name(), |tag| {
                        block_in_tag(state.block(), tag)
                    })
                }) && condition
                    .get("block")
                    .and_then(Value::as_str)
                    .is_none_or(|id| same_id(id, state.block().name()))
                    && condition
                        .get("properties")
                        .or_else(|| condition.get("state"))
                        .is_none_or(|p| state_properties(p, state))
            }),
        Some("minecraft:match_tool") => context
            .tool
            .is_some_and(|tool| condition.get("predicate").is_none_or(|p| item(p, tool))),
        Some("minecraft:inverted") => !test(&condition["term"]),
        Some("minecraft:any_of") => terms().iter().any(test),
        Some("minecraft:all_of") => terms().iter().all(test),
        Some("minecraft:random_chance") => chance(numbers.float(&condition["chance"])),
        Some("minecraft:random_chance_with_enchanted_bonus") => {
            chance(condition["unenchanted_chance"].as_f64())
        }
        Some("minecraft:survives_explosion") => true,
        Some("minecraft:damage_source_properties") => world
            .and_then(PredicateWorld::damage)
            .is_some_and(|damage| {
                condition
                    .get("predicate")
                    .is_none_or(|p| damage_source(p, damage, context.origin))
            }),
        Some("minecraft:enchantment_active_check") => world
            .and_then(PredicateWorld::enchantment_active)
            .is_some_and(|active| condition["active"].as_bool() == Some(active)),
        Some("minecraft:table_bonus") => {
            let level = condition["enchantment"].as_str().map_or(0, |id| {
                context.tool.map_or(0, |tool| tool.enchantment_level(id))
            });
            let chances = condition["chances"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            let index = usize::try_from(level).unwrap_or(0);
            chance(
                chances
                    .get(index.min(chances.len().saturating_sub(1)))
                    .and_then(Value::as_f64),
            )
        }
        Some("minecraft:weather_check") => world.is_some_and(|world| {
            condition
                .get("raining")
                .and_then(flag)
                .is_none_or(|raining| raining == world.raining())
                && condition
                    .get("thundering")
                    .and_then(flag)
                    .is_none_or(|thundering| thundering == world.thundering())
        }),
        Some("minecraft:time_check") => {
            let clock = condition["clock"].as_str().map(qualified);
            let ticks = world
                .zip(clock)
                .and_then(|(world, clock)| world.clock(&clock));
            ticks.is_some_and(|mut ticks| {
                if let Some(period) = condition["period"].as_i64().filter(|p| *p != 0) {
                    ticks %= period;
                }
                numbers.int_range(&condition["value"], i64::from(ticks as i32))
            })
        }
        Some("minecraft:environment_attribute_check") => world.is_some_and(|world| {
            condition["attribute"].as_str().is_some_and(|key| {
                world
                    .attribute(&qualified(key), context.origin)
                    .is_some_and(|actual| attribute_equals(&actual, &condition["value"]))
            })
        }),
        Some("minecraft:int_value_check") => numbers
            .int(&condition["value"])
            .is_some_and(|value| numbers.int_range(&condition["test"], value)),
        Some("minecraft:float_value_check") => numbers
            .float(&condition["value"])
            .is_some_and(|value| numbers.float_range(&condition["test"], value)),
        _ => false,
    }
}

#[cfg(test)]
mod tests;

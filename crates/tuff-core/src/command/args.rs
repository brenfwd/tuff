//! Argument readers for every parser type the implemented commands use,
//! keyed by the parser's resource location from the command tree, plus the
//! completions the server offers for them.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use tuff_vanilla_data::data_registry::DataRegistry;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, Item, Registry};

use crate::protocol::play::GameMode;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, NbtError, Tag};

use super::block::{self, BlockInput, BlockPredicate};
use super::coords::{Angle, Coordinates, Rotation};
use super::error::CommandError;
use super::item::{self, ItemInput, ItemPredicate};
use super::range::{self, Bounds};
use super::reader::Reader;
use super::selector::{self, Selector};

const MAX_MESSAGE_LENGTH: usize = 256;
pub const SWING_ANIMATIONS: &[&str] = &["none", "whack", "stab"];
const HEIGHTMAPS: &[&str] = &[
    "motion_blocking",
    "motion_blocking_no_leaves",
    "ocean_floor",
    "world_surface",
];

/// The parsers the server implements; other argument nodes are neither
/// sent to clients nor parsed.
pub const SUPPORTED_PARSERS: &[&str] = &[
    "brigadier:bool",
    "brigadier:double",
    "brigadier:float",
    "brigadier:integer",
    "brigadier:long",
    "brigadier:string",
    "minecraft:angle",
    "minecraft:block_pos",
    "minecraft:block_predicate",
    "minecraft:block_state",
    "minecraft:column_pos",
    "minecraft:component",
    "minecraft:context_float_provider",
    "minecraft:context_int_provider",
    "minecraft:dialog",
    "minecraft:dimension",
    "minecraft:entity",
    "minecraft:entity_anchor",
    "minecraft:feature",
    "minecraft:float_range",
    "minecraft:function",
    "minecraft:game_profile",
    "minecraft:gamemode",
    "minecraft:heightmap",
    "minecraft:hex_color",
    "minecraft:int_range",
    "minecraft:item_predicate",
    "minecraft:item_stack",
    "minecraft:loot_modifier",
    "minecraft:loot_predicate",
    "minecraft:loot_table",
    "minecraft:message",
    "minecraft:nbt_compound_tag",
    "minecraft:nbt_path",
    "minecraft:nbt_tag",
    "minecraft:objective",
    "minecraft:objective_criteria",
    "minecraft:operation",
    "minecraft:particle",
    "minecraft:resource",
    "minecraft:resource_key",
    "minecraft:resource_location",
    "minecraft:resource_or_tag",
    "minecraft:resource_or_tag_key",
    "minecraft:resource_selector",
    "minecraft:rotation",
    "minecraft:score_holder",
    "minecraft:scoreboard_slot",
    "minecraft:slot",
    "minecraft:slot_source",
    "minecraft:slots",
    "minecraft:style",
    "minecraft:swing_animation",
    "minecraft:swizzle",
    "minecraft:team",
    "minecraft:team_color",
    "minecraft:template_mirror",
    "minecraft:template_rotation",
    "minecraft:time",
    "minecraft:uuid",
    "minecraft:vec2",
    "minecraft:vec3",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Feet,
    Eyes,
}

impl Anchor {
    pub const NAMES: [&'static str; 2] = ["feet", "eyes"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "feet" => Some(Self::Feet),
            "eyes" => Some(Self::Eyes),
            _ => None,
        }
    }
}

/// A `game_profile` argument: online players by selector, or any name.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileSelector {
    Selector(Box<Selector>),
    Name(String),
}

/// A `score_holder` argument: entities by selector, every holder with a
/// score (`*`), or a name (an entity UUID or player name resolves to that
/// entity when it is around).
#[derive(Debug, Clone, PartialEq)]
pub enum ScoreHolderInput {
    Selector(Box<Selector>),
    Wildcard,
    Name(String),
}

/// A `scoreboard players operation` operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Assign,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Min,
    Max,
    Swap,
}

impl Operation {
    pub const NAMES: [&'static str; 9] = ["=", "+=", "-=", "*=", "/=", "%=", "<", ">", "><"];

    pub fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "=" => Self::Assign,
            "+=" => Self::Add,
            "-=" => Self::Subtract,
            "*=" => Self::Multiply,
            "/=" => Self::Divide,
            "%=" => Self::Modulo,
            "<" => Self::Min,
            ">" => Self::Max,
            "><" => Self::Swap,
            _ => return None,
        })
    }

    /// The new target value, and for a swap the new source value. Integer
    /// arithmetic wraps, division floors and the remainder is never
    /// negative, as in the game.
    pub fn apply(self, target: i32, source: i32) -> Result<(i32, Option<i32>), CommandError> {
        let div0 = || CommandError::simple("arguments.operation.div0");
        Ok(match self {
            Self::Assign => (source, None),
            Self::Add => (target.wrapping_add(source), None),
            Self::Subtract => (target.wrapping_sub(source), None),
            Self::Multiply => (target.wrapping_mul(source), None),
            Self::Divide => {
                if source == 0 {
                    return Err(div0());
                }
                let quotient = target.wrapping_div(source);
                let floor = if target.wrapping_rem(source) != 0 && ((target < 0) != (source < 0)) {
                    quotient - 1
                } else {
                    quotient
                };
                (floor, None)
            }
            Self::Modulo => {
                if source == 0 {
                    return Err(div0());
                }
                let rem = target.wrapping_rem(source);
                (if rem < 0 { rem + source } else { rem }, None)
            }
            Self::Min => (target.min(source), None),
            Self::Max => (target.max(source), None),
            Self::Swap => (source, Some(target)),
        })
    }
}

/// A `message` argument: the text and the selectors embedded in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub text: String,
    pub parts: Vec<(usize, usize, Selector)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
    Selector(Box<Selector>),
    Profile(ProfileSelector),
    Coordinates(Coordinates),
    Rotation(Rotation),
    Angle(Angle),
    BlockState(BlockInput),
    BlockPredicate(BlockPredicate),
    ItemStack(ItemInput),
    ItemPredicate(ItemPredicate),
    Component(TextComponent),
    Message(Message),
    Nbt(Tag),
    Particle(super::particle::ParticleInput),
    /// A named slot range of `/item`, with the slot ids it covers.
    Slots {
        name: String,
        ids: Vec<i32>,
    },
    NbtPath(super::nbt_path::NbtPath),
    /// A registry entry, as its canonical `namespace:path`.
    Resource(String),
    ResourceOrTag {
        name: String,
        tag: bool,
    },
    /// The registry entries a `resource_selector` pattern matched.
    Resources(Vec<String>),
    Identifier(String),
    /// A template rotation: quarter turns clockwise.
    TemplateRotation(u8),
    /// A template mirror: 0 none, 1 left-right, 2 front-back.
    TemplateMirror(u8),
    GameMode(GameMode),
    Time(i32),
    Uuid(u128),
    Swizzle([bool; 3]),
    Heightmap(&'static str),
    IntRange(Bounds<i32>),
    DoubleRange(Bounds<f64>),
    Anchor(Anchor),
    HexColor(i32),
    ScoreHolder(ScoreHolderInput),
    Criterion(crate::world::scoreboard::Criterion),
    Operation(Operation),
    DisplaySlot(crate::world::scoreboard::DisplaySlot),
    TeamColor(crate::world::scoreboard::TeamColor),
    /// A text style, kept in its NBT form.
    Style(Compound),
}

/// What an argument reader may need to know about who is typing.
#[derive(Debug, Clone, Copy)]
pub struct ParseContext {
    pub allow_selectors: bool,
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | ':' | '/' | '.' | '-')
}

fn valid_namespace(namespace: &str) -> bool {
    namespace != ".."
        && namespace
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '.' | '-'))
}

fn valid_path(path: &str) -> bool {
    path.chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '/' | '.' | '-'))
}

/// A resource location as typed: `path` or `namespace:path`, always
/// returned with a namespace.
pub fn read_identifier(reader: &mut Reader<'_>) -> Result<String, CommandError> {
    let start = reader.cursor();
    let raw = reader.read_while(is_identifier_char);
    let (namespace, path) = match raw.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", raw),
    };
    let namespace = if namespace.is_empty() {
        "minecraft"
    } else {
        namespace
    };
    if !valid_namespace(namespace) || !valid_path(path) {
        reader.set_cursor(start);
        return Err(CommandError::simple("argument.id.invalid").at(reader));
    }
    Ok(format!("{namespace}:{path}"))
}

/// Canonicalizes an identifier without validating it, for lookups.
pub fn qualify(id: &str) -> String {
    match id.split_once(':') {
        Some((namespace, path)) => format!("{namespace}:{path}"),
        None => format!("minecraft:{id}"),
    }
}

fn nbt_error(reader: &mut Reader<'_>, start: usize, err: NbtError) -> CommandError {
    let position = match err {
        NbtError::Snbt { position, .. } => position,
        _ => 0,
    };
    reader.set_cursor(start + position);
    CommandError::dynamic("argument.nbt.expected.value", [err.to_string()]).at(reader)
}

/// One SNBT value, leaving the reader after it.
pub fn read_nbt_value(reader: &mut Reader<'_>) -> Result<Tag, CommandError> {
    let start = reader.cursor();
    match Tag::parse_snbt_prefix(reader.remaining()) {
        Ok((tag, consumed)) => {
            reader.set_cursor(start + consumed);
            Ok(tag)
        }
        Err(err) => Err(nbt_error(reader, start, err)),
    }
}

pub fn read_nbt_compound(reader: &mut Reader<'_>) -> Result<Compound, CommandError> {
    let start = reader.cursor();
    if !reader.peek_is('{') {
        return Err(CommandError::simple("argument.nbt.expected.compound").at(reader));
    }
    match read_nbt_value(reader)? {
        Tag::Compound(compound) => Ok(compound),
        _ => {
            reader.set_cursor(start);
            Err(CommandError::simple("argument.nbt.expected.compound").at(reader))
        }
    }
}

fn sorted_names<T: Registry>() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = T::ALL.iter().map(|entry| entry.name()).collect();
    names.sort_unstable();
    names
}

/// The entry names of a registry a `resource` argument may name, sorted.
pub fn registry_entries(key: &str) -> Option<&'static [&'static str]> {
    if let Some(registry) = DataRegistry::get(key) {
        return Some(registry.entries);
    }
    static MOB_EFFECTS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static ATTRIBUTES: OnceLock<Vec<&'static str>> = OnceLock::new();
    static ENTITY_TYPES: OnceLock<Vec<&'static str>> = OnceLock::new();
    static SOUND_EVENTS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static ITEMS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static BLOCKS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static POIS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static ADVANCEMENTS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static RECIPES: OnceLock<Vec<&'static str>> = OnceLock::new();
    Some(match key {
        "minecraft:point_of_interest_type" => {
            POIS.get_or_init(sorted_names::<tuff_vanilla_data::registry::PointOfInterestType>)
        }
        "minecraft:mob_effect" => MOB_EFFECTS.get_or_init(sorted_names::<MobEffect>),
        "minecraft:attribute" => {
            ATTRIBUTES.get_or_init(sorted_names::<tuff_vanilla_data::registry::Attribute>)
        }
        "minecraft:entity_type" => ENTITY_TYPES.get_or_init(sorted_names::<EntityType>),
        "minecraft:sound_event" => SOUND_EVENTS.get_or_init(sorted_names::<SoundEvent>),
        "minecraft:item" => ITEMS.get_or_init(sorted_names::<Item>),
        "minecraft:block" => BLOCKS.get_or_init(sorted_names::<Block>),
        "minecraft:recipe" => RECIPES.get_or_init(|| {
            tuff_vanilla_data::recipe::ALL
                .iter()
                .map(|(name, _)| *name)
                .collect()
        }),
        "minecraft:advancement" => ADVANCEMENTS.get_or_init(|| {
            tuff_vanilla_data::advancement::ALL
                .iter()
                .map(|advancement| advancement.name)
                .collect()
        }),
        _ => return None,
    })
}

/// The tags of a registry a `resource_or_tag` argument may name.
pub fn registry_tags(key: &str) -> Vec<&'static str> {
    match key {
        "minecraft:worldgen/biome" => tuff_vanilla_data::tag::worldgen::biome::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        "minecraft:entity_type" => tuff_vanilla_data::tag::entity_type::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        "minecraft:item" => tuff_vanilla_data::tag::item::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        "minecraft:block" => tuff_vanilla_data::tag::block::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        "minecraft:worldgen/structure" => tuff_vanilla_data::tag::worldgen::structure::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        "minecraft:point_of_interest_type" => tuff_vanilla_data::tag::point_of_interest_type::ALL
            .iter()
            .map(|(name, _)| *name)
            .collect(),
        _ => Vec::new(),
    }
}

fn properties(json: Option<&str>) -> serde_json::Value {
    json.and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn bounded<T: PartialOrd + Copy + std::fmt::Display>(
    reader: &mut Reader<'_>,
    start: usize,
    value: T,
    (min, max): (Option<T>, Option<T>),
    (low_key, big_key): (&str, &str),
    show: impl Fn(T) -> String,
) -> Result<T, CommandError> {
    if let Some(min) = min
        && value < min
    {
        reader.set_cursor(start);
        return Err(CommandError::dynamic(low_key, [show(value), show(min)]).at(reader));
    }
    if let Some(max) = max
        && value > max
    {
        reader.set_cursor(start);
        return Err(CommandError::dynamic(big_key, [show(value), show(max)]).at(reader));
    }
    Ok(value)
}

fn parse_message(reader: &mut Reader<'_>, allow_selectors: bool) -> Result<Message, CommandError> {
    if reader.remaining_len() > MAX_MESSAGE_LENGTH {
        return Err(CommandError::dynamic(
            "argument.message.too_long",
            [
                reader.remaining().chars().count().to_string(),
                MAX_MESSAGE_LENGTH.to_string(),
            ],
        ));
    }
    let offset = reader.cursor();
    let text = reader.remaining().to_owned();
    let mut parts = Vec::new();
    if !allow_selectors {
        reader.set_cursor(reader.input().len());
        return Ok(Message { text, parts });
    }
    while reader.can_read() {
        if reader.peek_is(selector::SELECTOR_START) {
            let start = reader.cursor();
            match Selector::parse(reader, true) {
                Ok(selector) => {
                    parts.push((start - offset, reader.cursor() - offset, selector));
                    continue;
                }
                Err(err) => {
                    let tolerated = matches!(
                        err.key(),
                        Some(
                            "argument.entity.selector.missing" | "argument.entity.selector.unknown"
                        )
                    );
                    if !tolerated {
                        return Err(err);
                    }
                    reader.set_cursor(start + 1);
                }
            }
        } else {
            reader.skip();
        }
    }
    Ok(Message { text, parts })
}

/// A `#tag` parses as any id: it is looked up when the command runs, in the
/// tags loaded then.
fn parse_resource(
    reader: &mut Reader<'_>,
    registry: &str,
    tags: bool,
    resources: &Resources,
) -> Result<Value, CommandError> {
    let start = reader.cursor();
    if tags && reader.peek_is('#') {
        reader.skip();
        return Ok(Value::ResourceOrTag {
            name: read_identifier(reader)?,
            tag: true,
        });
    }
    let id = read_identifier(reader)?;
    let known = registry_entries(registry)
        .is_some_and(|entries| entries.binary_search(&id.as_str()).is_ok())
        || resources.contains(registry, &id);
    if !known {
        reader.set_cursor(start);
        return Err(CommandError::dynamic(
            "argument.resource.not_found",
            [id, registry.to_owned()],
        )
        .at(reader));
    }
    if tags {
        Ok(Value::ResourceOrTag {
            name: id,
            tag: false,
        })
    } else {
        Ok(Value::Resource(id))
    }
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| wildcard_match(rest, &text[skip..])),
        Some((b'?', rest)) => !text.is_empty() && wildcard_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && wildcard_match(rest, &text[1..]),
    }
}

/// Registries whose entries data packs add at reload: a selector that
/// matches no built-in entry parses as its pattern, which the command
/// matches against the live entries (see [`wildcard_matches`]).
pub const RELOADABLE_SELECTOR_REGISTRIES: [&str; 1] = ["minecraft:test_instance"];

/// Whether the resource selector `pattern` (`*` and `?` wildcards) selects
/// `id`.
pub fn wildcard_matches(pattern: &str, id: &str) -> bool {
    wildcard_match(pattern.as_bytes(), id.as_bytes())
}

fn parse_resource_selector(
    reader: &mut Reader<'_>,
    registry: &str,
    resources: &Resources,
) -> Result<Value, CommandError> {
    let start = reader.cursor();
    let raw = reader.read_while(|c| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '*' | '?')
    });
    let pattern = if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("minecraft:{raw}")
    };
    let mut matches: BTreeSet<String> = registry_entries(registry)
        .unwrap_or(&[])
        .iter()
        .filter(|id| wildcard_match(pattern.as_bytes(), id.as_bytes()))
        .map(|id| (*id).to_owned())
        .collect();
    matches.extend(
        resources
            .ids(registry)
            .filter(|id| wildcard_match(pattern.as_bytes(), id.as_bytes()))
            .map(str::to_owned),
    );
    if matches.is_empty() && RELOADABLE_SELECTOR_REGISTRIES.contains(&registry) {
        return Ok(Value::Resources(vec![pattern]));
    }
    if matches.is_empty() {
        reader.set_cursor(start);
        return Err(CommandError::dynamic(
            "argument.resource_selector.not_found",
            [pattern, registry.to_owned()],
        )
        .at(reader));
    }
    Ok(Value::Resources(matches.into_iter().collect()))
}

fn parse_component(reader: &mut Reader<'_>) -> Result<TextComponent, CommandError> {
    let start = reader.cursor();
    let tag = match Tag::parse_snbt_prefix(reader.remaining()) {
        Ok((tag, consumed)) => {
            reader.set_cursor(start + consumed);
            tag
        }
        Err(err) => {
            reader.set_cursor(start);
            return Err(
                CommandError::dynamic("argument.component.invalid", [err.to_string()]).at(reader),
            );
        }
    };
    TextComponent::from_nbt(&tag).ok_or_else(|| {
        reader.set_cursor(start);
        CommandError::dynamic(
            "argument.component.invalid",
            ["unsupported text component".to_owned()],
        )
        .at(reader)
    })
}

/// Resource ids registered beside the vanilla ones (feature entity types,
/// advancements), by registry. Clients never learn them, so they only
/// widen what parses.
#[derive(Debug, Clone, Default)]
pub struct Resources {
    by_registry: HashMap<String, BTreeSet<String>>,
    custom_items: std::sync::Arc<HashMap<String, ItemInput>>,
}

impl Resources {
    /// Lets item arguments name the custom item `id`, which parses as
    /// `input`.
    pub fn add_custom_item(&mut self, id: impl Into<String>, input: ItemInput) {
        std::sync::Arc::make_mut(&mut self.custom_items).insert(id.into(), input);
    }

    pub fn custom_item(&self, id: &str) -> Option<&ItemInput> {
        self.custom_items.get(id)
    }

    pub fn add(&mut self, registry: &str, id: impl Into<String>) {
        self.by_registry
            .entry(registry.to_owned())
            .or_default()
            .insert(id.into());
    }

    pub fn ids(&self, registry: &str) -> impl Iterator<Item = &str> {
        self.by_registry
            .get(registry)
            .into_iter()
            .flat_map(|ids| ids.iter().map(String::as_str))
    }

    pub fn contains(&self, registry: &str, id: &str) -> bool {
        self.by_registry
            .get(registry)
            .is_some_and(|ids| ids.contains(id))
    }
}

/// Recipe and advancement keys name data pack content, which reloads, so
/// they parse as any id and are looked up when the command runs.
const TEMPLATE_ROTATIONS: [&str; 4] = ["none", "clockwise_90", "180", "counterclockwise_90"];
const TEMPLATE_MIRRORS: [&str; 3] = ["none", "left_right", "front_back"];

const RELOADABLE_KEYS: [&str; 2] = ["minecraft:recipe", "minecraft:advancement"];

/// Reads one argument of `parser` with `properties` (the tree's raw JSON).
pub fn parse_argument(
    parser: &str,
    properties_json: Option<&str>,
    reader: &mut Reader<'_>,
    context: ParseContext,
) -> Result<Value, CommandError> {
    parse_argument_with(
        parser,
        properties_json,
        reader,
        context,
        &Resources::default(),
    )
}

/// [`parse_argument`], also accepting the ids in `resources`.
pub fn parse_argument_with(
    parser: &str,
    properties_json: Option<&str>,
    reader: &mut Reader<'_>,
    context: ParseContext,
    resources: &Resources,
) -> Result<Value, CommandError> {
    let props = properties(properties_json);
    let start = reader.cursor();
    Ok(match parser {
        "brigadier:bool" => Value::Bool(reader.read_bool()?),
        "brigadier:integer" => {
            let value = reader.read_int()?;
            let min = props["min"].as_i64().map(|v| v as i32);
            let max = props["max"].as_i64().map(|v| v as i32);
            Value::Int(bounded(
                reader,
                start,
                value,
                (min, max),
                ("argument.integer.low", "argument.integer.big"),
                |v| v.to_string(),
            )?)
        }
        "brigadier:long" => {
            let value = reader.read_long()?;
            Value::Long(bounded(
                reader,
                start,
                value,
                (props["min"].as_i64(), props["max"].as_i64()),
                ("argument.long.low", "argument.long.big"),
                |v| v.to_string(),
            )?)
        }
        "brigadier:float" => {
            let value = reader.read_float()?;
            Value::Float(bounded(
                reader,
                start,
                value,
                (
                    props["min"].as_f64().map(|v| v as f32),
                    props["max"].as_f64().map(|v| v as f32),
                ),
                ("argument.float.low", "argument.float.big"),
                super::java_float,
            )?)
        }
        "brigadier:double" => {
            let value = reader.read_double()?;
            Value::Double(bounded(
                reader,
                start,
                value,
                (props["min"].as_f64(), props["max"].as_f64()),
                ("argument.double.low", "argument.double.big"),
                super::java_double,
            )?)
        }
        "brigadier:string" => Value::Str(match props["type"].as_str() {
            Some("greedy") => {
                let rest = reader.remaining().to_owned();
                reader.set_cursor(reader.input().len());
                rest
            }
            Some("word") => reader.read_unquoted_string().to_owned(),
            _ => reader.read_string()?,
        }),
        "minecraft:entity" => {
            let selector = Selector::parse_with(reader, context.allow_selectors, resources)?;
            let single = props["amount"] == "single";
            let players = props["type"] == "players";
            if single && selector.max_results > 1 {
                reader.set_cursor(start);
                let key = if players {
                    "argument.player.toomany"
                } else {
                    "argument.entity.toomany"
                };
                return Err(CommandError::simple(key).at(reader));
            }
            if players && selector.includes_entities && !selector.current_entity {
                reader.set_cursor(start);
                return Err(CommandError::simple("argument.player.entities").at(reader));
            }
            Value::Selector(Box::new(selector))
        }
        "minecraft:game_profile" => {
            if reader.peek_is(selector::SELECTOR_START) {
                let selector = Selector::parse_with(reader, context.allow_selectors, resources)?;
                if selector.includes_entities {
                    reader.set_cursor(start);
                    return Err(CommandError::simple("argument.player.entities").at(reader));
                }
                Value::Profile(ProfileSelector::Selector(Box::new(selector)))
            } else {
                let name = reader.read_while(|c| c != ' ').to_owned();
                Value::Profile(ProfileSelector::Name(name))
            }
        }
        "minecraft:block_pos" => Value::Coordinates(Coordinates::parse_block_pos(reader)?),
        "minecraft:column_pos" => Value::Coordinates(Coordinates::parse_vec2(reader, true, false)?),
        "minecraft:vec3" => Value::Coordinates(Coordinates::parse_vec3(reader, true)?),
        "minecraft:vec2" => Value::Coordinates(Coordinates::parse_vec2(reader, false, true)?),
        "minecraft:rotation" => Value::Rotation(Rotation::parse(reader)?),
        "minecraft:angle" => Value::Angle(Angle::parse(reader)?),
        "minecraft:block_state" => Value::BlockState(block::parse_block_state(reader)?),
        "minecraft:block_predicate" => Value::BlockPredicate(block::parse_block_predicate(reader)?),
        "minecraft:item_stack" => Value::ItemStack(item::parse_item_stack_with(reader, resources)?),
        "minecraft:item_predicate" => {
            Value::ItemPredicate(item::parse_item_predicate_with(reader, resources)?)
        }
        "minecraft:component" => Value::Component(parse_component(reader)?),
        "minecraft:message" => Value::Message(parse_message(reader, context.allow_selectors)?),
        "minecraft:particle" => Value::Particle(super::particle::parse(reader)?),
        "minecraft:nbt_compound_tag" => Value::Nbt(Tag::Compound(read_nbt_compound(reader)?)),
        "minecraft:nbt_path" => Value::NbtPath(super::nbt_path::NbtPath::parse(reader)?),
        "minecraft:nbt_tag" => Value::Nbt(read_nbt_value(reader)?),
        "minecraft:resource_key"
            if RELOADABLE_KEYS.contains(&props["registry"].as_str().unwrap_or("")) =>
        {
            Value::Resource(read_identifier(reader)?)
        }
        "minecraft:resource" | "minecraft:resource_key" => {
            let registry = props["registry"].as_str().unwrap_or("");
            parse_resource(reader, registry, false, resources)?
        }
        "minecraft:resource_or_tag" => {
            let registry = props["registry"].as_str().unwrap_or("");
            parse_resource(reader, registry, true, resources)?
        }
        "minecraft:resource_or_tag_key" => {
            let tag = reader.peek_is('#');
            if tag {
                reader.skip();
            }
            Value::ResourceOrTag {
                name: read_identifier(reader)?,
                tag,
            }
        }
        "minecraft:resource_location" | "minecraft:dimension" => {
            Value::Identifier(read_identifier(reader)?)
        }
        "minecraft:context_float_provider" | "minecraft:context_int_provider"
            if reader
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.') =>
        {
            Value::Nbt(read_nbt_value(reader)?)
        }
        "minecraft:resource_selector" => {
            let registry = props["registry"].as_str().unwrap_or("");
            parse_resource_selector(reader, registry, resources)?
        }
        "minecraft:dialog"
        | "minecraft:loot_modifier"
        | "minecraft:loot_predicate"
        | "minecraft:loot_table"
        | "minecraft:context_float_provider"
        | "minecraft:context_int_provider" => {
            if reader.peek_is('{') || reader.peek_is('[') {
                Value::Nbt(read_nbt_value(reader)?)
            } else {
                Value::Identifier(read_identifier(reader)?)
            }
        }
        "minecraft:function" => {
            let tag = reader.peek_is('#');
            if tag {
                reader.skip();
            }
            Value::ResourceOrTag {
                name: read_identifier(reader)?,
                tag,
            }
        }
        "minecraft:gamemode" => {
            let name = reader.read_unquoted_string().to_owned();
            match GameMode::from_name(&name) {
                Some(mode) => Value::GameMode(mode),
                None => {
                    return Err(
                        CommandError::dynamic("argument.gamemode.invalid", [name]).at(reader)
                    );
                }
            }
        }
        "minecraft:time" => {
            let value = reader.read_float()?;
            let unit = reader.read_unquoted_string();
            let per_unit = match unit {
                "d" => 24_000,
                "s" => 20,
                "t" | "" => 1,
                _ => return Err(CommandError::simple("argument.time.invalid_unit").at(reader)),
            };
            let ticks = (f64::from(value) * f64::from(per_unit) + 0.5).floor() as i32;
            let min = props["min"].as_i64().map_or(0, |v| v as i32);
            if ticks < min {
                return Err(CommandError::dynamic(
                    "argument.time.tick_count_too_low",
                    [min.to_string(), ticks.to_string()],
                )
                .at(reader));
            }
            Value::Time(ticks)
        }
        "minecraft:uuid" => {
            let word = reader.peek_word();
            let candidate: String = word
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
                .collect();
            match tuff_storage::admin::parse_uuid_text(&candidate) {
                Some(uuid) => {
                    reader.set_cursor(start + candidate.len());
                    Value::Uuid(uuid)
                }
                None => return Err(CommandError::simple("argument.uuid.invalid").at(reader)),
            }
        }
        "minecraft:swizzle" => {
            let mut axes = [false; 3];
            while reader.can_read() && !reader.peek_is(' ') {
                let axis = match reader.read() {
                    Some('x') => 0,
                    Some('y') => 1,
                    Some('z') => 2,
                    _ => return Err(CommandError::simple("arguments.swizzle.invalid").at(reader)),
                };
                if axes[axis] {
                    return Err(CommandError::simple("arguments.swizzle.invalid").at(reader));
                }
                axes[axis] = true;
            }
            Value::Swizzle(axes)
        }
        "minecraft:slot" | "minecraft:slots" => {
            let name = reader.read_while(|c| c != ' ').to_owned();
            match crate::world::predicate::slot_range(&name) {
                Some(ids) if parser == "minecraft:slots" || ids.len() == 1 => {
                    Value::Slots { name, ids }
                }
                Some(_) => {
                    reader.set_cursor(start);
                    return Err(CommandError::simple("slot.only_single_allowed").at(reader));
                }
                None => {
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("slot.unknown", [name]).at(reader));
                }
            }
        }
        "minecraft:slot_source" => {
            let name = reader.read_while(|c| c != ' ').to_owned();
            match crate::world::predicate::slot_range(&name) {
                Some(ids) => Value::Slots { name, ids },
                None => {
                    reader.set_cursor(start);
                    if reader.peek_is('{') || reader.peek_is('[') {
                        Value::Nbt(read_nbt_value(reader)?)
                    } else {
                        Value::Identifier(read_identifier(reader)?)
                    }
                }
            }
        }
        "minecraft:swing_animation" => {
            let name = reader.read_unquoted_string();
            match SWING_ANIMATIONS.iter().find(|a| **a == name) {
                Some(animation) => Value::Str((*animation).to_owned()),
                None => {
                    let name = name.to_owned();
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("argument.enum.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:heightmap" => {
            let name = reader.read_unquoted_string();
            match HEIGHTMAPS.iter().find(|h| **h == name) {
                Some(heightmap) => Value::Heightmap(heightmap),
                None => {
                    let name = name.to_owned();
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("argument.enum.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:feature" => {
            if reader.peek_is('{') {
                Value::Nbt(Tag::Compound(read_nbt_compound(reader)?))
            } else {
                Value::Identifier(read_identifier(reader)?)
            }
        }
        "minecraft:template_rotation" => {
            let name = reader.read_unquoted_string().to_owned();
            match TEMPLATE_ROTATIONS.iter().position(|n| *n == name) {
                Some(turns) => Value::TemplateRotation(turns as u8),
                None => {
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("argument.enum.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:template_mirror" => {
            let name = reader.read_unquoted_string().to_owned();
            match TEMPLATE_MIRRORS.iter().position(|n| *n == name) {
                Some(mirror) => Value::TemplateMirror(mirror as u8),
                None => {
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("argument.enum.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:int_range" => Value::IntRange(range::parse_ints(reader)?),
        "minecraft:float_range" => Value::DoubleRange(range::parse_doubles(reader)?),
        "minecraft:entity_anchor" => {
            let name = reader.read_unquoted_string().to_owned();
            match Anchor::from_name(&name) {
                Some(anchor) => Value::Anchor(anchor),
                None => {
                    reader.set_cursor(start);
                    return Err(CommandError::dynamic("argument.anchor.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:hex_color" => {
            let text = reader.read_unquoted_string().to_owned();
            let digits = |from: usize, to: usize| u32::from_str_radix(&text[from..to], 16).ok();
            let rgb = match text.len() {
                3 => (|| {
                    let (r, g, b) = (digits(0, 1)?, digits(1, 2)?, digits(2, 3)?);
                    Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
                })(),
                6 => (|| Some(digits(0, 2)? << 16 | digits(2, 4)? << 8 | digits(4, 6)?))(),
                _ => None,
            };
            match rgb {
                Some(rgb) => Value::HexColor((0xFF << 24 | rgb) as i32),
                None => {
                    return Err(
                        CommandError::dynamic("argument.hexcolor.invalid", [text]).at(reader)
                    );
                }
            }
        }
        "minecraft:score_holder" => {
            if reader.peek_is(selector::SELECTOR_START) {
                let selector = Selector::parse_with(reader, context.allow_selectors, resources)?;
                if props["amount"] != "multiple" && selector.max_results > 1 {
                    reader.set_cursor(start);
                    return Err(CommandError::simple("argument.entity.toomany").at(reader));
                }
                Value::ScoreHolder(ScoreHolderInput::Selector(Box::new(selector)))
            } else {
                let name = reader.read_while(|c| c != ' ');
                if name == crate::world::scoreboard::WILDCARD {
                    Value::ScoreHolder(ScoreHolderInput::Wildcard)
                } else {
                    Value::ScoreHolder(ScoreHolderInput::Name(name.to_owned()))
                }
            }
        }
        "minecraft:objective" | "minecraft:team" => {
            Value::Str(reader.read_unquoted_string().to_owned())
        }
        "minecraft:objective_criteria" => {
            let name = reader.read_while(|c| c != ' ').to_owned();
            match crate::world::scoreboard::Criterion::parse(&name) {
                Some(criterion) => Value::Criterion(criterion),
                None => {
                    reader.set_cursor(start);
                    return Err(
                        CommandError::dynamic("argument.criteria.invalid", [name]).at(reader)
                    );
                }
            }
        }
        "minecraft:operation" => {
            let text = reader.read_while(|c| c != ' ');
            match Operation::from_text(text) {
                Some(operation) => Value::Operation(operation),
                None => {
                    return Err(CommandError::simple("arguments.operation.invalid").at(reader));
                }
            }
        }
        "minecraft:scoreboard_slot" => {
            let name = reader.read_unquoted_string().to_owned();
            match crate::world::scoreboard::DisplaySlot::from_name(&name) {
                Some(slot) => Value::DisplaySlot(slot),
                None => {
                    return Err(CommandError::dynamic(
                        "argument.scoreboardDisplaySlot.invalid",
                        [name],
                    )
                    .at(reader));
                }
            }
        }
        "minecraft:team_color" => {
            let name = reader.read_unquoted_string().to_owned();
            match crate::world::scoreboard::TeamColor::from_name(&name) {
                Some(color) => Value::TeamColor(color),
                None => {
                    return Err(CommandError::dynamic("argument.color.invalid", [name]).at(reader));
                }
            }
        }
        "minecraft:style" => match Tag::parse_snbt_prefix(reader.remaining()) {
            Ok((Tag::Compound(style), consumed)) => {
                if let Some(problem) = style_problem(&style) {
                    return Err(
                        CommandError::dynamic("argument.style.invalid", [problem]).at(reader)
                    );
                }
                reader.set_cursor(start + consumed);
                Value::Style(style)
            }
            Ok(_) => {
                return Err(CommandError::dynamic(
                    "argument.style.invalid",
                    ["not a compound".to_owned()],
                )
                .at(reader));
            }
            Err(err) => {
                return Err(
                    CommandError::dynamic("argument.style.invalid", [err.to_string()]).at(reader),
                );
            }
        },
        _ => return Err(CommandError::simple("command.unknown.argument").at(reader)),
    })
}

const STYLE_COLORS: &[&str] = &[
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

fn style_problem(style: &Compound) -> Option<String> {
    for (key, value) in style.iter() {
        let ok = match (key, value) {
            ("bold" | "italic" | "underlined" | "strikethrough" | "obfuscated", v) => {
                matches!(v, Tag::Byte(0 | 1))
            }
            ("color", Tag::String(name)) => {
                STYLE_COLORS.contains(&name.as_str())
                    || name.strip_prefix('#').is_some_and(|hex| {
                        hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())
                    })
            }
            ("color", Tag::Int(_)) => true,
            ("shadow_color", Tag::Int(_)) => true,
            ("shadow_color", Tag::List(items)) => items.len() == 4,
            ("insertion" | "font", Tag::String(_)) => true,
            ("click_event" | "hover_event", Tag::Compound(_)) => true,
            (
                "color" | "shadow_color" | "insertion" | "font" | "click_event" | "hover_event",
                _,
            ) => false,
            _ => true,
        };
        if !ok {
            return Some(format!("invalid value for {key}"));
        }
    }
    None
}

/// Whether `candidate` completes `input` at its start or after a `_`.
pub fn matches_sub_str(input: &str, candidate: &str) -> bool {
    let mut from = 0;
    loop {
        if candidate[from..].starts_with(input) {
            return true;
        }
        match candidate[from..].find('_') {
            Some(offset) => from += offset + 1,
            None => return false,
        }
    }
}

/// Whether `candidate` has `input` at its start or right after a `.`, `_`
/// or `/`.
fn matches_word(input: &str, candidate: &str) -> bool {
    let mut from = 0;
    loop {
        if candidate[from..].starts_with(input) {
            return true;
        }
        match candidate[from..].find(['.', '_', '/']) {
            Some(offset) => from += offset + 1,
            None => return false,
        }
    }
}

/// Whether a resource location completes `input`: the whole location when
/// `input` has a namespace, else its namespace or path.
pub fn matches_resource(input: &str, id: &str) -> bool {
    if input.contains(':') {
        return matches_word(input, id);
    }
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    matches_word(input, namespace) || matches_word(input, path)
}

/// Names the server knows for completing arguments.
#[derive(Debug, Default, Clone)]
pub struct SuggestContext {
    pub allow_selectors: bool,
    pub player_names: Vec<String>,
    pub dimensions: Vec<String>,
    pub objectives: Vec<String>,
    pub teams: Vec<String>,
    /// The advancement tree in effect; the compiled-in ids without one.
    pub advancements: Option<std::sync::Arc<dyn crate::world::advancement::Advancements>>,
    /// The tags in effect; the compiled-in ones without them.
    pub tags: Option<std::sync::Arc<tuff_datapack::tags::TagSet>>,
}

impl SuggestContext {
    fn tag_names(&self, registry: &str) -> Vec<&str> {
        match &self.tags {
            Some(tags) => tags.names(registry).collect(),
            None => registry_tags(registry),
        }
    }
}

pub fn suggest_words<'a>(
    partial: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let partial = partial.to_lowercase();
    candidates
        .into_iter()
        .filter(|c| matches_sub_str(&partial, &c.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

pub fn suggest_resources<'a>(
    partial: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Vec<String> {
    let partial = partial.to_lowercase();
    let partial = partial.strip_prefix(prefix).unwrap_or(&partial);
    candidates
        .into_iter()
        .filter(|c| matches_resource(partial, c))
        .map(|c| format!("{prefix}{c}"))
        .collect()
}

fn suggest_coordinates(partial: &str, fields: usize, local: bool) -> Vec<String> {
    let marker = if local { "^" } else { "~" };
    let typed: Vec<&str> = if partial.is_empty() {
        Vec::new()
    } else {
        partial.split(' ').collect()
    };
    let mut out = Vec::new();
    let build = |count: usize, typed: &[&str]| {
        let mut parts: Vec<String> = typed.iter().map(|s| s.to_string()).collect();
        while parts.len() < count {
            parts.push(marker.to_owned());
        }
        parts.join(" ")
    };
    if typed.len() < fields {
        let first = if typed.is_empty() { 1 } else { typed.len() + 1 };
        for count in first..=fields {
            out.push(build(count, &typed));
        }
    }
    out
}

/// Completions for the partial text of an argument of `parser`.
pub fn suggest_argument(
    parser: &str,
    properties_json: Option<&str>,
    partial: &str,
    context: &SuggestContext,
) -> Vec<String> {
    let props = properties(properties_json);
    let names = |names: &[&str]| suggest_words(partial, names.iter().copied());
    match parser {
        "brigadier:bool" => names(&["true", "false"]),
        "minecraft:entity" | "minecraft:game_profile" => {
            let mut out: Vec<String> =
                suggest_words(partial, context.player_names.iter().map(String::as_str));
            if context.allow_selectors && (partial.is_empty() || partial.starts_with('@')) {
                let entities = parser == "minecraft:entity" && props["type"] != "players";
                let mut selectors = vec!["@p", "@a", "@r", "@s"];
                if entities {
                    selectors.extend(["@e", "@n"]);
                }
                out.extend(
                    selectors
                        .into_iter()
                        .filter(|s| s.starts_with(partial))
                        .map(str::to_owned),
                );
            }
            out
        }
        "minecraft:gamemode" => names(&GameMode::ALL.map(GameMode::name)),
        "minecraft:score_holder" => {
            let mut out: Vec<String> =
                suggest_words(partial, context.player_names.iter().map(String::as_str));
            if context.allow_selectors && (partial.is_empty() || partial.starts_with('@')) {
                out.extend(
                    ["@p", "@a", "@r", "@s", "@e", "@n"]
                        .into_iter()
                        .filter(|s| s.starts_with(partial))
                        .map(str::to_owned),
                );
            }
            out
        }
        "minecraft:objective" => {
            suggest_words(partial, context.objectives.iter().map(String::as_str))
        }
        "minecraft:team" => suggest_words(partial, context.teams.iter().map(String::as_str)),
        "minecraft:operation" => names(&Operation::NAMES),
        "minecraft:scoreboard_slot" => {
            let slots: Vec<String> = crate::world::scoreboard::DisplaySlot::all()
                .map(|slot| slot.name())
                .collect();
            suggest_words(partial, slots.iter().map(String::as_str))
        }
        "minecraft:team_color" => names(
            &crate::world::scoreboard::TeamColor::ALL
                .map(crate::world::scoreboard::TeamColor::name),
        ),
        "minecraft:dimension" => {
            suggest_resources(partial, context.dimensions.iter().map(String::as_str), "")
        }
        "minecraft:resource_key"
            if props["registry"] == "minecraft:advancement" && context.advancements.is_some() =>
        {
            let tree = context.advancements.as_deref().into_iter();
            suggest_resources(partial, tree.flat_map(|tree| tree.names()), "")
        }
        "minecraft:resource" | "minecraft:resource_key" => {
            let registry = props["registry"].as_str().unwrap_or("");
            suggest_resources(
                partial,
                registry_entries(registry).unwrap_or(&[]).iter().copied(),
                "",
            )
        }
        "minecraft:resource_or_tag" | "minecraft:resource_or_tag_key" => {
            let registry = props["registry"].as_str().unwrap_or("");
            let mut out = Vec::new();
            if partial.starts_with('#') || partial.is_empty() {
                out.extend(suggest_resources(partial, context.tag_names(registry), "#"));
            }
            if !partial.starts_with('#') {
                out.extend(suggest_resources(
                    partial,
                    registry_entries(registry).unwrap_or(&[]).iter().copied(),
                    "",
                ));
            }
            out
        }
        "minecraft:block_state" | "minecraft:block_predicate" => {
            if partial.contains('[') || partial.contains('{') {
                return Vec::new();
            }
            let mut out = Vec::new();
            if parser == "minecraft:block_predicate"
                && (partial.starts_with('#') || partial.is_empty())
            {
                out.extend(suggest_resources(
                    partial,
                    context.tag_names("minecraft:block"),
                    "#",
                ));
            }
            if !partial.starts_with('#') {
                out.extend(suggest_resources(
                    partial,
                    registry_entries("minecraft:block")
                        .unwrap_or(&[])
                        .iter()
                        .copied(),
                    "",
                ));
            }
            out
        }
        "minecraft:item_stack" | "minecraft:item_predicate" => {
            if partial.contains('[') {
                return Vec::new();
            }
            let mut out = Vec::new();
            if parser == "minecraft:item_predicate"
                && (partial.starts_with('#') || partial.is_empty())
            {
                out.extend(suggest_resources(
                    partial,
                    context.tag_names("minecraft:item"),
                    "#",
                ));
            }
            if !partial.starts_with('#') {
                out.extend(suggest_resources(
                    partial,
                    registry_entries("minecraft:item")
                        .unwrap_or(&[])
                        .iter()
                        .copied(),
                    "",
                ));
            }
            out
        }
        "minecraft:vec3" | "minecraft:block_pos" => {
            suggest_coordinates(partial, 3, partial.starts_with('^'))
        }
        "minecraft:vec2" | "minecraft:column_pos" | "minecraft:rotation" => {
            suggest_coordinates(partial, 2, false)
        }
        "minecraft:entity_anchor" => names(&Anchor::NAMES),
        "minecraft:heightmap" => names(HEIGHTMAPS),
        "minecraft:swing_animation" => names(SWING_ANIMATIONS),
        "minecraft:template_rotation" => names(&TEMPLATE_ROTATIONS),
        "minecraft:template_mirror" => names(&TEMPLATE_MIRRORS),
        "minecraft:feature" => suggest_resources(
            partial,
            registry_entries("minecraft:worldgen/feature")
                .unwrap_or(&[])
                .iter()
                .copied(),
            "",
        ),
        "minecraft:time" => {
            let mut reader = Reader::new(partial);
            if reader.read_float().is_err() {
                return Vec::new();
            }
            let number = &partial[..reader.cursor()];
            let unit = &partial[reader.cursor()..];
            ["d", "s", "t"]
                .into_iter()
                .filter(|u| u.starts_with(unit))
                .map(|u| format!("{number}{u}"))
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests;

//! Advancement files (`advancement/<name>.json`) as tree entries, checked the
//! way the game's codecs check them. Every trigger must be one the tree's
//! trigger registry knows, and stored predicates the conditions name are
//! inlined. What a file parses to is owned by the entry, so it is freed
//! with the last tree holding it.

use serde_json::{Map, Value};
use tuff_vanilla_data::{Frozen, Item};

use super::entry::{Criterion, Display, Entry, Frame, Rewards};
use tuff_core::world::advancement::trigger::Registry;
use tuff_datapack::loot::Predicates;
use tuff_item::item::{Components, ItemStack, MintReason};
use tuff_item::registries::codecs::json_to_nbt;
use tuff_item::text::TextComponent;

type Object = Map<String, Value>;

/// The fields of trigger conditions that hold one loot condition (an
/// inline condition or a stored predicate's id).
const CONDITION_FIELDS: &[&str] = &[
    "player",
    "entity",
    "bystander",
    "cause",
    "child",
    "lightning",
    "location",
    "parent",
    "partner",
    "projectile",
    "source",
    "villager",
    "zombie",
];
/// The fields that hold a list of loot conditions.
const CONDITION_LIST_FIELDS: &[&str] = &["victims"];
const MAX_ICON_COUNT: u64 = 99;

/// The advancement `id` a pack file holds.
pub fn parse(
    id: &str,
    bytes: &[u8],
    triggers: &Registry,
    predicates: &Predicates,
) -> Result<Entry, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let object = object(&value)?;
    let parent = object
        .get("parent")
        .map(|parent| identifier(parent).map(Frozen::from))
        .transpose()?;
    let display = object.get("display").map(display).transpose()?;
    if let Some(display) = &display {
        match (&parent, &display.background) {
            (None, None) => return Err("Visible advancement roots must have background".into()),
            (Some(_), Some(_)) => return Err("Only advancement roots can have background".into()),
            _ => {}
        }
    }
    let criteria = self::object(field(object, "criteria")?)?
        .iter()
        .map(|(name, value)| criterion(name, value, triggers, predicates))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("criteria: {err}"))?;
    let requirements = object.get("requirements").map(requirements).transpose()?;
    let rewards = object
        .get("rewards")
        .map(rewards)
        .transpose()?
        .unwrap_or_default();
    let sends_telemetry_event = optional_bool(object, "sends_telemetry_event", false)?;
    Entry::new(
        Frozen::from(id.to_owned()),
        parent,
        display,
        criteria,
        requirements,
        rewards,
        sends_telemetry_event,
    )
}

fn object(value: &Value) -> Result<&Object, String> {
    value
        .as_object()
        .ok_or_else(|| format!("Not a JSON object: {value}"))
}

fn field<'v>(object: &'v Object, key: &str) -> Result<&'v Value, String> {
    object
        .get(key)
        .ok_or_else(|| format!("No key {key} in MapLike"))
}

fn string(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("Not a string: {value}"))
}

fn identifier(value: &Value) -> Result<String, String> {
    let text = string(value)?;
    tuff_datapack::identifier(text).ok_or_else(|| format!("Not a valid resource location: {text}"))
}

fn optional_bool(object: &Object, key: &str, default: bool) -> Result<bool, String> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("Not a boolean: {value}")),
    }
}

fn display(value: &Value) -> Result<Display, String> {
    let object = object(value)?;
    let frame = match object.get("frame") {
        None => Frame::Task,
        Some(frame) => match string(frame)? {
            "task" => Frame::Task,
            "challenge" => Frame::Challenge,
            "goal" => Frame::Goal,
            other => return Err(format!("Unknown advancement frame: {other}")),
        },
    };
    Ok(Display {
        icon: icon(field(object, "icon")?)?,
        title: text(field(object, "title")?),
        description: text(field(object, "description")?),
        frame,
        background: object
            .get("background")
            .map(|background| identifier(background).map(Frozen::from))
            .transpose()?,
        show_toast: optional_bool(object, "show_toast", true)?,
        announce_to_chat: optional_bool(object, "announce_to_chat", true)?,
        hidden: optional_bool(object, "hidden", false)?,
    })
}

/// A text component; one using styling this server does not model keeps
/// its plain text.
fn text(value: &Value) -> TextComponent {
    TextComponent::from_json_str(&value.to_string())
        .unwrap_or_else(|| TextComponent::new(plain(value)))
}

fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().map(plain).collect(),
        Value::Object(object) => {
            let own = object
                .get("text")
                .or_else(|| object.get("translate"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let extra: String = object
                .get("extra")
                .and_then(Value::as_array)
                .map(|parts| parts.iter().map(plain).collect())
                .unwrap_or_default();
            own + &extra
        }
        other => other.to_string(),
    }
}

fn item(value: &Value) -> Result<Item, String> {
    let id = identifier(value)?;
    Item::from_name(&id).ok_or_else(|| {
        format!("Unknown registry key in ResourceKey[minecraft:root / minecraft:item]: {id}")
    })
}

fn icon(value: &Value) -> Result<ItemStack, String> {
    if value.is_string() {
        return Ok(ItemStack::mint(item(value)?, 1, MintReason::Template));
    }
    let object = object(value)?;
    let count = match object.get("count") {
        None => 1,
        Some(count) => count
            .as_u64()
            .filter(|count| (1..=MAX_ICON_COUNT).contains(count))
            .ok_or_else(|| format!("Value must be within range [1;99]: {count}"))?,
    };
    let components = match object.get("components") {
        None => Components::default(),
        Some(patch) => json_to_nbt(patch)
            .and_then(|tag| tag.as_compound().map(Components::from_nbt))
            .ok_or_else(|| format!("Invalid components: {patch}"))?,
    };
    Ok(ItemStack::mint(
        item(field(object, "id")?)?,
        count as u8,
        MintReason::Template,
    )
    .with_components(components))
}

fn criterion(
    name: &str,
    value: &Value,
    triggers: &Registry,
    predicates: &Predicates,
) -> Result<Criterion, String> {
    let object = object(value)?;
    let trigger = identifier(field(object, "trigger")?)?;
    if triggers.get(&trigger).is_none() {
        return Err(format!(
            "{name}: No key {trigger} in registry minecraft:trigger_type"
        ));
    }
    let conditions = match object.get("conditions") {
        None => Value::Null,
        Some(conditions) => {
            let mut conditions = self::object(conditions)?.clone();
            inline_conditions(&mut conditions, predicates)
                .map_err(|err| format!("{name}: {err}"))?;
            Value::Object(conditions)
        }
    };
    Ok(Criterion {
        name: Frozen::from(name.to_owned()),
        trigger: Frozen::from(trigger),
        conditions,
    })
}

/// Inlines the stored predicates the condition fields name, so criteria
/// are tested without the predicate set at hand.
fn inline_conditions(conditions: &mut Object, predicates: &Predicates) -> Result<(), String> {
    for key in CONDITION_FIELDS {
        if let Some(condition) = conditions.get_mut(*key) {
            *condition = predicates.inline(condition)?;
        }
    }
    for key in CONDITION_LIST_FIELDS {
        if let Some(list) = conditions.get_mut(*key) {
            let Value::Array(list) = list else {
                return Err(format!("Not a list: {list}"));
            };
            for condition in list {
                *condition = predicates.inline(condition)?;
            }
        }
    }
    Ok(())
}

fn requirements(value: &Value) -> Result<Vec<Vec<Frozen<str>>>, String> {
    let groups = value
        .as_array()
        .ok_or_else(|| format!("Not a list: {value}"))?;
    groups
        .iter()
        .map(|group| {
            group
                .as_array()
                .ok_or_else(|| format!("Not a list: {group}"))?
                .iter()
                .map(|name| string(name).map(|name| Frozen::from(name.to_owned())))
                .collect()
        })
        .collect()
}

fn id_list(value: &Value, what: &str) -> Result<Box<[Frozen<str>]>, String> {
    match value {
        Value::String(text) if text.starts_with('#') => {
            Err(format!("{what} tags are not supported: {text}"))
        }
        Value::String(_) => Ok(Box::new([Frozen::from(identifier(value)?)])),
        Value::Array(ids) => ids
            .iter()
            .map(|id| identifier(id).map(Frozen::from))
            .collect(),
        other => Err(format!("Not a list: {other}")),
    }
}

fn rewards(value: &Value) -> Result<Rewards, String> {
    let object = object(value)?;
    let experience = match object.get("experience") {
        None => 0,
        Some(experience) => experience
            .as_i64()
            .and_then(|experience| i32::try_from(experience).ok())
            .ok_or_else(|| format!("Not an int: {experience}"))?,
    };
    let recipes = match object.get("recipes") {
        None => Box::default(),
        Some(Value::Array(_)) => id_list(&object["recipes"], "Recipe")?,
        Some(other) => return Err(format!("Not a list: {other}")),
    };
    Ok(Rewards {
        experience,
        loot: match object.get("loot") {
            None => Box::default(),
            Some(loot) => id_list(loot, "Loot table")?,
        },
        recipes,
        function: object
            .get("function")
            .map(|function| identifier(function).map(Frozen::from))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn predicates() -> Predicates {
        let mut files = BTreeMap::new();
        files.insert(
            "a:sneaking".to_owned(),
            (
                "file/test".to_owned(),
                br#"{"type":"minecraft:entity_properties","entity":"this",
                    "predicate":{"minecraft:flags":{"is_sneaking":true}}}"#
                    .to_vec(),
            ),
        );
        Predicates::new(files)
    }

    fn all_triggers() -> Registry {
        let mut registry = Registry::new();
        for kind in tuff_vanilla_data::registry::TriggerType::ALL {
            registry.register(kind.name(), std::sync::Arc::new(|_, _, _| false));
        }
        registry
    }

    fn read(json: &str) -> Result<Entry, String> {
        parse("a:test", json.as_bytes(), &all_triggers(), &predicates())
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_full_advancement_reads_with_the_game_defaults() {
        let entry = read(
            r#"{"display":{"icon":{"id":"minecraft:diamond","count":3},
                "title":{"text":"Gems","color":"aqua"},"description":"Find one",
                "background":"block/stone"},
              "criteria":{"z":{"trigger":"minecraft:tick"},
                "a":{"trigger":"inventory_changed",
                  "conditions":{"player":"a:sneaking","items":[{"items":"minecraft:diamond"}]}}},
              "requirements":[["a","z"]],
              "rewards":{"experience":5,"loot":"minecraft:chests/simple_dungeon",
                "recipes":["minecraft:stick"],"function":"a:reward"}}"#,
        )
        .unwrap();
        assert_eq!(&*entry.name, "a:test");
        assert!(entry.parent_name.is_none());
        let display = entry.display.as_ref().unwrap();
        assert_eq!(
            display.icon,
            ItemStack::mint(Item::Diamond, 3, MintReason::Test)
        );
        assert_eq!(display.frame, Frame::Task);
        assert!(display.show_toast && display.announce_to_chat && !display.hidden);
        assert_eq!(display.background.as_deref(), Some("minecraft:block/stone"));
        assert_eq!(display.title, TextComponent::new("Gems").color("aqua"));
        let names: Vec<&str> = entry.criteria.iter().map(|c| &*c.name).collect();
        assert_eq!(names, ["a", "z"], "sorted by name");
        assert_eq!(&*entry.criteria[0].trigger, "minecraft:inventory_changed");
        assert_eq!(
            entry.criteria[0].conditions["player"]["type"], "minecraft:entity_properties",
            "the stored predicate is inlined"
        );
        assert_eq!(entry.criteria[1].conditions, Value::Null);
        assert_eq!(entry.requirements.len(), 1);
        assert_eq!(entry.rewards.experience, 5);
        assert_eq!(&*entry.rewards.loot, ["minecraft:chests/simple_dungeon"]);
        assert_eq!(entry.rewards.function.as_deref(), Some("a:reward"));
        assert!(!entry.sends_telemetry_event);
    }

    #[tuff_core::watchdog]
    #[test]
    fn defaults_give_each_criterion_its_own_group() {
        let entry = read(
            r#"{"parent":"minecraft:story/root","display":{"icon":"minecraft:stone",
                "title":"t","description":"d","frame":"challenge","hidden":true},
              "criteria":{"b":{"trigger":"minecraft:impossible"},"a":{"trigger":"minecraft:impossible"}}}"#,
        )
        .unwrap();
        assert_eq!(entry.parent_name.as_deref(), Some("minecraft:story/root"));
        assert_eq!(entry.display.as_ref().unwrap().frame, Frame::Challenge);
        assert_eq!(entry.requirements.len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn invalid_files_are_refused() {
        for bad in [
            r#"{"criteria":{}}"#,
            r#"{}"#,
            r#"{"criteria":{"a":{"trigger":"minecraft:no_such"}}}"#,
            r#"{"criteria":{"a":{"trigger":"minecraft:tick"}},"requirements":[["a","b"]]}"#,
            r#"{"criteria":{"a":{"trigger":"minecraft:tick"}},"requirements":[]}"#,
            r#"{"criteria":{"a":{"trigger":"minecraft:tick","conditions":{"player":"a:missing"}}}}"#,
            r#"{"display":{"icon":"minecraft:stone","title":"t","description":"d"},
                "criteria":{"a":{"trigger":"minecraft:tick"}}}"#,
            r#"{"parent":"a:root","display":{"icon":"minecraft:stone","title":"t",
                "description":"d","background":"x"},"criteria":{"a":{"trigger":"minecraft:tick"}}}"#,
            r#"{"parent":"a:root","display":{"icon":{"id":"minecraft:stone","count":100},
                "title":"t","description":"d"},"criteria":{"a":{"trigger":"minecraft:tick"}}}"#,
            r#"{"parent":"a:root","display":{"icon":"minecraft:no_item","title":"t",
                "description":"d"},"criteria":{"a":{"trigger":"minecraft:tick"}}}"#,
            r#"{"parent":"Bad Id","criteria":{"a":{"trigger":"minecraft:tick"}}}"#,
            r##"{"criteria":{"a":{"trigger":"minecraft:tick"}},"rewards":{"loot":"#a:tables"}}"##,
            "[1]",
            "{not json",
        ] {
            assert!(read(bad).is_err(), "{bad}");
        }
    }
}

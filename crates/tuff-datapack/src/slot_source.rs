//! Slot sources (`slot_source/<name>.json`): composable selections of
//! container slots. A source is a `group` (also a bare list), `filtered`,
//! `limit_slots`, `slot_range`, `contents` or `empty`; sources refer to
//! stored ones by id.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum SlotSource {
    Empty,
    Group(Vec<SlotSourceRef>),
    Filtered {
        source: Box<SlotSourceRef>,
        item_filter: Value,
    },
    Limit {
        source: Box<SlotSourceRef>,
        limit: u32,
    },
    Range {
        source: String,
        slots: String,
    },
    Contents {
        source: Box<SlotSourceRef>,
        component: String,
    },
}

/// A stored source by id, or one written inline.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotSourceRef {
    Id(String),
    Inline(SlotSource),
}

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

const SOURCES: &[&str] = &[
    "this",
    "attacker",
    "direct_attacker",
    "attacking_player",
    "target_entity",
    "interacting_entity",
    "block_entity",
    "container",
];

fn reference(
    value: &Value,
    depth: usize,
    slots: &dyn Fn(&str) -> bool,
) -> Result<SlotSourceRef, String> {
    match value {
        Value::String(id) => Ok(SlotSourceRef::Id(normalize(id))),
        _ => parse(value, depth, slots).map(SlotSourceRef::Inline),
    }
}

/// A slot source from JSON; `slots` says whether a slot range name is
/// known.
pub fn parse(
    value: &Value,
    depth: usize,
    slots: &dyn Fn(&str) -> bool,
) -> Result<SlotSource, String> {
    if depth > MAX_DEPTH {
        return Err("slot source nests too deeply".to_owned());
    }
    if let Value::Array(terms) = value {
        return group(terms, depth, slots);
    }
    let object = value
        .as_object()
        .ok_or("slot source must be an object or a list")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .map(normalize)
        .ok_or("slot source has no type")?;
    let field = |name: &str| {
        object
            .get(name)
            .ok_or_else(|| format!("{kind} needs {name}"))
    };
    let child = |depth: usize| reference(field("slot_source")?, depth + 1, slots);
    Ok(match kind.as_str() {
        "minecraft:empty" => SlotSource::Empty,
        "minecraft:group" => match field("terms")? {
            Value::Array(terms) => group(terms, depth, slots)?,
            _ => return Err("terms must be a list".to_owned()),
        },
        "minecraft:filtered" => {
            let item_filter = field("item_filter")?;
            if !item_filter.is_object() {
                return Err("item_filter must be an object".to_owned());
            }
            SlotSource::Filtered {
                source: Box::new(child(depth)?),
                item_filter: item_filter.clone(),
            }
        }
        "minecraft:limit_slots" => {
            let limit = field("limit")?
                .as_u64()
                .filter(|limit| (1..=i32::MAX as u64).contains(limit))
                .ok_or("limit must be positive")? as u32;
            SlotSource::Limit {
                source: Box::new(child(depth)?),
                limit,
            }
        }
        "minecraft:slot_range" => {
            let source = object
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("container");
            if !SOURCES.contains(&source) {
                return Err(format!("unknown slot source target {source}"));
            }
            let range = field("slots")?
                .as_str()
                .filter(|range| slots(range))
                .ok_or("unknown slot range")?;
            SlotSource::Range {
                source: source.to_owned(),
                slots: range.to_owned(),
            }
        }
        "minecraft:contents" => SlotSource::Contents {
            source: Box::new(child(depth)?),
            component: normalize(
                field("component")?
                    .as_str()
                    .ok_or("component must be a string")?,
            ),
        },
        other => return Err(format!("unknown slot source type {other}")),
    })
}

fn group(
    terms: &[Value],
    depth: usize,
    slots: &dyn Fn(&str) -> bool,
) -> Result<SlotSource, String> {
    terms
        .iter()
        .map(|term| reference(term, depth + 1, slots))
        .collect::<Result<_, _>>()
        .map(SlotSource::Group)
}

/// Stored slot sources by id.
#[derive(Debug, Default)]
pub struct SlotSources {
    resolved: BTreeMap<String, Arc<SlotSource>>,
}

impl SlotSources {
    /// Reads pack slot source files; one that is not JSON or does not
    /// parse is logged and left out.
    pub fn new(files: BTreeMap<String, (String, Vec<u8>)>, slots: &dyn Fn(&str) -> bool) -> Self {
        let resolved = files
            .into_iter()
            .filter_map(|(id, (pack, bytes))| {
                let parsed = serde_json::from_slice::<Value>(&bytes)
                    .map_err(|err| err.to_string())
                    .and_then(|value| parse(&value, 0, slots));
                match parsed {
                    Ok(source) => Some((id, Arc::new(source))),
                    Err(err) => {
                        log::warn!("Couldn't parse data file {id} from {pack}: {err}");
                        None
                    }
                }
            })
            .collect();
        Self { resolved }
    }

    pub fn get(&self, id: &str) -> Option<Arc<SlotSource>> {
        self.resolved.get(id).cloned()
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.resolved.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn known(name: &str) -> bool {
        matches!(name, "weapon.mainhand" | "container.*")
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_every_type_and_the_bare_list() {
        let parsed = parse(
            &json!([
                {"type": "slot_range", "source": "this", "slots": "weapon.mainhand"},
                "a:other",
                {"type": "limit_slots", "limit": 2, "slot_source": {"type": "empty"}},
                {"type": "filtered", "item_filter": {"items": "stone"}, "slot_source": "b:c"},
                {"type": "contents", "component": "container", "slot_source": {"type": "group", "terms": []}}
            ]),
            0,
            &known,
        )
        .unwrap();
        let SlotSource::Group(terms) = parsed else {
            panic!("a bare list is a group");
        };
        assert_eq!(terms.len(), 5);
        assert_eq!(terms[1], SlotSourceRef::Id("a:other".into()));
        assert!(matches!(
            &terms[2],
            SlotSourceRef::Inline(SlotSource::Limit { limit: 2, .. })
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_bad_sources() {
        for bad in [
            json!({"type": "nope"}),
            json!({"type": "limit_slots", "limit": 0, "slot_source": {"type": "empty"}}),
            json!({"type": "slot_range", "slots": "nope"}),
            json!({"type": "slot_range", "source": "nobody", "slots": "weapon.mainhand"}),
            json!({"type": "filtered", "slot_source": {"type": "empty"}}),
            json!(5),
        ] {
            assert!(parse(&bad, 0, &known).is_err(), "{bad}");
        }
        let mut deep = json!({"type": "empty"});
        for _ in 0..40 {
            deep = json!([deep]);
        }
        assert!(parse(&deep, 0, &known).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn stored_sources_load_and_skip_broken_files() {
        let mut files = BTreeMap::new();
        files.insert(
            "a:ok".to_owned(),
            ("p".to_owned(), br#"{"type":"empty"}"#.to_vec()),
        );
        files.insert("a:bad".to_owned(), ("p".to_owned(), b"{".to_vec()));
        let sources = SlotSources::new(files, &known);
        assert_eq!(sources.get("a:ok").as_deref(), Some(&SlotSource::Empty));
        assert!(sources.get("a:bad").is_none());
    }
}

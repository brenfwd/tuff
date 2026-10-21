//! `pack.mcmeta`: a data pack's description, the pack formats it says it
//! supports, the feature flags it needs, its overlays and its filter.

use regex::{Regex, RegexBuilder};
use serde_json::Value;

use tuff_item::text::TextComponent;

/// A pack format version, ordered by major then minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Format {
    pub major: i32,
    pub minor: i32,
}

impl Format {
    pub const UNKNOWN: Self = Self {
        major: i32::MAX,
        minor: 0,
    };

    pub fn current() -> Self {
        let version = tuff_vanilla_data::version::DATA_PACK_VERSION;
        Self {
            major: version.major,
            minor: version.minor,
        }
    }

    fn parse(value: &Value, default_minor: i32) -> Option<Self> {
        let int = |value: &Value| {
            value
                .as_i64()
                .filter(|v| (0..=i64::from(i32::MAX)).contains(v))
                .map(|v| v as i32)
        };
        match value {
            Value::Array(parts) if (1..=256).contains(&parts.len()) => Some(Self {
                major: int(&parts[0])?,
                minor: parts.get(1).map_or(Some(default_minor), int)?,
            }),
            value => Some(Self {
                major: int(value)?,
                minor: default_minor,
            }),
        }
    }
}

/// How a pack's declared formats relate to this game version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    TooOld,
    TooNew,
    Unknown,
    Compatible,
}

impl Compatibility {
    pub fn of(range: (Format, Format), current: Format) -> Self {
        if range.0.major == i32::MAX {
            Self::Unknown
        } else if range.1 < current {
            Self::TooOld
        } else if current < range.0 {
            Self::TooNew
        } else {
            Self::Compatible
        }
    }
}

/// An overlay directory and the formats it applies to.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub directory: String,
    pub formats: (Format, Format),
}

/// One `filter.block` entry: a resource is hidden when its namespace and
/// its path (e.g. `recipe/stick.json`) each contain a match. A missing
/// pattern matches everything.
#[derive(Debug, Clone)]
pub struct FilterPattern {
    pub namespace: Option<Regex>,
    pub path: Option<Regex>,
}

impl PartialEq for FilterPattern {
    fn eq(&self, other: &Self) -> bool {
        let text = |r: &Option<Regex>| r.as_ref().map(|r| r.as_str().to_owned());
        text(&self.namespace) == text(&other.namespace) && text(&self.path) == text(&other.path)
    }
}

/// The longest pattern a filter may hold.
const MAX_PATTERN: usize = 4096;

fn pattern(entry: &serde_json::Map<String, Value>, key: &str) -> Result<Option<Regex>, String> {
    let Some(value) = entry.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{key} is not a string"))?;
    if text.len() > MAX_PATTERN {
        return Err(format!("{key} pattern is too long"));
    }
    RegexBuilder::new(text)
        .size_limit(1 << 20)
        .build()
        .map(Some)
        .map_err(|err| format!("invalid {key} pattern {text:?}: {err}"))
}

fn filter_section(section: &Value) -> Result<Vec<FilterPattern>, String> {
    let block = section
        .get("block")
        .and_then(Value::as_array)
        .ok_or("filter without a block list")?;
    block
        .iter()
        .map(|entry| {
            let entry = entry.as_object().ok_or("filter entry is not an object")?;
            Ok(FilterPattern {
                namespace: pattern(entry, "namespace")?,
                path: pattern(entry, "path")?,
            })
        })
        .collect()
}

/// Whether a `filter.block` list hides the resource at `path` (below the
/// namespace) in `namespace`. Namespace and path are matched across the
/// whole list separately, as the game does.
pub fn filtered(filter: &[FilterPattern], namespace: &str, path: &str) -> bool {
    let any = |pick: fn(&FilterPattern) -> &Option<Regex>, text: &str| {
        filter
            .iter()
            .any(|p| pick(p).as_ref().is_none_or(|r| r.is_match(text)))
    };
    any(|p| &p.namespace, namespace) && any(|p| &p.path, path)
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackMeta {
    pub description: TextComponent,
    /// The declared format range, [`Format::UNKNOWN`] when unreadable.
    pub formats: (Format, Format),
    pub features: Vec<String>,
    pub overlays: Vec<Overlay>,
    /// What the pack hides in the packs below it.
    pub filter: Vec<FilterPattern>,
}

fn int_range(value: &Value) -> Option<(i32, i32)> {
    let int = |value: &Value| value.as_i64().and_then(|v| i32::try_from(v).ok());
    match value {
        Value::Array(bounds) if bounds.len() == 2 => Some((int(&bounds[0])?, int(&bounds[1])?)),
        Value::Object(bounds) => Some((
            int(bounds.get("min_inclusive")?)?,
            int(bounds.get("max_inclusive")?)?,
        )),
        value => int(value).map(|v| (v, v)),
    }
}

fn whole_range((min, max): (i32, i32)) -> (Format, Format) {
    (
        Format {
            major: min,
            minor: 0,
        },
        Format {
            major: max,
            minor: i32::MAX,
        },
    )
}

/// The declared range of a `pack` or overlay entry: `min_format` and
/// `max_format` when both are present, else the older `supported_formats`
/// (`formats` for overlays), else the single `pack_format`.
fn declared_range(
    section: &serde_json::Map<String, Value>,
    old_field: &str,
) -> Option<(Format, Format)> {
    let min = section.get("min_format").map(|v| Format::parse(v, 0));
    let max = section
        .get("max_format")
        .map(|v| Format::parse(v, i32::MAX));
    match (min, max) {
        (Some(min), Some(max)) => {
            let range = (min?, max?);
            return (range.0 <= range.1).then_some(range);
        }
        (None, None) => {}
        _ => return None,
    }
    if let Some(supported) = section.get(old_field) {
        let (min, max) = int_range(supported)?;
        return (min <= max).then(|| whole_range((min, max)));
    }
    let format = section.get("pack_format")?.as_i64()?;
    let format = i32::try_from(format).ok()?;
    Some(whole_range((format, format)))
}

fn valid_overlay_directory(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

impl PackMeta {
    /// Reads `pack.mcmeta`. A missing or unreadable description is an
    /// error (the pack is not a pack); a format the game would reject only
    /// makes the compatibility unknown.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
        let pack = root
            .get("pack")
            .and_then(Value::as_object)
            .ok_or("missing pack section")?;
        let description = pack.get("description").ok_or("missing description")?;
        let description = TextComponent::from_json_str(&description.to_string())
            .ok_or("unreadable description")?;
        let formats =
            declared_range(pack, "supported_formats").unwrap_or((Format::UNKNOWN, Format::UNKNOWN));
        let features = root
            .get("features")
            .and_then(|f| f.get("enabled"))
            .and_then(Value::as_array)
            .map(|flags| {
                flags
                    .iter()
                    .filter_map(Value::as_str)
                    .map(crate::command::args::qualify)
                    .collect()
            })
            .unwrap_or_default();
        let mut overlays = Vec::new();
        if let Some(entries) = root
            .get("overlays")
            .and_then(|o| o.get("entries"))
            .and_then(Value::as_array)
        {
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let Some(directory) = entry.get("directory").and_then(Value::as_str) else {
                    continue;
                };
                if !valid_overlay_directory(directory) {
                    return Err(format!("invalid overlay directory {directory:?}"));
                }
                if let Some(formats) = declared_range(entry, "formats") {
                    overlays.push(Overlay {
                        directory: directory.to_owned(),
                        formats,
                    });
                }
            }
        }
        Ok(Self {
            description,
            formats,
            features,
            overlays,
            filter: match root.get("filter").map(filter_section) {
                None => Vec::new(),
                Some(Ok(filter)) => filter,
                Some(Err(err)) => {
                    log::error!("Failed to get filter section from pack: {err}");
                    Vec::new()
                }
            },
        })
    }

    pub fn compatibility(&self) -> Compatibility {
        Compatibility::of(self.formats, Format::current())
    }

    /// The overlay directories that apply to this game version, lowest
    /// priority first.
    pub fn active_overlays(&self) -> Vec<&str> {
        let current = Format::current();
        self.overlays
            .iter()
            .filter(|o| o.formats.0 <= current && current <= o.formats.1)
            .map(|o| o.directory.as_str())
            .collect()
    }

    /// The `pack.mcmeta` a new pack gets: this version's formats.
    pub fn new_pack_json(description: &TextComponent) -> String {
        let current = Format::current();
        let pack = serde_json::json!({
            "pack": {
                "description": description.to_value(),
                "min_format": current.major,
                "max_format": current.major,
            }
        });
        serde_json::to_string_pretty(&pack).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> i32 {
        Format::current().major
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_the_new_and_old_format_fields() {
        let new = format!(
            r#"{{"pack":{{"description":"d","min_format":{0},"max_format":[{0},5]}}}}"#,
            current()
        );
        let meta = PackMeta::parse(new.as_bytes()).unwrap();
        assert_eq!(meta.description.plain(), "d");
        assert_eq!(meta.compatibility(), Compatibility::Compatible);
        assert_eq!(meta.formats.1.minor, 5);

        let old = br#"{"pack":{"description":{"text":"old"},"pack_format":15}}"#;
        let meta = PackMeta::parse(old).unwrap();
        assert_eq!(meta.compatibility(), Compatibility::TooOld);

        let range =
            br#"{"pack":{"description":"r","pack_format":15,"supported_formats":[15,99999]}}"#;
        assert_eq!(
            PackMeta::parse(range).unwrap().compatibility(),
            Compatibility::Compatible
        );

        let future = br#"{"pack":{"description":"f","min_format":9999,"max_format":9999}}"#;
        assert_eq!(
            PackMeta::parse(future).unwrap().compatibility(),
            Compatibility::TooNew
        );

        let unknown = br#"{"pack":{"description":"u","min_format":5}}"#;
        assert_eq!(
            PackMeta::parse(unknown).unwrap().compatibility(),
            Compatibility::Unknown
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pack_needs_a_description() {
        assert!(PackMeta::parse(br#"{"pack":{"pack_format":81}}"#).is_err());
        assert!(PackMeta::parse(br#"{"other":{}}"#).is_err());
        assert!(PackMeta::parse(b"not json").is_err());
        assert!(PackMeta::parse(b"").is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn overlays_apply_by_format_and_features_are_read() {
        let json = format!(
            r#"{{"pack":{{"description":"d","min_format":1,"max_format":999}},
               "features":{{"enabled":["trade_rebalance"]}},
               "overlays":{{"entries":[
                 {{"directory":"now","min_format":{0},"max_format":{0}}},
                 {{"directory":"old","formats":[10,20]}},
                 {{"directory":"also","formats":{{"min_inclusive":1,"max_inclusive":{0}}}}}
               ]}}}}"#,
            current()
        );
        let meta = PackMeta::parse(json.as_bytes()).unwrap();
        assert_eq!(meta.features, ["minecraft:trade_rebalance"]);
        assert_eq!(meta.active_overlays(), ["now", "also"]);
        let bad = br#"{"pack":{"description":"d"},"overlays":{"entries":[{"directory":"../x","formats":1}]}}"#;
        assert!(PackMeta::parse(bad).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn filters_match_namespace_and_path_anywhere() {
        let json = br#"{"pack":{"description":"d","pack_format":1},
            "filter":{"block":[{"namespace":"^minecraft$","path":"^recipe/"},{"path":"stick"}]}}"#;
        let filter = PackMeta::parse(json).unwrap().filter;
        assert_eq!(filter.len(), 2);
        assert!(filtered(&filter, "minecraft", "recipe/oak_planks.json"));
        assert!(filtered(&filter, "a", "loot_table/stick.json"));
        assert!(!filtered(
            &filter,
            "minecraft",
            "loot_table/blocks/dirt.json"
        ));
        let namespace_only = br#"{"pack":{"description":"d","pack_format":1},
            "filter":{"block":[{"namespace":"inecraf"}]}}"#;
        let filter = PackMeta::parse(namespace_only).unwrap().filter;
        assert!(filtered(&filter, "minecraft", "tags/item/planks.json"));
        assert!(!filtered(&filter, "other", "tags/item/planks.json"));
        assert!(!filtered(&[], "minecraft", "recipe/stick.json"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_broken_filter_is_ignored_and_the_pack_still_reads() {
        for json in [
            br#"{"pack":{"description":"d"},"filter":{"block":[{"path":"("}]}}"#.as_slice(),
            br#"{"pack":{"description":"d"},"filter":{"block":[{"path":5}]}}"#,
            br#"{"pack":{"description":"d"},"filter":{}}"#,
        ] {
            assert!(PackMeta::parse(json).unwrap().filter.is_empty());
        }
        let huge = format!(
            r#"{{"pack":{{"description":"d"}},"filter":{{"block":[{{"path":"{}"}}]}}}}"#,
            "a".repeat(MAX_PATTERN + 1)
        );
        assert!(PackMeta::parse(huge.as_bytes()).unwrap().filter.is_empty());
        let blowup =
            r#"{"pack":{"description":"d"},"filter":{"block":[{"path":"(a{1000}){1000}"}]}}"#;
        assert!(
            PackMeta::parse(blowup.as_bytes())
                .unwrap()
                .filter
                .is_empty()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_new_pack_declares_this_version() {
        let json = PackMeta::new_pack_json(&TextComponent::new("mine"));
        let meta = PackMeta::parse(json.as_bytes()).unwrap();
        assert_eq!(meta.compatibility(), Compatibility::Compatible);
        assert_eq!(meta.description.plain(), "mine");
    }
}

//! Typed `name~value` item sub-predicates (`enchantments~[...]`, ...),
//! decoded from the NBT value the command carries and tested against a
//! stack's stored component values.

use tuff_nbt::{Compound, Tag};

use tuff_item::item::ItemStack;

use super::range::{self, Bounds};
use super::reader::Reader;

#[derive(Debug, Clone, PartialEq)]
pub enum ComponentPredicate {
    Damage {
        durability: Bounds<i32>,
        damage: Bounds<i32>,
    },
    Enchantments(Vec<EnchantmentEntry>),
    StoredEnchantments(Vec<EnchantmentEntry>),
    Potion(Vec<String>),
    CustomData(Compound),
    Container(CollectionPredicate),
    BundleContents(CollectionPredicate),
    /// Every field the predicate names equals the component's field.
    Subset(String, Tag),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnchantmentEntry {
    pub ids: Option<Vec<String>>,
    pub levels: Bounds<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionPredicate {
    pub contains: Vec<Vec<String>>,
    pub size: Bounds<i32>,
}

pub const NAMES: &[&str] = &[
    "minecraft:attribute_modifiers",
    "minecraft:bundle_contents",
    "minecraft:container",
    "minecraft:custom_data",
    "minecraft:damage",
    "minecraft:enchantments",
    "minecraft:firework_explosion",
    "minecraft:fireworks",
    "minecraft:jukebox_playable",
    "minecraft:potion_contents",
    "minecraft:stored_enchantments",
    "minecraft:trim",
    "minecraft:villager/variant",
    "minecraft:writable_book_content",
    "minecraft:written_book_content",
];

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// An integer, an `a..b` string or a `{min,max}` compound.
pub(super) fn bounds(tag: &Tag) -> Option<Bounds<i32>> {
    if let Some(value) = tag.as_int() {
        return Some(Bounds::exactly(value));
    }
    if let Some(text) = tag.as_str() {
        let mut reader = Reader::new(text);
        let parsed = range::parse_ints(&mut reader).ok()?;
        return (!reader.can_read()).then_some(parsed);
    }
    let map = tag.as_compound()?;
    let min = map.get("min").map(|t| t.as_int()).unwrap_or(None);
    let max = map.get("max").map(|t| t.as_int()).unwrap_or(None);
    Some(Bounds { min, max })
}

fn optional_bounds(map: &Compound, key: &str) -> Result<Bounds<i32>, String> {
    match map.get(key) {
        None => Ok(Bounds::ANY),
        Some(tag) => bounds(tag).ok_or_else(|| format!("invalid range for {key}")),
    }
}

/// One id or a list of ids; a `#tag` is not resolvable here.
fn ids(tag: &Tag) -> Result<Vec<String>, String> {
    let one = |tag: &Tag| match tag.as_str() {
        Some(id) if id.starts_with('#') => Err("tags are not supported here".to_owned()),
        Some(id) => Ok(normalize(id)),
        None => Err("expected an id".to_owned()),
    };
    match tag.as_list() {
        Some(list) => list.iter().map(one).collect(),
        None => Ok(vec![one(tag)?]),
    }
}

fn collection(tag: &Tag) -> Result<CollectionPredicate, String> {
    let map = tag.as_compound().ok_or("expected a compound")?;
    let map = match map.get("items") {
        Some(inner) => inner.as_compound().ok_or("expected a compound")?,
        None => map,
    };
    let mut contains = Vec::new();
    if let Some(list) = map.get("contains") {
        for entry in list.as_list().ok_or("expected a list")? {
            let entry = entry.as_compound().ok_or("expected a compound")?;
            contains.push(match entry.get("items") {
                Some(items) => ids(items)?,
                None => Vec::new(),
            });
        }
    }
    Ok(CollectionPredicate {
        contains,
        size: optional_bounds(map, "size")?,
    })
}

fn enchantment_entries(tag: &Tag) -> Result<Vec<EnchantmentEntry>, String> {
    let list = tag.as_list().ok_or("expected a list")?;
    list.iter()
        .map(|entry| {
            let map = entry.as_compound().ok_or("expected a compound")?;
            Ok(EnchantmentEntry {
                ids: map.get("enchantments").map(ids).transpose()?,
                levels: optional_bounds(map, "levels")?,
            })
        })
        .collect()
}

impl ComponentPredicate {
    /// Decodes the NBT after `name~`; `Err` is the malformed message.
    pub fn from_nbt(name: &str, tag: &Tag) -> Result<Self, String> {
        Ok(match name {
            "minecraft:damage" => {
                let map = tag.as_compound().ok_or("expected a compound")?;
                Self::Damage {
                    durability: optional_bounds(map, "durability")?,
                    damage: optional_bounds(map, "damage")?,
                }
            }
            "minecraft:enchantments" => Self::Enchantments(enchantment_entries(tag)?),
            "minecraft:stored_enchantments" => Self::StoredEnchantments(enchantment_entries(tag)?),
            "minecraft:potion_contents" => Self::Potion(ids(tag)?),
            "minecraft:custom_data" => match tag {
                Tag::Compound(map) => Self::CustomData(map.clone()),
                _ => return Err("expected a compound".to_owned()),
            },
            "minecraft:container" => Self::Container(collection(tag)?),
            "minecraft:bundle_contents" => Self::BundleContents(collection(tag)?),
            _ => Self::Subset(name.to_owned(), tag.clone()),
        })
    }

    pub fn test<M: tuff_item::item::Mode>(&self, stack: &ItemStack<M>) -> bool {
        let stored = stack.components.to_nbt();
        match self {
            Self::Damage { durability, damage } => {
                let used = stack.damage() as i32;
                let max = stored
                    .get("minecraft:max_damage")
                    .and_then(Tag::as_int)
                    .unwrap_or(0);
                stored.contains_key("minecraft:damage")
                    && damage.contains(used)
                    && durability.contains(max - used)
            }
            Self::Enchantments(entries) => {
                enchantments_match(entries, stored.get("minecraft:enchantments"))
            }
            Self::StoredEnchantments(entries) => {
                enchantments_match(entries, stored.get("minecraft:stored_enchantments"))
            }
            Self::Potion(wanted) => stored
                .get("minecraft:potion_contents")
                .and_then(|tag| match tag {
                    Tag::String(id) => Some(id.as_str()),
                    Tag::Compound(map) => map.get("potion").and_then(Tag::as_str),
                    _ => None,
                })
                .is_some_and(|id| wanted.iter().any(|w| w == &normalize(id))),
            Self::CustomData(wanted) => stored
                .get("minecraft:custom_data")
                .is_some_and(|tag| subset(&Tag::Compound(wanted.clone()), tag)),
            Self::Container(pred) => stored
                .get("minecraft:container")
                .is_some_and(|tag| pred.test(&container_ids(tag))),
            Self::BundleContents(pred) => stored
                .get("minecraft:bundle_contents")
                .is_some_and(|tag| pred.test(&container_ids(tag))),
            Self::Subset(name, wanted) => stored.get(name).is_some_and(|tag| subset(wanted, tag)),
        }
    }
}

fn enchantments_match(entries: &[EnchantmentEntry], stored: Option<&Tag>) -> bool {
    let Some(map) = stored.and_then(Tag::as_compound) else {
        return entries.is_empty();
    };
    entries.iter().all(|entry| {
        map.iter().any(|(id, level)| {
            entry
                .ids
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|w| w == id))
                && level.as_int().is_some_and(|l| entry.levels.contains(l))
        })
    })
}

fn container_ids(tag: &Tag) -> Vec<String> {
    tag.as_list()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.as_compound()?;
            let item = entry
                .get("item")
                .and_then(Tag::as_compound)
                .unwrap_or(entry);
            Some(item.get("id")?.as_str()?.to_owned())
        })
        .collect()
}

impl CollectionPredicate {
    fn test(&self, held: &[String]) -> bool {
        self.size.contains(held.len() as i32)
            && self.contains.iter().all(|wanted| {
                held.iter()
                    .any(|id| wanted.is_empty() || wanted.iter().any(|w| w == id))
            })
    }
}

/// Whether every field of `wanted` is present and equal in `value`;
/// lists need each wanted element to match some element.
pub fn subset(wanted: &Tag, value: &Tag) -> bool {
    match (wanted, value) {
        (Tag::Compound(w), Tag::Compound(v)) => w
            .iter()
            .all(|(key, w)| v.get(key).is_some_and(|v| subset(w, v))),
        (Tag::List(w), Tag::List(v)) => w.iter().all(|w| v.iter().any(|v| subset(w, v))),
        (w, v) => w == v,
    }
}

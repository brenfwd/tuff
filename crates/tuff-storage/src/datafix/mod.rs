//! Upgrades records saved by older game versions to the current format
//! before anything reads them: chunks, entity chunks, level data, saved data,
//! player files, statistics and advancements. Each record carries the data
//! version it was written with; every fix newer than that runs in version
//! order. A record without a data version is taken as current, records
//! older than [`MIN_DATA_VERSION`] are refused, and records newer than this
//! server are left alone for the callers to reject.
//!
//! Old worlds are untrusted input: every walk into nested items, entities
//! and text is bounded by [`MAX_DEPTH`], and text parsed from JSON by
//! [`text::MAX_JSON_DEPTH`].

pub mod block_entity;
pub mod chunk;
pub mod entity;
pub mod item;
pub mod layout;
pub mod legacy;
pub mod level;
mod pre2860;
pub mod structures;
pub mod text;
mod trial_registry;

#[cfg(test)]
mod tests;

use thiserror::Error;
use tuff_vanilla_data::version::DATA_VERSION;

use crate::document::Document;
use tuff_nbt::{Compound, Tag};

/// The oldest data that can be upgraded: 1.13, the first release with named
/// block states instead of numeric block ids.
pub const MIN_DATA_VERSION: i32 = 1519;
/// How deeply items, entities and block entities nested in one another are
/// followed. Deeper data is kept unchanged.
pub const MAX_DEPTH: u8 = 24;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DataFixError {
    #[error(
        "saved by a version too old to upgrade (data version {0}, oldest supported {MIN_DATA_VERSION}, Minecraft 1.13)"
    )]
    TooOld(i32),
    #[error("worlds with custom dimensions made before 1.16.2 cannot be loaded")]
    OldCustomWorld,
}

/// Which fixes still apply to one record, and how deep inside it a walk is.
#[derive(Debug, Clone, Copy)]
pub struct Fixer {
    from: i32,
    depth: u8,
}

impl Fixer {
    pub fn new(from: i32) -> Self {
        Self { from, depth: 0 }
    }

    pub fn from(self) -> i32 {
        self.from
    }

    /// Whether the fix of schema `version`, sub-version `sub`, still has to
    /// run. Sub-versions belong after their data version: data saved at a
    /// version has only been through that version's first schema.
    pub fn due(self, version: i32, sub: u8) -> bool {
        i64::from(version) * 10 + i64::from(sub) > i64::from(self.from) * 10
    }

    pub fn pending(self) -> bool {
        self.from < DATA_VERSION
    }

    /// This fixer, skipping every fix older than `version`: for data that
    /// only took its current shape at that version.
    pub fn at_least(self, version: i32) -> Self {
        Self {
            from: self.from.max(version),
            depth: self.depth,
        }
    }

    pub fn nested(self) -> Option<Self> {
        (self.depth < MAX_DEPTH).then_some(Self {
            from: self.from,
            depth: self.depth + 1,
        })
    }
}

fn version_of(root: &Compound) -> Option<i32> {
    root.get("DataVersion").and_then(number).map(|v| v as i32)
}

/// The fixer for a record at `root`, or `None` when nothing applies.
fn fixer_for(root: &Compound) -> Result<Option<Fixer>, DataFixError> {
    let Some(version) = version_of(root) else {
        return Ok(None);
    };
    if version < MIN_DATA_VERSION {
        return Err(DataFixError::TooOld(version));
    }
    Ok((version < DATA_VERSION).then(|| Fixer::new(version)))
}

/// A region chunk's root compound, for a dimension whose lowest section is
/// `min_section` (-4 for the overworld) and which is, or is not, the
/// overworld itself. Only chunks older than 1.18 depend on the section
/// bound, to place their sections and biomes.
pub fn chunk_in(
    root: &mut Compound,
    min_section: i32,
    overworld: bool,
) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    if fixer.from() < 2860 {
        legacy::upgrade(root, fixer, min_section);
    }
    chunk::upgrade(root, fixer, overworld);
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

/// An entity chunk's root compound (`{DataVersion, Position, Entities}`).
pub fn entity_chunk(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    for_each_compound(root, "Entities", |entity| entity::upgrade(entity, fixer));
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

/// A structure template's root compound: its block state palettes, the
/// block entities of its blocks and its entities.
pub fn structure(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    if let Some(palette) = root.get_list_mut("palette") {
        for state in palette {
            chunk::fix_block_state(state, fixer);
        }
    }
    if let Some(palettes) = root.get_list_mut("palettes") {
        for palette in palettes {
            if let Tag::List(palette) = palette {
                for state in palette {
                    chunk::fix_block_state(state, fixer);
                }
            }
        }
    }
    if let Some(inner) = fixer.nested() {
        for_each_compound(root, "blocks", |block| {
            if let Some(nbt) = block.get_compound_mut("nbt") {
                block_entity::upgrade(nbt, inner);
            }
        });
        for_each_compound(root, "entities", |entity| {
            if let Some(nbt) = entity.get_compound_mut("nbt") {
                entity::upgrade(nbt, inner);
            }
        });
    }
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

/// A player file's root compound.
pub fn player(root: &mut Compound) -> Result<(), DataFixError> {
    let Some(fixer) = fixer_for(root)? else {
        return Ok(());
    };
    entity::upgrade_player(root, fixer);
    root.insert("DataVersion", DATA_VERSION);
    Ok(())
}

/// Upgrades a document as read from a backend.
pub fn document(document: Document<'_>, root: &mut Compound) -> Result<(), DataFixError> {
    match document {
        Document::Level => level::level_dat(root),
        Document::SavedData(id) => level::saved_data(id, root),
        Document::DimensionData { id, .. } => level::saved_data(id, root),
        Document::Player(_) => player(root),
        Document::Stats(_) => level::stats(root),
        Document::Advancements(_) => level::advancements(root),
        Document::PlayerData { .. } => Ok(()),
    }
}

pub fn number(tag: &Tag) -> Option<f64> {
    Some(match *tag {
        Tag::Byte(v) => v.into(),
        Tag::Short(v) => v.into(),
        Tag::Int(v) => v.into(),
        Tag::Long(v) => v as f64,
        Tag::Float(v) => v.into(),
        Tag::Double(v) => v,
        _ => return None,
    })
}

pub fn get_number(compound: &Compound, key: &str) -> Option<f64> {
    compound.get(key).and_then(number)
}

pub fn get_i32(compound: &Compound, key: &str) -> Option<i32> {
    get_number(compound, key).map(|v| v as i32)
}

pub fn get_bool(compound: &Compound, key: &str) -> Option<bool> {
    get_number(compound, key).map(|v| v != 0.0)
}

pub(crate) fn namespaced(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

const GAME_EVENT_RENAMES_3084: &[(&str, &str)] = &[
    ("minecraft:block_press", "minecraft:block_activate"),
    ("minecraft:block_switch", "minecraft:block_activate"),
    ("minecraft:block_unpress", "minecraft:block_deactivate"),
    ("minecraft:block_unswitch", "minecraft:block_deactivate"),
    ("minecraft:drinking_finish", "minecraft:drink"),
    ("minecraft:elytra_free_fall", "minecraft:elytra_glide"),
    ("minecraft:entity_damaged", "minecraft:entity_damage"),
    ("minecraft:entity_dying", "minecraft:entity_die"),
    ("minecraft:entity_killed", "minecraft:entity_die"),
    ("minecraft:mob_interact", "minecraft:entity_interact"),
    ("minecraft:ravager_roar", "minecraft:entity_roar"),
    ("minecraft:ring_bell", "minecraft:block_change"),
    ("minecraft:shulker_close", "minecraft:container_close"),
    ("minecraft:shulker_open", "minecraft:container_open"),
    ("minecraft:wolf_shaking", "minecraft:entity_shake"),
];

pub(crate) fn rename_vibration_game_event(holder: &mut Compound, fixer: Fixer) {
    if !fixer.due(3084, 0) {
        return;
    }
    let Some(event) = holder
        .get_compound_mut("listener")
        .and_then(|listener| listener.get_compound_mut("event"))
    else {
        return;
    };
    let Some(name) = event.get_str("game_event") else {
        return;
    };
    let name = namespaced(name);
    if let Some((_, new)) = GAME_EVENT_RENAMES_3084.iter().find(|(old, _)| *old == name) {
        event.insert("game_event", *new);
    }
}

pub(crate) fn id_of(compound: &Compound) -> String {
    namespaced(compound.get_str("id").unwrap_or(""))
}

pub(crate) fn rename(compound: &mut Compound, from: &str, to: &str) {
    if let Some(value) = compound.remove(from) {
        compound.insert(to, value);
    }
}

pub(crate) fn rename_with(
    compound: &mut Compound,
    from: &str,
    to: &str,
    fix: impl FnOnce(Tag) -> Option<Tag>,
) {
    if let Some(value) = compound.remove(from)
        && let Some(value) = fix(value)
    {
        compound.insert(to, value);
    }
}

pub(crate) fn for_each_compound(
    compound: &mut Compound,
    key: &str,
    mut fix: impl FnMut(&mut Compound),
) {
    if let Some(list) = compound.get_list_mut(key) {
        for tag in list {
            if let Some(element) = tag.as_compound_mut() {
                fix(element);
            }
        }
    }
}

/// Renames an id through `table` (`(old, new)` pairs), comparing namespaced
/// forms as vanilla's renamers do.
pub(crate) fn renamed(
    id: &str,
    table: &'static [(&'static str, &'static str)],
) -> Option<&'static str> {
    let id = namespaced(id);
    table
        .iter()
        .find(|(old, _)| *old == id)
        .map(|(_, new)| *new)
}

/// Renames the string at `key` through `table`.
pub(crate) fn rename_id(
    compound: &mut Compound,
    key: &str,
    table: &'static [(&'static str, &'static str)],
) {
    if let Some(Tag::String(value)) = compound.get_mut(key)
        && let Some(new) = renamed(value, table)
    {
        *value = new.to_owned();
    }
}

pub(crate) fn dye_color_name(id: i32) -> &'static str {
    const COLORS: [&str; 16] = [
        "white",
        "orange",
        "magenta",
        "light_blue",
        "yellow",
        "lime",
        "pink",
        "gray",
        "light_gray",
        "cyan",
        "purple",
        "blue",
        "brown",
        "green",
        "red",
        "black",
    ];
    usize::try_from(id)
        .ok()
        .and_then(|i| COLORS.get(i))
        .copied()
        .unwrap_or("white")
}

/// `{x, y, z}` or `[x, y, z]` int lists as an int array, the way 1.20.5 made
/// every block position.
pub(crate) fn fix_block_pos(tag: Tag) -> Tag {
    match &tag {
        Tag::Compound(pos) => match (get_i32(pos, "X"), get_i32(pos, "Y"), get_i32(pos, "Z")) {
            (Some(x), Some(y), Some(z)) => Tag::IntArray(vec![x, y, z]),
            _ => tag,
        },
        _ => tag,
    }
}

/// Moves three int fields into one int-array block position.
pub(crate) fn inline_block_pos(compound: &mut Compound, x: &str, y: &str, z: &str, to: &str) {
    if let (Some(px), Some(py), Some(pz)) = (
        get_i32(compound, x),
        get_i32(compound, y),
        get_i32(compound, z),
    ) {
        compound.remove(x);
        compound.remove(y);
        compound.remove(z);
        compound.insert(to, vec![px, py, pz]);
    }
}

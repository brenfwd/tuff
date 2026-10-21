use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::version::DATA_VERSION;

/// Which whole-record document is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Document<'a> {
    /// The world's top-level record (vanilla's `level.dat`).
    Level,
    /// World-wide saved data by resource location, e.g.
    /// `minecraft:world_clocks`.
    SavedData(&'a str),
    /// Saved data scoped to one dimension, e.g. its world border.
    DimensionData { dimension: &'a str, id: &'a str },
    /// One player's record.
    Player(u128),
    /// One player's statistics: `{stats: {type: {entry: count}}, DataVersion}`
    /// (vanilla keeps them as JSON).
    Stats(u128),
    /// One player's advancement progress: `{advancement: {criteria:
    /// {name: date}, done}, DataVersion}` (vanilla keeps it as JSON, with
    /// `done` a boolean).
    Advancements(u128),
    /// One plugin's per-player record, `id` being the plugin's own name.
    PlayerData { id: &'a str, uuid: u128 },
}

/// An owned [`Document`] identifier, for listing what a backend holds
/// without borrowing from it. Kept in lock-step with `Document` by hand: a
/// new `Document` variant must be added here too, and to every `match` over
/// this type that has no catch-all arm (`DocumentId::borrow`, each backend's
/// `Documents::list`, and the converter's copy loop), or the build fails.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DocumentId {
    Level,
    SavedData(String),
    DimensionData { dimension: String, id: String },
    Player(u128),
    Stats(u128),
    Advancements(u128),
    PlayerData { id: String, uuid: u128 },
}

impl DocumentId {
    pub fn borrow(&self) -> Document<'_> {
        match self {
            Self::Level => Document::Level,
            Self::SavedData(id) => Document::SavedData(id),
            Self::DimensionData { dimension, id } => Document::DimensionData { dimension, id },
            Self::Player(uuid) => Document::Player(*uuid),
            Self::Stats(uuid) => Document::Stats(*uuid),
            Self::Advancements(uuid) => Document::Advancements(*uuid),
            Self::PlayerData { id, uuid } => Document::PlayerData { id, uuid: *uuid },
        }
    }
}

impl From<Document<'_>> for DocumentId {
    fn from(document: Document<'_>) -> Self {
        match document {
            Document::Level => Self::Level,
            Document::SavedData(id) => Self::SavedData(id.to_owned()),
            Document::DimensionData { dimension, id } => Self::DimensionData {
                dimension: dimension.to_owned(),
                id: id.to_owned(),
            },
            Document::Player(uuid) => Self::Player(uuid),
            Document::Stats(uuid) => Self::Stats(uuid),
            Document::Advancements(uuid) => Self::Advancements(uuid),
            Document::PlayerData { id, uuid } => Self::PlayerData {
                id: id.to_owned(),
                uuid,
            },
        }
    }
}

pub fn saved(data: Compound) -> Compound {
    Compound::new()
        .with("data", data)
        .with("DataVersion", DATA_VERSION)
}

pub fn inner(root: Option<Compound>) -> Compound {
    root.and_then(|mut root| match root.remove("data") {
        Some(Tag::Compound(data)) => Some(data),
        _ => None,
    })
    .unwrap_or_default()
}

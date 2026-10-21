//! Structures from the built-in data pack: structure sets (where structures
//! may start), structures (what starts there), template pools and processor
//! lists, and the structure templates themselves.
//!
//! Structure sets and the settings all structures share are typed. The
//! fields of each structure kind (`Structure::config`), template pools and
//! processor lists are JSON text that the world generator parses; the build
//! checks every kind, field and reference in them.

/// A `worldgen/structure_set` entry.
#[derive(Debug)]
pub struct StructureSet {
    pub name: &'static str,
    /// `(structure, weight)`.
    pub structures: &'static [(&'static str, i32)],
    pub placement: StructurePlacement,
}

#[derive(Debug)]
pub enum StructurePlacement {
    RandomSpread {
        spread: Spread,
        spacing: i32,
        separation: i32,
        spread_type: SpreadType,
    },
    ConcentricRings {
        spread: Spread,
        distance: i32,
        ring_spread: i32,
        count: i32,
        preferred_biomes: &'static [&'static str],
        /// The biome tag `preferred_biomes` came from, if it named one.
        preferred_tag: Option<&'static str>,
    },
    /// Starts only in the dimension's origin chunk.
    DimensionOrigin { spread: Spread },
}

impl StructurePlacement {
    pub fn spread(&self) -> &Spread {
        match self {
            Self::RandomSpread { spread, .. }
            | Self::ConcentricRings { spread, .. }
            | Self::DimensionOrigin { spread } => spread,
        }
    }
}

/// The settings both spreading placements share.
#[derive(Debug)]
pub struct Spread {
    pub locate_offset: [i32; 3],
    pub frequency_reduction: FrequencyReduction,
    pub frequency: f32,
    pub salt: i32,
    pub exclusion_zone: Option<ExclusionZone>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrequencyReduction {
    Default,
    LegacyType1,
    LegacyType2,
    LegacyType3,
}

#[derive(Debug)]
pub struct ExclusionZone {
    pub other_set: &'static str,
    pub chunk_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadType {
    Linear,
    Triangular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainAdaptation {
    None,
    BeardThin,
    BeardBox,
    Bury,
    Encapsulate,
}

/// A `worldgen/structure` entry.
#[derive(Debug)]
pub struct Structure {
    pub name: &'static str,
    /// The structure type, e.g. `minecraft:ruined_portal`.
    pub kind: &'static str,
    /// Where the structure may start, tags expanded.
    pub biomes: &'static [&'static str],
    /// The biome tag `biomes` came from, if it named one.
    pub biome_tag: Option<&'static str>,
    /// Index into [`crate::world::DECORATION_STEPS`].
    pub step: usize,
    pub terrain_adaptation: TerrainAdaptation,
    /// JSON: mob category to `{bounding_box, spawns}`.
    pub spawn_overrides: &'static str,
    /// JSON object of the kind's own fields.
    pub config: &'static str,
}

impl Structure {
    pub fn get(name: &str) -> Option<&'static Structure> {
        crate::lookup(STRUCTURES, name)
    }
}

impl StructureSet {
    pub fn get(name: &str) -> Option<&'static StructureSet> {
        crate::lookup(STRUCTURE_SETS, name)
    }
}

/// A template pool as JSON.
pub fn template_pool(name: &str) -> Option<&'static str> {
    crate::lookup(TEMPLATE_POOLS, name)
}

/// A processor list as JSON.
pub fn processor_list(name: &str) -> Option<&'static str> {
    crate::lookup(PROCESSOR_LISTS, name)
}

/// The gzip-compressed NBT of a structure template, e.g.
/// `minecraft:ruined_portal/portal_1`.
pub fn template(name: &str) -> Option<&'static [u8]> {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    let index = TEMPLATES
        .binary_search_by(|(key, _, _)| {
            let wanted = namespace.bytes().chain(*b":").chain(path.bytes());
            key.bytes().cmp(wanted)
        })
        .ok()?;
    let (_, offset, len) = TEMPLATES[index];
    Some(&TEMPLATE_BLOB[offset..offset + len])
}

/// Every template name, sorted.
pub fn template_names() -> impl ExactSizeIterator<Item = &'static str> {
    TEMPLATES.iter().map(|(name, _, _)| *name)
}

include!(concat!(env!("OUT_DIR"), "/structures.rs"));

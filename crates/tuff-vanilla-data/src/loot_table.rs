//! Every loot table in the built-in data pack.
//!
//! [`LootCondition`] and [`LootFunction`] parameters, and the rare
//! [`LootEntryKind::Unknown`] entry, are kept as raw JSON text: their
//! payload can reference the separate item/block/entity "predicate"
//! mini-language, which this crate does not model further. A `condition`
//! that names a stored predicate file rather than a registered condition
//! kind is resolved and inlined at build time, so nothing is lost.

use crate::Frozen;
use crate::Frozen::Static;
use crate::registry::Item;

#[derive(Clone, Debug)]
pub struct LootTable {
    /// `None` for an inline table that leaves its context type unspecified.
    pub kind: Option<LootTableKind>,
    pub random_sequence: Option<Frozen<str>>,
    /// Functions applied to every item this table produces, after its pools.
    pub functions: Frozen<[LootFunction]>,
    pub pools: Frozen<[LootPool]>,
}

impl LootTable {
    pub fn get(name: &str) -> Option<&'static LootTable> {
        crate::lookup(ALL, name)
    }
}

#[derive(Clone, Debug)]
pub struct LootPool {
    pub rolls: NumberProvider,
    pub bonus_rolls: NumberProvider,
    pub condition: Option<LootCondition>,
    pub functions: Frozen<[LootFunction]>,
    pub entries: Frozen<[LootEntry]>,
}

#[derive(Clone, Debug)]
pub struct LootEntry {
    pub kind: LootEntryKind,
    pub weight: u64,
    pub quality: i64,
    pub condition: Option<LootCondition>,
    pub functions: Frozen<[LootFunction]>,
}

#[derive(Clone, Debug)]
pub enum LootEntryKind {
    Item(Item),
    Tag {
        /// The item tag; `items` are its members when the table loaded.
        tag: Frozen<str>,
        items: Frozen<[Item]>,
        /// Roll each matching item as its own entry instead of picking one.
        expand: bool,
    },
    /// References another loot table by name; see [`LootTable::get`].
    LootTable(Frozen<str>),
    /// A loot table defined in place rather than referenced by name.
    Inline(Frozen<LootTable>),
    /// Resolved by a block entity's own logic (e.g. suspicious sand/gravel),
    /// keyed by this fixed name.
    Dynamic(Frozen<str>),
    Empty,
    Group(Frozen<[LootEntry]>),
    /// Rolls entries in order, using the first whose conditions pass.
    Alternatives(Frozen<[LootEntry]>),
    /// Rolls entries in order, stopping at the first whose conditions fail.
    Sequence(Frozen<[LootEntry]>),
    /// An entry kind this crate does not parse further (`minecraft:slots`,
    /// or one a datapack registered), kept as raw JSON.
    Unknown {
        kind: Frozen<str>,
        raw: Frozen<str>,
    },
}

#[derive(Clone, Debug)]
pub struct LootCondition {
    pub kind: Frozen<str>,
    pub params: Option<Frozen<str>>,
}

#[derive(Clone, Debug)]
pub struct LootFunction {
    pub kind: Frozen<str>,
    pub params: Option<Frozen<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NumberProvider {
    Constant(f64),
    /// Uniformly distributed between `min` and `max`, inclusive.
    Uniform {
        min: f64,
        max: f64,
    },
    /// The number of successes in `n` trials that each succeed with
    /// probability `p`.
    Binomial {
        n: f64,
        p: f64,
    },
    /// Any other provider kind (`score`, `storage`, ...) or one whose bounds
    /// are themselves nested providers, kept as raw JSON.
    Other {
        kind: Frozen<str>,
        params: Frozen<str>,
    },
}

impl NumberProvider {
    /// The provider's expected value, for callers that just need a rough
    /// count and do not want to depend on a random source. Panics on
    /// [`NumberProvider::Other`]; match on it directly if you need to handle
    /// every provider kind, e.g. by evaluating it against a random source.
    pub fn mean(&self) -> f64 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => (min + max) / 2.0,
            Self::Binomial { n, p } => n * p,
            Self::Other { ref kind, .. } => {
                panic!("no fallback mean for a {kind} number provider")
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/loot_tables.rs"));

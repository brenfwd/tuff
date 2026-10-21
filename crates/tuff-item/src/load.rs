//! Feature load phases and the errors registration reports.

use std::fmt;

/// The phases a server boot runs through, in order. Loading is phased
/// rather than dependency-solved: it lets `vanilla-mobs` reference an item
/// `vanilla-items` registered, or a loot table `vanilla-recipes`
/// registered, without either feature declaring a dependency on the
/// other -- only an implicit "types exist before content, content
/// exists before it is cross-referenced" ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Register registry *kinds* (calling `Registries::ext` or a
    /// core-owned registry's setup) and anything another feature's
    /// `Content` phase needs to already exist. No entries yet -- a
    /// feature that only ever adds entries to registries core already
    /// defines (the common case) has nothing to do here.
    Types,
    /// Register entries: species, commands, biomes, loot tables,
    /// growth behaviors, generators, session hooks. Most feature code
    /// runs here.
    Content,
    /// Look up other features' `Content`-phase entries (readable now,
    /// though the registry they live in is not yet frozen) and wire
    /// cross-references a feature could not resolve while everyone was
    /// still registering: a loot table naming an item predicate, a mob
    /// naming its loot table, a recipe naming an item id.
    Resolve,
    /// Core-only. No `Feature::load` is ever called with this phase;
    /// `load_all` runs it itself after every feature's `Resolve` call
    /// returns, by calling `Registries::freeze_all`. Listed here so
    /// [`Phase::ALL`] documents the full sequence in one place.
    Freeze,
}

impl Phase {
    pub const ALL: [Phase; 4] = [Phase::Types, Phase::Content, Phase::Resolve, Phase::Freeze];
}

/// Every registration/lookup failure is attributed to the feature and
/// key that caused it, so a startup failure names exactly what to fix
/// rather than panicking deep in a codegen'd lookup.
#[derive(Debug)]
pub struct LoadError {
    pub feature: &'static str,
    pub kind: LoadErrorKind,
}

#[derive(Debug)]
pub enum LoadErrorKind {
    /// A `register` call used an already-taken key. The fix is either
    /// to pick a different key, or -- if overriding vanilla content is
    /// genuinely intended -- to call `replace` instead of `register`.
    Conflict {
        registry: &'static str,
        key: String,
        first_registered_by: &'static str,
    },
    /// A `Resolve`-phase lookup named a key nothing ever registered.
    DanglingReference {
        registry: &'static str,
        key: String,
        /// Free-form description of where the reference came from
        /// (e.g. `"loot table minecraft:blocks/stone, pool 0, entry 2"`),
        /// since only the caller knows how to describe its own shape.
        referenced_from: String,
    },
    /// A registration was attempted from the wrong phase (e.g. a
    /// `Types`-only registry touched during `Content`).
    WrongPhase { expected: Phase, actual: Phase },
    /// Anything else a feature wants to report -- malformed embedded
    /// data, a session-hook slot double-booked, and so on.
    Other(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] ", self.feature)?;
        match &self.kind {
            LoadErrorKind::Conflict {
                registry,
                key,
                first_registered_by,
            } => write!(
                f,
                "{registry} entry {key:?} is already registered by {first_registered_by} \
                 (use `replace` if overriding it is intended)"
            ),
            LoadErrorKind::DanglingReference {
                registry,
                key,
                referenced_from,
            } => write!(
                f,
                "{referenced_from} references unknown {registry} entry {key:?}"
            ),
            LoadErrorKind::WrongPhase { expected, actual } => {
                write!(
                    f,
                    "registration only valid during {expected:?}, called during {actual:?}"
                )
            }
            LoadErrorKind::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

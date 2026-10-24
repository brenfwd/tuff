//! The doors through which a block leaves its place in the world.
//!
//! A block standing in a chunk belongs to its position. Breaking it
//! ([`BrokenBlock`]) and a piston lifting it ([`LiftedBlock`]) are the only
//! ways to take it out, each at most once per block: the position is
//! vacated until something is written there, and a vacated position yields
//! nothing. Block loot is rolled from a [`BrokenBlock`] taken by value, so
//! a block's drops come at most once, and a lifted block lands, breaks or
//! is burned exactly once. Neither token can be copied or built outside
//! this crate.
//!
//! ```compile_fail
//! use tuff_core::world::block_ledger::BrokenBlock;
//! fn copy(block: &BrokenBlock) -> BrokenBlock {
//!     block.clone()
//! }
//! ```
//!
//! ```compile_fail
//! use tuff_core::world::block_ledger::LiftedBlock;
//! fn copy(block: &LiftedBlock) -> LiftedBlock {
//!     block.clone()
//! }
//! ```
//!
//! ```compile_fail
//! let _ = tuff_core::world::block_ledger::BrokenBlock {
//!     state: tuff_vanilla_data::Block::Stone.default_state(),
//!     block_entity: None,
//!     trail: Default::default(),
//! };
//! ```
//!
//! In debug builds each token carries an id and the operations applied to
//! it, and a lifted block dropped without landing, breaking or being burned
//! panics with that history. Release builds log it and count it as lost.

use std::fmt;
#[cfg(debug_assertions)]
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};

use tuff_nbt::Compound;
use tuff_vanilla_data::BlockState;

/// Why a block breaks out of the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimReason {
    /// A player or fake player mined it.
    Mined,
    /// An explosion destroyed it.
    Exploded,
    /// A fluid washed it away.
    Washed,
    /// It could no longer stay where it is (support gone, decay, growth).
    Popped,
    /// A piston destroyed it by pushing into it.
    Crushed,
    /// It turned into another block, dropping what it held.
    Converted,
    /// A command or plugin broke it.
    Command,
    /// It became an entity (primed TNT, a falling block, a carried block).
    Entity,
}

/// Why a lifted block leaves existence without landing or breaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockBurn {
    /// Something else was written over its moving piston.
    Overwritten,
    /// The region it was sent to was gone.
    Orphaned,
}

/// What a broken block became instead of drops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityReason {
    PrimedTnt,
    Falling,
    Carried,
}

/// Totals of the block doors since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockLedgerTotals {
    pub lifted: u64,
    pub landed: u64,
    pub broken: u64,
    pub burned: u64,
    pub loaded: u64,
    pub stored: u64,
    pub lost: u64,
}

impl BlockLedgerTotals {
    pub fn since(self, before: Self) -> Self {
        Self {
            lifted: self.lifted - before.lifted,
            landed: self.landed - before.landed,
            broken: self.broken - before.broken,
            burned: self.burned - before.burned,
            loaded: self.loaded - before.loaded,
            stored: self.stored - before.stored,
            lost: self.lost - before.lost,
        }
    }
}

static LIFTED: AtomicU64 = AtomicU64::new(0);
static LANDED: AtomicU64 = AtomicU64::new(0);
static BROKEN: AtomicU64 = AtomicU64::new(0);
static BURNED: AtomicU64 = AtomicU64::new(0);
static LOADED: AtomicU64 = AtomicU64::new(0);
static STORED: AtomicU64 = AtomicU64::new(0);
static LOST: AtomicU64 = AtomicU64::new(0);

pub fn totals() -> BlockLedgerTotals {
    BlockLedgerTotals {
        lifted: LIFTED.load(Ordering::Relaxed),
        landed: LANDED.load(Ordering::Relaxed),
        broken: BROKEN.load(Ordering::Relaxed),
        burned: BURNED.load(Ordering::Relaxed),
        loaded: LOADED.load(Ordering::Relaxed),
        stored: STORED.load(Ordering::Relaxed),
        lost: LOST.load(Ordering::Relaxed),
    }
}

fn count(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// A token's id and the operations applied to it, in debug builds; empty
/// in release builds.
#[derive(Default)]
pub struct Trail {
    #[cfg(debug_assertions)]
    id: u64,
    #[cfg(debug_assertions)]
    entries: Vec<(String, &'static Location<'static>)>,
}

impl Trail {
    #[track_caller]
    fn begin(what: fmt::Arguments<'_>) -> Self {
        #[cfg_attr(not(debug_assertions), allow(unused_mut))]
        let mut trail = Self::default();
        #[cfg(debug_assertions)]
        {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            trail.id = NEXT.fetch_add(1, Ordering::Relaxed);
        }
        trail.note(what);
        trail
    }

    #[track_caller]
    fn note(&mut self, what: fmt::Arguments<'_>) {
        #[cfg(debug_assertions)]
        self.entries.push((what.to_string(), Location::caller()));
        #[cfg(not(debug_assertions))]
        let _ = what;
    }

    fn report(&self) -> String {
        #[cfg_attr(not(debug_assertions), allow(unused_mut))]
        let mut out = String::from("block");
        #[cfg(debug_assertions)]
        {
            use fmt::Write;
            let _ = write!(out, " #{}", self.id);
            for (i, (what, at)) in self.entries.iter().enumerate() {
                let _ = write!(out, "\n  {}. {what} at {}:{}", i + 1, at.file(), at.line());
            }
        }
        out
    }
}

impl fmt::Debug for Trail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(debug_assertions)]
        {
            write!(f, "#{}", self.id)
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = f;
            Ok(())
        }
    }
}

/// A block taken out of the world to break: what rolling its loot or
/// turning it into an entity consumes. Dropping one unused is fine (no
/// drops); it can never be used twice.
#[must_use = "a broken block is rolled into its drops, turned into an entity, or dropped for none"]
#[derive(Debug)]
pub struct BrokenBlock {
    state: BlockState,
    block_entity: Option<Compound>,
    trail: Trail,
}

impl BrokenBlock {
    #[track_caller]
    pub(crate) fn claimed(
        state: BlockState,
        block_entity: Option<Compound>,
        why: ClaimReason,
    ) -> Self {
        count(&BROKEN);
        Self {
            state,
            block_entity,
            trail: Trail::begin(format_args!("claimed {state:?}: {why:?}")),
        }
    }

    /// A block an entity held since it claimed it (see
    /// [`BrokenBlock::into_entity`]), broken as the entity lets go of it.
    #[track_caller]
    pub fn released(state: BlockState, why: EntityReason) -> Self {
        count(&BROKEN);
        Self {
            state,
            block_entity: None,
            trail: Trail::begin(format_args!("released {state:?}: {why:?}")),
        }
    }

    /// A block that is not in any world, for loot simulations (`/loot ...
    /// mine`).
    #[track_caller]
    pub fn simulated(state: BlockState, block_entity: Option<Compound>) -> Self {
        Self {
            state,
            block_entity,
            trail: Trail::begin(format_args!("simulated {state:?}")),
        }
    }

    /// A block for tests.
    #[doc(hidden)]
    #[track_caller]
    pub fn test(state: BlockState, block_entity: Option<Compound>) -> Self {
        Self::simulated(state, block_entity)
    }

    pub fn state(&self) -> BlockState {
        self.state
    }

    pub fn block_entity(&self) -> Option<&Compound> {
        self.block_entity.as_ref()
    }

    /// Takes the block entity data out, for a caller that spills it.
    pub fn take_block_entity(&mut self) -> Option<Compound> {
        self.block_entity.take()
    }

    /// The block becomes an entity instead of drops.
    #[track_caller]
    pub fn into_entity(mut self, why: EntityReason) -> BlockState {
        self.trail.note(format_args!("became an entity: {why:?}"));
        self.state
    }

    pub fn report(&self) -> String {
        self.trail.report()
    }
}

/// A block a piston lifted out of its place: it lands, breaks or is burned
/// exactly once.
#[must_use = "a lifted block must land, break or be burned"]
pub struct LiftedBlock(Option<Lifted>);

struct Lifted {
    state: BlockState,
    trail: Trail,
}

impl fmt::Debug for LiftedBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(lifted) => write!(f, "LiftedBlock({:?} {:?})", lifted.state, lifted.trail),
            None => f.write_str("LiftedBlock(spent)"),
        }
    }
}

impl LiftedBlock {
    #[track_caller]
    pub(crate) fn lifted(state: BlockState) -> Self {
        count(&LIFTED);
        Self(Some(Lifted {
            state,
            trail: Trail::begin(format_args!("lifted {state:?}")),
        }))
    }

    /// A lifted block a saved moving piston held, loaded with its chunk.
    #[track_caller]
    pub(crate) fn from_storage(state: BlockState) -> Self {
        count(&LOADED);
        Self(Some(Lifted {
            state,
            trail: Trail::begin(format_args!("loaded {state:?}")),
        }))
    }

    /// A lifted block for tests.
    #[doc(hidden)]
    #[track_caller]
    pub fn test(state: BlockState) -> Self {
        Self(Some(Lifted {
            state,
            trail: Trail::begin(format_args!("test {state:?}")),
        }))
    }

    fn take(&mut self) -> Lifted {
        self.0
            .take()
            .expect("a lifted block used after it was spent")
    }

    pub fn state(&self) -> BlockState {
        self.0
            .as_ref()
            .map_or(tuff_vanilla_data::Block::Air.default_state(), |l| l.state)
    }

    #[track_caller]
    pub fn note(&mut self, what: fmt::Arguments<'_>) {
        if let Some(lifted) = &mut self.0 {
            lifted.trail.note(what);
        }
    }

    /// The block lands and stands in the world again.
    #[track_caller]
    pub fn land(mut self) -> BlockState {
        count(&LANDED);
        self.take().state
    }

    /// The block breaks where it landed, or while it moved.
    #[track_caller]
    pub fn into_broken(mut self, block_entity: Option<Compound>) -> BrokenBlock {
        count(&BROKEN);
        let Lifted { state, mut trail } = self.take();
        trail.note(format_args!("broke"));
        BrokenBlock {
            state,
            block_entity,
            trail,
        }
    }

    #[track_caller]
    pub fn burn(mut self, why: BlockBurn) {
        count(&BURNED);
        let mut lifted = self.take();
        lifted.trail.note(format_args!("burned: {why:?}"));
    }

    /// The block stays saved in its chunk's moving piston and leaves
    /// memory with it.
    pub fn to_storage(mut self) {
        count(&STORED);
        self.take();
    }
}

impl Drop for LiftedBlock {
    fn drop(&mut self) {
        let Some(lifted) = self.0.take() else {
            return;
        };
        if tuff_item::item::conservation::is_releasing() {
            count(&STORED);
            return;
        }
        count(&LOST);
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!(
                "lifted {:?} dropped without landing, breaking or being burned\n{}",
                lifted.state,
                lifted.trail.report()
            );
        }
        log::error!(
            "block conservation: lifted {:?} dropped without landing\n{}",
            lifted.state,
            lifted.trail.report()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn rail() -> BlockState {
        Block::Rail.default_state()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lifted_block_lands_once() {
        let before = totals();
        assert_eq!(LiftedBlock::lifted(rail()).land(), rail());
        let after = totals().since(before);
        assert!(after.lifted >= 1 && after.landed >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lifted_block_breaks_with_its_state() {
        let broken = LiftedBlock::lifted(rail()).into_broken(None);
        assert_eq!(broken.state(), rail());
    }

    #[tuff_core::watchdog]
    #[test]
    fn burning_and_storing_spend_a_lifted_block() {
        LiftedBlock::lifted(rail()).burn(BlockBurn::Overwritten);
        LiftedBlock::from_storage(rail()).to_storage();
    }

    #[tuff_core::watchdog]
    #[test]
    fn dropping_a_lifted_block_loses_it() {
        let before = totals();
        let dropped = std::thread::spawn(|| drop(LiftedBlock::lifted(rail()))).join();
        assert_eq!(dropped.is_err(), cfg!(debug_assertions));
        assert!(totals().since(before).lost >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lifted_block_dropped_while_releasing_is_stored() {
        tuff_item::item::conservation::releasing(|| drop(LiftedBlock::lifted(rail())));
    }
}

//! The doors through which a block leaves its place in the world.
//!
//! A block standing in a chunk belongs to its position. Breaking it
//! ([`BrokenBlock`]) and holding it out of the world ([`HeldBlock`], for a
//! moving piston or an entity) are the only ways to take it out, each at
//! most once per block. A claim writes what remains in its place itself, so
//! holding a token always means the block is gone; only a piston's lift
//! leaves an inert ghost behind, until something is written there. Block
//! loot is rolled from a [`BrokenBlock`] taken by value, so a block's drops
//! come at most once, and a held block lands, breaks or is burned exactly
//! once. Neither token can be copied or built outside this crate.
//!
//! ```compile_fail
//! use tuff_core::world::block_ledger::BrokenBlock;
//! fn copy(block: &BrokenBlock) -> BrokenBlock {
//!     block.clone()
//! }
//! ```
//!
//! ```compile_fail
//! use tuff_core::world::block_ledger::HeldBlock;
//! fn copy(block: &HeldBlock) -> HeldBlock {
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
//! it, and a held block dropped without landing, breaking or being burned
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

/// What a claim leaves in the place of the block it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remains {
    /// Air.
    Air,
    /// The fluid the block held, if any, else air.
    Vanilla,
    /// A block the claimer puts there instead.
    State(BlockState),
    /// A fluid block.
    Fluid(BlockState),
}

/// Why a held block leaves existence without landing or breaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockBurn {
    /// Something else was written over its moving piston.
    Overwritten,
    /// The region it was sent to was gone.
    Orphaned,
    /// The entity holding it was killed or removed.
    Killed,
    /// It was thrown away on purpose.
    Discarded,
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

    /// The block drops itself as its own item, if it has one: what a block
    /// does that falls with nowhere to land.
    #[track_caller]
    pub fn into_item(mut self) -> Option<tuff_item::item::ItemStack> {
        self.trail.note(format_args!("dropped as its item"));
        let item = self.state.block().item()?;
        Some(tuff_item::item::ItemStack::mint(
            item,
            1,
            tuff_item::item::MintReason::Loot,
        ))
    }

    pub fn report(&self) -> String {
        self.trail.report()
    }
}

/// A block held out of its place by a piston or an entity: it lands, breaks or is burned
/// exactly once.
#[must_use = "a held block must land, break or be burned"]
pub struct HeldBlock(Option<Held>);

struct Held {
    state: BlockState,
    trail: Trail,
}

impl fmt::Debug for HeldBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(lifted) => write!(f, "HeldBlock({:?} {:?})", lifted.state, lifted.trail),
            None => f.write_str("HeldBlock(spent)"),
        }
    }
}

impl HeldBlock {
    #[track_caller]
    pub(crate) fn lifted(state: BlockState) -> Self {
        count(&LIFTED);
        Self(Some(Held {
            state,
            trail: Trail::begin(format_args!("lifted {state:?}")),
        }))
    }

    /// A block taken up by an entity (see `RegionActor::take_up`).
    #[track_caller]
    pub(crate) fn taken(state: BlockState, why: ClaimReason) -> Self {
        count(&LIFTED);
        Self(Some(Held {
            state,
            trail: Trail::begin(format_args!("taken up {state:?}: {why:?}")),
        }))
    }

    /// A held block a saved moving piston held, loaded with its chunk.
    #[track_caller]
    pub(crate) fn from_storage(state: BlockState) -> Self {
        count(&LOADED);
        Self(Some(Held {
            state,
            trail: Trail::begin(format_args!("loaded {state:?}")),
        }))
    }

    /// A lifted block for tests.
    #[doc(hidden)]
    #[track_caller]
    pub fn test(state: BlockState) -> Self {
        Self(Some(Held {
            state,
            trail: Trail::begin(format_args!("test {state:?}")),
        }))
    }

    fn take(&mut self) -> Held {
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
        let Held { state, mut trail } = self.take();
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

impl Drop for HeldBlock {
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
    fn a_held_block_lands_once() {
        let before = totals();
        assert_eq!(HeldBlock::lifted(rail()).land(), rail());
        let after = totals().since(before);
        assert!(after.lifted >= 1 && after.landed >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_block_taken_up_lands_once_or_is_burned_as_killed() {
        let before = totals();
        assert_eq!(HeldBlock::taken(rail(), ClaimReason::Entity).land(), rail());
        HeldBlock::taken(rail(), ClaimReason::Entity).burn(BlockBurn::Killed);
        HeldBlock::taken(rail(), ClaimReason::Entity).burn(BlockBurn::Discarded);
        let after = totals().since(before);
        assert!(after.lifted >= 3 && after.landed >= 1 && after.burned >= 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_held_block_breaks_with_its_state() {
        let broken = HeldBlock::lifted(rail()).into_broken(None);
        assert_eq!(broken.state(), rail());
    }

    #[tuff_core::watchdog]
    #[test]
    fn burning_and_storing_spend_a_held_block() {
        HeldBlock::lifted(rail()).burn(BlockBurn::Overwritten);
        HeldBlock::from_storage(rail()).to_storage();
    }

    #[tuff_core::watchdog]
    #[test]
    fn dropping_a_held_block_loses_it() {
        let before = totals();
        let dropped = std::thread::spawn(|| drop(HeldBlock::lifted(rail()))).join();
        assert_eq!(dropped.is_err(), cfg!(debug_assertions));
        assert!(totals().since(before).lost >= 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_held_block_dropped_while_releasing_is_stored() {
        tuff_item::item::conservation::releasing(|| drop(HeldBlock::lifted(rail())));
    }
}

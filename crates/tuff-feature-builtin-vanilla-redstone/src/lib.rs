//! Redstone: each block's reaction to the game's neighbour-update/signal
//! model, as rules over a [`RedstoneLevel`] the region implements.
//!
//! The update model itself (the `UPDATE_*` flags, the neighbour-changed
//! and shape-update cascade, [`RedstoneLevel`]/[`Reader`], and the
//! [`BlockBehavior`] dispatch seam) is core mechanism and lives in
//! `tuff_core::world::redstone`; it is re-exported here so this crate's
//! modules reach it through the crate root. What lives *here* is
//! the content: which block does what ([`RedstoneBehaviors`]), wire
//! propagation, diodes, and every consumer and source. [`RedstoneFeature`]
//! registers it into core at load time, so a server without this feature
//! has no redstone at all.
//!
//! Every behaviour implements core's [`BlockBehavior`] (the reactions
//! core's cascade calls) plus [`SignalBehavior`] (the signal queries only
//! this crate's own [`Signals`] and wire propagation ask). What each
//! state emits by itself, which states conduct and where comparators read
//! from is dumped from the game (`tuff_vanilla_data::redstone`).

use std::sync::{Arc, LazyLock};

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::redstone::Component;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::feature::{Feature, LoadContext};
use tuff_item::item::ItemStack;
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub use tuff_core::world::redstone::{
    BlockBehavior, BlockBehaviors, Effect, MAX_CHAINED_UPDATES, Reader, RedstoneLevel,
    SHAPE_UPDATE_ORDER, TickPriority, UPDATE_ALL, UPDATE_CLIENTS, UPDATE_INVISIBLE,
    UPDATE_KNOWN_SHAPE, UPDATE_LIMIT, UPDATE_MOVED_BY_PISTON, UPDATE_NEIGHBORS, UPDATE_NONE,
    UPDATE_ORDER, UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS, UPDATE_SKIP_ON_PLACE,
    UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE, UPDATE_SUPPRESS_DROPS, update_neighbour_for_output_signal,
};

mod bulb;
mod chest;
mod command_block;
mod comparator;
mod consumers;
mod crafter;
mod daylight;
mod diode;
mod dispenser;
mod dripleaf;
mod jukebox;
mod lectern;
pub mod piston;
mod rail;
mod rod;
mod sculk;
mod shelf;
mod skull;
mod sources;
mod test_block;
mod tripwire;
pub mod wire;

/// The directions in their protocol order, which signal scans follow.
pub const DIRECTIONS: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

/// Level operations only this crate's behaviours need, kept out of
/// core's [`RedstoneLevel`] because they lean on `vanilla-blocks`
/// placement rules core must not depend on.
pub trait RedstoneLevelExt: RedstoneLevel {
    /// Removes the block, leaving its fluid behind.
    fn remove_block(&mut self, pos: BlockPos) {
        let state = self.block(pos);
        let left = tuff_vanilla_common::placement::remains(state);
        self.set_block(pos, left, UPDATE_ALL);
    }
}

impl<T: RedstoneLevel + ?Sized> RedstoneLevelExt for T {}

/// The signal queries of a block, on top of the reactions core dispatches
/// through [`BlockBehavior`]. Only this crate asks them (through
/// [`Signals`] and wire propagation), so core never sees this trait.
pub trait SignalBehavior: BlockBehavior {
    /// The weak signal toward a reader looking in `direction`.
    fn signal(&self, _s: &Signals, _pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        state.static_signal(direction)
    }

    /// The strong signal toward a reader looking in `direction`.
    fn direct_signal(
        &self,
        _s: &Signals,
        _pos: BlockPos,
        state: BlockState,
        direction: Facing,
    ) -> u8 {
        state.static_direct_signal(direction)
    }

    /// Whether wire next to the block (in `direction` from the wire, or
    /// above or below it for `None`) connects to it.
    fn wire_connects(&self, state: BlockState, direction: Option<Facing>) -> bool {
        state.is_signal_source() && direction.is_some()
    }
}

struct Inert;

impl BlockBehavior for Inert {}

impl SignalBehavior for Inert {}

static INERT: Inert = Inert;

/// The redstone behaviour of each block, indexed by block.
pub struct RedstoneBehaviors {
    by_block: Vec<&'static dyn SignalBehavior>,
}

impl RedstoneBehaviors {
    /// The game's behaviours.
    pub fn vanilla() -> Self {
        let mut behaviors = Self {
            by_block: vec![&INERT as &dyn SignalBehavior; Block::ALL.len()],
        };
        for block in Block::ALL {
            if let Some(component) = block.component() {
                behaviors.register(block, component_behaviour(component));
            }
        }
        behaviors.register(Block::Piston, &piston::Base);
        behaviors.register(Block::StickyPiston, &piston::Base);
        behaviors.register(Block::PistonHead, &piston::Head);
        behaviors.register(Block::SlimeBlock, &piston::Slime);
        behaviors.register(Block::HoneyBlock, &piston::Honey);
        behaviors.register(Block::TrappedChest, &chest::TrappedChest);
        for &block in rod::BLOCKS {
            behaviors.register(block, &rod::LightningRod);
        }
        behaviors.register(Block::Rail, &rail::Rail);
        behaviors.register(Block::PoweredRail, &rail::PoweredRail);
        behaviors.register(Block::ActivatorRail, &rail::PoweredRail);
        behaviors.register(Block::DetectorRail, &rail::DetectorRail);
        behaviors.register(Block::TripwireHook, &tripwire::Hook);
        behaviors.register(Block::Tripwire, &tripwire::Wire);
        behaviors.register(Block::Lectern, &lectern::Lectern);
        behaviors.register(Block::Jukebox, &jukebox::Jukebox);
        behaviors.register(Block::Crafter, &crafter::Crafter);
        behaviors.register(Block::BigDripleaf, &dripleaf::BigDripleaf);
        for block in [
            Block::CommandBlock,
            Block::RepeatingCommandBlock,
            Block::ChainCommandBlock,
        ] {
            behaviors.register(block, &command_block::CommandBlock);
        }
        for block in sculk::BLOCKS {
            behaviors.register(block, &sculk::Sensor);
        }
        for block in Block::ALL {
            if skull::is_head(block) {
                behaviors.register(block, &skull::Skull);
            }
        }
        for block in bulb::BLOCKS {
            behaviors.register(block, &bulb::CopperBulb);
        }
        behaviors.register(Block::TestBlock, &test_block::TestBlock);
        behaviors.register(Block::StructureBlock, &test_block::StructureBlock);
        for block in shelf::blocks() {
            behaviors.register(*block, &shelf::Shelf);
        }
        behaviors
    }

    pub fn register(&mut self, block: Block, behaviour: &'static dyn SignalBehavior) {
        self.by_block[block as usize] = behaviour;
    }

    pub fn signal_behavior(&self, block: Block) -> &'static dyn SignalBehavior {
        self.by_block[block as usize]
    }
}

fn component_behaviour(component: Component) -> &'static dyn SignalBehavior {
    match component {
        Component::Wire => &wire::Wire,
        Component::Torch | Component::WallTorch => &sources::Torch,
        Component::Repeater => &diode::Repeater,
        Component::Comparator => &diode::Comparator,
        Component::Observer => &sources::Observer,
        Component::Lever => &sources::Lever,
        Component::Button => &sources::Button,
        Component::PressurePlate => &sources::PressurePlate,
        Component::PowerBlock => &INERT,
        Component::Lamp => &consumers::Lamp,
        Component::Door => &consumers::Door,
        Component::Trapdoor => &consumers::Trapdoor,
        Component::FenceGate => &consumers::FenceGate,
        Component::NoteBlock => &consumers::NoteBlock,
        Component::Tnt => &consumers::Tnt,
        Component::Hopper => &consumers::Hopper,
        Component::Bell => &consumers::Bell,
        Component::Dropper | Component::Dispenser => &consumers::Dispenser,
        Component::DaylightDetector => &daylight::DaylightDetector,
        Component::Target => &sources::Target,
    }
}

// The one table this crate's own signal math ([`Signals`], wire
// propagation) reads. `RedstoneFeature` registers a handle to this same
// table into core, so core's dispatch and this crate's signal queries can
// never disagree about what a block does.
static REGISTRY: LazyLock<RedstoneBehaviors> = LazyLock::new(RedstoneBehaviors::vanilla);

/// The behaviour of `block`.
pub fn behaviour(block: Block) -> &'static dyn SignalBehavior {
    REGISTRY.signal_behavior(block)
}

/// Core's view of [`REGISTRY`]: the same behaviours, upcast to the
/// [`BlockBehavior`] reactions core's cascade calls.
struct VanillaTable;

impl BlockBehaviors for VanillaTable {
    fn get(&self, block: Block) -> &'static dyn BlockBehavior {
        behaviour(block)
    }

    fn best_neighbour_signal(&self, level: &dyn Reader, pos: BlockPos) -> u8 {
        Signals::new(level).best_neighbor_signal(pos)
    }

    fn fill_signal(&self, slots: &[&Option<ItemStack>]) -> u8 {
        comparator::fill_signal(slots.iter().copied())
    }
}

/// Registers vanilla redstone into core.
pub struct RedstoneFeature;

impl Feature for RedstoneFeature {
    fn id(&self) -> &'static str {
        "vanilla-redstone"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let id = cx.feature_id();
            cx.registries()
                .block_behaviors
                .fill(id, Arc::new(VanillaTable))?;
            cx.registries()
                .region_hooks
                .push(Arc::new(sculk::SculkHooks));
            cx.registries()
                .dispensers
                .fill(id, Arc::new(dispenser::DispenserRuntime))?;
            cx.registries().block_entity_tickers.register(
                id,
                tuff_core::world::block_entity::ContainerKind::Crafter,
                Arc::new(crafter::CrafterTicker),
            )?;
        }
        Ok(())
    }
}

/// Signal queries over a [`Reader`]. Wire reading its own input sees no
/// signal from wire (`wire: false`), as the game does.
pub struct Signals<'a> {
    pub level: &'a dyn Reader,
    pub wire: bool,
}

impl<'a> Signals<'a> {
    pub fn new(level: &'a dyn Reader) -> Self {
        Self { level, wire: true }
    }

    pub fn block(&self, pos: BlockPos) -> BlockState {
        self.level.block(pos)
    }

    fn signal_of(&self, pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        behaviour(state.block()).signal(self, pos, state, direction)
    }

    fn direct_signal_of(&self, pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        behaviour(state.block()).direct_signal(self, pos, state, direction)
    }

    pub fn is_signal_source(&self, state: BlockState) -> bool {
        if state.block() == Block::RedstoneWire {
            self.wire
        } else {
            state.is_signal_source()
        }
    }

    /// The strong signal the block at `pos` gives toward `direction`.
    pub fn direct_signal(&self, pos: BlockPos, direction: Facing) -> u8 {
        self.direct_signal_of(pos, self.block(pos), direction)
    }

    /// The strongest strong signal reaching `pos` from its neighbours.
    pub fn direct_signal_to(&self, pos: BlockPos) -> u8 {
        let mut best = 0;
        for direction in DIRECTIONS {
            best = best.max(self.direct_signal(step(pos, direction), direction));
            if best >= 15 {
                break;
            }
        }
        best
    }

    /// The signal the block at `pos` gives toward `direction`, a conductor
    /// passing on the strong signals it receives.
    pub fn signal(&self, pos: BlockPos, direction: Facing) -> u8 {
        let state = self.block(pos);
        let signal = self.signal_of(pos, state, direction);
        if state.is_redstone_conductor() {
            signal.max(self.direct_signal_to(pos))
        } else {
            signal
        }
    }

    pub fn has_signal(&self, pos: BlockPos, direction: Facing) -> bool {
        self.signal(pos, direction) > 0
    }

    /// Whether any neighbour powers `pos`.
    pub fn has_neighbor_signal(&self, pos: BlockPos) -> bool {
        DIRECTIONS
            .into_iter()
            .any(|direction| self.signal(step(pos, direction), direction) > 0)
    }

    /// The strongest signal reaching `pos`.
    pub fn best_neighbor_signal(&self, pos: BlockPos) -> u8 {
        let mut best = 0;
        for direction in DIRECTIONS {
            let signal = self.signal(step(pos, direction), direction);
            if signal >= 15 {
                return 15;
            }
            best = best.max(signal);
        }
        best
    }

    /// The side input a diode reads from `pos` in `direction`.
    pub fn control_input(&self, pos: BlockPos, direction: Facing, only_diodes: bool) -> u8 {
        let state = self.block(pos);
        if only_diodes {
            return if diode::is_diode(state) {
                self.direct_signal(pos, direction)
            } else {
                0
            };
        }
        if state.block() == Block::RedstoneBlock {
            15
        } else if state.block() == Block::RedstoneWire {
            state.power().unwrap_or(0)
        } else if self.is_signal_source(state) {
            self.direct_signal(pos, direction)
        } else {
            0
        }
    }
}

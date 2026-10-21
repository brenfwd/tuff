//! The block-update/signal mechanism: the world capability a per-block
//! reaction needs ([`Reader`], [`RedstoneLevel`]), the generic per-block
//! dispatch interface ([`BlockBehavior`]/[`BlockBehaviors`]), and the
//! game's neighbour-changed/shape-update cascade vocabulary (the
//! `UPDATE_*` flags, [`Effect`], [`TickPriority`]).
//!
//! Deliberately **not** here: what any actual block does. Redstone wire
//! propagation, diode facing rules, dispenser/lamp/door reactions, and
//! which block gets which behavior are all `vanilla-redstone` content,
//! registered into [`crate::registry::Registries::block_behaviors`] at
//! feature load time. This
//! module only defines the seam: the trait a feature's behavior objects
//! implement, and the trait a feature's lookup table implements so core
//! can ask "what does this block do" without naming `vanilla-redstone`
//! at all. A server with no such feature loaded dispatches to
//! [`INERT_BEHAVIOR`] for every block -- inert core, unchanged.
//!
//! [`RegionActor`](crate::world::region::RegionActor) implements
//! [`RedstoneLevel`]; `region/redstone.rs` is the cascade that drives
//! it and looks up behaviors through
//! [`Shared::redstone_behaviors`](super::Shared::redstone_behaviors).

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::placement::step;
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

/// Tell the six neighbours the block changed.
pub const UPDATE_NEIGHBORS: u32 = 1;
/// Send the change to clients (always done here; kept for the flag values).
pub const UPDATE_CLIENTS: u32 = 2;
/// Keep the change from clients (they are told anyway here; kept for the
/// flag values).
pub const UPDATE_INVISIBLE: u32 = 4;
/// Do not let neighbours reshape.
pub const UPDATE_KNOWN_SHAPE: u32 = 16;
/// A block a shape update destroys drops nothing.
pub const UPDATE_SUPPRESS_DROPS: u32 = 32;
/// The change is a piston moving the block.
pub const UPDATE_MOVED_BY_PISTON: u32 = 64;
/// Shape updates skip redstone wire.
pub const UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE: u32 = 128;
/// Skip block entity side effects (not modeled; kept for the flag values).
pub const UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS: u32 = 256;
/// Do not run the new block's placement reaction.
pub const UPDATE_SKIP_ON_PLACE: u32 = 512;
/// Lift entities out of a dirt path or farmland turned into its base block.
pub const UPDATE_PUSH_ENTITIES: u32 = 1024;
pub const UPDATE_ALL: u32 = UPDATE_NEIGHBORS | UPDATE_CLIENTS;
/// The game's quiet set for changes nothing needs told of (a growth stage):
/// no neighbour-changed updates, though neighbours still reshape.
pub const UPDATE_NONE: u32 = UPDATE_INVISIBLE | UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS;
/// The level event of a block breaking: its particles and sound.
pub const LEVEL_EVENT_BLOCK_BREAK: i32 = 2001;
/// How deep shape updates may recurse.
pub const UPDATE_LIMIT: u32 = 512;
/// The most updates one cascade may queue, the game's default.
pub const MAX_CHAINED_UPDATES: usize = 1_000_000;

/// The order neighbour-changed updates visit a block's neighbours.
pub const UPDATE_ORDER: [Facing; 6] = [
    Facing::West,
    Facing::East,
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
];
/// The order shape updates visit a block's neighbours.
pub const SHAPE_UPDATE_ORDER: [Facing; 6] = [
    Facing::West,
    Facing::East,
    Facing::North,
    Facing::South,
    Facing::Down,
    Facing::Up,
];

/// Tick priorities; lower runs first among ticks due the same game tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickPriority {
    ExtremelyHigh = -3,
    VeryHigh = -2,
    High = -1,
    Normal = 0,
    Low = 1,
    VeryLow = 2,
    ExtremelyLow = 3,
}

impl TickPriority {
    pub fn from_value(value: i32) -> Self {
        match value.clamp(-3, 3) {
            -3 => Self::ExtremelyHigh,
            -2 => Self::VeryHigh,
            -1 => Self::High,
            0 => Self::Normal,
            1 => Self::Low,
            2 => Self::VeryLow,
            _ => Self::ExtremelyLow,
        }
    }
}

/// What a behaviour asks the world to do beyond setting blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Sound {
        sound: SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
    },
    LevelEvent {
        event: i32,
        pos: BlockPos,
        data: i32,
    },
    /// A command block's power changed.
    CommandBlockPowered(BlockPos, bool),
    /// A command block's scheduled tick.
    CommandBlockTick(BlockPos),
    /// A player placed a command block.
    CommandBlockPlaced(BlockPos),
    /// The signal beside a test block changed: whether any now reaches it.
    TestBlockPower(BlockPos, bool),
    /// A test block's scheduled reset.
    TestBlockReset(BlockPos),
    /// The signal beside a structure block changed: whether any now reaches it.
    StructureBlockPower(BlockPos, bool),
    /// A crafter's trigger state changed.
    CrafterTriggered(BlockPos, bool),
    /// A crafter crafts and dispenses.
    CrafterDispense(BlockPos),
    /// A waterlogged block asks for its water to tick.
    WaterTick(BlockPos),
    /// A note block plays its note.
    PlayNote(BlockPos),
    /// A bell rings, struck from no particular side.
    RingBell(BlockPos),
    /// TNT at the position turns into primed TNT (the block is removed by
    /// the caller).
    PrimeTnt(BlockPos),
    /// A dispenser or dropper fires.
    Dispense(BlockPos),
    /// The block drops its loot as if broken.
    DropResources(BlockPos, BlockState),
    /// A game event at the centre of the block, from no entity.
    GameEvent(crate::world::game_event::GameEvent, BlockPos),
}

/// A block a piston is moving: the data of a moving piston block entity.
/// It takes two game ticks (`progress` 0, 0.5, 1) and becomes `moved` at
/// its destination on the tick after reaching 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovingBlock {
    /// The block being moved (the piston head for the arm, the piston
    /// itself when a piston retracts).
    pub moved: BlockState,
    /// The facing of the piston moving it.
    pub facing: Facing,
    /// Whether the piston is extending (the block moves along `facing`).
    pub extending: bool,
    /// Whether this is the piston's own arm or base rather than a pushed
    /// block.
    pub source: bool,
    pub progress: f32,
    pub progress_before: f32,
    /// The game tick it last moved.
    pub last_ticked: u64,
}

impl MovingBlock {
    pub fn new(moved: BlockState, facing: Facing, extending: bool, source: bool) -> Self {
        Self {
            moved,
            facing,
            extending,
            source,
            progress: 0.0,
            progress_before: 0.0,
            last_ticked: 0,
        }
    }

    /// The direction the block travels.
    pub fn movement(&self) -> Facing {
        if self.extending {
            self.facing
        } else {
            self.facing.opposite()
        }
    }
}

/// Which blocks pistons move together with their block entity; core
/// keeps the game's rule (none) when no feature fills the slot.
pub trait BlockEntityCarrier: Send + Sync {
    fn carries(&self, state: BlockState) -> bool;

    /// How viewers see `state` move, given its block entity data when the
    /// region moving it has that at hand.
    fn look(&self, _state: BlockState, _nbt: Option<&tuff_nbt::Compound>) -> CarriedLook {
        CarriedLook::Model
    }
}

/// What stands in for a block a piston moves with its block entity.
#[derive(Debug, PartialEq)]
pub enum CarriedLook {
    /// The moving piston draws the block's own model.
    Model,
    /// A block display of the moving state, for blocks clients draw only
    /// through their block entity (chests).
    BlockDisplay,
    /// An item display of `item`, transformed from the block's corner, for
    /// looks only the item carries (banner patterns).
    ItemDisplay {
        item: Box<tuff_item::item::ItemStack>,
        translation: [f32; 3],
        rotation: [f32; 4],
    },
}

/// What a comparator reads from a minecart on a detector rail.
pub struct CartReading<'a> {
    /// A command block minecart's last success count.
    pub command_success: Option<i32>,
    /// A container minecart's slots.
    pub slots: Option<&'a [Option<ItemStack>]>,
}

/// Read access to blocks for signal calculations.
pub trait Reader {
    /// The block at `pos`; unloaded blocks read as air.
    fn block(&self, pos: BlockPos) -> BlockState;
    /// The signal a comparator at `pos` holds.
    fn comparator_output(&self, pos: BlockPos) -> u8;
    /// The comparator reading of the block at `pos`, read from `direction`
    /// (pointing from the block toward the comparator).
    fn analog_output(&self, pos: BlockPos, state: BlockState, direction: Facing) -> u8;
    /// The comparator reading of the one item frame in the block at `pos`
    /// facing `direction`; `None` when there is no such frame or several.
    fn item_frame_output(&self, _pos: BlockPos, _direction: Facing) -> Option<u8> {
        None
    }

    /// The slots of the container at `pos`, a double chest's both halves.
    fn container_slots(&self, _pos: BlockPos, _state: BlockState) -> Vec<&Option<ItemStack>> {
        Vec::new()
    }

    /// The minecarts on rails whose box touches `area`, by entity id.
    fn minecarts_in(&self, _area: [f64; 6]) -> Vec<CartReading<'_>> {
        Vec::new()
    }

    /// How many players have the container at `pos` open.
    fn open_count(&self, _pos: BlockPos) -> usize {
        0
    }

    /// The saved NBT of a block entity this server does not model (a
    /// jukebox), if there is one at `pos`.
    fn saved_nbt_at(&self, _pos: BlockPos) -> Option<&tuff_nbt::Compound> {
        None
    }
}

pub use super::orientation::{Orientation, SideBias};

/// The feature flag that turns on the experimental redstone rules.
pub const REDSTONE_EXPERIMENTS: &str = "minecraft:redstone_experiments";

/// The world the redstone rules act on.
pub trait RedstoneLevel: Reader {
    /// Sets a block with the game's update flags; false if nothing changed
    /// or the position is not writable here.
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: u32) -> bool;
    /// Tells the block at `pos` that its neighbour, a `source`, changed.
    fn neighbor_changed(&mut self, pos: BlockPos, source: Block);
    /// Tells the six neighbours of `pos` except the one in `skip`.
    fn update_neighbors_except(&mut self, pos: BlockPos, source: Block, skip: Option<Facing>);
    /// Lets the block at `pos` reshape for its neighbour at
    /// `neighbour_pos`, in `direction`, being `neighbour`.
    fn shape_update(
        &mut self,
        direction: Facing,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        neighbour: BlockState,
        flags: u32,
        limit: u32,
    );
    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32, priority: TickPriority);
    fn has_scheduled_tick(&self, pos: BlockPos, block: Block) -> bool;
    /// Whether a tick for the block at `pos` is due in the batch running
    /// now and has not run yet.
    fn will_tick_this_tick(&self, pos: BlockPos, block: Block) -> bool;
    fn set_comparator_output(&mut self, pos: BlockPos, value: u8);
    fn game_time(&self) -> u64;
    fn random_float(&mut self) -> f32;
    /// Redstone torch toggles still counting toward burnout, oldest first.
    fn torch_toggles(&mut self) -> &mut Vec<(BlockPos, u64)>;
    /// How many entities touch the box, living ones only if `living`.
    fn entities_touching(&self, bounds: [f64; 6], living: bool) -> usize;
    /// Whether an arrow touches the box.
    fn arrow_touching(&self, bounds: [f64; 6]) -> bool;
    /// How many minecarts touch the box.
    fn minecarts_touching(&self, _bounds: [f64; 6]) -> usize {
        0
    }
    fn effect(&mut self, effect: Effect);

    /// The block at `pos` when this world may change it (loaded and owned
    /// here), else `None`.
    fn owned_block(&self, pos: BlockPos) -> Option<BlockState> {
        Some(self.block(pos))
    }
    /// The block at `pos` when a piston may move it: owned here, or loaded
    /// in a neighbouring world that takes the move as
    /// [`RedstoneLevel::piston_set`] messages. `None` when unloaded.
    fn reachable_block(&self, pos: BlockPos) -> Option<BlockState> {
        self.owned_block(pos)
    }
    /// A piston's change to the block at `pos`: its loot drops first when
    /// `drops`, then it becomes `state`, which starts moving `moving` when
    /// given. `carry` is where the block it replaces is itself moving to,
    /// if anywhere. A world may hand a position it does not own to its
    /// owner, which applies it later.
    fn piston_set(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        drops: bool,
        moving: Option<MovingBlock>,
        _carry: Option<BlockPos>,
    ) {
        if drops {
            let old = self.block(pos);
            self.effect(Effect::DropResources(pos, old));
        }
        self.set_block(pos, state, flags);
        if let Some(moving) = moving {
            self.start_moving(pos, moving);
        }
    }
    /// Whether a piston may move the block entity block `state` at `pos`
    /// toward `direction`, taking its block entity along.
    fn carries_block_entity(&self, _state: BlockState, _pos: BlockPos, _direction: Facing) -> bool {
        false
    }
    /// The lowest and highest block y of the world.
    fn build_limits(&self) -> (i32, i32) {
        (-64, 319)
    }
    /// Whether `pos` is inside the world border.
    fn within_border(&self, _pos: BlockPos) -> bool {
        true
    }
    /// Queues a block event for the block at `pos`, run with
    /// [`BlockBehavior::trigger_event`] in the block event phase of the tick
    /// while `block` is still there. Equal events queued twice run once.
    fn block_event(&mut self, _pos: BlockPos, _block: Block, _kind: u8, _data: u8) {}
    /// Whether scheduled block ticks are running.
    fn is_handling_tick(&self) -> bool {
        false
    }
    /// The block moving through the moving piston at `pos`.
    fn moving_block(&self, _pos: BlockPos) -> Option<MovingBlock> {
        None
    }
    /// Gives the moving piston just set at `pos` the block it moves.
    fn start_moving(&mut self, _pos: BlockPos, _moving: MovingBlock) {}
    /// Finishes the move through `pos` at once, the block arriving (or the
    /// piston's own arm vanishing).
    fn finish_moving(&mut self, _pos: BlockPos) {}
    /// Breaks the block at `pos` like a player would without a tool:
    /// particles, loot with `drops`, and what it held left behind.
    fn destroy_block(&mut self, pos: BlockPos, drops: bool) {
        let state = self.block(pos);
        if state.block() == Block::Air {
            return;
        }
        self.effect(Effect::LevelEvent {
            event: LEVEL_EVENT_BLOCK_BREAK,
            pos,
            data: state.id() as i32,
        });
        if drops {
            self.effect(Effect::DropResources(pos, state));
        }
        let left = if state.waterlogged() == Some(true) {
            Block::Water.default_state()
        } else {
            Block::Air.default_state()
        };
        self.set_block(pos, left, UPDATE_ALL);
    }

    /// The sky light at `pos` less the darkening of the sky by time of day
    /// and weather, or `None` where there is no sky or the light is not
    /// known yet.
    fn sky_brightness(&self, _pos: BlockPos) -> Option<u8> {
        None
    }
    /// The angle of the sun in degrees, 0 at noon.
    fn sun_angle(&self) -> f32 {
        0.0
    }

    fn update_neighbors_at(&mut self, pos: BlockPos, source: Block) {
        self.update_neighbors_except(pos, source, None);
    }

    /// Whether the world runs the experimental redstone rules.
    fn redstone_experiments(&self) -> bool {
        false
    }

    /// An orientation drawn uniformly from the level's random source.
    fn random_orientation(&mut self) -> Orientation {
        Orientation::from_roll((self.random_float() * Orientation::COUNT as f32) as u32)
    }

    /// The orientation a block updating its neighbours starts from: none
    /// without the experimental rules, else a random one with a left side
    /// bias turned to `up` and `front` where given.
    fn initial_orientation(
        &mut self,
        front: Option<Facing>,
        up: Option<Facing>,
    ) -> Option<Orientation> {
        if !self.redstone_experiments() {
            return None;
        }
        let mut orientation = self.random_orientation().with_side_bias(SideBias::Left);
        if let Some(up) = up {
            orientation = orientation.with_up(up);
        }
        if let Some(front) = front {
            orientation = orientation.with_front(front);
        }
        Some(orientation)
    }

    /// The orientation the neighbour update now running carries.
    fn incoming_orientation(&self) -> Option<Orientation> {
        None
    }

    /// A neighbour-changed update at `pos` carrying `orientation`.
    fn neighbor_changed_oriented(
        &mut self,
        pos: BlockPos,
        source: Block,
        _orientation: Option<Orientation>,
    ) {
        self.neighbor_changed(pos, source);
    }

    /// Updates the neighbours of `pos` but `skip`, each carrying
    /// `orientation` turned to face it.
    fn update_neighbors_oriented(
        &mut self,
        pos: BlockPos,
        source: Block,
        skip: Option<Facing>,
        _orientation: Option<Orientation>,
    ) {
        self.update_neighbors_except(pos, source, skip);
    }

    /// Shows debug viewers the orientation an experimental wire update
    /// used at `pos`.
    fn wire_orientation_debug(&mut self, _pos: BlockPos, _orientation: Orientation) {}
}

/// A block's reaction to the update/signal system core dispatches
/// through. Every method defaults to the behavior of a block with no
/// such role at all (see [`INERT_BEHAVIOR`]).
///
/// This is deliberately a *subset* of what a real redstone component
/// needs (compare `tuff_feature_builtin_vanilla_redstone::SignalBehavior`, which adds the
/// signal-reading methods `wire`/diode propagation uses): only the
/// methods core's own cascade (`world/region/redstone.rs`) calls
/// directly belong here. Everything else is `vanilla-redstone`'s own
/// business, invisible to core.
pub trait BlockBehavior: Send + Sync {
    /// A neighbour of the block changed.
    fn neighbor_changed(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
    }

    /// A scheduled tick of the block ran.
    fn tick(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// The block was set, replacing `old` (possibly a state of itself).
    fn on_place(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _old: BlockState,
        _moved: bool,
    ) {
    }

    /// A creative player is about to break the block, `state`.
    fn creative_break(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// The block, `state`, was replaced by another block.
    fn after_removal(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _moved: bool,
    ) {
    }

    /// The block's reaction to its neighbour in `direction` becoming
    /// `neighbour`, or `None` to leave it to the placement rules.
    fn update_shape(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _direction: Facing,
        _neighbour: BlockState,
    ) -> Option<BlockState> {
        None
    }

    /// Shape updates a change sends past the direct neighbours.
    fn update_indirect_shapes(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _flags: u32,
        _limit: u32,
    ) {
    }

    /// The placed state adjusted to the power around it.
    fn placement(&self, _level: &dyn Reader, _pos: BlockPos, state: BlockState) -> BlockState {
        state
    }

    /// Runs after a player placed the block.
    fn placed(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// Whether [`BlockBehavior::entity_inside`] should run when an entity
    /// touches the block, beyond pressure plates and buttons.
    fn senses_entities(&self) -> bool {
        false
    }

    /// An entity touches the block.
    fn entity_inside(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// Whether the block reacts through this behavior although it has no
    /// dumped redstone component; its reactions then wait for the chunks
    /// around it to load, as components' do.
    fn is_active(&self) -> bool {
        false
    }

    /// A block event queued for the block ran; true tells nearby clients
    /// (a Block Action packet with `kind` and `data`).
    fn trigger_event(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
        _kind: u8,
        _data: u8,
    ) -> bool {
        false
    }

    /// The arm a moving piston block shows at `short` while it moves
    /// along `facing`, when the block is a piston part.
    fn moving_arm(&self, _moved: BlockState, _facing: Facing, _short: bool) -> Option<BlockState> {
        None
    }

    /// The base a retracting piston leaves in place while its arm moves.
    fn arm_base(&self, _moved: BlockState) -> Option<BlockState> {
        None
    }

    /// Whether entities pushed by this block as it moves bounce off it.
    fn bounces_pushed(&self) -> bool {
        false
    }

    /// Whether this block carries what stands on it as it moves sideways.
    fn drags_riders(&self) -> bool {
        false
    }

    /// Whether [`BlockBehavior::slow_tick`] runs for the block (only blocks
    /// holding block entity data are visited).
    fn has_slow_tick(&self) -> bool {
        false
    }

    /// Runs once every 20 game ticks for each such block.
    fn slow_tick(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// Lightning struck the block.
    fn struck_by_lightning(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        _state: BlockState,
    ) {
    }

    /// A triggering explosion (a wind burst) reached the block.
    fn exploded(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// A projectile struck the block.
    fn projectile_hit(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) {}

    /// The signal the block holds for a projectile hit on `face` at
    /// `location`, and for how many ticks; `None` for blocks that give no
    /// signal for hits.
    fn hit_signal(&self, _face: Facing, _location: [f64; 3], _arrow: bool) -> Option<(u8, u32)> {
        None
    }

    /// The comparator reading of the block, read from `direction`, when
    /// this behavior defines it; `None` falls back to the dumped source.
    fn analog_output(
        &self,
        _level: &dyn Reader,
        _pos: BlockPos,
        _state: BlockState,
        _direction: Facing,
    ) -> Option<u8> {
        None
    }

    /// A player right-clicked the block; true when it reacted.
    fn used(&self, _level: &mut dyn RedstoneLevel, _pos: BlockPos, _state: BlockState) -> bool {
        false
    }
}

/// The default: a block with no redstone role at all. Every
/// `BlockBehavior` method keeps its default (no-op / identity)
/// implementation.
pub struct InertBehavior;

impl BlockBehavior for InertBehavior {}

pub static INERT_BEHAVIOR: InertBehavior = InertBehavior;

/// A feature's per-block behavior table, registered as a single
/// [`tuff_item::registry::HookSlot`] (`Registries::block_behaviors`) -- one
/// lookup table is active per server, the same way there is one active
/// growth-behavior table or one active command registry. Looked up by
/// `Block` rather than by name for O(1) dispatch on a hot path (every
/// block update/tick/interaction).
///
/// Returns `&'static` so a lookup holds no borrow of the table (or of the
/// `Shared` holding it): the cascade calls a behavior with `&mut self` as
/// the level right after looking it up. Behaviors are stateless dispatch
/// objects, so this costs a feature nothing -- a `static` per behavior, or
/// a one-time `Box::leak` at load for a dynamically built one.
pub trait BlockBehaviors: Send + Sync {
    fn get(&self, block: Block) -> &'static dyn BlockBehavior;

    /// The strongest signal reaching `pos` from its six neighbours (what
    /// powers TNT there).
    fn best_neighbour_signal(&self, _level: &dyn Reader, _pos: BlockPos) -> u8 {
        0
    }

    /// The comparator reading of container slots: how full they are.
    fn fill_signal(&self, _slots: &[&Option<ItemStack>]) -> u8 {
        0
    }
}

/// The table an inert core (no feature filled `block_behaviors`) uses:
/// every block is [`INERT_BEHAVIOR`].
pub struct NoBlockBehaviors;

impl BlockBehaviors for NoBlockBehaviors {
    fn get(&self, _block: Block) -> &'static dyn BlockBehavior {
        &INERT_BEHAVIOR
    }
}

/// Tells comparators beside `pos` (directly, or through one conductor)
/// that the comparator reading of the block there changed. A pure
/// function of the update-model vocabulary above (no per-block
/// behavior dispatch), so it lives here rather than in a feature.
pub fn update_neighbour_for_output_signal(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    source: Block,
) {
    for direction in Facing::HORIZONTAL {
        let beside = step(pos, direction);
        let state = level.block(beside);
        if state.block() == Block::Comparator {
            level.neighbor_changed(beside, source);
        } else if state.is_redstone_conductor() {
            let further = step(beside, direction);
            if level.block(further).block() == Block::Comparator {
                level.neighbor_changed(further, source);
            }
        }
    }
}

#[cfg(test)]
mod tick_priority_tests {
    use super::TickPriority;

    #[tuff_core::watchdog]
    #[test]
    fn priorities_cover_minus_three_to_three_and_clamp() {
        for v in -3..=3 {
            assert_eq!(TickPriority::from_value(v) as i32, v);
        }
        assert_eq!(TickPriority::from_value(-9), TickPriority::ExtremelyHigh);
        assert_eq!(TickPriority::from_value(9), TickPriority::ExtremelyLow);
    }
}

//! Fluid rules and the block state properties the fluid simulation reads,
//! extracted from the server jar by `build/FluidProperties.java`.

use crate::block::{BlockState, Facing};
pub use crate::registry::Fluid;
use crate::registry::SoundEvent;

include!(concat!(env!("OUT_DIR"), "/fluids.rs"));

/// How a fluid acts beyond its numbers: what it does to the blocks it
/// replaces and to water below it, and whether it slows while rising.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FluidBehaviour {
    /// Replaced blocks drop their loot.
    Water,
    /// Replaced blocks fizz away; flowing down onto water makes stone; a
    /// rising level usually waits four times longer.
    Lava,
}

#[derive(Debug)]
pub struct FluidRules {
    pub fluid: Fluid,
    pub source: Fluid,
    pub flowing: Fluid,
    tick_delay: [u32; 2],
    drop_off: [u8; 2],
    slope_find_distance: [u8; 2],
    pub behaviour: FluidBehaviour,
}

impl FluidRules {
    /// Ticks between spreading steps.
    pub fn tick_delay(&self, fast_lava: bool) -> u32 {
        self.tick_delay[fast_lava as usize]
    }

    /// How much the amount falls per block spread sideways.
    pub fn drop_off(&self, fast_lava: bool) -> i32 {
        self.drop_off[fast_lava as usize] as i32
    }

    /// How far sideways the fluid looks for a way down.
    pub fn slope_find_distance(&self, fast_lava: bool) -> i32 {
        self.slope_find_distance[fast_lava as usize] as i32
    }
}

impl Fluid {
    /// The rules of a flowing fluid kind; `None` for the empty fluid.
    pub fn rules(self) -> Option<&'static FluidRules> {
        RULES.iter().find(|rules| rules.fluid == self)
    }

    /// Whether both are the same liquid (source and flowing kinds match).
    pub fn is_same(self, other: Fluid) -> bool {
        match (self.rules(), other.rules()) {
            (Some(a), Some(b)) => a.source == b.source,
            _ => self == other,
        }
    }
}

/// Whether lava is fast in a dimension type (the nether's).
pub fn fast_lava(dimension_type: &str) -> bool {
    FAST_LAVA
        .iter()
        .find(|(name, _)| *name == dimension_type)
        .map_or(FAST_LAVA_DEFAULT, |(_, fast)| *fast)
}

struct FluidStateData {
    fluid: Fluid,
    amount: u8,
    falling: bool,
    source: bool,
    own_height: f32,
    block: BlockState,
}

/// A fluid with its amount (1-8) and whether it is falling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FluidState(u8);

impl FluidState {
    pub const EMPTY: Self = Self(0);

    fn data(self) -> &'static FluidStateData {
        &FLUID_STATES[self.0 as usize]
    }

    pub fn fluid(self) -> Fluid {
        self.data().fluid
    }

    pub fn rules(self) -> Option<&'static FluidRules> {
        self.fluid().rules()
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn amount(self) -> i32 {
        self.data().amount as i32
    }

    pub fn is_falling(self) -> bool {
        self.data().falling
    }

    pub fn is_source(self) -> bool {
        self.data().source
    }

    /// The surface height within its block, ignoring what is above.
    pub fn own_height(self) -> f32 {
        self.data().own_height
    }

    /// The surface height: full when the same liquid lies above.
    pub fn height(self, same_above: bool) -> f32 {
        if same_above && !self.is_empty() {
            1.0
        } else {
            self.own_height()
        }
    }

    /// The liquid block that holds this fluid state on its own.
    pub fn block(self) -> BlockState {
        self.data().block
    }

    /// Whether `other` flowing in `direction` may replace this fluid;
    /// `same_above` is whether the same liquid lies above it.
    pub fn can_be_replaced_with(self, other: Fluid, direction: Facing, same_above: bool) -> bool {
        let row = REPLACEABLE[self.0 as usize * 2 + same_above as usize];
        row >> (other.id() as usize * 6 + direction_index(direction)) & 1 == 1
    }

    fn find(fluid: Fluid, amount: u8, falling: bool, source: bool) -> Self {
        FLUID_STATES
            .iter()
            .position(|s| {
                s.fluid == fluid && s.amount == amount && s.falling == falling && s.source == source
            })
            .map_or(Self::EMPTY, |id| Self(id as u8))
    }

    /// The source state of `fluid`'s liquid.
    pub fn source(fluid: Fluid, falling: bool) -> Self {
        match fluid.rules() {
            Some(rules) => Self::find(rules.source, 8, falling, true),
            None => Self::EMPTY,
        }
    }

    /// A flowing state of `fluid`'s liquid.
    pub fn flowing(fluid: Fluid, amount: i32, falling: bool) -> Self {
        match fluid.rules() {
            Some(rules) if (1..=8).contains(&amount) => {
                Self::find(rules.flowing, amount as u8, falling, false)
            }
            _ => Self::EMPTY,
        }
    }
}

/// What placing a fluid into a block that holds liquids does.
#[derive(Debug)]
pub struct LiquidPlacement {
    pub result: BlockState,
    /// The fluid whose tick is scheduled at the block.
    pub tick: Option<Fluid>,
    /// `(sound, volume, pitch)` played at the block.
    pub sounds: &'static [(SoundEvent, f32, f32)],
}

/// The part of one face of a block's collision shape, as an id into the
/// table of distinct faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollisionFace(u8);

impl CollisionFace {
    /// Whether the two faces together cover the whole face of a block.
    pub fn covers(self, other: Self) -> bool {
        let index = self.0 as usize * COLLISION_FACE_COUNT + other.0 as usize;
        COLLISION_COVERS[index / 64] >> (index % 64) & 1 == 1
    }
}

/// Whether a fluid can pass from `from` into its neighbour `to` in
/// `direction`: their collision shapes do not close off the shared face.
pub fn passes_between(from: BlockState, direction: Facing, to: BlockState) -> bool {
    !from
        .collision_face(direction)
        .covers(to.collision_face(direction.opposite()))
}

/// The game's direction order: down, up, north, south, west, east.
const fn direction_index(direction: Facing) -> usize {
    match direction {
        Facing::Down => 0,
        Facing::Up => 1,
        Facing::North => 2,
        Facing::South => 3,
        Facing::West => 4,
        Facing::East => 5,
    }
}

impl BlockState {
    pub fn fluid_state(self) -> FluidState {
        FluidState(STATE_FLUID[self.id() as usize])
    }

    /// An ice block, whose sides never count as solid for flow.
    pub fn is_ice(self) -> bool {
        STATE_FLUID_FLAGS[self.id() as usize] & 1 != 0
    }

    /// The block takes liquids itself (waterloggable blocks, seagrass, ...)
    /// instead of being replaced by them.
    pub fn is_liquid_container(self) -> bool {
        STATE_FLUID_FLAGS[self.id() as usize] & 2 != 0
    }

    /// A plain liquid block (water or lava), as opposed to a block that
    /// merely holds a fluid.
    pub fn is_liquid_block(self) -> bool {
        STATE_FLUID_FLAGS[self.id() as usize] & 4 != 0
    }

    /// Whether a liquid container would take `fluid`.
    pub fn can_place_liquid(self, fluid: Fluid) -> bool {
        STATE_FLUID_FLAGS[self.id() as usize] >> (3 + fluid.id()) & 1 != 0
    }

    /// What placing `fluid`'s source here does, if the block takes it.
    pub fn place_liquid(self, fluid: Fluid) -> Option<&'static LiquidPlacement> {
        let start = PLACEMENTS.partition_point(|(id, _, _)| *id < self.id());
        PLACEMENTS[start..]
            .iter()
            .take_while(|(id, _, _)| *id == self.id())
            .find(|(_, placed, _)| *placed == fluid)
            .map(|(_, _, placement)| placement)
    }

    pub fn collision_face(self, direction: Facing) -> CollisionFace {
        let set = STATE_COLLISION_FACES[self.id() as usize] as usize;
        CollisionFace(COLLISION_FACE_SETS[set][direction_index(direction)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Block;

    #[test]
    fn liquid_blocks_hold_their_fluid() {
        let water = Block::Water.default_state();
        assert_eq!(water.fluid_state(), FluidState::source(Fluid::Water, false));
        assert_eq!(water.fluid_state().amount(), 8);
        let level_3 = water.with_level(3).unwrap();
        let state = level_3.fluid_state();
        assert_eq!((state.fluid(), state.amount()), (Fluid::FlowingWater, 5));
        assert_eq!(state.block(), level_3);
        let falling = water.with_level(8).unwrap().fluid_state();
        assert!(falling.is_falling() && falling.amount() == 8);
        let stairs = Block::OakStairs
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        assert!(stairs.fluid_state().is_source());
        assert!(Block::Kelp.default_state().fluid_state().is_source());
        assert!(water.is_liquid_block());
        assert!(!Block::BubbleColumn.default_state().is_liquid_block());
        assert!(Block::Stone.default_state().fluid_state().is_empty());
    }

    #[test]
    fn rules_follow_the_dimension() {
        let lava = Fluid::Lava.rules().unwrap();
        assert!(!fast_lava("minecraft:overworld"));
        assert!(fast_lava("minecraft:the_nether"));
        assert_eq!((lava.tick_delay(false), lava.tick_delay(true)), (30, 10));
        assert_eq!((lava.drop_off(false), lava.drop_off(true)), (2, 1));
        let water = Fluid::FlowingWater.rules().unwrap();
        assert_eq!(
            (
                water.tick_delay(false),
                water.drop_off(false),
                water.slope_find_distance(false)
            ),
            (5, 1, 4)
        );
        assert!(Fluid::FlowingLava.is_same(Fluid::Lava));
        assert!(!Fluid::Water.is_same(Fluid::Lava));
    }

    #[test]
    fn liquid_containers_take_water_sources_only() {
        let slab = Block::OakSlab.default_state();
        assert!(slab.is_liquid_container());
        assert!(slab.can_place_liquid(Fluid::Water));
        assert!(!slab.can_place_liquid(Fluid::FlowingWater));
        assert!(!slab.can_place_liquid(Fluid::Lava));
        let placed = slab.place_liquid(Fluid::Water).unwrap();
        assert_eq!(placed.result.waterlogged(), Some(true));
        assert_eq!(placed.tick, Some(Fluid::Water));
        let wet = slab.with_waterlogged(true).unwrap();
        assert!(wet.place_liquid(Fluid::Water).is_none());
        let campfire = Block::Campfire.default_state();
        let placed = campfire.place_liquid(Fluid::Water).unwrap();
        assert_eq!(placed.result.lit(), Some(false));
        assert!(!placed.sounds.is_empty());
    }

    #[test]
    fn full_blocks_close_every_face() {
        let stone = Block::Stone.default_state();
        let air = Block::Air.default_state();
        assert!(!passes_between(air, Facing::North, stone));
        assert!(!passes_between(stone, Facing::Up, air));
        assert!(passes_between(air, Facing::East, air));
        let bottom_slab = Block::OakSlab.default_state();
        assert!(passes_between(air, Facing::Down, bottom_slab));
        assert!(!passes_between(bottom_slab, Facing::Down, air));
    }

    #[test]
    fn water_only_gives_way_downward_to_other_fluids() {
        let water = FluidState::flowing(Fluid::Water, 3, false);
        assert!(water.can_be_replaced_with(Fluid::FlowingLava, Facing::Down, false));
        assert!(!water.can_be_replaced_with(Fluid::FlowingLava, Facing::North, false));
        assert!(!water.can_be_replaced_with(Fluid::FlowingWater, Facing::Down, false));
        let low_lava = FluidState::flowing(Fluid::Lava, 3, false);
        assert!(!low_lava.can_be_replaced_with(Fluid::FlowingWater, Facing::North, false));
        assert!(low_lava.can_be_replaced_with(Fluid::FlowingWater, Facing::North, true));
        assert!(FluidState::EMPTY.can_be_replaced_with(Fluid::Water, Facing::Up, false));
    }
}

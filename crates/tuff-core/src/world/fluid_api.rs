//! Fluids as a seam: [`FluidWorld`] is the world a fluid sees (the region
//! implements it), [`FluidBehaviors`] is how fluids flow, filled through
//! `Registries::fluid_behaviors`. With no feature, fluids never move.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::fluid::{Fluid, FluidState, LiquidPlacement};

use tuff_world_model::BlockPos;

/// Per-dimension fluid rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// The dimension's `fast_lava` attribute.
    pub fast_lava: bool,
    pub water_source_conversion: bool,
    pub lava_source_conversion: bool,
}

/// What a fluid sees of and does to the world.
pub trait FluidWorld {
    /// The block at `pos`; `None` where the world is not loaded.
    fn block(&self, pos: BlockPos) -> Option<BlockState>;
    fn settings(&self) -> Settings;
    /// A uniform random number below `bound`.
    fn random(&mut self, bound: u32) -> u32;
    /// Sets a block and lets its neighbours react.
    fn set(&mut self, pos: BlockPos, state: BlockState);
    fn schedule(&mut self, pos: BlockPos, fluid: Fluid, delay: u32);
    /// A water flow breaks `state` (which then drops its loot), leaving
    /// the fluid block `left`.
    fn wash_away(&mut self, pos: BlockPos, state: BlockState, left: BlockState);
    /// A liquid container at `pos` takes `fluid`'s source: `result` and the
    /// placement's tick and sounds.
    fn place_liquid(&mut self, pos: BlockPos, placement: &'static LiquidPlacement);
    /// Lava hisses at `pos`.
    fn fizz(&mut self, pos: BlockPos);
    /// `fluid` flows from the neighbour opposite `direction` into `pos`: a
    /// world applies it where it owns `pos` and otherwise hands it to the
    /// owner, who checks it with [`FluidBehaviors::still_flows`].
    fn spread_to(&mut self, pos: BlockPos, direction: Facing, fluid: FluidState);
    /// The brightness at `pos` (the greater of its sky and block light),
    /// when the world knows its light.
    fn raw_brightness(&self, _pos: BlockPos) -> Option<u8> {
        None
    }
}

/// How fluids flow. Every method defaults to doing nothing.
pub trait FluidBehaviors: Send + Sync {
    /// The unit direction the fluid at `pos` pushes what is in it, read
    /// through `block` (`None` where unloaded); zero for still fluid.
    fn flow(&self, _block: &dyn Fn(BlockPos) -> Option<BlockState>, _pos: BlockPos) -> [f64; 3] {
        [0.0; 3]
    }

    /// A scheduled fluid tick of `kind` at `pos`.
    fn tick(&self, _world: &mut dyn FluidWorld, _pos: BlockPos, _kind: Fluid) {}

    /// A generated chunk's marked block at `pos` settles: its fluid's
    /// first tick, and the shape it should take, if it changes.
    fn post_process(&self, _world: &mut dyn FluidWorld, _pos: BlockPos) -> Option<BlockState> {
        None
    }

    /// The fluid-holding block at `pos` was placed or a neighbour changed.
    fn liquid_changed(&self, _world: &mut dyn FluidWorld, _pos: BlockPos, _state: BlockState) {}

    /// The neighbour of the fluid-holding `state` at `pos` became
    /// `neighbour`.
    fn shape_changed(
        &self,
        _world: &mut dyn FluidWorld,
        _pos: BlockPos,
        _state: BlockState,
        _neighbour: BlockState,
    ) {
    }

    /// Whether `fluid` still flows into `pos` from the neighbour opposite
    /// `direction` (a spread another region computed).
    fn still_flows(
        &self,
        _world: &dyn FluidWorld,
        _pos: BlockPos,
        _direction: Facing,
        _fluid: FluidState,
    ) -> bool {
        false
    }

    /// Lets `fluid` flow into `pos`, which holds `state`.
    fn apply_spread(
        &self,
        _world: &mut dyn FluidWorld,
        _pos: BlockPos,
        _state: BlockState,
        _direction: Facing,
        _fluid: FluidState,
    ) {
    }
}

/// Fluids that never move.
pub struct NoFluidBehaviors;

impl FluidBehaviors for NoFluidBehaviors {}

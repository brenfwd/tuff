use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::damage_type::DamageType;

use tuff_world_model::BlockPos;

use super::*;

/// A block change in one region that affects a block in a neighbouring
/// one. The receiver applies it after its block events when it has not
/// yet run the tick it was made on, else at the start of its next tick as
/// of the sender's tick; it reads the rest of the sender's blocks from its
/// halo, which is sent before these.
#[derive(Debug, Clone)]
pub enum BlockUpdate {
    /// The neighbour of `pos` in `direction` became `neighbour`: a shape
    /// update with the game's update flags and remaining depth.
    Shape {
        pos: BlockPos,
        direction: Facing,
        neighbour: BlockState,
        flags: u32,
        limit: u32,
        cause: crate::world::region_api::BlockCause,
    },
    /// A neighbour of `pos`, a `source` block, changed ("neighbour
    /// changed", what redstone reacts to).
    Neighbour {
        pos: BlockPos,
        source: tuff_vanilla_data::Block,
        cause: crate::world::region_api::BlockCause,
    },
    /// A fluid spreading from the neighbour opposite `direction` flows into
    /// `pos` as `fluid`, unless the block there no longer lets it.
    Fluid {
        pos: BlockPos,
        direction: Facing,
        fluid: tuff_vanilla_data::fluid::FluidState,
    },
    /// The part of a placed block that lies across the border, like a bed's
    /// head.
    Place {
        pos: BlockPos,
        state: BlockState,
        cause: crate::world::region_api::BlockCause,
    },
    /// Fire in the neighbour lit the TNT that was at `pos`.
    PrimeTnt { pos: BlockPos },
    /// A piston in the neighbour moves the block at `pos`: its loot drops
    /// first when `drops`, then it becomes `state` (set with the game's
    /// update `flags`), which starts moving `moving` when given. `tick` is
    /// when the piston moved, so a moving block that arrives late catches
    /// up. The piston saw `expected` there; when the block really there
    /// differs and was being carried on to `carry`, the region owning
    /// `carry` is told with [`BlockUpdate::Carried`].
    Piston {
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        drops: bool,
        moving: Option<crate::world::redstone::MovingBlock>,
        tick: u64,
        expected: BlockState,
        carry: Option<BlockPos>,
    },
    /// The block moving into `pos` is really `moved`: the piston that
    /// moved it read a stale copy of its old position.
    Carried { pos: BlockPos, moved: BlockState },
    /// The block entity of the block moving into `pos` from the sender
    /// (`None`: it turned out to have none), which its landing waits for.
    CarriedEntity {
        pos: BlockPos,
        nbt: Option<tuff_nbt::Compound>,
    },
    /// A piston in the neighbour ends the move through `pos` at once.
    FinishMove { pos: BlockPos },
    /// The neighbour broke the block `pos` belongs with (a creative
    /// `player`'s break, or a removal destroying it): it goes too, dropping
    /// its loot when `drops`, if it is still `expected`.
    BrokenWith {
        pos: BlockPos,
        expected: BlockState,
        player: Option<PlayerId>,
        drops: bool,
    },
    /// An explosion in the neighbour breaks the block at `pos`; `decay` is
    /// the radius drops decay with, if they do.
    Exploded {
        pos: BlockPos,
        decay: Option<f32>,
        cause: Box<crate::world::explosion::ExplosionCause>,
    },
}

/// The player who set off an explosion (lit the TNT), for credit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Igniter {
    pub id: PlayerId,
    pub uuid: u128,
    pub name: String,
}

/// An explosion's reach into entities, for a region to apply to its own.
#[derive(Debug, Clone)]
pub struct Blast {
    pub center: [f64; 3],
    pub radius: f32,
    pub damage_type: &'static DamageType,
    pub igniter: Option<Igniter>,
    pub culprit: Option<crate::world::entity::Culprit>,
    /// Set for a wind burst, which pushes but never hurts.
    pub wind: Option<crate::world::explosion::WindBurst>,
}

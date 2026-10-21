use std::sync::Arc;

use tokio::sync::{oneshot, watch};
use tuff_vanilla_data::block::Facing;

use crate::world::entity::RegionEntity;
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use super::*;

#[derive(Debug)]
pub enum RegionInternal {
    NeighborUp(RegionHandle),
    /// Players viewing the container of an entity just handed to this
    /// region; they keep their menu, now served from here.
    EntityViewers {
        entity: i32,
        viewers: Vec<PlayerHandle>,
    },
    NeighborDown(RegionPos),
    /// The boxes of a neighbour's entities reaching into this region, with
    /// whether each is a living thing; empty once none do.
    Footprints {
        from: RegionPos,
        boxes: Vec<([f64; 6], bool)>,
    },
    /// A neighbour's copy of one of its chunks bordering this region, or
    /// `None` once it unloads.
    Halo {
        pos: ChunkPos,
        chunk: Option<Chunk>,
    },
    /// A neighbour started loading one of its chunks bordering this
    /// region; its `Halo` follows once it is installed.
    HaloLoading {
        pos: ChunkPos,
    },
    /// A neighbour's points of interest in one of its chunks near this
    /// region, or `None` once it unloads.
    PoiMirror {
        pos: ChunkPos,
        records: Option<Arc<[tuff_world_model::poi::Record]>>,
    },
    /// Tickets taken on or given back to this region's points of interest
    /// by entities in a neighbour.
    PoiTickets(Vec<(BlockPos, crate::world::poi::Ticket)>),
    /// An entity that moved into this region, with the sequence number of
    /// its last report to viewers.
    Entity {
        entity: Box<dyn RegionEntity>,
        seq: u64,
    },
    /// Command block chains and conditions across a border.
    CommandBlock(CommandRemote),
    /// A dropped item merging with one in a neighbour region.
    BorderMerge(crate::world::items::BorderMerge),
    /// A player's answer to offer `offer` of `item`: it has room for
    /// `taken` (0 refuses). Ignored unless that offer is still the item's
    /// outstanding one, so a late answer or one reaching a region the
    /// item has left grants nothing.
    ItemTaken {
        item: i32,
        offer: u64,
        player: PlayerHandle,
        taken: u8,
    },
    /// A player's answer to shoulder offer `offer` of `entity`. The region
    /// always settles with `PlayerEvent::ShoulderGranted`.
    ShoulderTaken {
        entity: i32,
        offer: u64,
        player: PlayerHandle,
        accepted: bool,
    },
    Place(Place),
    /// A click on a double chest whose half at `pos` this region owns, from
    /// the region owning the other half at `from`.
    OpenHalf {
        place: Place,
        pos: BlockPos,
        from: BlockPos,
        spectator: bool,
        reply: RegionHandle,
    },
    /// The answer to [`Self::OpenHalf`]: the far half, if it opened.
    HalfOpened {
        place: Place,
        spectator: bool,
        half: Option<OpenedHalf>,
    },
    /// The contents of the half of a double chest at `pos`, owned by the
    /// sender and next to this region (`None` once it has no container),
    /// so hoppers, golems and comparators here can read the joined chest.
    HalfSnapshot {
        pos: BlockPos,
        items: Option<Vec<Option<tuff_item::item::ItemSnapshot>>>,
    },
    /// Asks for [`Self::HalfSnapshot`] of the half at `pos`.
    HalfRequest {
        pos: BlockPos,
        reply: RegionHandle,
    },
    /// Puts `stack` into the half at `pos`; what does not fit spills.
    HalfInsert {
        pos: BlockPos,
        face: Option<Facing>,
        stack: ItemStack,
        cause: crate::world::region_api::BlockCause,
    },
    /// A copper golem `golem` at `at` takes up to `stack.count` of `stack`
    /// from the half at `pos`; the owner replies with `HalfDrained`.
    HalfDrain {
        pos: BlockPos,
        stack: tuff_item::item::ItemSnapshot,
        golem: i32,
        at: BlockPos,
        reply: RegionHandle,
        cause: crate::world::region_api::BlockCause,
    },
    /// What the owner of a far half actually removed for golem `golem`.
    HalfDrained {
        golem: i32,
        at: BlockPos,
        stack: ItemStack,
    },
    /// A hopper at `hopper` pulls one item from `slot` of the half at `pos`.
    HalfPull {
        pos: BlockPos,
        slot: usize,
        hopper: BlockPos,
        reply: RegionHandle,
    },
    /// The item a [`Self::HalfPull`] removed, for the hopper to receive.
    HalfPulled {
        hopper: BlockPos,
        stack: ItemStack,
    },
    /// The player no longer views the half at `pos`.
    ReleaseHalf {
        player: PlayerId,
        pos: BlockPos,
    },
    /// Block changes from a neighbour, made on its game tick `tick` (`None`:
    /// as of whenever they apply).
    BlockUpdates {
        tick: Option<u64>,
        updates: Vec<BlockUpdate>,
    },
    /// An explosion in a neighbour reaching this region's players and
    /// items.
    Blast(Blast),
    /// Saves every dirty chunk; `done` is told once they are written.
    SaveAll {
        done: Option<oneshot::Sender<()>>,
        /// Told once the dirty chunks were snapshotted for writing.
        snapshotted: Option<oneshot::Sender<()>>,
    },
    /// A chunk the generation pipeline finished for this region.
    Generated {
        pos: ChunkPos,
        finished: Box<crate::world::generator::Finished>,
    },
    /// Work a feature posts back to the region from elsewhere (an async
    /// search finishing), run on the actor.
    Run(crate::world::region_api::RegionTask),
    /// Area work from a neighbour (or a query's answers), run in the tick
    /// phase [`crate::world::area`] defines.
    Area(crate::world::area::Deferred),
    /// Chunks to keep loaded regardless of viewers, or to release.
    ForceChunks {
        chunks: Vec<ChunkPos>,
        on: bool,
    },
    Shutdown {
        done: watch::Sender<bool>,
    },
}

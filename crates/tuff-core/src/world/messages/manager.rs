use tokio::sync::oneshot;

use crate::world::dimension::DimensionId;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use super::*;

/// Why chunks stay loaded without a viewer. Each kind holds its chunks
/// on its own, so one letting go never unloads another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkTicket {
    /// `/forceload`.
    Forced,
    /// A dragon fight's arena while players are near it.
    DragonFight,
    /// Thrown ender pearls a region's players are flying, held by that
    /// region.
    EnderPearl(RegionPos),
    /// Chunks held for a fixed number of ticks, then let go.
    Timed(TimedTicket),
}

/// Tickets that lapse on their own unless asked for again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimedTicket {
    /// A player's spawn point while they are being placed there.
    PlayerSpawn,
    /// The arrival of an entity that went through a portal.
    Portal,
}

impl TimedTicket {
    pub const fn timeout(self) -> u64 {
        match self {
            Self::PlayerSpawn => 20,
            Self::Portal => 300,
        }
    }

    /// How many chunks around the centre the ticket holds.
    pub const fn radius(self) -> i32 {
        3
    }

    /// The chunks a ticket centred on `center` holds.
    pub fn chunks_around(self, center: ChunkPos) -> Vec<ChunkPos> {
        let radius = self.radius();
        (-radius..=radius)
            .flat_map(|dz| {
                (-radius..=radius).map(move |dx| ChunkPos::new(center.x + dx, center.z + dz))
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum ManagerMessage {
    Acquire {
        dimension: DimensionId,
        region: RegionPos,
        reply: oneshot::Sender<RegionHandle>,
    },
    Release {
        dimension: DimensionId,
        region: RegionPos,
    },
    /// Handles of every running region, without taking tickets.
    Regions {
        reply: oneshot::Sender<Vec<RegionHandle>>,
    },
    /// `/save-all`: every region writes its dirty chunks now; `done` is
    /// told once they and the forced chunks are written.
    SaveAll {
        done: Option<oneshot::Sender<()>>,
        /// Told once every region has taken its snapshot.
        snapshotted: Option<oneshot::Sender<()>>,
        /// The world save these chunk writes belong to.
        epoch: Option<tuff_storage::journal::BarrierHandle>,
    },
    /// Keeps `chunks` loaded (`on`) under `ticket` or lets that ticket
    /// go, and answers which of them changed state for that ticket.
    ForceLoad {
        dimension: DimensionId,
        chunks: Vec<ChunkPos>,
        ticket: ChunkTicket,
        on: bool,
        reply: oneshot::Sender<Vec<bool>>,
    },
    /// Holds the chunks around `center` under a timed `ticket`, restarting
    /// its timeout on chunks it already holds.
    Hold {
        dimension: DimensionId,
        center: ChunkPos,
        ticket: TimedTicket,
    },
    /// The chunks of a dimension `/forceload` holds.
    ForcedChunks {
        dimension: DimensionId,
        reply: oneshot::Sender<Vec<ChunkPos>>,
    },
    /// Closes every region of a discarded runtime dimension and forgets
    /// its held chunks; `done` is told once the regions are down.
    DropDimension {
        dimension: DimensionId,
        done: oneshot::Sender<()>,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
    /// Test hook: panics the manager's loop with this message.
    #[cfg(feature = "test-hooks")]
    Panic(&'static str),
}

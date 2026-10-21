use super::*;

/// Neighbour regions, the halo and cross-region update queues.
pub(super) struct Border {
    pub(super) neighbors: HashMap<RegionPos, RegionHandle>,
    pub(super) halo: ChunkMap<Chunk>,
    pub(super) halo_dirty: HashSet<ChunkPos>,
    /// Chunks of neighbour regions that are loading there or whose halo
    /// copy has not been through a light job here yet, with the tick they
    /// were first seen. Packets of chunks touching them wait.
    pub(super) halo_pending: HashMap<ChunkPos, u64>,
    /// Block updates from neighbouring regions with the tick they were
    /// made on, waiting for the tick phase that applies them.
    pub(super) pending_updates: Vec<(Option<u64>, Vec<BlockUpdate>)>,
    /// Area broadcasts and queries waiting for their tick phase.
    pub(super) area: crate::world::area::Queue,
    /// Chunks whose neighbourhood is loaded, dropped whenever a column or
    /// halo chunk is added or removed.
    pub(super) neighbourhood_cache: Option<Vec<ChunkPos>>,
}

impl Border {
    pub(super) fn new(neighbors: HashMap<RegionPos, RegionHandle>) -> Self {
        Self {
            neighbors,
            halo: ChunkMap::default(),
            halo_dirty: HashSet::new(),
            halo_pending: HashMap::new(),
            pending_updates: Vec::new(),
            area: Default::default(),
            neighbourhood_cache: None,
        }
    }

    pub(super) fn region_of_neighbour(&self, pos: BlockPos) -> Option<&RegionHandle> {
        self.neighbors.get(&ChunkPos::of_block(pos).region())
    }
}

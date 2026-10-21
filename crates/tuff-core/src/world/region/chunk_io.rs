use super::*;

/// Chunk loading, generation, storage and point-of-interest state.
pub(super) struct ChunkIo {
    pub(super) loading: HashMap<ChunkPos, Loading>,
    /// Viewers of chunks that turned out not to exist, given the chunk if it
    /// is ever made for someone else.
    pub(super) peekers: HashMap<ChunkPos, HashMap<PlayerId, PlayerHandle>>,
    /// Loading chunks waiting on the generation pipeline.
    pub(super) generating: HashMap<ChunkPos, Prepared>,
    /// Chunks whose generation request was withdrawn; one still delivered
    /// is installed rather than lost.
    pub(super) withdrawn: HashSet<ChunkPos>,
    /// `None` while away on an I/O job.
    /// Whether a batch of chunk I/O is out with storage.
    pub(super) io_busy: bool,
    pub(super) io_queue: Vec<IoOp>,
    /// Chunks pre-generation jobs wait on.
    pub(super) pregen: pregen::Pending,
    /// Chunks tickets keep loaded (`/forceload`, the dragon fight).
    pub(super) forced: HashSet<ChunkPos>,
    pub(super) chunk_queries: Vec<ChunkQuery>,
    /// Points of interest in the loaded chunks.
    pub(super) pois: crate::world::poi::PoiStore,
    pub(super) anti_xray: xray::AntiXray,
}

impl ChunkIo {
    pub(super) fn new(pos: RegionPos, anti_xray: xray::AntiXray) -> Self {
        Self {
            loading: HashMap::new(),
            peekers: HashMap::new(),
            generating: HashMap::new(),
            withdrawn: HashSet::new(),
            io_busy: false,
            io_queue: Vec::new(),
            pregen: Default::default(),
            forced: HashSet::new(),
            chunk_queries: Vec::new(),
            pois: crate::world::poi::PoiStore::for_region(pos),
            anti_xray,
        }
    }

    pub(super) fn poi_mirror(
        &self,
        chunk: ChunkPos,
    ) -> Option<Arc<[tuff_world_model::poi::Record]>> {
        let records = self.pois.records(chunk);
        (!records.is_empty()).then(|| records.into())
    }

    /// Lets the parked loads go to storage.
    pub(super) fn release_parked(&mut self) {
        for pos in std::mem::take(&mut self.pregen.parked) {
            if self.loading.contains_key(&pos) {
                self.io_queue.push(IoOp::Read(pos));
            }
        }
    }
}

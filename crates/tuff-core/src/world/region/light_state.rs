use super::*;

/// Lighting work and pending light updates.
pub(super) struct LightState {
    /// Away on a rayon job while `None`.
    pub(super) lighting: Option<Box<Lighting>>,
    pub(super) light_ops: Vec<LightOp>,
    /// Halo chunks added by the light job in flight.
    pub(super) halo_in_job: Vec<ChunkPos>,
    /// Sky and block light sections changed in chunks viewers already
    /// hold, sent once per tick.
    pub(super) light_dirty: ChunkMap<loading::LightDirty>,
    pub(super) sky_darken: u8,
}

impl LightState {
    pub(super) fn new(lighting: Lighting) -> Self {
        Self {
            lighting: Some(Box::new(lighting)),
            light_ops: Vec::new(),
            halo_in_job: Vec::new(),
            light_dirty: ChunkMap::default(),
            sky_darken: 0,
        }
    }

    pub(super) fn light_idle(&self) -> bool {
        self.lighting.is_some() && self.light_ops.is_empty()
    }
}

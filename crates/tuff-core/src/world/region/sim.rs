use super::*;

/// Fluid, redstone and scheduled tick simulation state.
pub(super) struct Sim {
    pub(super) fluid_settings: crate::world::fluid_api::Settings,
    pub(super) fluid_outbox: fluids::Outbox,
    /// Orders ticks scheduled for the same game tick.
    pub(super) tick_order: i64,
    /// The state of the game's cheap generator for random tick positions.
    pub(super) rand_value: i32,
    pub(super) redstone: redstone::RedstoneState,
    /// What block changes are attributed to now.
    pub(super) cause: crate::world::region_api::BlockCause,
    /// The hooks that observe block changes, read once from the features.
    pub(super) block_watchers: std::cell::OnceCell<BlockWatchers>,
    /// The hooks that observe container changes.
    pub(super) container_watchers: std::cell::OnceCell<BlockWatchers>,
}

pub(super) type BlockWatchers = Arc<[Arc<dyn crate::world::region_api::RegionHooks>]>;

impl Default for Sim {
    fn default() -> Self {
        Self {
            fluid_settings: crate::world::fluid_api::Settings {
                fast_lava: false,
                water_source_conversion: false,
                lava_source_conversion: false,
            },
            fluid_outbox: HashMap::new(),
            tick_order: 0,
            rand_value: 0,
            redstone: Default::default(),
            cause: Default::default(),
            block_watchers: Default::default(),
            container_watchers: Default::default(),
        }
    }
}

impl Sim {
    pub(super) fn next_tick_order(&mut self) -> i64 {
        self.tick_order += 1;
        self.tick_order
    }

    /// Marks the block ticks about to run, for
    /// [`RedstoneLevel::will_tick_this_tick`].
    pub(super) fn begin_tick_batch(&mut self, ticks: &[crate::world::ticks::Scheduled<Block>]) {
        self.redstone.ticks_this_tick = ticks.iter().map(|tick| (tick.pos, tick.kind)).collect();
    }

    pub(super) fn tick_ran(&mut self, pos: BlockPos, block: Block) {
        self.redstone.ticks_this_tick.remove(&(pos, block));
    }
}

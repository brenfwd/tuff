//! Scheduled block and fluid ticks in a region, and the region as the
//! world fluids (`crate::world::fluid_api`) read and write.
//!
//! Ticks only run in chunks whose eight neighbours are loaded (owned or in
//! the halo), which is as far as a fluid tick reads, so nothing ever reads
//! an unloaded block; the rest wait, as ticks do in the game's non-ticking
//! chunks. Spreads into a neighbouring region travel as
//! [`BlockUpdate::Fluid`] and are checked again where they land.

use std::collections::{HashMap, HashSet};

use rand::Rng;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::fluid::{Fluid, FluidState, LiquidPlacement};

use super::{ChunkPos, RegionActor};
use crate::protocol::play;
use crate::world::fluid_api::{FluidWorld, Settings};
use crate::world::messages::BlockUpdate;
use crate::world::region_api::BlockCause;
use crate::world::ticks::{self, ChunkTicks, MAX_PER_TICK, Scheduled};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::RegionPos;

const POST_PROCESSING: &str = "PostProcessing";

impl RegionActor {
    /// The fluid settings of this dimension under the current game rules.
    pub(super) fn current_fluid_settings(&self) -> Settings {
        Settings {
            fast_lava: tuff_vanilla_data::fluid::fast_lava(self.dimension().kind.name),
            water_source_conversion: self.shared.rule_bool("water_source_conversion"),
            lava_source_conversion: self.shared.rule_bool("lava_source_conversion"),
        }
    }

    pub(super) fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: Fluid, delay: u32) {
        let order = self.sim.next_tick_order();
        let due = self.now() + delay as u64;
        if let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) {
            column.fluid_ticks.schedule(Scheduled {
                pos,
                kind: fluid,
                due,
                priority: 0,
                order,
            });
            column.blocks_dirty = true;
        }
    }

    /// Whether `chunk` and its eight neighbours are all loaded here or in
    /// the halo.
    pub(super) fn neighbourhood_loaded(&self, chunk: ChunkPos) -> bool {
        if self.chunk_io.forced.contains(&chunk) && self.columns.contains_key(&chunk) {
            return true;
        }
        (-1..=1).all(|dx| {
            (-1..=1).all(|dz| {
                let at = ChunkPos::new(chunk.x + dx, chunk.z + dz);
                self.columns.contains_key(&at) || self.border.halo.contains_key(&at)
            })
        })
    }

    pub(super) fn loaded_neighbourhoods(&self) -> Vec<ChunkPos> {
        let mut loaded: Vec<ChunkPos> = self
            .columns
            .keys()
            .filter(|pos| self.neighbourhood_loaded(**pos))
            .copied()
            .collect();
        loaded.sort_by_key(|pos| (pos.x, pos.z));
        loaded
    }

    /// Positions of every player that can keep a chunk of this region
    /// simulated, or `None` while the simulation distance reaches the view
    /// distance (everything simulates). Past what one region's player index
    /// answers, the neighbouring regions' are read too.
    fn simulation_sources(&self) -> Option<Vec<[f64; 3]>> {
        if self.shared.live.simulation_distance() >= self.shared.live.view_distance() {
            return None;
        }
        let sim = self.shared.live.simulation_distance();
        let mut sources: Vec<[f64; 3]> = self
            .ents
            .presence
            .values()
            .filter(|p| p.alive)
            .map(|p| p.position)
            .collect();
        let index = self.shared.players.borrow();
        let span = i32::from((sim + 1) * 16 > crate::world::nearby::REACH as i32);
        for dx in -span..=span {
            for dz in -span..=span {
                let region = RegionPos::new(self.pos.x + dx, self.pos.z + dz);
                sources.extend(
                    index
                        .near(self.dimension, region)
                        .iter()
                        .map(|p| p.position),
                );
                sources.extend_from_slice(index.spectators_near(self.dimension, region));
            }
        }
        Some(sources)
    }

    fn near_any(sources: &[[f64; 3]], sim: i32, chunk: ChunkPos) -> bool {
        sources.iter().any(|position| {
            let x = (position[0] / 16.0).floor() as i32;
            let z = (position[2] / 16.0).floor() as i32;
            (x - chunk.x).abs() <= sim && (z - chunk.z).abs() <= sim
        })
    }

    /// Whether a player is within the simulation distance of `chunk`;
    /// always true while that reaches the view distance.
    pub(super) fn simulates(&self, chunk: ChunkPos) -> bool {
        match self.simulation_sources() {
            None => true,
            Some(sources) => {
                Self::near_any(&sources, self.shared.live.simulation_distance(), chunk)
            }
        }
    }

    /// Chunks with work in `pending` that may run it now, in a fixed order.
    pub(super) fn ready_chunks(&self, pending: impl Fn(&super::Column) -> bool) -> Vec<ChunkPos> {
        let fresh;
        let loaded = match &self.border.neighbourhood_cache {
            Some(cached) => cached,
            None => {
                fresh = self.loaded_neighbourhoods();
                &fresh
            }
        };
        let sources = self.simulation_sources();
        let sim = self.shared.live.simulation_distance();
        loaded
            .iter()
            .filter(|pos| {
                self.columns.get(pos).is_some_and(&pending)
                    && sources
                        .as_ref()
                        .is_none_or(|s| Self::near_any(s, sim, **pos))
            })
            .copied()
            .collect()
    }

    fn due_ticks<T: Copy + Eq + std::hash::Hash>(
        &mut self,
        list: impl Fn(&mut super::Column) -> &mut ChunkTicks<T>,
        ready: &[ChunkPos],
    ) -> Vec<Scheduled<T>> {
        let ready: HashSet<&ChunkPos> = ready.iter().collect();
        let mut columns: Vec<(&ChunkPos, &mut super::Column)> = self
            .columns
            .iter_mut()
            .filter(|(pos, _)| ready.contains(pos))
            .collect();
        columns.sort_by_key(|(pos, _)| (pos.x, pos.z));
        let mut chunks: Vec<&mut ChunkTicks<T>> = columns
            .into_iter()
            .map(|(_, column)| list(column))
            .collect();
        ticks::drain(&mut chunks, self.sched.tick, MAX_PER_TICK)
    }

    /// Runs the block ticks that are due, in chunks whose neighbours are
    /// loaded (redstone reads around its block).
    pub(super) fn run_block_ticks(&mut self) {
        let _span = tuff_timings::span!("region.scheduled_ticks");
        let ready = self.ready_chunks(|column| !column.block_ticks.is_empty());
        if ready.is_empty() {
            return;
        }
        let due = self.due_ticks(|column| &mut column.block_ticks, &ready);
        self.sim.begin_tick_batch(&due);
        self.sim.redstone.handling_tick = true;
        for tick in due {
            self.sim.tick_ran(tick.pos, tick.kind);
            self.block_tick(tick.pos, tick.kind);
        }
        self.sim.redstone.handling_tick = false;
        self.sim.begin_tick_batch(&[]);
    }

    /// Runs the fluid ticks that are due.
    pub(super) fn run_fluid_ticks(&mut self) {
        let _span = tuff_timings::span!("region.fluid_ticks");
        let ready = self.ready_chunks(|column| !column.fluid_ticks.is_empty());
        if ready.is_empty() {
            return;
        }
        for tick in self.due_ticks(|column| &mut column.fluid_ticks, &ready) {
            let fluids = self.shared.fluid_behaviors.clone();
            let cause = fluid_cause(tick.kind);
            self.with_cause(cause, |region| fluids.tick(region, tick.pos, tick.kind));
        }
        self.flush_fluid_outbox();
    }

    /// Runs the post-processing of chunks that became ready for it (lit,
    /// with their neighbours loaded): the first fluid tick of marked fluids,
    /// and marked blocks taking the shape their neighbours give them.
    pub(super) fn run_post_processing(&mut self) {
        let _span = tuff_timings::span!("region.post_processing");
        let ready = self.ready_chunks(|column| column.lit && !column.post_processing.is_empty());
        for chunk in ready {
            let Some(column) = self.columns.get_mut(&chunk) else {
                continue;
            };
            let positions = std::mem::take(&mut column.post_processing);
            column.blocks_dirty = true;
            for pos in positions {
                self.post_process(pos);
            }
        }
        self.flush_fluid_outbox();
    }

    fn post_process(&mut self, pos: BlockPos) {
        let fluids = self.shared.fluid_behaviors.clone();
        if let Some(shaped) = fluids.post_process(self, pos) {
            self.set_block(pos, shaped);
        }
    }

    fn flush_fluid_outbox(&mut self) {
        let outbox = std::mem::take(&mut self.sim.fluid_outbox);
        self.send_block_updates(outbox);
    }

    /// A neighbour of the fluid-holding block at `pos` changed (the
    /// game's neighbour-changed update).
    pub(super) fn fluid_neighbour_changed(&mut self, pos: BlockPos) {
        let Some(state) = self.block(pos) else {
            return;
        };
        if state.fluid_state().is_empty() || !self.owns(pos) {
            return;
        }
        let fluids = self.shared.fluid_behaviors.clone();
        fluids.liquid_changed(self, pos, state);
    }

    /// The neighbour of the fluid-holding block at `pos` became
    /// `neighbour` (a shape update).
    pub(super) fn fluid_shape_changed(&mut self, pos: BlockPos, neighbour: BlockState) {
        let Some(state) = self.block(pos) else {
            return;
        };
        if state.fluid_state().is_empty() || !self.owns(pos) {
            return;
        }
        let fluids = self.shared.fluid_behaviors.clone();
        fluids.shape_changed(self, pos, state, neighbour);
    }

    /// A liquid block was just set at `pos`.
    pub(super) fn liquid_placed(&mut self, pos: BlockPos, state: BlockState) {
        let fluids = self.shared.fluid_behaviors.clone();
        fluids.liquid_changed(self, pos, state);
    }

    /// Applies a spread a neighbouring region computed, if it still holds.
    pub(super) fn receive_spread(&mut self, pos: BlockPos, direction: Facing, fluid: FluidState) {
        let fluids = self.shared.fluid_behaviors.clone();
        if !self.owns(pos) || !fluids.still_flows(self, pos, direction, fluid) {
            return;
        }
        if let Some(state) = self.block(pos) {
            fluids.apply_spread(self, pos, state, direction, fluid);
        }
    }

    /// A liquid container takes a liquid (a bucket poured into it, or a
    /// flow): the new state, the tick and the placement sounds.
    pub(super) fn apply_liquid_placement(
        &mut self,
        pos: BlockPos,
        placement: &'static LiquidPlacement,
    ) {
        self.put_block(pos, placement.result);
        if let Some(fluid) = placement.tick
            && let Some(rules) = fluid.rules()
        {
            let delay = rules.tick_delay(self.sim.fluid_settings.fast_lava);
            self.schedule_fluid_tick(pos, fluid, delay);
        }
        for (sound, volume, pitch) in placement.sounds {
            self.play_sound(*sound, pos, *volume, *pitch, None);
        }
    }
}

impl FluidWorld for RegionActor {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        RegionActor::block(self, pos)
    }

    fn settings(&self) -> Settings {
        self.sim.fluid_settings
    }

    fn random(&mut self, bound: u32) -> u32 {
        self.rng.gen_range(0..bound.max(1))
    }

    fn set(&mut self, pos: BlockPos, state: BlockState) {
        self.put_block(pos, state);
    }

    fn schedule(&mut self, pos: BlockPos, fluid: Fluid, delay: u32) {
        self.schedule_fluid_tick(pos, fluid, delay);
    }

    fn raw_brightness(&self, pos: BlockPos) -> Option<u8> {
        use tuff_world_model::light::LightLayer;
        let column = self.columns.get(&ChunkPos::new(pos.x >> 4, pos.z >> 4))?;
        let light = column.chunk.light().filter(|_| column.lit)?;
        let dimension = self.dimension();
        let shape = dimension.shape;
        let block = light
            .value(shape, LightLayer::Block, pos.x, pos.y, pos.z)
            .unwrap_or(0);
        let sky = if dimension.has_sky_light() {
            light.value(shape, LightLayer::Sky, pos.x, pos.y, pos.z)?
        } else {
            0
        };
        Some(sky.max(block))
    }

    fn wash_away(&mut self, pos: BlockPos, _state: BlockState) {
        let Some(broken) = self.claim(pos, crate::world::block_ledger::ClaimReason::Washed) else {
            return;
        };
        if !self.shared.rule_bool("block_drops") {
            return;
        }
        let stacks = self.shared.block_loot().destroyed(broken, &mut self.rng);
        self.drop_items(pos, stacks);
    }

    fn place_liquid(&mut self, pos: BlockPos, placement: &'static LiquidPlacement) {
        self.apply_liquid_placement(pos, placement);
    }

    fn fizz(&mut self, pos: BlockPos) {
        self.level_event(play::level_event::LAVA_FIZZ, pos, 0, false);
    }

    fn spread_to(&mut self, pos: BlockPos, direction: Facing, fluid: FluidState) {
        let chunk = ChunkPos::of_block(pos);
        if self.pos.contains(chunk) {
            if self.owns(pos)
                && let Some(state) = self.block(pos)
            {
                let fluids = self.shared.fluid_behaviors.clone();
                fluids.apply_spread(self, pos, state, direction, fluid);
            }
            return;
        }
        self.sim
            .fluid_outbox
            .entry(chunk.region())
            .or_default()
            .push(BlockUpdate::Fluid {
                pos,
                direction,
                fluid,
            });
    }
}

/// The positions a loaded chunk's `PostProcessing` list marks, taken out of
/// its data.
pub(super) fn take_post_processing(
    extra: &mut Compound,
    chunk: ChunkPos,
    shape: ChunkShape,
) -> Vec<BlockPos> {
    let Some(Tag::List(sections)) = extra.remove(POST_PROCESSING) else {
        return Vec::new();
    };
    let min_section = shape.min_y >> 4;
    let mut positions = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let Tag::List(packed) = section else {
            continue;
        };
        let section_y = min_section + index as i32;
        for entry in packed {
            let Tag::Short(packed) = entry else {
                continue;
            };
            let packed = *packed as u16 as i32;
            positions.push(BlockPos::new(
                (chunk.x << 4) + (packed & 15),
                (section_y << 4) + (packed >> 4 & 15),
                (chunk.z << 4) + (packed >> 8 & 15),
            ));
        }
    }
    positions
}

/// Stores positions still waiting for post-processing in the chunk's
/// `PostProcessing` list, one list of packed positions per section.
pub(super) fn store_post_processing(
    extra: &mut Compound,
    positions: &[BlockPos],
    shape: ChunkShape,
) {
    if positions.is_empty() {
        return;
    }
    let min_section = shape.min_y >> 4;
    let mut sections = vec![Vec::new(); shape.sections()];
    for pos in positions {
        let Some(section) = sections.get_mut(((pos.y >> 4) - min_section) as usize) else {
            continue;
        };
        let packed = (pos.x & 15) | (pos.y & 15) << 4 | (pos.z & 15) << 8;
        section.push(Tag::Short(packed as i16));
    }
    extra.insert(
        POST_PROCESSING,
        Tag::List(sections.into_iter().map(Tag::List).collect()),
    );
}

/// Where the fluid spreads queued for neighbouring regions wait until the
/// batch that produced them ends.
pub(super) type Outbox = HashMap<RegionPos, Vec<BlockUpdate>>;

/// Water or lava flowing, as the block it flows from.
pub(super) fn fluid_cause(fluid: Fluid) -> BlockCause {
    match fluid.rules().map(|rules| &rules.behaviour) {
        Some(tuff_vanilla_data::fluid::FluidBehaviour::Water) => {
            BlockCause::Block(tuff_vanilla_data::Block::Water)
        }
        Some(tuff_vanilla_data::fluid::FluidBehaviour::Lava) => {
            BlockCause::Block(tuff_vanilla_data::Block::Lava)
        }
        None => BlockCause::World,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn post_processing_round_trips_through_stored_chunk_data() {
        let shape = ChunkShape::OVERWORLD;
        let chunk = ChunkPos::new(-2, 3);
        let positions = vec![BlockPos::new(-17, -60, 63), BlockPos::new(-32, 100, 48)];
        let mut extra = Compound::new();
        store_post_processing(&mut extra, &positions, shape);
        let Some(Tag::List(sections)) = extra.get(POST_PROCESSING) else {
            panic!("stored");
        };
        assert_eq!(sections.len(), shape.sections());
        assert_eq!(take_post_processing(&mut extra, chunk, shape), positions);
        assert!(extra.get(POST_PROCESSING).is_none());
    }
}

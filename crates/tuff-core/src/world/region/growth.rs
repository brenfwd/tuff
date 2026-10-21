//! Random ticks in a region, and the region as the world block behaviours
//! ([`crate::world::growth::GrowthLevel`]) read and write.
//!
//! Like fluid ticks, random ticks only run in chunks whose eight neighbours
//! are loaded here or in the halo, standing in for the game's ticking
//! chunks, and only once the chunk is lit.

use std::collections::HashMap;

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::{Block, BlockState};

use super::{ChunkPos, RegionActor};
use crate::world::growth::{self, GrowthLevel};
use crate::world::messages::BlockUpdate;
use crate::world::placement;
use crate::world::redstone::{RedstoneLevel, TickPriority, UPDATE_ALL, UPDATE_LIMIT};
use crate::world::region_api::BlockCause;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, is_air, ticks_randomly};
use tuff_world_model::light::LightLayer;

/// The blocks one chunk's random ticks and weather landed on this tick.
#[derive(Default)]
struct RandomTicks {
    precipitation: Vec<BlockPos>,
    blocks: Vec<BlockPos>,
}

/// Draws one chunk's random tick and weather positions from its own
/// stream, seeded by the region's draw for the tick and the chunk, so
/// chunks plan independently. Keeps the blocks that tick randomly now;
/// the caller re-reads each before ticking it.
fn plan_random_ticks(
    column: Option<&super::Column>,
    chunk: ChunkPos,
    speed: i32,
    seed: u64,
) -> RandomTicks {
    use rand::SeedableRng;
    let mut plan = RandomTicks::default();
    let Some(column) = column else {
        return plan;
    };
    let key = (u64::from(chunk.x as u32) << 32) | u64::from(chunk.z as u32);
    let mut rng = StdRng::seed_from_u64(seed ^ key.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut rand_value: i32 = rng.r#gen();
    let (min_x, min_z) = (chunk.x << 4, chunk.z << 4);
    for _ in 0..speed {
        if rng.gen_range(0..48) == 0 {
            plan.precipitation.push(growth::random_block_pos(
                &mut rand_value,
                min_x,
                0,
                min_z,
                15,
            ));
        }
    }
    let chunk_data = &column.chunk;
    let shape = chunk_data.shape();
    let mut next = 0;
    while let Some(index) = chunk_data.next_ticking_section(next) {
        next = index + 1;
        let section = chunk_data.section(index);
        let base = shape.section_base(index);
        for _ in 0..speed {
            let pos = growth::random_block_pos(&mut rand_value, min_x, base, min_z, 15);
            let local = (((pos.y - base) as usize) << 8)
                | (((pos.z & 15) as usize) << 4)
                | (pos.x & 15) as usize;
            if ticks_randomly(section.get(local)) {
                plan.blocks.push(pos);
            }
        }
    }
    plan
}

/// The fewest chunks per pool task when random tick planning goes
/// parallel: `TUFF_RANDOM_TICK_PARALLEL_MIN`, else the default. Below
/// twice this many ticking chunks the plan runs on the tick's own thread.
fn random_tick_parallel_min() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("TUFF_RANDOM_TICK_PARALLEL_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256)
            .max(1)
    })
}

impl RegionActor {
    pub(super) fn random_block_pos(&mut self, x: i32, y: i32, z: i32, y_mask: i32) -> BlockPos {
        growth::random_block_pos(&mut self.sim.rand_value, x, y, z, y_mask)
    }

    /// Runs this tick's random ticks: per ticking chunk, snow and ice from
    /// the weather, then `random_tick_speed` blocks of every section that
    /// has blocks that tick randomly.
    pub(super) fn run_random_ticks(&mut self) {
        use rayon::prelude::*;
        let _span = tuff_timings::span!("region.random_ticks");
        let speed = self.shared.rule_int("random_tick_speed").max(0);
        let ready = self.ready_chunks(|column| column.lit);
        let behaviours = self.shared.growth_behaviors.clone();
        for chunk in &ready {
            if let Some(column) = self.columns.get_mut(chunk) {
                column.inhabited_time += 1;
            }
        }
        if speed == 0 {
            return;
        }
        let seed = u64::from(self.sim.rand_value as u32)
            ^ self.sched.tick.wrapping_mul(0xD1B5_4A32_D192_ED03);
        let columns = &self.columns;
        let plan = |&chunk: &ChunkPos| plan_random_ticks(columns.get(&chunk), chunk, speed, seed);
        let min_len = random_tick_parallel_min();
        let plans: Vec<RandomTicks> = if ready.len() < min_len.saturating_mul(2) {
            ready.iter().map(plan).collect()
        } else {
            let tags = tuff_datapack::tags::current();
            crate::threads::tick_pool().install(|| {
                ready
                    .par_iter()
                    .with_min_len(min_len)
                    .map(|chunk| tuff_datapack::tags::sync_scope(tags.clone(), || plan(chunk)))
                    .collect()
            })
        };
        for plan in plans {
            for pos in plan.precipitation {
                self.tick_precipitation(pos);
            }
            for pos in plan.blocks {
                let Some(state) = self.block(pos).filter(|s| ticks_randomly(*s)) else {
                    continue;
                };
                self.with_cause(BlockCause::Block(state.block()), |region| {
                    behaviours.random_tick(region, pos, state)
                });
            }
        }
    }

    /// Freezes water and piles snow at the top of the column holding `pos`,
    /// and lets the block under the top react to the precipitation.
    fn tick_precipitation(&mut self, pos: BlockPos) {
        let Some(top_y) = self.motion_blocking_height(pos.x, pos.z) else {
            return;
        };
        let top = BlockPos::new(pos.x, top_y, pos.z);
        let below = BlockPos::new(pos.x, top_y - 1, pos.z);
        let behaviours = self.shared.growth_behaviors.clone();
        self.with_cause(BlockCause::Weather, |region| {
            behaviours.precipitation(region, top, below)
        });
    }

    /// Whether rain falls on a thing standing at `position` whose top is
    /// at `top`: on its feet's block or on the block at its top.
    pub(super) fn rained_on(&self, position: [f64; 3], top: f64) -> bool {
        if !GrowthLevel::is_raining(self) {
            return false;
        }
        let feet = BlockPos::containing(position);
        let head = BlockPos::new(feet.x, top.floor() as i32, feet.z);
        let behaviours = self.shared.growth_behaviors.clone();
        behaviours.rains_at(self, feet) || behaviours.rains_at(self, head)
    }

    pub(super) fn light_at(&self, pos: BlockPos) -> Option<(u8, u8)> {
        let chunk_pos = ChunkPos::of_block(pos);
        let chunk: &Chunk = match self.columns.get(&chunk_pos) {
            Some(column) if column.lit => &column.chunk,
            Some(_) => return None,
            None => self.border.halo.get(&chunk_pos)?,
        };
        let light = chunk.light()?;
        let dimension = self.dimension();
        let shape = dimension.shape;
        let block = light
            .value(shape, LightLayer::Block, pos.x, pos.y, pos.z)
            .unwrap_or(0);
        let sky = if dimension.has_sky_light() {
            light
                .value(shape, LightLayer::Sky, pos.x, pos.y, pos.z)
                .unwrap_or(15)
        } else {
            0
        };
        Some((sky, block))
    }

    /// Removes the block at `pos` as breaking it would, leaving its fluid.
    fn remove_block(&mut self, pos: BlockPos, drop: bool) {
        if !self.owns(pos) {
            return;
        }
        let Some(old) = self.block(pos) else {
            return;
        };
        if is_air(old) {
            return;
        }
        let saved = self.saved_block_entity(pos);
        if old.block().random_tick_kind() != Some("leaves") {
            self.break_event(pos, old, None);
        }
        if drop && self.shared.rule_bool("block_drops") {
            let stacks = self
                .shared
                .block_loot()
                .destroyed(old, saved.as_ref(), &mut self.rng);
            self.drop_items(pos, stacks);
        }
        self.set_block_flags(pos, placement::remains(old), UPDATE_ALL, UPDATE_LIMIT);
    }

    /// Lets the block at `pos` react to its neighbour in `direction`
    /// becoming `neighbour` before it takes its new shape, as leaves and
    /// coral schedule a tick.
    pub(super) fn shape_tick(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        direction: tuff_vanilla_data::block::Facing,
        neighbour: BlockState,
    ) {
        if !self.shared.growth_behaviors.has_neighbour(state.block()) {
            return;
        }
        let behaviours = self.shared.growth_behaviors.clone();
        behaviours.neighbour_changed(self, pos, state, direction, neighbour);
    }

    /// Runs a due block tick: the placement rules' tick first, then the
    /// block's scheduled behaviour, else its redstone reaction.
    pub(super) fn block_tick(&mut self, pos: BlockPos, kind: Block) {
        self.with_cause(BlockCause::Block(kind), |region| {
            region.run_block_tick(pos, kind)
        });
    }

    fn run_block_tick(&mut self, pos: BlockPos, kind: Block) {
        let Some(state) = self.block(pos) else {
            return;
        };
        if state.block() != kind || self.placement_tick(pos, state) {
            return;
        }
        if self.shared.growth_behaviors.has_scheduled(kind) {
            let behaviours = self.shared.growth_behaviors.clone();
            behaviours.scheduled_tick(self, pos, state);
        } else {
            self.redstone_tick(pos, state);
        }
    }

    fn players_near(&self, pos: BlockPos, radius: i32) -> bool {
        let center = [pos.x as f64, pos.y as f64, pos.z as f64];
        let close = |p: [f64; 3]| {
            let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < radius as f64
        };
        if self
            .ents
            .presence
            .values()
            .any(|p| p.is_physical() && close(p.position))
        {
            return true;
        }
        let index = self.shared.players.borrow();
        index
            .near(self.dimension, self.pos)
            .iter()
            .any(|p| close(p.position))
    }
}

impl GrowthLevel for RegionActor {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        RegionActor::block(self, pos)
    }

    fn set(&mut self, pos: BlockPos, state: BlockState, flags: u32) {
        if self.owns(pos) {
            self.set_block_flags(pos, state, flags, UPDATE_LIMIT);
        } else {
            self.put_block(pos, state);
        }
    }

    fn destroy(&mut self, pos: BlockPos, drop: bool) {
        self.remove_block(pos, drop);
    }

    fn drop_item(&mut self, pos: BlockPos, stack: tuff_item::item::ItemStack) {
        if self.owns(pos) {
            self.drop_items(pos, vec![stack]);
        }
    }

    fn drop_resources(&mut self, pos: BlockPos) {
        if !self.owns(pos) || !self.shared.rule_bool("block_drops") {
            return;
        }
        let Some(state) = self.block(pos) else {
            return;
        };
        let saved = self.saved_block_entity(pos);
        let stacks = self
            .shared
            .block_loot()
            .destroyed(state, saved.as_ref(), &mut self.rng);
        self.drop_items(pos, stacks);
    }

    fn rng(&mut self) -> &mut StdRng {
        &mut self.rng
    }

    fn container_open_count(&self, pos: BlockPos) -> usize {
        self.menus.viewers.get(&pos).map_or(0, Vec::len)
    }

    fn light(&self, pos: BlockPos) -> Option<(u8, u8)> {
        self.light_at(pos)
    }

    fn sky_darken(&self) -> u8 {
        self.light.sky_darken
    }

    fn min_y(&self) -> i32 {
        self.dimension().shape.min_y
    }

    fn max_y(&self) -> i32 {
        self.dimension().shape.max_y()
    }

    fn sea_level(&self) -> i32 {
        self.dimension().sea_level()
    }

    fn biome_climate(&self, biome: u16) -> tuff_world_model::biomes::BiomeClimate {
        self.shared
            .dimensions
            .biomes()
            .climate(biome)
            .unwrap_or_else(|| tuff_world_model::biomes::BiomeClimate::vanilla(biome))
    }

    fn biome(&self, pos: BlockPos) -> Option<u16> {
        let chunk_pos = ChunkPos::of_block(pos);
        let chunk = self
            .columns
            .get(&chunk_pos)
            .map(|c| &c.chunk)
            .or_else(|| self.border.halo.get(&chunk_pos))?;
        Some(chunk.biome(pos.x, pos.y, pos.z))
    }

    fn is_raining(&self) -> bool {
        let kind = self.dimension().kind;
        kind.has_skylight && !kind.has_ceiling && self.shared.weather.borrow().is_raining()
    }

    fn is_thundering(&self) -> bool {
        self.is_raining() && self.shared.weather.borrow().is_thundering()
    }

    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        RegionActor::motion_blocking_height(self, x, z)
    }

    fn world_surface_height(&self, x: i32, z: i32) -> Option<i32> {
        RegionActor::world_surface_height(self, x, z)
    }

    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32) {
        RedstoneLevel::schedule_tick(self, pos, block, delay, TickPriority::Normal);
    }

    fn level_event(&mut self, event: i32, pos: BlockPos, data: i32) {
        RegionActor::level_event(self, event, pos, data, false);
    }

    fn trail_particle(&mut self, from: [f64; 3], target: [f64; 3], color: i32, duration: i32) {
        let mut options = Vec::with_capacity(32);
        for c in target {
            options.extend_from_slice(&c.to_be_bytes());
        }
        options.extend_from_slice(&color.to_be_bytes());
        let mut rest = duration as u32;
        while rest >= 0x80 {
            options.push((rest & 0x7f) as u8 | 0x80);
            rest >>= 7;
        }
        options.push(rest as u8);
        let packet = crate::world::packets([crate::world::framed(
            &crate::protocol::play::ParticlesWithOptions {
                particle: tuff_vanilla_data::registry::ParticleType::Trail.id(),
                options: &options,
                force: false,
                position: from,
                spread: [0.0; 3],
                speed: 0.0,
                count: 1,
            },
        )]);
        self.send_near(from, super::blocks::PARTICLE_RANGE, None, packet);
    }

    fn rule_bool(&self, name: &str) -> bool {
        self.shared.rule_bool(name)
    }

    fn rule_int(&self, name: &str) -> i32 {
        self.shared.rule_int(name)
    }

    fn dimension(&self) -> &'static tuff_vanilla_data::dimension_type::DimensionType {
        RegionActor::dimension(self).kind
    }

    fn water_evaporates(&self) -> bool {
        RegionActor::dimension(self).environment().water_evaporates
    }

    fn difficulty(&self) -> u8 {
        self.shared.difficulty.borrow().id()
    }

    fn player_within(&self, pos: BlockPos, radius: i32) -> bool {
        self.players_near(pos, radius)
    }

    fn prime_tnt(&mut self, pos: BlockPos) {
        if self.owns(pos) {
            if self.shared.rule_bool("tnt_explodes") {
                self.spawn_primed_tnt(pos, None, None);
            }
        } else {
            let update = BlockUpdate::PrimeTnt { pos };
            let region = ChunkPos::of_block(pos).region();
            self.send_block_updates(HashMap::from([(region, vec![update])]));
        }
    }

    fn environment(&self) -> &'static tuff_vanilla_data::world::Environment {
        self.dimension().environment()
    }

    fn clock_ticks(&self, clock: &str) -> i64 {
        let clocks = self.shared.clocks.borrow();
        crate::world::clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks)
    }

    fn merge_block_entity(&mut self, pos: BlockPos, data: tuff_nbt::Compound) -> bool {
        let Some(state) = self.block(pos).filter(|_| self.owns(pos)) else {
            return false;
        };
        self.load_block_entity_data(pos, state, &data)
    }

    fn has_scheduled_tick(&self, pos: BlockPos, block: Block) -> bool {
        RedstoneLevel::has_scheduled_tick(self, pos, block)
    }

    fn summon(&mut self, nbt: tuff_nbt::Compound, position: [f64; 3], fresh: bool) -> bool {
        RegionActor::summon(self, nbt, position, fresh)
    }

    fn play_block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
    ) {
        RegionActor::play_sound(self, sound, pos, volume, pitch, None);
    }

    fn light_portal(&mut self, pos: BlockPos) -> bool {
        self.owns(pos) && RegionActor::light_portal(self, pos)
    }

    fn seed(&self) -> i64 {
        self.shared.seed
    }

    fn block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
    ) {
        RegionActor::play_sound(self, sound, pos, volume, pitch, None);
    }

    fn spawn_saved_entity(&mut self, nbt: tuff_nbt::Compound) {
        let id = self.shared.next_entity_id();
        let Some(entity) = self.shared.entity_types.load(&nbt, id) else {
            return;
        };
        if self.columns.contains_key(&entity.base().chunk()) {
            self.receive_entity(entity, 0);
        }
    }

    fn feature_host(&self) -> Option<std::sync::Arc<dyn crate::world::features::FeatureHost>> {
        self.shared.services.feature_host()
    }

    fn chunk_window(&self, center: ChunkPos) -> Option<Vec<(ChunkPos, Chunk)>> {
        let mut window = Vec::with_capacity(9);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let chunk = self
                    .columns
                    .get(&pos)
                    .map(|column| &column.chunk)
                    .or_else(|| self.border.halo.get(&pos))?;
                window.push((pos, chunk.clone()));
            }
        }
        Some(window)
    }
}

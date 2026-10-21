//! An in-memory world for unit tests of block behaviours.

use std::collections::HashMap;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_vanilla_data::{Block, BlockState};

use super::GrowthLevel;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, ChunkShape, plains};
use tuff_world_model::pos::ChunkPos;

pub struct TestLevel {
    pub blocks: HashMap<BlockPos, BlockState>,
    /// Sky and block light everywhere.
    pub light: (u8, u8),
    pub sky_darken: u8,
    pub raining: bool,
    pub biome: u16,
    pub rules: HashMap<&'static str, i32>,
    pub rng: StdRng,
    pub ticks: Vec<(BlockPos, Block, u32)>,
    pub events: Vec<(i32, BlockPos, i32)>,
    pub destroyed: Vec<(BlockPos, BlockState, bool)>,
    pub primed: Vec<BlockPos>,
    pub drops: Vec<(BlockPos, tuff_item::item::ItemStack)>,
    pub player: Option<BlockPos>,
    pub clock: i64,
    pub merged: Vec<(BlockPos, tuff_nbt::Compound)>,
    pub spawned: Vec<(tuff_nbt::Compound, [f64; 3])>,
    pub trails: Vec<([f64; 3], [f64; 3], i32, i32)>,
    pub sounds: Vec<(tuff_vanilla_data::registry::SoundEvent, BlockPos)>,
    pub dimension: &'static tuff_vanilla_data::dimension_type::DimensionType,
    pub evaporates: bool,
    pub resources_dropped: Vec<(BlockPos, BlockState)>,
    pub open_containers: Vec<BlockPos>,
}

impl TestLevel {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            light: (15, 0),
            sky_darken: 0,
            raining: false,
            biome: plains(),
            rules: HashMap::new(),
            rng: StdRng::seed_from_u64(1),
            ticks: Vec::new(),
            events: Vec::new(),
            destroyed: Vec::new(),
            primed: Vec::new(),
            drops: Vec::new(),
            player: None,
            clock: 0,
            merged: Vec::new(),
            spawned: Vec::new(),
            trails: Vec::new(),
            sounds: Vec::new(),
            dimension: tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld")
                .expect("overworld"),
            evaporates: false,
            resources_dropped: Vec::new(),
            open_containers: Vec::new(),
        }
    }

    pub fn put(&mut self, pos: BlockPos, state: BlockState) {
        if tuff_world_model::chunk::is_air(state) {
            self.blocks.remove(&pos);
        } else {
            self.blocks.insert(pos, state);
        }
    }

    pub fn fill(&mut self, from: BlockPos, to: BlockPos, state: BlockState) {
        for x in from.x..=to.x {
            for y in from.y..=to.y {
                for z in from.z..=to.z {
                    self.put(BlockPos::new(x, y, z), state);
                }
            }
        }
    }

    /// Runs a behaviour on the block at `pos`.
    pub fn run(&mut self, tick: super::TickFn, pos: BlockPos) {
        let state = self.state_at(pos);
        tick(self, pos, state);
    }

    pub fn count(&self, block: Block) -> usize {
        self.blocks.values().filter(|s| s.block() == block).count()
    }
}

impl GrowthLevel for TestLevel {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        if !ChunkShape::OVERWORLD.contains_y(pos.y) {
            return Some(Block::VoidAir.default_state());
        }
        Some(
            self.blocks
                .get(&pos)
                .copied()
                .unwrap_or(Block::Air.default_state()),
        )
    }

    fn set(&mut self, pos: BlockPos, state: BlockState, _flags: u32) {
        self.put(pos, state);
    }

    fn destroy(&mut self, pos: BlockPos, drop: bool) {
        let old = self.state_at(pos);
        self.destroyed.push((pos, old, drop));
        let remains = tuff_vanilla_common::placement::remains(old);
        self.put(pos, remains);
    }

    fn drop_item(&mut self, pos: BlockPos, stack: tuff_item::item::ItemStack) {
        self.drops.push((pos, stack));
    }

    fn drop_resources(&mut self, pos: BlockPos) {
        let state = self.state_at(pos);
        self.resources_dropped.push((pos, state));
    }

    fn rng(&mut self) -> &mut StdRng {
        &mut self.rng
    }

    fn container_open_count(&self, pos: BlockPos) -> usize {
        self.open_containers.iter().filter(|p| **p == pos).count()
    }

    fn light(&self, _pos: BlockPos) -> Option<(u8, u8)> {
        Some(self.light)
    }

    fn sky_darken(&self) -> u8 {
        self.sky_darken
    }

    fn min_y(&self) -> i32 {
        ChunkShape::OVERWORLD.min_y
    }

    fn max_y(&self) -> i32 {
        ChunkShape::OVERWORLD.max_y()
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn biome(&self, _pos: BlockPos) -> Option<u16> {
        Some(self.biome)
    }

    fn is_raining(&self) -> bool {
        self.raining
    }

    fn is_thundering(&self) -> bool {
        false
    }

    fn world_surface_height(&self, x: i32, z: i32) -> Option<i32> {
        let top = self
            .blocks
            .iter()
            .filter(|(pos, state)| {
                pos.x == x && pos.z == z && !tuff_world_model::chunk::is_air(**state)
            })
            .map(|(pos, _)| pos.y + 1)
            .max();
        Some(top.unwrap_or(self.min_y()))
    }

    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        let top = self
            .blocks
            .iter()
            .filter(|(pos, state)| {
                pos.x == x && pos.z == z && tuff_core::world::contact::blocks_motion(**state)
            })
            .map(|(pos, _)| pos.y + 1)
            .max();
        Some(top.unwrap_or(self.min_y()))
    }

    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32) {
        self.ticks.push((pos, block, delay));
    }

    fn level_event(&mut self, event: i32, pos: BlockPos, data: i32) {
        self.events.push((event, pos, data));
    }

    fn trail_particle(&mut self, from: [f64; 3], target: [f64; 3], color: i32, duration: i32) {
        self.trails.push((from, target, color, duration));
    }

    fn rule_bool(&self, name: &str) -> bool {
        self.rules.get(name).map_or_else(
            || tuff_core::world::game_rules::GameRules::default().bool(name),
            |v| *v != 0,
        )
    }

    fn rule_int(&self, name: &str) -> i32 {
        self.rules
            .get(name)
            .copied()
            .unwrap_or_else(|| tuff_core::world::game_rules::GameRules::default().int(name))
    }

    fn dimension(&self) -> &'static tuff_vanilla_data::dimension_type::DimensionType {
        self.dimension
    }

    fn water_evaporates(&self) -> bool {
        self.evaporates
    }

    fn difficulty(&self) -> u8 {
        2
    }

    fn player_within(&self, pos: BlockPos, radius: i32) -> bool {
        self.player.is_some_and(|p| {
            let d = [
                (p.x - pos.x) as f64,
                (p.y - pos.y) as f64,
                (p.z - pos.z) as f64,
            ];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < radius as f64
        })
    }

    fn prime_tnt(&mut self, pos: BlockPos) {
        self.primed.push(pos);
    }

    fn environment(&self) -> &'static tuff_vanilla_data::world::Environment {
        tuff_vanilla_data::world::Environment::of("minecraft:overworld")
    }

    fn clock_ticks(&self, _clock: &str) -> i64 {
        self.clock
    }

    fn merge_block_entity(&mut self, pos: BlockPos, data: tuff_nbt::Compound) -> bool {
        self.merged.push((pos, data));
        true
    }

    fn has_scheduled_tick(&self, pos: BlockPos, block: Block) -> bool {
        self.ticks.iter().any(|(p, b, _)| *p == pos && *b == block)
    }

    fn summon(&mut self, nbt: tuff_nbt::Compound, position: [f64; 3], _fresh: bool) -> bool {
        self.spawned.push((nbt, position));
        true
    }

    fn play_block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        _volume: f32,
        _pitch: f32,
    ) {
        self.sounds.push((sound, pos));
    }

    fn seed(&self) -> i64 {
        1
    }

    fn block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        _volume: f32,
        _pitch: f32,
    ) {
        self.sounds.push((sound, pos));
    }

    fn spawn_saved_entity(&mut self, nbt: tuff_nbt::Compound) {
        self.spawned.push((nbt, [0.0; 3]));
    }

    fn feature_host(&self) -> Option<std::sync::Arc<dyn tuff_core::world::features::FeatureHost>> {
        Some(std::sync::Arc::new(
            tuff_feature_builtin_vanilla_worldgen::feature::host::Features,
        ))
    }

    fn chunk_window(&self, center: ChunkPos) -> Option<Vec<(ChunkPos, Chunk)>> {
        let mut window = Vec::with_capacity(9);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let pos = ChunkPos::new(center.x + dx, center.z + dz);
                let mut chunk = Chunk::uniform(
                    ChunkShape::OVERWORLD,
                    Block::Air.default_state(),
                    self.biome,
                );
                for (at, state) in &self.blocks {
                    if ChunkPos::of_block(*at) == pos {
                        chunk.set(at.x, at.y, at.z, *state);
                    }
                }
                window.push((pos, chunk));
            }
        }
        Some(window)
    }
}

impl TestLevel {
    pub fn state_at(&self, pos: BlockPos) -> BlockState {
        self.blocks
            .get(&pos)
            .copied()
            .unwrap_or(Block::Air.default_state())
    }
}

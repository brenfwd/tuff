//! Random ticks and what blocks do with them: crops, plants, saplings,
//! spreading grass, decaying leaves, melting ice, weathering copper, and
//! fire. Behaviours are plain functions of a [`GrowthLevel`], registered
//! per block in [`Behaviours`] from the kinds the game data assigns, so
//! they unit-test on an in-memory world and a feature can register its own.

pub mod aging;
pub mod bonemeal;
pub mod bubble_column;
pub mod composter;
pub mod cover;
pub mod crops;
pub mod dripstone;
pub mod fire;
pub mod frogspawn;
pub mod frosted_ice;
pub mod plants;
pub mod sniffer_egg;
pub mod sponge;
pub mod spread;
pub mod ticking;
pub mod trees;
pub mod turtle_egg;
pub mod weather;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::light::Direction as LightDirection;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::features::{FeatureHost, FeatureRegion, FeatureSpan};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_item::random::WorldgenRandom;
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::BiomeClimate;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

pub use tuff_core::world::growth::{GrowthLevel, random_block_pos};

/// A window of chunks features can grow in, with the random they draw from.
pub struct Features<'a> {
    pub region: &'a mut dyn FeatureRegion,
    pub random: &'a mut WorldgenRandom,
}

impl Features<'_> {
    /// Places a configured feature by name.
    pub fn place_configured(&mut self, name: &str, pos: BlockPos) -> bool {
        self.region.place_configured(name, self.random, pos)
    }

    /// Places a placed feature by name, through its placement modifiers.
    pub fn place_placed(&mut self, name: &str, pos: BlockPos) -> bool {
        self.region.place_placed(name, self.random, pos)
    }
}

/// The game's namespace prefix added where a name has none.
pub fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

/// The temperature at a block of a biome with the given climate.
pub fn climate_temperature<L: GrowthLevel + ?Sized>(
    level: &L,
    climate: BiomeClimate,
    pos: BlockPos,
) -> f32 {
    match level.feature_host() {
        Some(host) => host.temperature(
            climate.temperature,
            climate.frozen,
            pos.x,
            pos.y,
            pos.z,
            level.sea_level(),
        ),
        None => climate.temperature,
    }
}

/// Derived lookups every level has.
pub trait GrowthLevelExt: GrowthLevel {
    /// Runs `grow` on a copy of the 3x3 chunks around `center` as world
    /// generation features see them, then applies what it changed if it
    /// returns true. False (and nothing run) if those chunks are not all
    /// loaded.
    fn with_features(
        &mut self,
        center: BlockPos,
        grow: &mut dyn FnMut(&mut Features) -> bool,
    ) -> bool {
        self.with_features_flagged(center, UPDATE_ALL, grow)
    }

    /// [`Self::with_features`] applying the changes with `flags`.
    fn with_features_flagged(
        &mut self,
        center: BlockPos,
        flags: u32,
        grow: &mut dyn FnMut(&mut Features) -> bool,
    ) -> bool {
        let middle = ChunkPos::of_block(center);
        let Some(host) = self.feature_host() else {
            return false;
        };
        let Some(window) = self.chunk_window(middle) else {
            return false;
        };
        let shape = window[0].1.shape();
        let seed = self.seed();
        let span = FeatureSpan {
            min_y: shape.min_y,
            height: shape.height,
            sea_level: self.sea_level(),
            seed,
        };
        let mut random = WorldgenRandom::legacy(self.rng().r#gen());
        let Some(changes) =
            host.with_window(middle, &window, span, &mut random, &mut |region, random| {
                grow(&mut Features { region, random })
            })
        else {
            return false;
        };
        for (pos, state) in changes.blocks {
            self.set(pos, state, flags);
        }
        for (pos, data) in changes.block_entities {
            self.merge_block_entity(pos, data);
        }
        true
    }

    fn state(&self, pos: BlockPos) -> BlockState {
        self.block(pos).unwrap_or(Block::Air.default_state())
    }

    fn is_empty(&self, pos: BlockPos) -> bool {
        self.block(pos).is_some_and(is_air)
    }

    fn is_inside_build_height(&self, y: i32) -> bool {
        y >= self.min_y() && y <= self.max_y()
    }

    /// The brighter of sky light darkened by `darken` and block light.
    fn raw_brightness(&self, pos: BlockPos, darken: u8) -> u8 {
        let (sky, block) = self.light(pos).unwrap_or((0, 0));
        sky.saturating_sub(darken).max(block)
    }

    /// The light a player would see at `pos` now.
    fn max_local_raw_brightness(&self, pos: BlockPos) -> u8 {
        self.raw_brightness(pos, self.sky_darken())
    }

    fn block_light(&self, pos: BlockPos) -> u8 {
        self.light(pos).map_or(0, |(_, block)| block)
    }

    fn random_int(&mut self, bound: i32) -> i32 {
        self.rng().gen_range(0..bound.max(1))
    }

    fn random_float(&mut self) -> f32 {
        self.rng().r#gen::<f32>()
    }

    fn random_double(&mut self) -> f64 {
        self.rng().r#gen::<f64>()
    }

    fn random_bool(&mut self) -> bool {
        self.rng().r#gen::<bool>()
    }

    /// Whether rain falls on `pos`: it rains, nothing above blocks it and
    /// the biome is warm enough for rain rather than snow.
    fn is_raining_at(&self, pos: BlockPos) -> bool {
        if !self.is_raining() {
            return false;
        }
        let Some(top) = self.motion_blocking_height(pos.x, pos.z) else {
            return false;
        };
        if top > pos.y {
            return false;
        }
        if self
            .world_surface_height(pos.x, pos.z)
            .is_none_or(|surface| surface > pos.y)
        {
            return false;
        }
        self.precipitation_at(pos) == Precipitation::Rain
    }

    /// The biome a block at `pos` sits in: the stored quart nearest it
    /// after the game's jitter, falling back to the plain quart where the
    /// neighbour is not loaded.
    fn zoomed_biome(&self, pos: BlockPos) -> Option<u16> {
        let Some(host) = self.feature_host() else {
            return self.biome(pos);
        };
        let [qx, qy, qz] = host.zoom(obfuscated_seed(&*host, self.seed()), pos.x, pos.y, pos.z);
        self.biome(BlockPos::new(qx << 2, qy << 2, qz << 2))
            .or_else(|| self.biome(pos))
    }

    fn precipitation_at(&self, pos: BlockPos) -> Precipitation {
        let Some(biome) = self.zoomed_biome(pos) else {
            return Precipitation::None;
        };
        let climate = self.biome_climate(biome);
        if !climate.has_precipitation {
            return Precipitation::None;
        }
        let temperature = climate_temperature(self, climate, pos);
        if temperature < 0.15 {
            Precipitation::Snow
        } else {
            Precipitation::Rain
        }
    }
}

impl<T: GrowthLevel + ?Sized> GrowthLevelExt for T {}

fn obfuscated_seed(host: &dyn FeatureHost, seed: i64) -> i64 {
    thread_local! {
        static LAST: std::cell::Cell<Option<(i64, i64)>> = const { std::cell::Cell::new(None) };
    }
    LAST.with(|last| match last.get() {
        Some((from, obfuscated)) if from == seed => obfuscated,
        _ => {
            let obfuscated = host.obfuscate_seed(seed);
            last.set(Some((seed, obfuscated)));
            obfuscated
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precipitation {
    None,
    Rain,
    Snow,
}

pub fn above(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y + 1, pos.z)
}

pub fn below(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y - 1, pos.z)
}

pub fn offset(pos: BlockPos, x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

/// Directions in the game's `values()` order.
pub const DIRECTIONS: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

/// Horizontal directions in the game's plane order.
pub const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

fn light_direction(direction: Facing) -> LightDirection {
    match direction {
        Facing::Down => LightDirection::Down,
        Facing::Up => LightDirection::Up,
        Facing::North => LightDirection::North,
        Facing::South => LightDirection::South,
        Facing::West => LightDirection::West,
        Facing::East => LightDirection::East,
    }
}

/// How much light `to` takes away passing into it from `from` in
/// `direction`: 16 when their faces together block it completely.
pub fn light_dampening_into(from: BlockState, to: BlockState, direction: Facing) -> u8 {
    let dampening = to.light_dampening();
    if from.is_empty_light_shape() && to.is_empty_light_shape() {
        return dampening;
    }
    let direction = light_direction(direction);
    let from_face = if from.is_empty_light_shape() {
        tuff_vanilla_data::light::FaceShape::EMPTY
    } else {
        from.light_face(direction)
    };
    let to_face = if to.is_empty_light_shape() {
        tuff_vanilla_data::light::FaceShape::EMPTY
    } else {
        to.light_face(direction.opposite())
    };
    if from_face.occludes(to_face) {
        16
    } else {
        dampening
    }
}

/// Whether `state` can stay at `pos` in `level`, unloaded blocks reading
/// as air.
pub fn can_survive(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    placement::can_survive(state, pos, &|p| level.state(p))
}

/// Whether `state` can stay at `pos`, including the light rules some plants
/// have.
pub fn survives(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    can_survive(level, state, pos) && light_allows(level, state, pos)
}

/// The light rules of crops and mushrooms, on top of their ground rules.
/// Where light is not known yet the rules let the plant be.
pub fn light_allows(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    let block = state.block();
    let kind = block.random_tick_kind().or(block.bonemeal_kind());
    let light = || level.light(pos).map(|(sky, block)| sky.max(block));
    match kind {
        Some("crop" | "beetroot" | "torchflower") => light().is_none_or(|light| light >= 8),
        Some("pitcher_crop") => {
            state.half() != Some(tuff_vanilla_data::block::Half::Lower)
                || light().is_none_or(|light| light >= 8)
        }
        Some("mushroom") => {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT,
                level.state(below(pos)).block(),
            ) || light().is_none_or(|light| light < 13)
        }
        _ => true,
    }
}

/// What a block does when ticked.
pub type TickFn = fn(&mut dyn GrowthLevel, BlockPos, BlockState);

/// What a block does when its neighbour in a direction changed to a state
/// (before it takes its new shape), such as scheduling a tick.
pub type NeighbourFn = fn(&mut dyn GrowthLevel, BlockPos, BlockState, Facing, BlockState);

/// What a block does when a neighbour, a block of the given kind, sends it
/// a block update.
pub type BlockUpdateFn = fn(&mut dyn GrowthLevel, BlockPos, BlockState, Block);

/// What bone meal does to a block.
#[derive(Clone, Copy)]
pub struct Bonemeal {
    /// Bone meal is used up on the block.
    pub valid: fn(&mut dyn GrowthLevel, BlockPos, BlockState) -> bool,
    /// The growth happens (drawn after `valid`).
    pub success: fn(&mut dyn GrowthLevel, BlockPos, BlockState) -> bool,
    pub perform: TickFn,
}

pub fn always(_: &mut dyn GrowthLevel, _: BlockPos, _: BlockState) -> bool {
    true
}

/// Block behaviours by block: random ticks, scheduled ticks and bone meal.
/// Built from the kinds the game data gives each block; a kind name maps to
/// a behaviour, and single blocks can be given their own.
pub struct Behaviours {
    random_kinds: HashMap<&'static str, TickFn>,
    bonemeal_kinds: HashMap<&'static str, Bonemeal>,
    random: HashMap<Block, TickFn>,
    scheduled: HashMap<Block, TickFn>,
    placed: HashMap<Block, TickFn>,
    neighbour: HashMap<Block, NeighbourFn>,
    block_update: HashMap<Block, BlockUpdateFn>,
    bonemeal: HashMap<Block, Bonemeal>,
}

impl Behaviours {
    pub fn builtin() -> Self {
        let mut behaviours = Self {
            random_kinds: HashMap::new(),
            bonemeal_kinds: HashMap::new(),
            random: HashMap::new(),
            scheduled: HashMap::new(),
            placed: HashMap::new(),
            neighbour: HashMap::new(),
            block_update: HashMap::new(),
            bonemeal: HashMap::new(),
        };
        crops::register(&mut behaviours);
        plants::register(&mut behaviours);
        trees::register(&mut behaviours);
        spread::register(&mut behaviours);
        weather::register(&mut behaviours);
        aging::register(&mut behaviours);
        fire::register(&mut behaviours);
        dripstone::register(&mut behaviours);
        cover::register(&mut behaviours);
        ticking::register(&mut behaviours);
        bonemeal::register(&mut behaviours);
        bubble_column::register(&mut behaviours);
        sniffer_egg::register(&mut behaviours);
        turtle_egg::register(&mut behaviours);
        frogspawn::register(&mut behaviours);
        frosted_ice::register(&mut behaviours);
        sponge::register(&mut behaviours);
        composter::register(&mut behaviours);
        behaviours.resolve();
        behaviours
    }

    pub fn register_random_kind(&mut self, kind: &'static str, tick: TickFn) {
        self.random_kinds.insert(kind, tick);
    }

    pub fn register_bonemeal_kind(&mut self, kind: &'static str, bonemeal: Bonemeal) {
        self.bonemeal_kinds.insert(kind, bonemeal);
    }

    /// Gives one block its own random tick, whatever its kind.
    pub fn register_random(&mut self, block: Block, tick: TickFn) {
        self.random.insert(block, tick);
    }

    pub fn register_scheduled(&mut self, block: Block, tick: TickFn) {
        self.scheduled.insert(block, tick);
    }

    /// Runs when a block of `block` is set, unless the set skips placement
    /// reactions.
    pub fn register_placed(&mut self, block: Block, placed: TickFn) {
        self.placed.insert(block, placed);
    }

    pub fn register_neighbour(&mut self, block: Block, changed: NeighbourFn) {
        self.neighbour.insert(block, changed);
    }

    pub fn register_block_update(&mut self, block: Block, update: BlockUpdateFn) {
        self.block_update.insert(block, update);
    }

    pub fn register_bonemeal(&mut self, block: Block, bonemeal: Bonemeal) {
        self.bonemeal.insert(block, bonemeal);
    }

    /// Assigns every block the behaviour of its kind, keeping blocks
    /// registered on their own.
    pub fn resolve(&mut self) {
        for block in Block::ALL {
            if let Some(kind) = block.random_tick_kind()
                && let Some(tick) = self.random_kinds.get(kind)
            {
                self.random.entry(block).or_insert(*tick);
            }
            if let Some(kind) = block.bonemeal_kind()
                && let Some(bonemeal) = self.bonemeal_kinds.get(kind)
            {
                self.bonemeal.entry(block).or_insert(*bonemeal);
            }
        }
    }

    /// Runs the random tick of the block at `pos` and of its fluid.
    pub fn random_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        if state.is_randomly_ticking()
            && let Some(tick) = self.random.get(&state.block())
        {
            tick(level, pos, state);
        }
        if state.is_fluid_randomly_ticking() {
            fire::lava_tick(level, pos, state);
        }
    }

    /// Whether `block` takes scheduled ticks here.
    pub fn has_scheduled(&self, block: Block) -> bool {
        self.scheduled.contains_key(&block)
    }

    pub fn scheduled_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        if let Some(tick) = self.scheduled.get(&state.block()) {
            tick(level, pos, state);
        }
    }

    pub fn has_placed(&self, block: Block) -> bool {
        self.placed.contains_key(&block)
    }

    pub fn placed(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        if let Some(placed) = self.placed.get(&state.block()) {
            placed(level, pos, state);
        }
    }

    pub fn has_neighbour(&self, block: Block) -> bool {
        self.neighbour.contains_key(&block)
    }

    pub fn neighbour_changed(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) {
        if let Some(changed) = self.neighbour.get(&state.block()) {
            changed(level, pos, state, direction, neighbour);
        }
    }

    pub fn bonemeal(&self, block: Block) -> Option<Bonemeal> {
        self.bonemeal.get(&block).copied()
    }
}

/// Runs `speed` random ticks in the section whose lowest corner is `min`.
pub fn tick_section(
    behaviours: &Behaviours,
    level: &mut dyn GrowthLevel,
    rand_value: &mut i32,
    min: BlockPos,
    speed: i32,
) {
    for _ in 0..speed {
        let pos = random_block_pos(rand_value, min.x, min.y, min.z, 15);
        if let Some(state) = level.block(pos) {
            behaviours.random_tick(level, pos, state);
        }
    }
}

impl tuff_core::world::growth::GrowthBehaviors for Behaviours {
    fn random_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        Behaviours::random_tick(self, level, pos, state);
    }

    fn precipitation(&self, level: &mut dyn GrowthLevel, top: BlockPos, below: BlockPos) {
        weather::precipitation(level, top, below);
    }

    fn rains_at(&self, level: &dyn GrowthLevel, pos: BlockPos) -> bool {
        level.is_raining_at(pos)
    }

    fn has_scheduled(&self, block: Block) -> bool {
        Behaviours::has_scheduled(self, block)
    }

    fn scheduled_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        Behaviours::scheduled_tick(self, level, pos, state);
    }

    fn has_placed(&self, block: Block) -> bool {
        Behaviours::has_placed(self, block)
    }

    fn placed(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
        Behaviours::placed(self, level, pos, state);
    }

    fn has_neighbour(&self, block: Block) -> bool {
        Behaviours::has_neighbour(self, block)
    }

    fn neighbour_changed(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) {
        Behaviours::neighbour_changed(self, level, pos, state, direction, neighbour);
    }

    fn has_block_update(&self, block: Block) -> bool {
        self.block_update.contains_key(&block)
    }

    fn block_update(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        state: BlockState,
        source: Block,
    ) {
        if let Some(update) = self.block_update.get(&state.block()) {
            update(level, pos, state, source);
        }
    }

    fn light_allows(&self, level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
        light_allows(level, state, pos)
    }

    fn bonemeal(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        face: Facing,
    ) -> Option<BlockPos> {
        bonemeal::apply(self, level, pos, face)
    }
}

/// The state with its `age` one higher, if it has one.
pub fn aged(state: BlockState, age: u8) -> BlockState {
    state.with_age(age).unwrap_or(state)
}

/// The highest `age` a block's states have.
pub fn max_age(block: Block) -> u8 {
    block
        .states()
        .filter_map(BlockState::age)
        .max()
        .unwrap_or(0)
}

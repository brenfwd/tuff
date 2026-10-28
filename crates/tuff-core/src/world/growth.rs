//! The seam block behaviours (random ticks, scheduled ticks, placement and
//! neighbour reactions, bone meal) plug into. Core owns the world a
//! behaviour sees ([`GrowthLevel`], which `RegionActor` implements) and the
//! reactions core's region calls ([`GrowthBehaviors`]); which block does
//! what belongs to a feature. With no feature every block is inert.

use rand::rngs::StdRng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::BiomeClimate;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

/// The world as block behaviours see it.
pub trait GrowthLevel {
    /// The block at `pos`, `None` where it is not loaded.
    fn block(&self, pos: BlockPos) -> Option<BlockState>;
    /// Sets a block with the game's update flags (`UPDATE_*` in
    /// [`crate::world::redstone`]), so neighbours, observers and shape
    /// rules react as they would to any other change.
    fn set(&mut self, pos: BlockPos, state: BlockState, flags: u32);
    /// Removes the block (leaving any fluid it held), showing it break and
    /// dropping its loot when `drop` is set.
    fn destroy(&mut self, pos: BlockPos, drop: bool);
    /// Drops an item at the block, as a broken block drops its loot.
    fn drop_item(&mut self, pos: BlockPos, stack: ItemStack);
    /// Drops the loot of the block at `pos` without removing it.
    fn drop_resources(&mut self, _pos: BlockPos) {}
    fn rng(&mut self) -> &mut StdRng;
    /// How many players have the container at `pos` open.
    fn container_open_count(&self, _pos: BlockPos) -> usize {
        0
    }
    /// Stored sky and block light at `pos`, `None` where unknown.
    fn light(&self, pos: BlockPos) -> Option<(u8, u8)>;
    /// How far time of day and weather darken the sky now.
    fn sky_darken(&self) -> u8;
    fn min_y(&self) -> i32;
    fn max_y(&self) -> i32;
    fn sea_level(&self) -> i32;
    /// The world seed.
    fn seed(&self) -> i64;
    /// The biome id at `pos`.
    fn biome(&self, pos: BlockPos) -> Option<u16>;
    /// The climate of biome `biome`, custom biomes included.
    fn biome_climate(&self, biome: u16) -> BiomeClimate {
        BiomeClimate::vanilla(biome)
    }
    /// Whether clients see rain (and thunder) now.
    fn is_raining(&self) -> bool;
    fn is_thundering(&self) -> bool;
    /// The first y above the column's highest motion blocking block.
    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32>;
    /// The first y above the column's highest non-air block.
    fn world_surface_height(&self, x: i32, z: i32) -> Option<i32>;
    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32);
    fn level_event(&mut self, event: i32, pos: BlockPos, data: i32);
    /// Shows a trail particle flying from `from` to `target` over `duration` ticks.
    fn trail_particle(&mut self, from: [f64; 3], target: [f64; 3], color: i32, duration: i32);
    fn rule_bool(&self, name: &str) -> bool;
    fn rule_int(&self, name: &str) -> i32;
    /// A server config integer, `default` when unset.
    fn config_int(&self, name: &str, default: i64) -> i64;
    fn config_bool(&self, name: &str, default: bool) -> bool;
    /// The dimension's infiniburn block tag and whether water evaporates.
    fn dimension(&self) -> &'static tuff_vanilla_data::dimension_type::DimensionType;
    fn water_evaporates(&self) -> bool;
    fn difficulty(&self) -> u8;
    /// Whether a non-spectator player is closer than `radius` blocks.
    fn player_within(&self, pos: BlockPos, radius: i32) -> bool;
    /// Lights TNT at `pos` into a primed TNT entity.
    fn prime_tnt(&mut self, pos: BlockPos);
    /// The dimension's environment attributes.
    fn environment(&self) -> &'static tuff_vanilla_data::world::Environment;
    /// Ticks of the named world clock.
    fn clock_ticks(&self, clock: &str) -> i64;
    /// Whether a tick of `block` is already scheduled at `pos`.
    fn has_scheduled_tick(&self, pos: BlockPos, block: Block) -> bool;
    /// Loads an entity from `nbt` at `position`; `fresh` sets it up like a
    /// newly spawned mob. False if it could not be made.
    fn summon(&mut self, nbt: tuff_nbt::Compound, position: [f64; 3], fresh: bool) -> bool;
    /// Merges `data` over the block entity at `pos`, which a block change
    /// has just given its default. False if nothing changed.
    fn merge_block_entity(&mut self, pos: BlockPos, data: tuff_nbt::Compound) -> bool;
    /// Plays a block sound to the players around `pos`.
    fn play_block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
    );
    /// Fire was placed at `pos`: lights a nether portal there if a frame
    /// around it is empty, replacing the fire. Returns whether it did.
    fn light_portal(&mut self, _pos: BlockPos) -> bool {
        false
    }
    /// Copies of the 3x3 chunks around `center`, or `None` if they are
    /// not all loaded. Behaviours that grow world generation features
    /// build their feature window from these.
    fn chunk_window(&self, center: ChunkPos) -> Option<Vec<(ChunkPos, Chunk)>>;
    /// The host that grows world generation features, if one is running.
    fn feature_host(&self) -> Option<std::sync::Arc<dyn crate::world::features::FeatureHost>>;
    /// A block sound at `pos`, for everyone nearby.
    fn block_sound(
        &mut self,
        sound: tuff_vanilla_data::registry::SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
    );
    /// Adds the entity saved as `nbt` (with its `id` and `Pos`) to the
    /// world, set up as loaded, if this region owns where it stands.
    fn spawn_saved_entity(&mut self, nbt: tuff_nbt::Compound);
}

/// The block reactions core's region calls. A feature fills
/// `Registries::growth_behaviors` with one; [`NoGrowthBehaviors`] stands
/// in when none does.
pub trait GrowthBehaviors: Send + Sync {
    /// Runs the random tick of the block at `pos`, which the caller has
    /// checked ticks randomly.
    fn random_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState);
    /// Lets precipitation act on the top of a column: `top` is the first
    /// block above its highest motion blocking block, `below` the one
    /// under it.
    fn precipitation(&self, level: &mut dyn GrowthLevel, top: BlockPos, below: BlockPos);
    /// Whether `block` takes scheduled ticks here.
    fn has_scheduled(&self, block: Block) -> bool;
    fn scheduled_tick(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState);
    fn has_placed(&self, block: Block) -> bool;
    fn placed(&self, level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState);
    fn has_neighbour(&self, block: Block) -> bool;
    fn neighbour_changed(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    );
    /// Whether `block` reacts to block updates from its neighbours.
    fn has_block_update(&self, _block: Block) -> bool {
        false
    }
    /// The block at `pos` got a block update from a neighbouring `source`.
    fn block_update(
        &self,
        _level: &mut dyn GrowthLevel,
        _pos: BlockPos,
        _state: BlockState,
        _source: Block,
    ) {
    }
    /// Whether light lets `state` stay at `pos` (crops need light,
    /// mushrooms need dark).
    fn light_allows(&self, level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool;
    /// Uses bone meal on the block at `pos`, clicked on `face`: where it
    /// grew something, or `None` if it does nothing and is not used up.
    fn bonemeal(
        &self,
        level: &mut dyn GrowthLevel,
        pos: BlockPos,
        face: Facing,
    ) -> Option<BlockPos>;
    /// Whether rain (not snow) falls on `pos` now.
    fn rains_at(&self, _level: &dyn GrowthLevel, _pos: BlockPos) -> bool {
        false
    }
}

/// No block reacts to anything.
pub struct NoGrowthBehaviors;

impl GrowthBehaviors for NoGrowthBehaviors {
    fn random_tick(&self, _: &mut dyn GrowthLevel, _: BlockPos, _: BlockState) {}

    fn precipitation(&self, _: &mut dyn GrowthLevel, _: BlockPos, _: BlockPos) {}

    fn has_scheduled(&self, _: Block) -> bool {
        false
    }

    fn scheduled_tick(&self, _: &mut dyn GrowthLevel, _: BlockPos, _: BlockState) {}

    fn has_placed(&self, _: Block) -> bool {
        false
    }

    fn placed(&self, _: &mut dyn GrowthLevel, _: BlockPos, _: BlockState) {}

    fn has_neighbour(&self, _: Block) -> bool {
        false
    }

    fn neighbour_changed(
        &self,
        _: &mut dyn GrowthLevel,
        _: BlockPos,
        _: BlockState,
        _: Facing,
        _: BlockState,
    ) {
    }

    fn light_allows(&self, _: &dyn GrowthLevel, _: BlockState, _: BlockPos) -> bool {
        true
    }

    fn bonemeal(&self, _: &mut dyn GrowthLevel, _: BlockPos, _: Facing) -> Option<BlockPos> {
        None
    }
}

/// The level event bone meal plays where it grew something.
pub const BONEMEAL_EVENT: i32 = 1505;

/// The value [`random_block_pos`] leaves behind after `draws` draws from
/// `rand_value`, in O(log draws): the generator is affine, so steps compose.
pub fn skip_random_block_pos(rand_value: i32, draws: u64) -> i32 {
    let (mut mul, mut add) = (1u32, 0u32);
    let (mut step_mul, mut step_add) = (3u32, 1013904223u32);
    let mut left = draws;
    while left > 0 {
        if left & 1 == 1 {
            mul = mul.wrapping_mul(step_mul);
            add = add.wrapping_mul(step_mul).wrapping_add(step_add);
        }
        step_add = step_add.wrapping_mul(step_mul).wrapping_add(step_add);
        step_mul = step_mul.wrapping_mul(step_mul);
        left >>= 1;
    }
    (rand_value as u32).wrapping_mul(mul).wrapping_add(add) as i32
}

/// The game's cheap random block position in a 16-wide cube at `x`, `y`,
/// `z`: one step of its own generator gives all three coordinates.
pub fn random_block_pos(rand_value: &mut i32, x: i32, y: i32, z: i32, y_mask: i32) -> BlockPos {
    *rand_value = rand_value.wrapping_mul(3).wrapping_add(1013904223);
    let value = *rand_value >> 2;
    BlockPos::new(
        x + (value & 15),
        y + (value >> 16 & y_mask),
        z + (value >> 8 & 15),
    )
}

pub const FREEZING_KEY: &str = "tuff.world.freezing.enabled";
pub const FIRE_DELAY_MIN_KEY: &str = "tuff.world.fireTickDelay.min";
pub const FIRE_DELAY_MAX_KEY: &str = "tuff.world.fireTickDelay.max";

pub fn config_keys() -> Vec<crate::config::Key> {
    use crate::config::{Key, Value};
    let delay = |name: &str, default: i64, what: &str| {
        Key::new(
            name,
            Value::Int(default),
            &format!("{what} number of ticks until a fire ticks again"),
        )
        .validated(|value| match value {
            Value::Int(n) if (1..=1_000_000).contains(n) => Ok(()),
            _ => Err("must be between 1 and 1000000".to_owned()),
        })
    };
    vec![
        Key::new(
            FREEZING_KEY,
            Value::Bool(true),
            "Whether precipitation freezes water into ice and piles snow layers",
        ),
        delay(FIRE_DELAY_MIN_KEY, 30, "Least"),
        delay(FIRE_DELAY_MAX_KEY, 39, "Most"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn skipping_matches_drawing_one_at_a_time() {
        for start in [0, 1, -7, i32::MAX, i32::MIN, 123_456_789] {
            let mut value = start;
            for draws in 0..200u64 {
                assert_eq!(
                    skip_random_block_pos(start, draws),
                    value,
                    "{start} {draws}"
                );
                random_block_pos(&mut value, 0, 0, 0, 15);
            }
        }
    }
}

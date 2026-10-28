//! Ice and snow: melting by block light, water freezing in cold biomes and
//! snow piling up while it snows.

use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, Precipitation, can_survive};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("ice", ice_tick);
    behaviours.register_random_kind("snow_layer", snow_tick);
}

fn ice_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.block_light(pos) as i32 > 11 - state.light_dampening() as i32 {
        melt(level, pos);
    }
}

/// Ice turns into water, or vanishes where water evaporates.
pub fn melt(level: &mut dyn GrowthLevel, pos: BlockPos) {
    if level.water_evaporates() {
        level.set(pos, Block::Air.default_state(), UPDATE_ALL);
    } else {
        level.set(pos, Block::Water.default_state(), UPDATE_ALL);
    }
}

fn snow_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if level.block_light(pos) > 11 {
        level.destroy(pos, true);
    }
}

fn temperature(level: &dyn GrowthLevel, biome: u16, pos: BlockPos) -> f32 {
    let climate = level.biome_climate(biome);
    super::climate_temperature(level, climate, pos)
}

fn is_water(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::fluid::key::WATER,
        level.state(pos).fluid_state().fluid(),
    )
}

/// Whether the water at `pos` freezes in `biome`: a still water block, cold
/// enough and dark enough, not surrounded by water on all four sides.
pub fn should_freeze(level: &dyn GrowthLevel, biome: u16, pos: BlockPos) -> bool {
    if temperature(level, biome, pos) >= 0.15
        || !level.is_inside_build_height(pos.y)
        || level.block_light(pos) >= 10
    {
        return false;
    }
    let state = level.state(pos);
    if state.block() != Block::Water || !state.fluid_state().is_source() {
        return false;
    }
    !super::HORIZONTAL
        .into_iter()
        .all(|direction| is_water(level, step(pos, direction)))
}

/// Whether snow settles at `pos` in `biome`.
pub fn should_snow(level: &dyn GrowthLevel, biome: u16, pos: BlockPos) -> bool {
    let snows =
        level.biome_climate(biome).has_precipitation && temperature(level, biome, pos) < 0.15;
    if !snows || !level.is_inside_build_height(pos.y) || level.block_light(pos) >= 10 {
        return false;
    }
    let state = level.state(pos);
    (is_air(state) || state.block() == Block::Snow)
        && can_survive(level, Block::Snow.default_state(), pos)
}

/// What the weather does to the top of a column: `top` is the first block
/// above its highest motion blocking block, `below` the block under it.
/// Cauldrons fill from the rain or snow falling on them.
pub fn precipitation(level: &mut dyn GrowthLevel, top: BlockPos, below: BlockPos) {
    let Some(biome) = level.zoomed_biome(top) else {
        return;
    };
    let freezing = level.config_bool(tuff_core::world::growth::FREEZING_KEY, true);
    if freezing && should_freeze(level, biome, below) {
        level.set(below, Block::Ice.default_state(), UPDATE_ALL);
    }
    if !level.is_raining() {
        return;
    }
    let max_height = level.rule_int("max_snow_accumulation_height");
    if freezing && max_height > 0 && should_snow(level, biome, top) {
        let state = level.state(top);
        if state.block() == Block::Snow {
            let layers = state.layers().unwrap_or(1);
            if (layers as i32) < max_height.min(8)
                && let Some(deeper) = state.with_layers(layers + 1)
            {
                level.set(top, deeper, UPDATE_ALL);
            }
        } else {
            level.set(top, Block::Snow.default_state(), UPDATE_ALL);
        }
    }
    let kind = level.precipitation_at(below);
    if kind != Precipitation::None {
        fill_cauldron(level, below, kind);
    }
}

fn fill_cauldron(level: &mut dyn GrowthLevel, pos: BlockPos, kind: Precipitation) {
    let state = level.state(pos);
    let (water, snow) = (kind == Precipitation::Rain, kind == Precipitation::Snow);
    let chance = if water { 0.05 } else { 0.1 };
    let filled = match state.block() {
        Block::Cauldron => {
            if level.random_float() >= chance {
                return;
            }
            if water {
                Block::WaterCauldron.default_state()
            } else {
                Block::PowderSnowCauldron.default_state()
            }
        }
        Block::WaterCauldron | Block::PowderSnowCauldron => {
            let matches = (state.block() == Block::WaterCauldron && water)
                || (state.block() == Block::PowderSnowCauldron && snow);
            if level.random_float() >= chance || !matches {
                return;
            }
            let fill = state.level().unwrap_or(1);
            match state.with_level(fill + 1).filter(|_| fill < 3) {
                Some(fuller) => fuller,
                None => return,
            }
        }
        _ => return,
    };
    level.set(pos, filled, UPDATE_ALL);
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn biome(name: &str) -> u16 {
        tuff_world_model::chunk::biome_id(name).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn still_water_freezes_in_cold_biomes_unless_surrounded() {
        let mut level = TestLevel::new();
        level.biome = biome("minecraft:snowy_plains");
        let pos = BlockPos::new(0, 62, 0);
        level.put(pos, Block::Water.default_state());
        precipitation(&mut level, BlockPos::new(0, 63, 0), pos);
        assert_eq!(level.state(pos).block(), Block::Ice);

        level.put(pos, Block::Water.default_state());
        for direction in super::super::HORIZONTAL {
            level.put(step(pos, direction), Block::Water.default_state());
        }
        precipitation(&mut level, BlockPos::new(0, 63, 0), pos);
        assert_eq!(level.state(pos).block(), Block::Water);

        let mut warm = TestLevel::new();
        warm.put(pos, Block::Water.default_state());
        precipitation(&mut warm, BlockPos::new(0, 63, 0), pos);
        assert_eq!(warm.state(pos).block(), Block::Water);
    }

    #[tuff_core::watchdog]
    #[test]
    fn snow_piles_up_to_the_game_rule_while_it_snows() {
        let mut level = TestLevel::new();
        level.biome = biome("minecraft:snowy_plains");
        let ground = BlockPos::new(0, 63, 0);
        let top = BlockPos::new(0, 64, 0);
        level.put(ground, Block::Stone.default_state());
        precipitation(&mut level, top, ground);
        assert!(is_air(level.state(top)), "no snow without weather");
        level.raining = true;
        precipitation(&mut level, top, ground);
        assert_eq!(level.state(top).block(), Block::Snow);
        level.rules.insert("max_snow_accumulation_height", 3);
        for _ in 0..5 {
            precipitation(&mut level, top, ground);
        }
        assert_eq!(level.state(top).layers(), Some(3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn switching_freezing_off_stops_ice_and_snow_but_not_cauldrons() {
        let mut level = TestLevel::new();
        level.biome = biome("minecraft:snowy_plains");
        level.raining = true;
        let water = BlockPos::new(0, 62, 0);
        let ground = BlockPos::new(5, 63, 0);
        let top = BlockPos::new(5, 64, 0);
        level.put(water, Block::Water.default_state());
        level.put(ground, Block::Stone.default_state());
        level
            .rules
            .insert(tuff_core::world::growth::FREEZING_KEY, 0);
        precipitation(&mut level, BlockPos::new(0, 63, 0), water);
        precipitation(&mut level, top, ground);
        assert_eq!(level.state(water).block(), Block::Water);
        assert!(is_air(level.state(top)));
        level
            .rules
            .insert(tuff_core::world::growth::FREEZING_KEY, 1);
        precipitation(&mut level, BlockPos::new(0, 63, 0), water);
        precipitation(&mut level, top, ground);
        assert_eq!(level.state(water).block(), Block::Ice);
        assert_eq!(level.state(top).block(), Block::Snow);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rain_fills_cauldrons_a_level_at_a_time() {
        let mut level = TestLevel::new();
        level.raining = true;
        let pos = BlockPos::new(0, 63, 0);
        let top = BlockPos::new(0, 64, 0);
        level.put(pos, Block::Cauldron.default_state());
        for _ in 0..2000 {
            precipitation(&mut level, top, pos);
        }
        assert_eq!(level.state(pos).block(), Block::WaterCauldron);
        assert_eq!(level.state(pos).level(), Some(3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn ice_and_snow_melt_in_block_light() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::Ice.default_state());
        level.light = (15, 10);
        level.run(ice_tick, pos);
        assert_eq!(
            level.state(pos).block(),
            Block::Ice,
            "sky light melts nothing"
        );
        level.light = (0, 12);
        level.run(ice_tick, pos);
        assert_eq!(level.state(pos).block(), Block::Water);
        level.put(pos, Block::Snow.default_state());
        level.run(snow_tick, pos);
        assert!(is_air(level.state(pos)));
    }
}

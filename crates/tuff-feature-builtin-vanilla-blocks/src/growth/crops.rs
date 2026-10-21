//! Crops, stems, farmland, nether wart, sweet berries and cocoa.

use tuff_vanilla_data::block::{Facing, Half};
use tuff_vanilla_data::growth::block_tag;
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockState};

use super::{
    Behaviours, Bonemeal, GrowthLevel, GrowthLevelExt, HORIZONTAL, above, aged, always, below,
    max_age, offset,
};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS, UPDATE_PUSH_ENTITIES};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("crop", crop_tick);
    behaviours.register_random_kind("beetroot", slow_crop_tick);
    behaviours.register_random_kind("torchflower", slow_crop_tick);
    behaviours.register_random_kind("pitcher_crop", pitcher_tick);
    behaviours.register_random_kind("nether_wart", nether_wart_tick);
    behaviours.register_random_kind("farmland", farmland_tick);
    behaviours.register_random_kind("stem", stem_tick);
    behaviours.register_random_kind("sweet_berry_bush", sweet_berry_tick);
    behaviours.register_random_kind("cocoa", cocoa_tick);

    behaviours.register_bonemeal_kind(
        "crop",
        Bonemeal {
            valid: crop_not_grown,
            success: always,
            perform: |level, pos, state| {
                let increase = level.random_int(4) + 2;
                grow_crop(level, pos, state, increase as u8);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "beetroot",
        Bonemeal {
            valid: crop_not_grown,
            success: always,
            perform: |level, pos, state| {
                let increase = (level.random_int(4) + 2) / 3;
                grow_crop(level, pos, state, increase as u8);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "torchflower",
        Bonemeal {
            valid: crop_not_grown,
            success: always,
            perform: |level, pos, state| grow_crop(level, pos, state, 1),
        },
    );
    behaviours.register_bonemeal_kind(
        "pitcher_crop",
        Bonemeal {
            valid: |level, pos, state| {
                lower_pitcher(level, pos, state).is_some_and(|(pos, state)| {
                    can_pitcher_grow(level, pos, state, state.age().unwrap_or(0) + 1)
                })
            },
            success: always,
            perform: |level, pos, state| {
                if let Some((pos, state)) = lower_pitcher(level, pos, state) {
                    grow_pitcher(level, state, pos, 1);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "stem",
        Bonemeal {
            valid: |_, _, state| state.age() != Some(7),
            success: always,
            perform: |level, pos, state| {
                let age = (state.age().unwrap_or(0) + level.random_int(4) as u8 + 2).min(7);
                let grown = aged(state, age);
                level.set(pos, grown, UPDATE_CLIENTS);
                if age == 7 {
                    stem_tick(level, pos, grown);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "sweet_berry_bush",
        Bonemeal {
            valid: |_, _, state| state.age().is_some_and(|age| age < 3),
            success: always,
            perform: |level, pos, state| {
                let age = (state.age().unwrap_or(0) + 1).min(3);
                level.set(pos, aged(state, age), UPDATE_CLIENTS);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "cocoa",
        Bonemeal {
            valid: |_, _, state| state.age().is_some_and(|age| age < 2),
            success: always,
            perform: |level, pos, state| {
                level.set(
                    pos,
                    aged(state, state.age().unwrap_or(0) + 1),
                    UPDATE_CLIENTS,
                );
            },
        },
    );
}

/// Crops need light 8 where they stand.
pub fn has_sufficient_light(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    level.raw_brightness(pos, 0) >= 8
}

fn crop_not_grown(_: &mut dyn GrowthLevel, _: BlockPos, state: BlockState) -> bool {
    state.age().is_some_and(|age| age < max_age(state.block()))
}

fn grow_crop(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState, increase: u8) {
    let age = (state.age().unwrap_or(0) + increase).min(max_age(state.block()));
    level.set(pos, aged(state, age), UPDATE_CLIENTS);
}

/// How fast a crop of `block` grows at `pos`: farmland under it and its
/// neighbours speeds it up, the same crop crowding it slows it down.
pub fn growth_speed(level: &dyn GrowthLevel, block: Block, pos: BlockPos) -> f32 {
    let mut speed = 1.0f32;
    let ground = below(pos);
    for dx in -1..=1 {
        for dz in -1..=1 {
            let mut block_speed = 0.0f32;
            let state = level.state(offset(ground, dx, 0, dz));
            if tuff_datapack::tags::is(tags::key::GROWS_CROPS, state.block()) {
                block_speed = 1.0;
                if state.moisture().unwrap_or(0) > 0 {
                    block_speed = 3.0;
                }
            }
            if dx != 0 || dz != 0 {
                block_speed /= 4.0;
            }
            speed += block_speed;
        }
    }
    let is = |p: BlockPos| level.state(p).block() == block;
    let (north, south) = (step(pos, Facing::North), step(pos, Facing::South));
    let (west, east) = (step(pos, Facing::West), step(pos, Facing::East));
    let horizontal = is(west) || is(east);
    let vertical = is(north) || is(south);
    if (horizontal && vertical)
        || is(step(west, Facing::North))
        || is(step(east, Facing::North))
        || is(step(east, Facing::South))
        || is(step(west, Facing::South))
    {
        speed /= 2.0;
    }
    speed
}

fn growth_roll(level: &mut dyn GrowthLevel, block: Block, pos: BlockPos) -> bool {
    let speed = growth_speed(level, block, pos);
    level.random_int((25.0 / speed) as i32 + 1) == 0
}

fn crop_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.raw_brightness(pos, 0) < 9 {
        return;
    }
    let Some(age) = state.age() else {
        return;
    };
    if age < max_age(state.block()) && growth_roll(level, state.block(), pos) {
        level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
    }
}

/// Beetroots and torchflowers skip a third of their ticks.
fn slow_crop_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.random_int(3) != 0 {
        crop_tick(level, pos, state);
    }
}

fn nether_wart_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let age = state.age().unwrap_or(0);
    if age < 3 && level.random_int(10) == 0 {
        level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
    }
}

const PITCHER_MAX_AGE: u8 = 4;

fn is_lower_pitcher(state: BlockState) -> bool {
    state.block() == Block::PitcherCrop && state.half() == Some(Half::Lower)
}

fn lower_pitcher(
    level: &dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
) -> Option<(BlockPos, BlockState)> {
    if is_lower_pitcher(state) {
        return Some((pos, state));
    }
    let lower = level.state(below(pos));
    is_lower_pitcher(lower).then_some((below(pos), lower))
}

fn can_pitcher_grow(level: &dyn GrowthLevel, pos: BlockPos, state: BlockState, age: u8) -> bool {
    let grows_into = |p: BlockPos| {
        let state = level.state(p);
        tuff_world_model::chunk::is_air(state) || state.block() == Block::PitcherCrop
    };
    state.age().unwrap_or(0) < PITCHER_MAX_AGE
        && has_sufficient_light(level, pos)
        && level.is_inside_build_height(pos.y + 1)
        && (age < 3 || grows_into(above(pos)))
}

fn grow_pitcher(level: &mut dyn GrowthLevel, state: BlockState, pos: BlockPos, increase: u8) {
    let age = (state.age().unwrap_or(0) + increase).min(PITCHER_MAX_AGE);
    if !can_pitcher_grow(level, pos, state, age) {
        return;
    }
    let lower = aged(state, age);
    level.set(pos, lower, UPDATE_CLIENTS);
    if age >= 3
        && let Some(upper) = lower.with_half(Half::Upper)
    {
        level.set(above(pos), upper, UPDATE_ALL);
    }
}

fn pitcher_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if growth_roll(level, state.block(), pos) {
        grow_pitcher(level, state, pos, 1);
    }
}

fn is_near_water(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    for dx in -4..=4 {
        for dy in 0..=1 {
            for dz in -4..=4 {
                let fluid = level.state(offset(pos, dx, dy, dz)).fluid_state().fluid();
                if tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, fluid) {
                    return true;
                }
            }
        }
    }
    false
}

fn maintains_farmland(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tags::key::MAINTAINS_FARMLAND,
        level.state(above(pos)).block(),
    )
}

/// Farmland turns back into its base block when it dries out. Covered
/// farmland reverts through the placement rules' block tick instead.
fn turn_to_base(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if let Some(base) = state.block().base_block() {
        level.set(pos, base.default_state(), UPDATE_ALL | UPDATE_PUSH_ENTITIES);
    }
}

fn farmland_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let moisture = state.moisture().unwrap_or(0);
    if is_near_water(level, pos) || level.is_raining_at(above(pos)) {
        if moisture < 7
            && let Some(wet) = state.with_moisture(7)
        {
            level.set(pos, wet, UPDATE_CLIENTS);
        }
    } else if moisture > 0 {
        if let Some(drier) = state.with_moisture(moisture - 1) {
            level.set(pos, drier, UPDATE_CLIENTS);
        }
    } else if !maintains_farmland(level, pos) {
        turn_to_base(level, pos, state);
    }
}

fn stem_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.raw_brightness(pos, 0) < 9 {
        return;
    }
    let block = state.block();
    if !growth_roll(level, block, pos) {
        return;
    }
    let age = state.age().unwrap_or(0);
    if age < 7 {
        level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
        return;
    }
    let direction = HORIZONTAL[level.random_int(4) as usize];
    let target = step(pos, direction);
    let Some(stem) = block.stem() else {
        return;
    };
    let ground = level.state(below(target)).block();
    if level.is_empty(target) && block_tag(stem.fruit_support).contains(&ground) {
        level.set(target, stem.fruit.default_state(), UPDATE_ALL);
        if let Some(attached) = stem.attached.default_state().with_facing(direction) {
            level.set(pos, attached, UPDATE_ALL);
        }
    }
}

fn sweet_berry_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let age = state.age().unwrap_or(0);
    if age < 3 && level.random_int(5) == 0 && level.raw_brightness(above(pos), 0) >= 9 {
        level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
    }
}

fn cocoa_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.random_int(5) == 0 {
        let age = state.age().unwrap_or(0);
        if age < 2 {
            level.set(pos, aged(state, age + 1), UPDATE_CLIENTS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn farmland(moisture: u8) -> BlockState {
        Block::Farmland
            .default_state()
            .with_moisture(moisture)
            .unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn growth_speed_follows_moist_farmland_and_crowding() {
        let mut level = TestLevel::new();
        for x in -1..=1 {
            for z in -1..=1 {
                level.put(BlockPos::new(x, 63, z), farmland(7));
            }
        }
        let wheat = Block::Wheat.default_state();
        level.put(BlockPos::new(0, 64, 0), wheat);
        assert_eq!(
            growth_speed(&level, Block::Wheat, BlockPos::new(0, 64, 0)),
            10.0
        );
        level.put(BlockPos::new(1, 64, 0), wheat);
        assert_eq!(
            growth_speed(&level, Block::Wheat, BlockPos::new(0, 64, 0)),
            10.0
        );
        level.put(BlockPos::new(0, 64, 1), wheat);
        assert_eq!(
            growth_speed(&level, Block::Wheat, BlockPos::new(0, 64, 0)),
            5.0
        );
        level.put(BlockPos::new(0, 63, 0), farmland(0));
        assert_eq!(
            growth_speed(&level, Block::Wheat, BlockPos::new(0, 64, 0)),
            4.0
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn crops_need_light_to_grow() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(below(pos), farmland(7));
        level.put(pos, Block::Wheat.default_state());
        level.light = (0, 8);
        for _ in 0..500 {
            level.run(crop_tick, pos);
        }
        assert_eq!(level.state(pos).age(), Some(0));
        level.light = (15, 0);
        for _ in 0..500 {
            level.run(crop_tick, pos);
        }
        assert_eq!(level.state(pos).age(), Some(7));
    }

    #[tuff_core::watchdog]
    #[test]
    fn farmland_dries_out_and_reverts_away_from_water() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 63, 0);
        level.put(pos, farmland(2));
        for _ in 0..3 {
            level.run(farmland_tick, pos);
        }
        assert_eq!(level.state(pos).block(), Block::Dirt);
        level.put(pos, farmland(0));
        level.put(BlockPos::new(4, 63, -4), Block::Water.default_state());
        level.run(farmland_tick, pos);
        assert_eq!(level.state(pos).moisture(), Some(7));
    }

    #[tuff_core::watchdog]
    #[test]
    fn grown_stems_set_fruit_on_supporting_ground() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        for x in -1..=1 {
            for z in -1..=1 {
                level.put(BlockPos::new(x, 63, z), farmland(7));
            }
        }
        level.put(pos, Block::PumpkinStem.default_state().with_age(7).unwrap());
        for _ in 0..200 {
            let state = level.state(pos);
            if state.block() != Block::PumpkinStem {
                break;
            }
            stem_tick(&mut level, pos, state);
        }
        let attached = level.state(pos);
        assert_eq!(attached.block(), Block::AttachedPumpkinStem);
        let facing = attached.facing().unwrap();
        assert_eq!(level.state(step(pos, facing)).block(), Block::Pumpkin);
    }
}

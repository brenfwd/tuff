//! Blocks that age: copper weathering by its neighbours' ages, and coral
//! dying out of water.

use tuff_vanilla_data::block::Type as ChestType;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, DIRECTIONS, GrowthLevel, GrowthLevelExt, offset};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("weathering", weathering_tick);
    for block in Block::ALL {
        if block.dead_coral().is_some() {
            behaviours.register_scheduled(block, coral_tick);
            behaviours.register_placed(block, coral_check);
            behaviours.register_neighbour(block, |level, pos, state, _, _| {
                coral_check(level, pos, state)
            });
        }
    }
}

/// Weathering blocks within this many blocks (taxicab) slow or stop each
/// other's aging.
const SCAN_DISTANCE: i32 = 4;

/// The chance a random tick tries to age a weathering block at all.
const AGING_CHANCE: f32 = 0.056_888_89;

fn weathering_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.random_float() >= AGING_CHANCE {
        return;
    }
    if is_copper_door(state.block()) && state.half() != Some(tuff_vanilla_data::block::Half::Lower)
    {
        return;
    }
    if is_copper_chest(state.block())
        && (state.r#type() == Some(ChestType::Right) || level.container_open_count(pos) > 0)
    {
        return;
    }
    if let Some(next) = next_state(level, pos, state) {
        level.set(pos, next, UPDATE_ALL);
    }
}

fn is_copper_door(block: Block) -> bool {
    matches!(
        block,
        Block::CopperDoor
            | Block::ExposedCopperDoor
            | Block::WeatheredCopperDoor
            | Block::OxidizedCopperDoor
    )
}

fn is_copper_chest(block: Block) -> bool {
    matches!(
        block,
        Block::CopperChest
            | Block::ExposedCopperChest
            | Block::WeatheredCopperChest
            | Block::OxidizedCopperChest
    )
}

/// The next age of the block, if its neighbours let it and the roll
/// succeeds: any younger neighbour stops it, older ones speed it up.
pub fn next_state(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
) -> Option<BlockState> {
    let own = state.block().weathering()?;
    let next = own.next?;
    let mut older = 0u32;
    let mut same = 0u32;
    for dx in -SCAN_DISTANCE..=SCAN_DISTANCE {
        for dy in -SCAN_DISTANCE..=SCAN_DISTANCE {
            for dz in -SCAN_DISTANCE..=SCAN_DISTANCE {
                if dx.abs() + dy.abs() + dz.abs() > SCAN_DISTANCE || (dx, dy, dz) == (0, 0, 0) {
                    continue;
                }
                let Some(other) = level.state(offset(pos, dx, dy, dz)).block().weathering() else {
                    continue;
                };
                if other.group != own.group {
                    continue;
                }
                if other.age < own.age {
                    return None;
                }
                if other.age > own.age {
                    older += 1;
                } else {
                    same += 1;
                }
            }
        }
    }
    let chance = ((older + 1) / (older + same + 1)) as f32;
    let actual = chance * chance * own.chance_modifier;
    if level.random_float() >= actual {
        return None;
    }
    Some(with_properties_of(next, state))
}

pub use tuff_vanilla_data::growth::with_properties_of;

fn is_water(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::fluid::key::WATER,
        level.state(pos).fluid_state().fluid(),
    )
}

fn has_water(level: &dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    state.waterlogged() == Some(true)
        || DIRECTIONS
            .into_iter()
            .any(|direction| is_water(level, step(pos, direction)))
}

/// Coral out of water starts dying.
fn coral_check(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if !has_water(level, pos, state) {
        let delay = 60 + level.random_int(40) as u32;
        level.schedule_tick(pos, state.block(), delay);
    }
}

fn coral_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if has_water(level, pos, state) {
        return;
    }
    let Some(dead) = state.block().dead_coral() else {
        return;
    };
    let mut dead_state = dead.default_state();
    if let Some(dry) = dead_state.with_waterlogged(false) {
        dead_state = dry;
    }
    if let Some(facing) = state.facing()
        && let Some(faced) = dead_state.with_facing(facing)
    {
        dead_state = faced;
    }
    level.set(pos, dead_state, UPDATE_CLIENTS);
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::block::Facing;

    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn copper_ages_one_step_at_a_time_keeping_its_shape() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        let stairs = Block::CutCopperStairs
            .default_state()
            .with_facing(Facing::East)
            .unwrap();
        level.put(pos, stairs);
        for _ in 0..2000 {
            level.run(weathering_tick, pos);
            if level.state(pos).block() != Block::CutCopperStairs {
                break;
            }
        }
        let aged = level.state(pos);
        assert_eq!(aged.block(), Block::ExposedCutCopperStairs);
        assert_eq!(aged.facing(), Some(Facing::East));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_same_age_neighbour_stops_aging_but_only_older_ones_do_not() {
        let pos = BlockPos::new(0, 64, 0);
        let state = Block::CopperBlock.default_state();
        let mut level = TestLevel::new();
        level.put(pos, state);
        level.put(BlockPos::new(1, 64, 0), Block::CopperBlock.default_state());
        for _ in 0..500 {
            assert_eq!(next_state(&mut level, pos, state), None);
        }
        let mut level = TestLevel::new();
        level.put(pos, state);
        level.put(
            BlockPos::new(1, 64, 0),
            Block::ExposedCopper.default_state(),
        );
        let aged = (0..500)
            .filter(|_| next_state(&mut level, pos, state).is_some())
            .count();
        assert!(aged > 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn copper_door_ages_only_its_lower_half() {
        use tuff_vanilla_data::block::Half;
        let pos = BlockPos::new(0, 64, 0);
        let door = |half| Block::CopperDoor.default_state().with_half(half).unwrap();
        let mut level = TestLevel::new();
        level.put(pos, door(Half::Upper));
        for _ in 0..2000 {
            level.run(weathering_tick, pos);
        }
        assert_eq!(level.state(pos).block(), Block::CopperDoor);
        level.put(pos, door(Half::Lower));
        for _ in 0..2000 {
            level.run(weathering_tick, pos);
        }
        assert_ne!(level.state(pos).block(), Block::CopperDoor);
    }

    #[tuff_core::watchdog]
    #[test]
    fn copper_chest_ages_only_its_left_half_and_only_when_closed() {
        let pos = BlockPos::new(0, 64, 0);
        let chest = |kind| Block::CopperChest.default_state().with_type(kind).unwrap();
        let aged = |level: &TestLevel| level.state(pos).block() != Block::CopperChest;
        let mut level = TestLevel::new();
        level.put(pos, chest(ChestType::Right));
        for _ in 0..4000 {
            level.run(weathering_tick, pos);
        }
        assert!(!aged(&level));
        level.put(pos, chest(ChestType::Left));
        level.open_containers.push(pos);
        for _ in 0..4000 {
            level.run(weathering_tick, pos);
        }
        assert!(!aged(&level));
        level.open_containers.clear();
        for _ in 0..4000 {
            level.run(weathering_tick, pos);
        }
        assert!(aged(&level));
    }

    #[tuff_core::watchdog]
    #[test]
    fn younger_copper_nearby_stops_aging() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::ExposedCopper.default_state());
        level.put(BlockPos::new(2, 64, 1), Block::CopperBlock.default_state());
        let state = level.state(pos);
        for _ in 0..500 {
            assert_eq!(next_state(&mut level, pos, state), None);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn coral_out_of_water_dies() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        let coral = Block::TubeCoralFan
            .default_state()
            .with_waterlogged(false)
            .unwrap();
        level.put(pos, coral);
        level.run(coral_check, pos);
        assert_eq!(level.ticks.len(), 1);
        level.run(coral_tick, pos);
        assert_eq!(level.state(pos).block(), Block::DeadTubeCoralFan);

        level.put(pos, Block::BrainCoralBlock.default_state());
        level.put(BlockPos::new(0, 65, 0), Block::Water.default_state());
        level.run(coral_tick, pos);
        assert_eq!(level.state(pos).block(), Block::BrainCoralBlock);
    }
}

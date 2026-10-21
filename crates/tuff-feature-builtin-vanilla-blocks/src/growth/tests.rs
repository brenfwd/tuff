//! Behaviours driven the way random ticks reach them.

use tuff_vanilla_data::Block;

use super::testing::TestLevel;
use super::{Behaviours, GrowthLevel, tick_section};
use tuff_world_model::BlockPos;

#[tuff_core::watchdog]
#[test]
fn a_field_of_wheat_ripens_under_random_ticks_at_the_default_speed() {
    let behaviours = Behaviours::builtin();
    let mut level = TestLevel::new();
    let farmland = Block::Farmland.default_state().with_moisture(7).unwrap();
    level.fill(BlockPos::new(0, 63, 0), BlockPos::new(15, 63, 15), farmland);
    level.put(BlockPos::new(7, 63, 7), Block::Water.default_state());
    let wheat = Block::Wheat.default_state();
    for x in 0..16 {
        for z in 0..16 {
            if (x, z) != (7, 7) {
                level.put(BlockPos::new(x, 64, z), wheat);
            }
        }
    }
    let speed = level.rule_int("random_tick_speed");
    assert_eq!(speed, 3);
    let mut rand_value = 1;
    let mut ticks = 0;
    while level
        .blocks
        .values()
        .any(|state| state.block() == Block::Wheat && state.age() != Some(7))
    {
        ticks += 1;
        assert!(ticks < 400_000, "the field ripens");
        tick_section(
            &behaviours,
            &mut level,
            &mut rand_value,
            BlockPos::new(0, 48, 0),
            speed,
        );
        tick_section(
            &behaviours,
            &mut level,
            &mut rand_value,
            BlockPos::new(0, 64, 0),
            speed,
        );
    }
    assert_eq!(level.count(Block::Wheat), 255);
    assert_eq!(level.count(Block::Farmland), 255, "moist farmland stays");
}

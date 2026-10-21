//! Systems that read the day (darkness, sunburn, bees) follow the
//! `advance_time` rule: a frozen clock holds their answers.

use super::*;
use crate::world::clock::{WorldClocks, clock_id};
use crate::world::dimension::DimensionId;

fn at(world: &TestWorld, ticks: i64) {
    let clock = clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(clock).set(ticks));
}

fn tick_clocks(world: &TestWorld, advance: bool, times: u32) {
    world.clocks.send_modify(|clocks: &mut WorldClocks| {
        clocks.set_advancing(advance);
        for _ in 0..times {
            clocks.tick();
        }
    });
}

fn day_facts(world: &TestWorld) -> (bool, bool, bool) {
    let dimension = world.shared.dimension(DimensionId::default());
    (
        crate::world::is_dark(&world.shared, dimension),
        crate::world::monsters_burn(&world.shared, dimension),
        crate::world::bees_stay_in_hive(&world.shared, dimension),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frozen_clock_holds_night_and_day_for_the_systems_that_read_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());

    at(&world, 18000);
    tick_clocks(&world, false, 30000);
    assert_eq!(day_facts(&world), (true, false, true));

    at(&world, 6000);
    tick_clocks(&world, false, 30000);
    assert_eq!(day_facts(&world), (false, true, false));

    tick_clocks(&world, true, 12000);
    assert_eq!(day_facts(&world), (true, false, true));
    world.shutdown().await;
}

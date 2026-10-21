//! Drowned: an amphibious zombie that paths through water as well as over
//! land, swims after targets in the water, heads into water by day, and
//! still walks and melees like any other zombie on land by night.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::registry::EntityType;

/// A sealed box of water, walled, floored and roofed with stone, matching
/// the guardian tests' pool.
pub(super) fn water_pool(
    min: i32,
    max: i32,
    height: i32,
) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            let inside = (min..=max).contains(&x) && (min..=max).contains(&z);
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            for y in SKY + 1..=SKY + height {
                let state = if inside {
                    Block::Water.default_state()
                } else {
                    Block::Stone.default_state()
                };
                blocks.push((BlockPos::new(x, y, z), state));
            }
            blocks.push((
                BlockPos::new(x, SKY + height + 1, z),
                Block::Stone.default_state(),
            ));
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_swim_around_in_water() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    let start = [6.5, SKY as f64 + 3.0, 6.5];
    summon(&region, EntityType::Drowned, Compound::new(), start).await;
    wait_for(&world, &region, 300, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Drowned && crate::world::distance_sq(s.position, start) > 0.25
        })
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_chase_and_hit_players_on_land_by_night() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Drowned,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 12.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack");
    let attacker = hit.attacker.expect("the drowned is to blame");
    assert_eq!(attacker.kind, EntityType::Drowned);
    let drowned = &mobs(&region, EntityType::Drowned).await[0];
    assert!(
        crate::world::distance_sq(drowned.position, position) < 3.0 * 3.0,
        "it walked over: {:?}",
        drowned.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_leave_players_on_land_alone_by_day() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 6)).await;
    summon(
        &region,
        EntityType::Drowned,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(200).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "a drowned on land by day ignores a dry player"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_swim_after_players_in_the_water() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    summon(
        &region,
        EntityType::Drowned,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let position = [10.5, SKY as f64 + 4.0, 10.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Drowned));
    let drowned = &mobs(&region, EntityType::Drowned).await[0];
    assert!(
        drowned.position[1] > SKY as f64 + 2.0,
        "it swam up to the player: {:?}",
        drowned.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_head_back_into_water_by_day() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 10..=12 {
        for z in 10..=12 {
            blocks.push((BlockPos::new(x, SKY - 1, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Drowned,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    let water = |p: [f64; 3]| (10.0..13.0).contains(&p[0]) && (10.0..13.0).contains(&p[2]);
    wait_for(&world, &region, 1200, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Drowned && water(s.position))
    })
    .await;
    world.shutdown().await;
}

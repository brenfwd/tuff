//! Crowd mode keeps the behaviours players notice: tempting, breeding and
//! targeting work with it on (threshold 1, so every region is crowded) as
//! with it off.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::registry::EntityType;

fn crowd(world: &TestWorld, on: bool) {
    use tuff_core::config::Value;
    let config = &world.shared.config;
    config
        .set("tuff.mobs.crowd.enabled", Value::Bool(on))
        .unwrap();
    config
        .set("tuff.mobs.crowd.threshold", Value::Int(1))
        .unwrap();
}

async fn tempted(on: bool) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    crowd(&world, on);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(1, 14)).await;
    for x in [2.5, 3.5, 4.5] {
        summon(
            &region,
            EntityType::Cow,
            Compound::new(),
            [x, SKY as f64 + 1.0, 2.5],
        )
        .await;
    }
    let player = [9.5, SKY as f64 + 1.0, 8.5];
    region
        .request(player_update(&viewer, player, Some(Item::Wheat)))
        .await;
    let found = wait_for(&world, &region, 600, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Cow)
            .all(|s| crate::world::distance_sq(s.position, player) < 4.0 * 4.0)
    })
    .await;
    assert!(
        found
            .iter()
            .filter(|s| s.kind == EntityType::Cow)
            .all(|s| crate::world::distance_sq(s.position, player) < 4.0 * 4.0),
        "crowd {on}: cows followed the wheat"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cows_follow_wheat_with_crowd_mode_on_and_off() {
    tempted(false).await;
    tempted(true).await;
}

async fn bred(on: bool) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    crowd(&world, on);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(1, 6)).await;
    for x in [2.5, 4.5] {
        summon(
            &region,
            EntityType::Cow,
            Compound::new().with("InLove", 600),
            [x, SKY as f64 + 1.0, 3.5],
        )
        .await;
    }
    let found = wait_for(&world, &region, 600, |found| {
        found.iter().filter(|s| s.kind == EntityType::Cow).count() == 3
    })
    .await;
    assert_eq!(
        found.iter().filter(|s| s.kind == EntityType::Cow).count(),
        3,
        "crowd {on}: a calf was born"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cows_in_love_breed_with_crowd_mode_on_and_off() {
    bred(false).await;
    bred(true).await;
}

async fn targeted(on: bool) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    crowd(&world, on);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, SKY as f64 + 1.0, 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:mob_attack", "crowd {on}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_target_players_with_crowd_mode_on_and_off() {
    targeted(false).await;
    targeted(true).await;
}

//! `tuff.mobs.collisionCap` limits pushes per mob but never starves cramming.

use super::*;
use tuff_core::config::Value;
use tuff_vanilla_data::registry::EntityType;

async fn pigs_left(cap: Option<i64>, count: usize) -> usize {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    if let Some(cap) = cap {
        world
            .shared
            .config
            .set("tuff.mobs.collisionCap", Value::Int(cap))
            .unwrap();
    }
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(24, 24)).await;
    for _ in 0..count {
        summon(
            &region,
            EntityType::Pig,
            Compound::new().with("NoAI", true),
            [24.5, SKY as f64 + 1.0, 24.5],
        )
        .await;
    }
    world.ticks(100).await;
    let left = mobs(&region, EntityType::Pig).await.len();
    world.shutdown().await;
    left
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collision_cap_default_still_crams_to_the_rule() {
    let left = pigs_left(None, 30).await;
    assert!((23..30).contains(&left), "{left} left");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collision_cap_small_still_crams_to_the_rule() {
    let left = pigs_left(Some(2), 30).await;
    assert!((23..30).contains(&left), "{left} left");
}

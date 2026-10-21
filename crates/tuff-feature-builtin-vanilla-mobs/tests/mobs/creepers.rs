//! Charged creepers: their victims drop their heads.

use super::*;
use tuff_vanilla_data::registry::EntityType;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_killed_by_a_charged_creeper_drop_their_heads() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true).with("Health", 1.0f32),
        [8.5, y, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Creeper,
        Compound::new()
            .with("powered", true)
            .with("ignited", true)
            .with("Fuse", 2i16),
        [5.5, y, 5.5],
    )
    .await;
    wait_for(&world, &region, 60, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Item && s.name == "minecraft:zombie_head")
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_charged_blast_drops_only_one_head() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let frail = || Compound::new().with("NoAI", true).with("Health", 1.0f32);
    for (kind, x, z) in [
        (EntityType::Zombie, 8.5, 5.5),
        (EntityType::Piglin, 5.5, 8.5),
        (EntityType::Skeleton, 3.5, 5.5),
        (EntityType::WitherSkeleton, 5.5, 3.5),
    ] {
        summon(&region, kind, frail(), [x, y, z]).await;
    }
    summon(
        &region,
        EntityType::Creeper,
        Compound::new()
            .with("powered", true)
            .with("ignited", true)
            .with("Fuse", 2i16),
        [5.5, y, 5.5],
    )
    .await;
    let is_head = |name: &str| {
        matches!(
            name,
            "minecraft:zombie_head"
                | "minecraft:skeleton_skull"
                | "minecraft:piglin_head"
                | "minecraft:wither_skeleton_skull"
        )
    };
    wait_for(&world, &region, 60, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Item && is_head(&s.name))
    })
    .await;
    world.ticks(40).await;
    let found = snapshots(&region).await;
    assert!(
        found
            .iter()
            .all(|s| s.kind == EntityType::Item || s.kind == EntityType::ExperienceOrb),
        "every victim died"
    );
    let heads = found
        .iter()
        .filter(|s| s.kind == EntityType::Item && is_head(&s.name))
        .count();
    assert_eq!(heads, 1);
    world.shutdown().await;
}

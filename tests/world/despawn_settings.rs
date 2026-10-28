//! Item and orb lifetimes, merge radii and mob despawn distances follow config.

use super::*;
use tuff_core::config::Value;
use tuff_nbt::Compound;
use tuff_vanilla_data::registry::EntityType;

async fn started() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    (dir, world, region, player, events)
}

async fn summon(region: &RegionHandle, nbt: Compound, position: [f64; 3]) {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "summoned");
}

fn stone(age: i16) -> Compound {
    Compound::new()
        .with("id", "minecraft:item")
        .with("Age", age)
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1i32),
        )
}

async fn count(region: &RegionHandle, kind: EntityType) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .iter()
        .filter(|entity| entity.kind == kind)
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_despawns_at_the_configured_age() {
    let (_dir, world, region, _player, _events) = started().await;
    world
        .shared
        .config
        .set("tuff.world.item.despawnTicks", Value::Int(40))
        .unwrap();
    summon(&region, stone(0), [1.5, 200.0, 1.5]).await;
    world.ticks(30).await;
    assert_eq!(count(&region, EntityType::Item).await, 1);
    world.ticks(20).await;
    assert_eq!(count(&region, EntityType::Item).await, 0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_still_despawns_at_6000_by_default() {
    let (_dir, world, region, _player, _events) = started().await;
    summon(&region, stone(5990), [1.5, 200.0, 1.5]).await;
    world.ticks(5).await;
    assert_eq!(count(&region, EntityType::Item).await, 1);
    world.ticks(10).await;
    assert_eq!(count(&region, EntityType::Item).await, 0);
}

async fn merge_after(radius: Option<&str>) -> usize {
    let (_dir, world, region, _player, _events) = started().await;
    if let Some(radius) = radius {
        world
            .shared
            .config
            .set("tuff.world.item.mergeRadius", Value::String(radius.into()))
            .unwrap();
    }
    summon(&region, stone(0), [1.5, 200.0, 1.5]).await;
    summon(&region, stone(0), [2.5, 200.0, 1.5]).await;
    world.ticks(45).await;
    count(&region, EntityType::Item).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_item_merge_radius_changes_which_neighbours_merge() {
    assert_eq!(merge_after(None).await, 2);
    assert_eq!(merge_after(Some("1.0")).await, 1);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_orb_despawns_at_the_configured_age() {
    let (_dir, world, region, _player, _events) = started().await;
    world
        .shared
        .config
        .set("tuff.world.experience.despawnTicks", Value::Int(30))
        .unwrap();
    let orb = Compound::new()
        .with("id", "minecraft:experience_orb")
        .with("Value", 3i16);
    summon(&region, orb, [1.5, 200.0, 1.5]).await;
    world.ticks(20).await;
    assert_eq!(count(&region, EntityType::ExperienceOrb).await, 1);
    world.ticks(20).await;
    assert_eq!(count(&region, EntityType::ExperienceOrb).await, 0);
}

async fn zombie_survives(distance: Option<i64>) -> bool {
    let (_dir, world, region, player, _events) = started().await;
    if let Some(distance) = distance {
        world
            .shared
            .config
            .set("tuff.world.despawn.monster.distance", Value::Int(distance))
            .unwrap();
    }
    let at = [1.5, 200.0, 1.5];
    summon(&region, Compound::new().with("id", "minecraft:zombie"), at).await;
    present(&region, &player, [41.5, 200.0, 1.5]).await;
    world.ticks(5).await;
    count(&region, EntityType::Zombie).await == 1
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_despawns_beyond_the_configured_distance() {
    assert!(zombie_survives(None).await);
    assert!(!zombie_survives(Some(16)).await);
}

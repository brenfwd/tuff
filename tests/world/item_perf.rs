//! Load test: thousands of resting item entities (merge candidates and a viewer).

use super::*;
use std::time::Instant;
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;
const ITEMS: usize = 3000;
const KINDS: [&str; 6] = [
    "minecraft:dirt",
    "minecraft:stone",
    "minecraft:cobblestone",
    "minecraft:oak_log",
    "minecraft:sand",
    "minecraft:gravel",
];

async fn count_items(region: &RegionHandle) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .iter()
        .filter(|entity| entity.kind == EntityType::Item)
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thousands_of_resting_items_tick_within_budget() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, SKY, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &region, floor).await;

    let spawn_started = Instant::now();
    for index in 0..ITEMS {
        let mut nbt = Compound::new().with("id", "minecraft:item").with(
            "Item",
            Compound::new()
                .with("id", KINDS[index % KINDS.len()])
                .with("count", 1i32),
        );
        nbt.insert("Motion", Tag::List(vec![Tag::Double(0.0); 3]));
        let x = 0.5 + (index % 16) as f64 + (index / 256) as f64 * 0.01;
        let z = 0.5 + ((index / 16) % 16) as f64;
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position: [x, f64::from(SKY + 1), z],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let spawn = spawn_started.elapsed();
    assert_eq!(count_items(&region).await, ITEMS);

    let mut per_tick = std::time::Duration::MAX;
    for _ in 0..10 {
        let batch_started = Instant::now();
        world.ticks(10).await;
        per_tick = per_tick.min(batch_started.elapsed() / 10);
    }
    let left = count_items(&region).await;
    eprintln!(
        "item perf: spawn {spawn:?} for {ITEMS}, {per_tick:?}/tick, {left} items after merging"
    );
    assert!(left < ITEMS, "stacks of one never merged");
    assert!(
        per_tick < std::time::Duration::from_millis(200),
        "item tick too slow: {per_tick:?}"
    );
}

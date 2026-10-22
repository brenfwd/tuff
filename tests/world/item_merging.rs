//! Dropped items merging with the items around them.

use super::*;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

async fn dropped(region: &RegionHandle) -> Vec<(i32, [f64; 3], ItemStack)> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let mut found = Vec::new();
    for entity in answer.await.unwrap() {
        if entity.kind != EntityType::Item {
            continue;
        }
        let (reply, data) = oneshot::channel();
        region
            .request(RegionRequest::EntityData {
                id: entity.id,
                reply,
            })
            .await;
        let data = data.await.unwrap().expect("the item exists here");
        let stack = data
            .get("Item")
            .and_then(|tag| tag.as_compound())
            .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
            .expect("a saved stack");
        found.push((entity.id, entity.position, stack));
    }
    found.sort_by_key(|(id, ..)| *id);
    found
}

async fn drop_item(region: &RegionHandle, id: &str, count: i32, x: f64, extra: Compound) {
    let mut nbt = extra
        .with("id", "minecraft:item")
        .with("Item", Compound::new().with("id", id).with("count", count));
    nbt.insert("Motion", Tag::List(vec![Tag::Double(0.0); 3]));
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position: [x, f64::from(SKY + 1), 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resting_items_merge_into_the_bigger_stack() {
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

    drop_item(&region, "minecraft:dirt", 5, 2.3, Compound::new()).await;
    drop_item(&region, "minecraft:dirt", 20, 2.6, Compound::new()).await;
    drop_item(&region, "minecraft:dirt", 10, 2.9, Compound::new()).await;
    drop_item(&region, "minecraft:dirt", 60, 2.5, Compound::new()).await;
    drop_item(&region, "minecraft:stone", 3, 2.5, Compound::new()).await;
    drop_item(&region, "minecraft:dirt", 1, 8.5, Compound::new()).await;
    let never = Compound::new().with("PickupDelay", 32767i16);
    drop_item(&region, "minecraft:dirt", 1, 12.5, never.clone()).await;
    drop_item(&region, "minecraft:dirt", 1, 12.6, Compound::new()).await;
    let before = dropped(&region).await;
    assert_eq!(before.len(), 8);

    world.ticks(81).await;
    let after = dropped(&region).await;
    let counts: Vec<(i32, u8)> = after
        .iter()
        .map(|(id, _, stack)| (*id, stack.count()))
        .collect();
    let dirt_near: Vec<u8> = after
        .iter()
        .filter(|(_, pos, stack)| pos[0] < 4.0 && stack.item == tuff_vanilla_data::Item::Dirt)
        .map(|(_, _, stack)| stack.count())
        .collect();
    let mut sorted = dirt_near.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [35, 60], "{counts:?}");
    let merged = after
        .iter()
        .find(|(_, _, stack)| stack.count() == 35)
        .unwrap();
    assert_eq!(
        merged.0, before[1].0,
        "the biggest of the three absorbs the rest"
    );
    assert!(
        after
            .iter()
            .any(|(_, _, stack)| stack.item == tuff_vanilla_data::Item::Stone && stack.count() == 3),
        "other items stay apart"
    );
    assert_eq!(
        after.iter().filter(|(_, pos, _)| pos[0] > 4.0).count(),
        3,
        "a lone item and one that can never be picked up stay: {counts:?}"
    );

    world.shutdown().await;
}

async fn drop_at(region: &RegionHandle, id: &str, count: i32, x: f64, z: f64) {
    let mut nbt = Compound::new()
        .with("id", "minecraft:item")
        .with("Item", Compound::new().with("id", id).with("count", count));
    nbt.insert("Motion", Tag::List(vec![Tag::Double(0.0); 3]));
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

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_merge_across_a_region_border_into_the_bigger_stack() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, ChunkPos::new(31, 0)).await;
    view_around(&world, &player, &mut events, ChunkPos::new(32, 0)).await;
    let west = world.region(RegionPos::new(0, 0)).await;
    let east = world.region(RegionPos::new(1, 0)).await;
    let floor = |range: std::ops::Range<i32>| -> Vec<_> {
        range
            .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, SKY, z)))
            .map(|pos| (pos, Block::Stone.default_state()))
            .collect()
    };
    place_blocks(&world, &west, floor(500..512)).await;
    place_blocks(&world, &east, floor(512..524)).await;

    drop_at(&west, "minecraft:dirt", 5, 511.8, 3.5).await;
    drop_at(&east, "minecraft:dirt", 20, 512.2, 3.5).await;
    drop_at(&west, "minecraft:gravel", 30, 511.8, 12.5).await;
    drop_at(&east, "minecraft:gravel", 7, 512.2, 12.5).await;
    drop_at(&west, "minecraft:stone", 2, 511.8, 8.5).await;
    drop_at(&east, "minecraft:sand", 2, 512.2, 8.5).await;
    world.ticks(81).await;

    let (west_items, east_items) = (dropped(&west).await, dropped(&east).await);
    let all: Vec<(tuff_vanilla_data::Item, u8, f64)> = west_items
        .iter()
        .chain(&east_items)
        .map(|(_, pos, stack)| (stack.item, stack.count(), pos[0]))
        .collect();
    let of = |item| -> Vec<(u8, f64)> {
        all.iter()
            .filter(|(i, ..)| *i == item)
            .map(|(_, count, x)| (*count, *x))
            .collect()
    };
    let dirt = of(tuff_vanilla_data::Item::Dirt);
    assert_eq!(dirt.len(), 1, "{all:?}");
    assert_eq!(dirt[0].0, 25);
    assert!(dirt[0].1 > 512.0, "the east stack was bigger: {all:?}");
    let gravel = of(tuff_vanilla_data::Item::Gravel);
    assert_eq!(gravel.len(), 1, "{all:?}");
    assert_eq!(gravel[0].0, 37);
    assert!(gravel[0].1 < 512.0, "the west stack was bigger: {all:?}");
    assert_eq!(of(tuff_vanilla_data::Item::Stone).len(), 1, "{all:?}");
    assert_eq!(of(tuff_vanilla_data::Item::Sand).len(), 1, "{all:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_border_merge_whose_check_fails_keeps_both_stacks() {
    use crate::world::conservation::{Subject, Transfer};
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, ChunkPos::new(31, 0)).await;
    view_around(&world, &player, &mut events, ChunkPos::new(32, 0)).await;
    let west = world.region(RegionPos::new(0, 0)).await;
    let east = world.region(RegionPos::new(1, 0)).await;
    let floor = |range: std::ops::Range<i32>| -> Vec<_> {
        range
            .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, SKY, z)))
            .map(|pos| (pos, Block::Stone.default_state()))
            .collect()
    };
    place_blocks(&world, &west, floor(500..512)).await;
    place_blocks(&world, &east, floor(512..524)).await;
    world.shared.conservation.fail_next(Transfer::BorderMerge);
    drop_at(&west, "minecraft:dirt", 5, 511.8, 3.5).await;
    drop_at(&east, "minecraft:dirt", 20, 512.2, 3.5).await;
    world.ticks(81).await;

    let counts: Vec<u8> = dropped(&west)
        .await
        .iter()
        .chain(&dropped(&east).await)
        .map(|(_, _, stack)| stack.count())
        .collect();
    assert_eq!(counts.len(), 2, "a refused merge moves nothing: {counts:?}");
    assert_eq!(counts.iter().map(|c| u32::from(*c)).sum::<u32>(), 25);
    let dimension = world.dimension(OVERWORLD);
    assert!([&west, &east].iter().any(|region| {
        world
            .shared
            .conservation
            .quarantined(Subject::Region(dimension, region.pos))
    }));
    world.shutdown().await;
}

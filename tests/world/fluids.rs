//! Fluids driven through the region actors: buckets starting flow, water
//! filling a broken block, flow across a region border, and fluid ticks
//! surviving a restart.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, BucketUse};
use tuff_vanilla_data::registry::EntityType;

const FLOOR_Y: i32 = 200;

/// A stone floor at [`FLOOR_Y`] over the blocks `x`, `z` (inclusive), with
/// three blocks of air above it, set without neighbour updates. `rim`
/// walls in the edges, so flows find no drop there.
pub(super) async fn floor(region: &RegionHandle, x: (i32, i32), z: (i32, i32), rim: bool) {
    let mut changes = Vec::new();
    for bx in x.0..=x.1 {
        for bz in z.0..=z.1 {
            changes.push((BlockPos::new(bx, FLOOR_Y, bz), Block::Stone.default_state()));
            let edge = bx == x.0 || bx == x.1 || bz == z.0 || bz == z.1;
            let above = if rim && edge {
                Block::Stone
            } else {
                Block::Air
            };
            for y in 1..=3 {
                changes.push((BlockPos::new(bx, FLOOR_Y + y, bz), above.default_state()));
            }
        }
    }
    blocks(
        region,
        BlockOp::Set {
            changes,
            destroy: false,
            keep: false,
            strict: true,
            quiet: false,
            data: None,
        },
    )
    .await;
}

pub(super) fn pour(
    player: &PlayerHandle,
    ground: BlockPos,
    item: Item,
    sequence: i32,
) -> RegionRequest {
    let fluid = match item.bucket() {
        Some(tuff_vanilla_data::item::Bucket::Fluid { fluid, .. }) => fluid,
        _ => None,
    };
    let eye = [
        ground.x as f64 + 0.5,
        ground.y as f64 + 3.0,
        ground.z as f64 + 0.5,
    ];
    RegionRequest::UseBucket(BucketUse {
        player: player.clone(),
        position: [eye[0], eye[1] - 1.62, eye[2]],
        eye,
        look: crate::world::raycast::view_vector(0.0, 90.0),
        reach: 4.5,
        off_hand: false,
        item,
        fluid,
        mob: None,
        mob_data: None,
        mob_components: None,
        sneaking: false,
        creative: false,
        sequence,
        hit: None,
    })
}

async fn used(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

async fn amount(region: &RegionHandle, pos: BlockPos) -> i32 {
    block_at(region, pos)
        .await
        .map_or(0, |state| state.fluid_state().amount())
}

/// Ticks until the fluid amount at `pos` is `expected`, failing after
/// `budget` ticks. A busy region may see several ticks at once, so tests
/// wait for states rather than count steps.
async fn settle(
    world: &TestWorld,
    region: &RegionHandle,
    pos: BlockPos,
    expected: i32,
    budget: u64,
) {
    let mut last = 0;
    for _ in 0..budget / 5 {
        last = amount(region, pos).await;
        if last == expected {
            return;
        }
        world.ticks(5).await;
    }
    panic!("fluid at {pos:?} is {last} after {budget} ticks, expected {expected}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn poured_water_flows_and_fills_a_broken_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    floor(&region, (79, 96), (79, 96), true).await;
    let wall = BlockPos::new(88, FLOOR_Y + 1, 84);
    blocks(
        &region,
        BlockOp::Set {
            changes: vec![(wall, Block::Stone.default_state())],
            destroy: false,
            keep: false,
            strict: true,
            quiet: false,
            data: None,
        },
    )
    .await;

    let ground = BlockPos::new(88, FLOOR_Y, 88);
    region
        .request(pour(&player, ground, Item::WaterBucket, 1))
        .await;
    assert!(matches!(
        used(&mut events, 1).await,
        ActionOutcome::Used { .. }
    ));
    let around = BlockPos::new(88, FLOOR_Y + 1, 83);
    settle(&world, &region, around, 1, 400).await;
    let source = BlockPos::new(88, FLOOR_Y + 1, 88);
    assert!(
        block_at(&region, source)
            .await
            .unwrap()
            .fluid_state()
            .is_source()
    );
    assert_eq!(amount(&region, BlockPos::new(91, FLOOR_Y + 1, 88)).await, 5);
    assert_eq!(amount(&region, BlockPos::new(88, FLOOR_Y + 1, 85)).await, 5);

    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: wall,
            tool: None,
            mode: BreakMode::Creative,
            sequence: 2,
        })
        .await;
    used(&mut events, 2).await;
    settle(&world, &region, around, 3, 400).await;
    assert_eq!(amount(&region, wall).await, 4, "water fills the gap");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_flows_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, ChunkPos::new(31, 6)).await;
    view_around(&world, &player, &mut events, ChunkPos::new(32, 6)).await;
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    floor(&west, (500, 511), (95, 113), false).await;
    floor(&east, (512, 520), (95, 113), false).await;
    world.ticks(3).await;

    let ground = BlockPos::new(509, FLOOR_Y, 104);
    west.request(pour(&player, ground, Item::WaterBucket, 1))
        .await;
    used(&mut events, 1).await;
    settle(&world, &east, BlockPos::new(514, FLOOR_Y + 1, 105), 2, 600).await;
    settle(&world, &east, BlockPos::new(516, FLOOR_Y + 1, 104), 1, 600).await;
    world.ticks(20).await;
    assert_eq!(amount(&east, BlockPos::new(512, FLOOR_Y + 1, 104)).await, 5);
    assert_eq!(amount(&east, BlockPos::new(517, FLOOR_Y + 1, 104)).await, 0);

    west.request(pour(&player, ground, Item::Bucket, 2)).await;
    used(&mut events, 2).await;
    settle(&world, &east, BlockPos::new(516, FLOOR_Y + 1, 104), 0, 800).await;
    world.ticks(40).await;
    for x in 510..=516 {
        assert_eq!(
            amount(&east, BlockPos::new(x, FLOOR_Y + 1, 104)).await
                + amount(&west, BlockPos::new(x, FLOOR_Y + 1, 104)).await,
            0,
            "drained at x {x}"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fluid_ticks_are_saved_with_the_chunk_and_resume() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    let source = BlockPos::new(88, FLOOR_Y + 1, 88);

    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    floor(&region, (79, 96), (79, 96), true).await;
    blocks(
        &region,
        BlockOp::Set {
            changes: vec![(source, Block::Lava.default_state())],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    world.ticks(2).await;
    world.shutdown().await;

    let root = stored_chunk(dir.path(), center, false).expect("chunk saved");
    let ticks = root.get_list("fluid_ticks").expect("fluid ticks saved");
    let tick = ticks
        .iter()
        .filter_map(Tag::as_compound)
        .find(|tick| tick.get_str("i") == Some("minecraft:lava"))
        .expect("the lava's tick");
    assert_eq!(
        (tick.get_int("x"), tick.get_int("y"), tick.get_int("z")),
        (Some(88), Some(FLOOR_Y + 1), Some(88))
    );
    let delay = tick.get_int("t").unwrap();
    assert!((1..30).contains(&delay), "delay {delay} left of 30");

    let world = TestWorld::start(dir.path());
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    settle(&world, &region, BlockPos::new(89, FLOOR_Y + 1, 88), 6, 400).await;
    world.shutdown().await;
}

pub(super) async fn drop_item(region: &RegionHandle, position: [f64; 3]) -> i32 {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", "minecraft:item").with(
                "Item",
                Compound::new()
                    .with("id", "minecraft:dirt")
                    .with("count", 1),
            ),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "the item loads from NBT");
    item_position(region).await.0
}

pub(super) async fn item_position(region: &RegionHandle) -> (i32, [f64; 3]) {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let item = answer
        .await
        .unwrap()
        .into_iter()
        .find(|entity| entity.kind == tuff_vanilla_data::registry::EntityType::Item)
        .expect("the item is in the region");
    (item.id, item.position)
}

pub(super) async fn watch_item(
    world: &TestWorld,
    region: &RegionHandle,
    budget: u64,
    done: impl Fn([f64; 3]) -> bool,
) -> [f64; 3] {
    let mut last = [0.0; 3];
    for _ in 0..budget / 5 {
        last = item_position(region).await.1;
        if done(last) {
            return last;
        }
        world.ticks(5).await;
    }
    panic!("the item is at {last:?} after {budget} ticks");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_float_up_in_still_water() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    floor(&region, (84, 92), (84, 92), true).await;
    let mut water = Vec::new();
    for x in 85..=91 {
        for z in 85..=91 {
            for y in 1..=3 {
                water.push((
                    BlockPos::new(x, FLOOR_Y + y, z),
                    Block::Water.default_state(),
                ));
            }
        }
    }
    blocks(
        &region,
        BlockOp::Set {
            changes: water,
            destroy: false,
            keep: false,
            strict: true,
            quiet: false,
            data: None,
        },
    )
    .await;
    drop_item(&region, [88.5, FLOOR_Y as f64 + 1.0, 88.5]).await;
    let surface = FLOOR_Y as f64 + 3.5;
    let top = watch_item(&world, &region, 400, |p| p[1] > surface).await;
    assert!((top[0] - 88.5).abs() < 1e-6 && (top[2] - 88.5).abs() < 1e-6);
    world.ticks(100).await;
    let floating = item_position(&region).await.1;
    assert!(
        floating[1] > FLOOR_Y as f64 + 3.0 && floating[1] < FLOOR_Y as f64 + 4.0,
        "the item bobs at the surface: {floating:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flowing_water_carries_items_downstream() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let region = east_flowing_channel(&world, &player, &mut events).await;
    let start = [86.5, FLOOR_Y as f64 + 1.0, 88.5];
    drop_item(&region, start).await;
    let moved = watch_item(&world, &region, 300, |p| p[0] > start[0] + 2.0).await;
    assert!(
        (moved[2] - start[2]).abs() < 0.5,
        "the current runs along x here: {moved:?}"
    );
    world.shutdown().await;
}

async fn mob_position(region: &RegionHandle, kind: EntityType) -> [f64; 3] {
    mobs(region, kind)
        .await
        .first()
        .expect("the mob is in the region")
        .position
}

/// A rimmed channel with water poured at its west end, flowing east along
/// z 88 by the time this returns.
async fn east_flowing_channel(
    world: &TestWorld,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> RegionHandle {
    let center = ChunkPos::new(5, 5);
    view_around(world, player, events, center).await;
    let region = world.region(center.region()).await;
    floor(&region, (79, 96), (79, 96), true).await;
    let ground = BlockPos::new(84, FLOOR_Y, 88);
    region
        .request(pour(player, ground, Item::WaterBucket, 1))
        .await;
    used(events, 1).await;
    settle(world, &region, BlockPos::new(91, FLOOR_Y + 1, 88), 1, 400).await;
    region
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flowing_water_carries_mobs_downstream() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let region = east_flowing_channel(&world, &player, &mut events).await;
    let start = [86.5, FLOOR_Y as f64 + 1.0, 88.5];
    let no_ai = || Compound::new().with("NoAI", true);
    summon(&region, EntityType::Cow, no_ai(), start).await;
    let mut cow = start;
    for _ in 0..60 {
        cow = mob_position(&region, EntityType::Cow).await;
        if cow[0] > start[0] + 2.0 {
            break;
        }
        world.ticks(5).await;
    }
    assert!(cow[0] > start[0] + 2.0, "the cow drifts east: {cow:?}");
    assert!((cow[2] - start[2]).abs() < 0.5, "along x only: {cow:?}");
    summon(&region, EntityType::Cod, no_ai(), start).await;
    world.ticks(40).await;
    let cod = mob_position(&region, EntityType::Cod).await;
    assert!(
        (cod[0] - start[0]).abs() < 0.5,
        "fish are not pushed by currents: {cod:?}"
    );
    world.shutdown().await;
}

//! Bubble columns forming over soul sand and magma under water, changing
//! with their base, vanishing without it, and carrying items.

use super::fluids::{drop_item, watch_item};
use super::*;
use crate::world::messages::BlockOp;
use tuff_nbt::Tag;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::EntityType;

const BASE_Y: i32 = 200;
const TOP_Y: i32 = BASE_Y + 5;
const X: i32 = 88;
const Z: i32 = 88;

async fn set(region: &RegionHandle, changes: Vec<(BlockPos, BlockState)>, strict: bool) {
    blocks(
        region,
        BlockOp::Set {
            changes,
            destroy: false,
            keep: false,
            strict,
            quiet: false,
            data: None,
        },
    )
    .await;
}

/// A one-block-wide stone shaft with water from above the base to
/// [`TOP_Y`] and air over it, the base itself stone.
async fn shaft(region: &RegionHandle) {
    let mut changes = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            for y in BASE_Y - 1..=TOP_Y + 3 {
                let pos = BlockPos::new(X + dx, y, Z + dz);
                let state = if dx != 0 || dz != 0 || y == BASE_Y - 1 || y == BASE_Y {
                    if y > TOP_Y { Block::Air } else { Block::Stone }
                } else if y <= TOP_Y {
                    Block::Water
                } else {
                    Block::Air
                };
                changes.push((pos, state.default_state()));
            }
        }
    }
    set(region, changes, true).await;
}

/// The drag of each cell from above the base to the top: `None` where
/// there is no column.
async fn cells(region: &RegionHandle) -> Vec<Option<bool>> {
    let mut out = Vec::new();
    for y in BASE_Y + 1..=TOP_Y {
        let state = block_at(region, BlockPos::new(X, y, Z)).await.unwrap();
        out.push((state.block() == Block::BubbleColumn).then(|| state.get("drag") == Some("true")));
    }
    out
}

async fn wait_cells(world: &TestWorld, region: &RegionHandle, expected: Option<bool>) {
    let mut last = Vec::new();
    for _ in 0..20 {
        last = cells(region).await;
        if last.iter().all(|cell| *cell == expected) {
            return;
        }
        world.ticks(5).await;
    }
    panic!("the shaft is {last:?}, expected every cell {expected:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn columns_form_change_and_vanish_with_their_base() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    shaft(&region).await;
    world.ticks(30).await;
    assert!(cells(&region).await.iter().all(Option::is_none));

    let base = BlockPos::new(X, BASE_Y, Z);
    set(
        &region,
        vec![(base, Block::SoulSand.default_state())],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(false)).await;

    set(
        &region,
        vec![(base, Block::MagmaBlock.default_state())],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(true)).await;

    set(&region, vec![(base, Block::Stone.default_state())], false).await;
    wait_cells(&world, &region, None).await;
    let top = block_at(&region, BlockPos::new(X, TOP_Y, Z)).await.unwrap();
    assert_eq!(
        top,
        Block::Water.default_state(),
        "the column leaves water behind"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_column_grows_through_water_poured_on_top_and_stops_at_a_gap() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    shaft(&region).await;
    let base = BlockPos::new(X, BASE_Y, Z);
    set(
        &region,
        vec![(base, Block::SoulSand.default_state())],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(false)).await;

    let middle = BlockPos::new(X, BASE_Y + 3, Z);
    set(&region, vec![(middle, Block::Stone.default_state())], false).await;
    world.ticks(20).await;
    let cells_now = cells(&region).await;
    assert_eq!(&cells_now[..2], &[Some(false), Some(false)]);
    assert!(cells_now[3..].iter().all(Option::is_none), "{cells_now:?}");

    set(&region, vec![(middle, Block::Water.default_state())], false).await;
    wait_cells(&world, &region, Some(false)).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_ride_an_upward_column_out_of_the_water_and_sink_in_a_downward_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    shaft(&region).await;
    let base = BlockPos::new(X, BASE_Y, Z);
    set(
        &region,
        vec![(base, Block::SoulSand.default_state())],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(false)).await;

    let middle = [X as f64 + 0.5, BASE_Y as f64 + 1.0, Z as f64 + 0.5];
    drop_item(&region, middle).await;
    let surface = TOP_Y as f64 + 1.0;
    watch_item(&world, &region, 200, |p| p[1] > surface + 0.5).await;

    set(
        &region,
        vec![(base, Block::MagmaBlock.default_state())],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(true)).await;
    watch_item(&world, &region, 200, |p| p[1] < BASE_Y as f64 + 1.2).await;
    world.shutdown().await;
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
    assert!(summoned.await.unwrap());
}

async fn watch_entity(
    world: &TestWorld,
    region: &RegionHandle,
    kind: EntityType,
    budget: u64,
    done: impl Fn(f64) -> bool,
) {
    let mut last = None;
    for _ in 0..budget {
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::Entities { reply }).await;
        last = answer
            .await
            .unwrap()
            .into_iter()
            .find(|entity| entity.kind == kind)
            .map(|entity| entity.position[1]);
        if last.is_some_and(&done) {
            return;
        }
        world.ticks(1).await;
    }
    panic!("the {} is at y {last:?} after {budget} ticks", kind.name());
}

/// A 3x3 pool of columns over `base` inside a stone rim, wide enough for
/// a boat to float on without touching the rim.
async fn pool(world: &TestWorld, region: &RegionHandle, base: Block) {
    let mut changes = Vec::new();
    for dx in -3..=3i32 {
        for dz in -3..=3i32 {
            for y in BASE_Y - 1..=TOP_Y + 3 {
                let rim = dx.abs() == 3 || dz.abs() == 3;
                let state = if y > TOP_Y {
                    Block::Air
                } else if rim || y <= BASE_Y {
                    Block::Stone
                } else {
                    Block::Water
                };
                changes.push((BlockPos::new(X + dx, y, Z + dz), state.default_state()));
            }
        }
    }
    set(region, changes, true).await;
    set_bases(world, region, base).await;
}

async fn set_bases(world: &TestWorld, region: &RegionHandle, base: Block) {
    let mut changes = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            changes.push((BlockPos::new(X + dx, BASE_Y, Z + dz), base.default_state()));
        }
    }
    set(region, changes, false).await;
    wait_cells(world, region, Some(base == Block::MagmaBlock)).await;
}

async fn column_over(world: &TestWorld, region: &RegionHandle, base: Block) {
    shaft(region).await;
    set(
        region,
        vec![(BlockPos::new(X, BASE_Y, Z), base.default_state())],
        false,
    )
    .await;
    wait_cells(world, region, Some(base == Block::MagmaBlock)).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn primed_tnt_rides_an_upward_column() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    column_over(&world, &region, Block::SoulSand).await;
    let middle = [X as f64 + 0.5, BASE_Y as f64 + 2.5, Z as f64 + 0.5];
    summon(
        &region,
        Compound::new()
            .with("id", "minecraft:tnt")
            .with("fuse", 1000i16),
        middle,
    )
    .await;
    watch_entity(&world, &region, EntityType::Tnt, 200, |y| {
        y > TOP_Y as f64 + 1.0
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_is_pulled_under_by_a_downward_column_and_tossed_by_an_upward_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    pool(&world, &region, Block::MagmaBlock).await;
    let surface = [X as f64 + 0.5, TOP_Y as f64 + 0.6, Z as f64 + 0.5];
    summon(
        &region,
        Compound::new().with("id", "minecraft:oak_boat"),
        surface,
    )
    .await;
    watch_entity(&world, &region, EntityType::OakBoat, 200, |y| {
        y < TOP_Y as f64 - 0.5
    })
    .await;

    set_bases(&world, &region, Block::SoulSand).await;
    watch_entity(&world, &region, EntityType::OakBoat, 300, |y| {
        y > TOP_Y as f64 + 2.0
    })
    .await;
    world.shutdown().await;
}

fn still_projectile(id: &str) -> Compound {
    Compound::new()
        .with("id", id)
        .with("acceleration_power", 0.0f64)
        .with(
            "Motion",
            Tag::List(vec![Tag::Double(0.0), Tag::Double(0.0), Tag::Double(0.0)]),
        )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fireballs_rise_in_an_upward_column_and_skulls_sink_in_a_downward_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    column_over(&world, &region, Block::SoulSand).await;
    let low = [X as f64 + 0.5, BASE_Y as f64 + 1.5, Z as f64 + 0.5];
    summon(&region, still_projectile("minecraft:small_fireball"), low).await;
    watch_entity(&world, &region, EntityType::SmallFireball, 200, |y| {
        y > TOP_Y as f64 + 1.0
    })
    .await;

    set(
        &region,
        vec![(
            BlockPos::new(X, BASE_Y, Z),
            Block::MagmaBlock.default_state(),
        )],
        false,
    )
    .await;
    wait_cells(&world, &region, Some(true)).await;
    let high = [X as f64 + 0.5, BASE_Y as f64 + 4.5, Z as f64 + 0.5];
    summon(&region, still_projectile("minecraft:wither_skull"), high).await;
    watch_entity(&world, &region, EntityType::WitherSkull, 200, |y| {
        y < BASE_Y as f64 + 3.0
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cactus_destroys_boats_minecarts_and_end_crystals() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    let kinds = [
        ("minecraft:oak_boat", EntityType::OakBoat),
        ("minecraft:minecart", EntityType::Minecart),
        ("minecraft:end_crystal", EntityType::EndCrystal),
    ];
    let mut changes = Vec::new();
    for (i, _) in kinds.iter().enumerate() {
        let x = X + 3 * i as i32;
        changes.push((
            BlockPos::new(x, BASE_Y - 1, Z),
            Block::Stone.default_state(),
        ));
        changes.push((BlockPos::new(x, BASE_Y, Z), Block::Cactus.default_state()));
    }
    set(&region, changes, false).await;
    for (i, (id, _)) in kinds.iter().enumerate() {
        let at = [
            (X + 3 * i as i32) as f64 + 0.5,
            BASE_Y as f64 + 1.0,
            Z as f64 + 0.5,
        ];
        summon(&region, Compound::new().with("id", *id), at).await;
    }
    world.ticks(40).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let left = answer.await.unwrap();
    for (_, kind) in kinds {
        assert!(
            left.iter().all(|e| e.kind != kind),
            "{} survived {:?}",
            kind.name(),
            left.iter()
                .map(|e| (e.kind.name(), e.position))
                .collect::<Vec<_>>()
        );
    }
    world.shutdown().await;
}

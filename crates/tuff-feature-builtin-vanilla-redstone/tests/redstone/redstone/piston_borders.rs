//! Pistons at region borders and the world border, and what moving
//! blocks do to entities moving into them.

use super::*;
use crate::world::messages::BreakMode;
use tuff_vanilla_data::registry::EntityType;

/// A bench straddling the border between regions (0, 0) and (1, 0) at
/// x 512, wide enough for a copy of a circuit inside region (0, 0).
async fn wide_border_bench(dir: &Path) -> (Bench, RegionHandle) {
    let b = bench(
        dir,
        &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)],
        (496, 518),
        (84, 88),
    )
    .await;
    let east = b.world.region(ChunkPos::new(32, 0).region()).await;
    (b, east)
}

fn region_of<'a>(b: &'a Bench, east: &'a RegionHandle, pos: BlockPos) -> &'a RegionHandle {
    if pos.x < 512 { &b.region } else { east }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_observer_across_the_border_sees_a_push_on_the_same_tick_as_one_beside_it() {
    let dir = TestDir::new();
    let (b, east) = wide_border_bench(dir.path()).await;
    let pistons = [498, 510];
    for x in pistons {
        let observer = at(x + 3, 1, 86);
        set(
            &b.region,
            &[
                (at(x, 1, 86), "minecraft:piston[facing=east]"),
                (at(x + 1, 1, 86), "minecraft:stone"),
            ],
        )
        .await;
        set(
            region_of(&b, &east, observer),
            &[(observer, "minecraft:observer[facing=west]")],
        )
        .await;
    }
    b.world.ticks(6).await;
    set(
        &b.region,
        &[
            (at(498, 1, 87), "minecraft:redstone_block"),
            (at(510, 1, 87), "minecraft:redstone_block"),
        ],
    )
    .await;
    let mut fired = false;
    for tick in 1..=10 {
        b.world.ticks(1).await;
        let near = state(&b.region, at(501, 1, 86)).await.powered();
        let across = state(&east, at(513, 1, 86)).await.powered();
        assert_eq!(across, near, "tick {tick}");
        fired |= near == Some(true);
    }
    assert!(fired, "the observers saw the push");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lamp_switching_off_as_it_is_pushed_across_the_border_arrives_off() {
    let dir = TestDir::new();
    let (b, east) = wide_border_bench(dir.path()).await;
    let (lamp, lamp_power) = (at(512, 1, 86), at(512, 2, 86));
    set(
        &b.region,
        &[
            (at(510, 1, 86), "minecraft:piston[facing=east]"),
            (at(511, 1, 86), "minecraft:stone"),
            (at(510, 1, 87), "minecraft:repeater[facing=south,delay=2]"),
        ],
    )
    .await;
    set(
        &east,
        &[
            (lamp, "minecraft:redstone_lamp"),
            (lamp_power, "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(6).await;
    assert_eq!(state(&east, lamp).await.lit(), Some(true));
    set(&east, &[(lamp_power, "minecraft:air")]).await;
    set(&b.region, &[(at(510, 1, 88), "minecraft:redstone_block")]).await;
    b.world.ticks(3).await;
    assert_eq!(
        state(&b.region, at(510, 1, 86)).await.extended(),
        Some(false)
    );
    assert_eq!(state(&east, lamp).await.lit(), Some(true));
    b.world.ticks(1).await;
    assert_eq!(
        state(&b.region, at(510, 1, 86)).await.extended(),
        Some(true),
        "the push started on the tick the lamp switched off"
    );
    let pushed = state(&east, lamp).await;
    assert!(
        pushed.block() == Block::MovingPiston || pushed.lit() == Some(false),
        "{pushed:?}"
    );
    b.world.ticks(2).await;
    let landed = state(&east, at(513, 1, 86)).await;
    assert_eq!(landed.block(), Block::RedstoneLamp);
    assert_eq!(
        landed.lit(),
        Some(false),
        "the lamp that moved is the one really there, not the halo's copy"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_creative_break_of_a_head_across_the_border_removes_its_base() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    let (base, head) = (at(511, 1, 86), at(512, 1, 86));
    set(
        &b.region,
        &[
            (base, "minecraft:piston[facing=east]"),
            (at(511, 1, 87), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(5).await;
    assert_eq!(state(&east, head).await.block(), Block::PistonHead);
    east.request(RegionRequest::Break {
        player: b.player.clone(),
        pos: head,
        tool: None,
        mode: BreakMode::Creative,
        sequence: 1,
    })
    .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, base).await.block(), Block::Air);
    assert_eq!(state(&east, head).await.block(), Block::Air);
    assert_eq!(item_count(&b.region).await + item_count(&east).await, 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_creative_break_of_a_bed_across_the_border_drops_nothing() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    let (foot, head) = (at(511, 1, 86), at(512, 1, 86));
    set(
        &b.region,
        &[(foot, "minecraft:red_bed[facing=east,part=foot]")],
    )
    .await;
    set(&east, &[(head, "minecraft:red_bed[facing=east,part=head]")]).await;
    b.world.ticks(2).await;
    b.region
        .request(RegionRequest::Break {
            player: b.player.clone(),
            pos: foot,
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;
    b.world.ticks(3).await;
    assert_eq!(state(&b.region, foot).await.block(), Block::Air);
    assert_eq!(state(&east, head).await.block(), Block::Air);
    assert_eq!(item_count(&b.region).await + item_count(&east).await, 0);
    b.world.shutdown().await;
}

/// The last block column inside the default world border (x below
/// 29999984).
const EDGE: i32 = 29_999_983;

async fn edge_bench(dir: &Path) -> Bench {
    bench(
        dir,
        &[ChunkPos::of_block(BlockPos::new(EDGE, 0, 86))],
        (EDGE - 4, EDGE + 2),
        (84, 88),
    )
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_pushes_out_of_the_world_border_but_not_a_block_beyond_it() {
    let dir = TestDir::new();
    let b = edge_bench(dir.path()).await;
    let (out, blocked) = (86, 88);
    set(
        &b.region,
        &[
            (at(EDGE - 1, 1, out), "minecraft:piston[facing=east]"),
            (at(EDGE, 1, out), "minecraft:stone"),
            (at(EDGE - 1, 1, blocked), "minecraft:piston[facing=east]"),
            (at(EDGE, 1, blocked), "minecraft:stone"),
            (at(EDGE + 1, 1, blocked), "minecraft:stone"),
        ],
    )
    .await;
    set(
        &b.region,
        &[
            (at(EDGE - 2, 1, out), "minecraft:redstone_block"),
            (at(EDGE - 2, 1, blocked), "minecraft:redstone_block"),
        ],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(
        blocks_named(&b, (EDGE - 1..=EDGE + 1).map(|x| at(x, 1, out))).await,
        vec![Block::Piston, Block::PistonHead, Block::Stone],
        "a block inside the border may be pushed past it"
    );
    assert_eq!(
        blocks_named(&b, (EDGE - 1..=EDGE + 2).map(|x| at(x, 1, blocked))).await,
        vec![Block::Piston, Block::Stone, Block::Stone, Block::Air],
        "a block outside the border does not move"
    );
    assert_eq!(
        state(&b.region, at(EDGE - 1, 1, blocked)).await.extended(),
        Some(false)
    );
    b.world.shutdown().await;
}

/// A piston at `(84, 1, z)` facing east under a stone at `(85, 1, z)`,
/// with glass walls around the cell on top of the stone.
async fn pushed_from_under(b: &Bench, z: i32) {
    set(
        &b.region,
        &[
            (at(84, 1, z), "minecraft:piston[facing=east]"),
            (at(85, 1, z), "minecraft:stone"),
            (at(84, 2, z), "minecraft:glass"),
            (at(86, 2, z), "minecraft:glass"),
            (at(85, 2, z - 1), "minecraft:glass"),
            (at(85, 2, z + 1), "minecraft:glass"),
        ],
    )
    .await;
}

/// Powers the pistons of [`pushed_from_under`] at each `z` and returns
/// the lowest each entity of its kind in that row got while they pushed.
async fn lowest_while_pushed(b: &Bench, rows: &[(i32, EntityType)]) -> Vec<f64> {
    let power: Vec<(BlockPos, &str)> = rows
        .iter()
        .map(|(z, _)| (at(83, 1, *z), "minecraft:redstone_block"))
        .collect();
    set(&b.region, &power).await;
    let mut lowest = vec![f64::INFINITY; rows.len()];
    for _ in 0..4 {
        b.world.ticks(1).await;
        for (row, (z, kind)) in rows.iter().enumerate() {
            for entity in super::mobs(&b.region, *kind).await {
                if (entity.position[2] - (f64::from(*z) + 0.5)).abs() < 0.5 {
                    lowest[row] = lowest[row].min(entity.position[1]);
                }
            }
        }
    }
    lowest
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_stands_on_a_block_sliding_out_from_under_it() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    pushed_from_under(&b, 86).await;
    let top = f64::from(FLOOR_Y + 2);
    super::summon(
        &b.region,
        EntityType::Pig,
        Compound::new(),
        [85.5, top, 86.5],
    )
    .await;
    b.world.ticks(10).await;
    let lowest = lowest_while_pushed(&b, &[(86, EntityType::Pig)]).await;
    assert!(
        lowest[0] > top - 1.0e-3,
        "the pig stays on the moving block: {lowest:?}"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_and_an_orb_rest_on_a_block_sliding_out_from_under_them() {
    let dir = TestDir::new();
    let b = piston_bench(dir.path()).await;
    let top = f64::from(FLOOR_Y + 2);
    pushed_from_under(&b, 85).await;
    pushed_from_under(&b, 88).await;
    let item = Compound::new()
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:dirt")
                .with("count", 1i32),
        )
        .with("PickupDelay", 32767i16);
    super::summon(&b.region, EntityType::Item, item, [85.7, top, 85.5]).await;
    super::summon(
        &b.region,
        EntityType::ExperienceOrb,
        Compound::new().with("Value", 1i16),
        [85.7, top, 88.5],
    )
    .await;
    b.world.ticks(10).await;
    let lowest = lowest_while_pushed(
        &b,
        &[(85, EntityType::Item), (88, EntityType::ExperienceOrb)],
    )
    .await;
    assert!(
        lowest.iter().all(|y| *y > top - 1.0e-3),
        "the item and the orb stay on the moving block: {lowest:?}"
    );
    b.world.shutdown().await;
}

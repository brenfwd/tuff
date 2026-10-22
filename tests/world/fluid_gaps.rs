//! Sponges soaking up water, broken ice leaving water behind, and barriers
//! only creative players waterlog or drain.

use super::*;
use crate::world::messages::{BlockOp, BucketUse};
use tuff_item::item::Enchantments;
use tuff_vanilla_data::BlockState;

const FLOOR_Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

async fn set_strict(region: &RegionHandle, changes: Vec<(BlockPos, BlockState)>) {
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

/// A stone floor with a stone rim around a square of `radius` around
/// [`X`], [`Z`] one block deep, filled with `fill`, and air above.
async fn basin(region: &RegionHandle, radius: i32, fill: Block) {
    let mut changes = Vec::new();
    for dx in -radius - 1..=radius + 1 {
        for dz in -radius - 1..=radius + 1 {
            let rim = dx.abs() > radius || dz.abs() > radius;
            let pos = |y| BlockPos::new(X + dx, y, Z + dz);
            changes.push((pos(FLOOR_Y), Block::Stone.default_state()));
            let inside = if rim { Block::Stone } else { fill };
            changes.push((pos(FLOOR_Y + 1), inside.default_state()));
            for y in FLOOR_Y + 2..=FLOOR_Y + 4 {
                changes.push((pos(y), Block::Air.default_state()));
            }
        }
    }
    set_strict(region, changes).await;
}

async fn waters_in(region: &RegionHandle, radius: i32) -> usize {
    let mut count = 0;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let pos = BlockPos::new(X + dx, FLOOR_Y + 1, Z + dz);
            if block_at(region, pos)
                .await
                .is_some_and(|s| s.block() == Block::Water)
            {
                count += 1;
            }
        }
    }
    count
}

async fn start() -> (TestDir, TestWorld, RegionHandle, PlayerHandle, Events) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    (dir, world, region, player, events)
}

type Events = mpsc::UnboundedReceiver<PlayerEvent>;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_placed_sponge_soaks_up_the_pool_around_it_and_turns_wet() {
    let (_dir, world, region, _player, _events) = start().await;
    basin(&region, 4, Block::Water).await;
    assert_eq!(waters_in(&region, 4).await, 81);
    let sponge = BlockPos::new(X, FLOOR_Y + 1, Z);
    place_blocks(&world, &region, [(sponge, Block::Sponge.default_state())]).await;
    assert_eq!(
        block_at(&region, sponge).await.map(BlockState::block),
        Some(Block::WetSponge)
    );
    assert_eq!(
        waters_in(&region, 4).await,
        80 - 65,
        "the first removal's update makes the sponge soak again inside it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sponge_soaks_up_water_that_flows_up_to_it() {
    let (_dir, world, region, _player, _events) = start().await;
    basin(&region, 3, Block::Air).await;
    let sponge = BlockPos::new(X, FLOOR_Y + 1, Z);
    let source = BlockPos::new(X + 3, FLOOR_Y + 1, Z);
    set_strict(&region, vec![(sponge, Block::Sponge.default_state())]).await;
    place_blocks(&world, &region, [(source, Block::Water.default_state())]).await;
    let mut last = None;
    for _ in 0..30 {
        last = block_at(&region, sponge).await;
        if last.is_some_and(|s| s.block() == Block::WetSponge) {
            break;
        }
        world.ticks(2).await;
    }
    assert_eq!(last.map(BlockState::block), Some(Block::WetSponge));
    assert_eq!(waters_in(&region, 3).await, 0, "the source went too");
    world.shutdown().await;
}

async fn break_ice(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut Events,
    pos: BlockPos,
    tool: Option<ItemStack>,
    mode: BreakMode,
    sequence: i32,
) -> Option<BlockState> {
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos,
            tool: tool.as_ref().map(tuff_item::item::ItemSnapshot::of),
            mode,
            sequence,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seq,
            outcome: ActionOutcome::Broken { .. },
        } if seq == sequence => Some(()),
        _ => None,
    })
    .await;
    block_at(region, pos).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broken_ice_leaves_water_over_ground_unless_silk_touched_or_creative() {
    let (_dir, world, region, player, mut events) = start().await;
    let ice = Block::Ice.default_state();
    let at = |dx: i32, y: i32| BlockPos::new(X + dx, y, Z);
    set_strict(
        &region,
        vec![
            (at(0, FLOOR_Y), Block::Stone.default_state()),
            (at(0, FLOOR_Y + 1), ice),
            (at(1, FLOOR_Y), Block::Stone.default_state()),
            (at(1, FLOOR_Y + 1), ice),
            (at(2, FLOOR_Y), Block::Stone.default_state()),
            (at(2, FLOOR_Y + 1), ice),
            (at(3, FLOOR_Y), Block::Air.default_state()),
            (at(3, FLOOR_Y + 1), ice),
            (at(4, FLOOR_Y), Block::Stone.default_state()),
            (at(4, FLOOR_Y + 1), Block::PackedIce.default_state()),
        ],
    )
    .await;
    let water = Block::Water.default_state();
    let air = Some(Block::Air.default_state());
    let melted = break_ice(
        &region,
        &player,
        &mut events,
        at(0, FLOOR_Y + 1),
        None,
        BreakMode::Finish,
        1,
    )
    .await;
    assert_eq!(melted, Some(water));
    let mut silk = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    silk.components.enchantments =
        Some(Enchantments::new([("minecraft:silk_touch".to_owned(), 1)]));
    let kept = break_ice(
        &region,
        &player,
        &mut events,
        at(1, FLOOR_Y + 1),
        Some(silk),
        BreakMode::Finish,
        2,
    )
    .await;
    assert_eq!(kept, air, "silk touch keeps it frozen");
    let creative = break_ice(
        &region,
        &player,
        &mut events,
        at(2, FLOOR_Y + 1),
        None,
        BreakMode::Creative,
        3,
    )
    .await;
    assert_eq!(creative, air);
    let over_air = break_ice(
        &region,
        &player,
        &mut events,
        at(3, FLOOR_Y + 1),
        None,
        BreakMode::Finish,
        4,
    )
    .await;
    assert_eq!(over_air, air, "nothing under it to hold the water");
    let packed = break_ice(
        &region,
        &player,
        &mut events,
        at(4, FLOOR_Y + 1),
        None,
        BreakMode::Finish,
        5,
    )
    .await;
    assert_eq!(packed, air, "packed ice never melts");
    world.shutdown().await;
}

fn bucket(
    player: &PlayerHandle,
    target: BlockPos,
    item: Item,
    creative: bool,
    sequence: i32,
) -> RegionRequest {
    let fluid = match item.bucket() {
        Some(tuff_vanilla_data::item::Bucket::Fluid { fluid, .. }) => fluid,
        _ => None,
    };
    let eye = [
        target.x as f64 + 0.5,
        target.y as f64 + 2.5,
        target.z as f64 + 0.5,
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
        creative,
        sequence,
        hit: None,
    })
}

async fn outcome(events: &mut Events, sequence: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seq,
            outcome,
        } if seq == sequence => Some(outcome),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_creative_players_waterlog_and_drain_barriers() {
    let (_dir, world, region, player, mut events) = start().await;
    basin(&region, 2, Block::Barrier).await;
    let barrier = BlockPos::new(X, FLOOR_Y + 1, Z);
    let above = BlockPos::new(X, FLOOR_Y + 2, Z);

    let wet = Block::Barrier
        .default_state()
        .with_waterlogged(true)
        .unwrap();
    region
        .request(bucket(&player, barrier, Item::WaterBucket, true, 1))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Used { .. }
    ));
    assert_eq!(block_at(&region, barrier).await, Some(wet));

    region
        .request(bucket(&player, barrier, Item::Bucket, false, 2))
        .await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Rejected(_)
    ));
    assert_eq!(block_at(&region, barrier).await, Some(wet));

    region
        .request(bucket(&player, barrier, Item::Bucket, true, 3))
        .await;
    assert!(matches!(
        outcome(&mut events, 3).await,
        ActionOutcome::Used { .. }
    ));
    assert_eq!(
        block_at(&region, barrier).await,
        Some(Block::Barrier.default_state())
    );

    region
        .request(bucket(&player, barrier, Item::WaterBucket, false, 4))
        .await;
    assert!(matches!(
        outcome(&mut events, 4).await,
        ActionOutcome::Used { .. }
    ));
    assert_eq!(
        block_at(&region, barrier).await,
        Some(Block::Barrier.default_state()),
        "survival water goes on top"
    );
    assert_eq!(
        block_at(&region, above).await.map(BlockState::block),
        Some(Block::Water)
    );
    world.shutdown().await;
}

fn aim_down(player: &PlayerHandle, over: BlockPos, item: Item, sequence: i32) -> RegionRequest {
    let mut place = place(player, over, item, sequence);
    place.eye = [
        over.x as f64 + 0.5,
        over.y as f64 + 2.6,
        over.z as f64 + 0.5,
    ];
    place.pitch = 90.0;
    RegionRequest::PlaceOnWater(place)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lily_pads_and_frogspawn_go_on_still_water_only() {
    let (_dir, world, region, player, mut events) = start().await;
    basin(&region, 2, Block::Water).await;
    let flowing = BlockPos::new(X + 2, FLOOR_Y + 1, Z);
    set_strict(
        &region,
        vec![(
            flowing,
            Block::Water.default_state().with("level", "2").unwrap(),
        )],
    )
    .await;
    let surface = |dx: i32| BlockPos::new(X + dx, FLOOR_Y + 2, Z);

    region
        .request(aim_down(
            &player,
            BlockPos::new(X, FLOOR_Y + 1, Z),
            Item::LilyPad,
            1,
        ))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(
        block_at(&region, surface(0)).await.map(BlockState::block),
        Some(Block::LilyPad)
    );

    region
        .request(aim_down(
            &player,
            BlockPos::new(X - 1, FLOOR_Y + 1, Z),
            Item::Frogspawn,
            2,
        ))
        .await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(
        block_at(&region, surface(-1)).await.map(BlockState::block),
        Some(Block::Frogspawn)
    );

    region
        .request(aim_down(&player, flowing, Item::LilyPad, 3))
        .await;
    assert!(matches!(
        outcome(&mut events, 3).await,
        ActionOutcome::Rejected(_)
    ));
    assert_eq!(
        block_at(&region, surface(2)).await,
        Some(Block::Air.default_state())
    );
    world.shutdown().await;
}

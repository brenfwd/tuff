//! Support rules of bells, chorus and hanging moss through a real region:
//! placement is refused without support, and losing support breaks them
//! (chorus and moss one tick later).

use super::*;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

async fn items(region: &RegionHandle) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::Item)
        .count()
}

async fn state(region: &RegionHandle, pos: BlockPos) -> BlockState {
    block_at(region, pos).await.unwrap()
}

struct Bench {
    world: TestWorld,
    region: RegionHandle,
    player: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
}

async fn bench(dir: &Path) -> Bench {
    let world = TestWorld::start(dir);
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    Bench {
        world,
        region,
        player,
        events,
    }
}

fn at(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, SKY + y, z)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chorus_is_only_placed_on_its_ground_and_breaks_a_tick_after_losing_it() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    place_blocks(
        &b.world,
        &b.region,
        [
            (at(4, 0, 4), Block::Stone.default_state()),
            (at(8, 0, 8), Block::EndStone.default_state()),
        ],
    )
    .await;

    let refused = place(&b.player, at(4, 0, 4), Item::ChorusFlower, 1);
    b.region.request(RegionRequest::Place(refused)).await;
    assert!(matches!(
        outcome(&mut b.events, 1).await,
        ActionOutcome::Rejected(_)
    ));
    assert_eq!(state(&b.region, at(4, 1, 4)).await.block(), Block::Air);

    let planted = place(&b.player, at(8, 0, 8), Item::ChorusPlant, 2);
    b.region.request(RegionRequest::Place(planted)).await;
    assert!(matches!(
        outcome(&mut b.events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    let plant = state(&b.region, at(8, 1, 8)).await;
    assert_eq!(plant.block(), Block::ChorusPlant);
    assert_eq!(plant.get("down"), Some("true"));

    let topped = place(&b.player, at(8, 1, 8), Item::ChorusFlower, 3);
    b.region.request(RegionRequest::Place(topped)).await;
    assert!(matches!(
        outcome(&mut b.events, 3).await,
        ActionOutcome::Placed { .. }
    ));
    assert_eq!(
        state(&b.region, at(8, 2, 8)).await.block(),
        Block::ChorusFlower
    );
    assert_eq!(state(&b.region, at(8, 1, 8)).await.get("up"), Some("true"));

    place_blocks(
        &b.world,
        &b.region,
        [(at(8, 0, 8), Block::Air.default_state())],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, at(8, 1, 8)).await.block(), Block::Air);
    assert_eq!(state(&b.region, at(8, 2, 8)).await.block(), Block::Air);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wall_bell_turns_double_and_back_and_drops_with_its_last_wall() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let (wall, far, bell) = (at(8, 1, 8), at(8, 1, 6), at(8, 1, 7));
    place_blocks(&b.world, &b.region, [(wall, Block::Stone.default_state())]).await;

    let mut hang = place(&b.player, wall, Item::Bell, 1);
    hang.face = tuff_world_model::Face::North;
    hang.cursor = [0.5, 0.5, 0.0];
    b.region.request(RegionRequest::Place(hang)).await;
    assert!(matches!(
        outcome(&mut b.events, 1).await,
        ActionOutcome::Placed { .. }
    ));
    let placed = state(&b.region, bell).await;
    assert_eq!(placed.block(), Block::Bell);
    assert_eq!(placed.get("attachment"), Some("single_wall"));
    assert_eq!(placed.facing(), Some(Facing::South));

    place_blocks(&b.world, &b.region, [(far, Block::Stone.default_state())]).await;
    assert_eq!(
        state(&b.region, bell).await.get("attachment"),
        Some("double_wall")
    );

    place_blocks(&b.world, &b.region, [(wall, Block::Air.default_state())]).await;
    let single = state(&b.region, bell).await;
    assert_eq!(single.get("attachment"), Some("single_wall"));
    assert_eq!(single.facing(), Some(Facing::North));

    place_blocks(&b.world, &b.region, [(far, Block::Air.default_state())]).await;
    assert_eq!(state(&b.region, bell).await.block(), Block::Air);
    b.world.ticks(2).await;
    assert!(items(&b.region).await >= 1, "the bell drops");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hanging_moss_needs_a_ceiling_and_falls_apart_without_it() {
    let dir = TestDir::new();
    let mut b = bench(dir.path()).await;
    let ceiling = at(8, 6, 8);
    place_blocks(
        &b.world,
        &b.region,
        [(ceiling, Block::Stone.default_state())],
    )
    .await;

    let refused = place(&b.player, at(4, 0, 4), Item::PaleHangingMoss, 1);
    b.region.request(RegionRequest::Place(refused)).await;
    assert!(matches!(
        outcome(&mut b.events, 1).await,
        ActionOutcome::Rejected(_)
    ));

    for (sequence, clicked) in [(2, ceiling), (3, at(8, 5, 8))] {
        let mut hang = place(&b.player, clicked, Item::PaleHangingMoss, sequence);
        hang.face = tuff_world_model::Face::Down;
        hang.cursor = [0.5, 0.0, 0.5];
        hang.eye = [8.5, (SKY + 3) as f64, 8.5];
        b.region.request(RegionRequest::Place(hang)).await;
        assert!(
            matches!(
                outcome(&mut b.events, sequence).await,
                ActionOutcome::Placed { .. }
            ),
            "moss under {clicked:?}"
        );
    }
    assert_eq!(
        state(&b.region, at(8, 5, 8)).await.get("tip"),
        Some("false")
    );
    assert_eq!(state(&b.region, at(8, 4, 8)).await.get("tip"), Some("true"));

    place_blocks(&b.world, &b.region, [(ceiling, Block::Air.default_state())]).await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, at(8, 5, 8)).await.block(), Block::Air);
    assert_eq!(state(&b.region, at(8, 4, 8)).await.block(), Block::Air);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cactus_and_cane_drop_items_when_they_lose_support() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    place_blocks(
        &b.world,
        &b.region,
        [
            (at(4, -1, 4), Block::Stone.default_state()),
            (at(4, 0, 4), Block::Sand.default_state()),
            (at(4, 1, 4), Block::Cactus.default_state()),
            (at(8, -1, 8), Block::Stone.default_state()),
            (at(8, 0, 8), Block::Sand.default_state()),
            (at(9, 0, 8), Block::Water.default_state()),
            (at(8, 1, 8), Block::SugarCane.default_state()),
        ],
    )
    .await;
    b.world.ticks(2).await;
    assert_eq!(state(&b.region, at(4, 1, 4)).await.block(), Block::Cactus);
    assert_eq!(
        state(&b.region, at(8, 1, 8)).await.block(),
        Block::SugarCane
    );

    place_blocks(
        &b.world,
        &b.region,
        [
            (at(4, 0, 4), Block::Air.default_state()),
            (at(8, 0, 8), Block::Air.default_state()),
        ],
    )
    .await;
    b.world.ticks(4).await;
    assert_eq!(state(&b.region, at(4, 1, 4)).await.block(), Block::Air);
    assert_eq!(state(&b.region, at(8, 1, 8)).await.block(), Block::Air);
    assert_eq!(items(&b.region).await, 2);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_candle_stays_when_its_support_is_removed() {
    let dir = TestDir::new();
    let b = bench(dir.path()).await;
    place_blocks(
        &b.world,
        &b.region,
        [
            (at(4, 0, 4), Block::Stone.default_state()),
            (at(4, 1, 4), Block::Candle.default_state()),
        ],
    )
    .await;
    place_blocks(
        &b.world,
        &b.region,
        [(at(4, 0, 4), Block::Air.default_state())],
    )
    .await;
    b.world.ticks(5).await;
    assert_eq!(state(&b.region, at(4, 1, 4)).await.block(), Block::Candle);
    assert_eq!(items(&b.region).await, 0);
    b.world.shutdown().await;
}

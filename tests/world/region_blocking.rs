//! Blocks refused or reacting to entities: placement obstruction and
//! unstable TNT.

use super::*;
use tuff_vanilla_data::registry::EntityType;

async fn summon(region: &RegionHandle, id: &str, position: [f64; 3]) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", id),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(answer.await.unwrap(), "{id} summoned");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_blocks_placing_a_block_into_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let floor = BlockPos::new(3, Generator::new(1).height(3, 3), 3);
    let above = BlockPos::new(floor.x, floor.y + 1, floor.z);
    summon(
        &region,
        "minecraft:minecart",
        [3.5, f64::from(above.y), 3.5],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&alice, floor, Item::Stone, 1)))
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        block_at(&region, above).await.map(|s| s.block()),
        Some(Block::Air)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_unstable_tnt_primes_it_without_a_drop() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut events, &[pos]).await;
    let at = BlockPos::new(3, Generator::new(1).height(3, 3) + 1, 3);
    let unstable = Block::Tnt.default_state().with("unstable", "true").unwrap();
    place_blocks(&world, &region, [(at, unstable)]).await;
    region
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: at,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    world.ticks(1).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let kinds: Vec<_> = answer.await.unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EntityType::Tnt]);
    world.shutdown().await;
}

//! Using a light block cycles its level, for game masters only.

use super::*;
use crate::world::messages::BlockOp;

const Y: i32 = 200;
const X: i32 = 90;
const Z: i32 = 90;

async fn level(region: &RegionHandle, pos: BlockPos) -> String {
    let state: tuff_vanilla_data::BlockState = block_at(region, pos).await.unwrap();
    state.get("level").unwrap().to_string()
}

async fn click(
    region: &RegionHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    place: Place,
) {
    let sequence = place.sequence;
    region.request(RegionRequest::Place(place)).await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence: seq, .. } if seq == sequence => Some(()),
        _ => None,
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_light_block_cycles_its_level_only_for_game_masters() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(101);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    let pos = BlockPos::new(X, Y, Z);
    let light = Block::Light.default_state().with("level", "14").unwrap();
    blocks(
        &region,
        BlockOp::Set {
            changes: vec![(pos, light)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;

    let mut plain = place(&player, pos, Item::Stick, 1);
    plain.held = None;
    click(&region, &mut events, plain).await;
    assert_eq!(level(&region, pos).await, "14");

    let mut op = place(&player, pos, Item::Stick, 2);
    op.held = None;
    op.game_master = true;
    click(&region, &mut events, op).await;
    assert_eq!(level(&region, pos).await, "15");

    let mut op = place(&player, pos, Item::Stick, 3);
    op.held = None;
    op.game_master = true;
    click(&region, &mut events, op).await;
    assert_eq!(level(&region, pos).await, "0");
    world.shutdown().await;
}

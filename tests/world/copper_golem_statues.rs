//! Using a copper golem statue cycles its pose, except with an axe.

use super::*;
use crate::world::messages::BlockOp;
use tuff_vanilla_data::BlockState;

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

type Events = mpsc::UnboundedReceiver<PlayerEvent>;

async fn click(region: &RegionHandle, events: &mut Events, place: Place) -> ActionOutcome {
    let sequence = place.sequence;
    region.request(RegionRequest::Place(place)).await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seq,
            outcome,
        } if seq == sequence => Some(outcome),
        _ => None,
    })
    .await
}

async fn pose(region: &RegionHandle, pos: BlockPos) -> String {
    let state: BlockState = block_at(region, pos).await.unwrap();
    state.get("copper_golem_pose").unwrap().to_string()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_statue_cycles_its_pose_and_a_comparator_reads_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    let pos = BlockPos::new(X, Y, Z);
    let statue = Block::CopperGolemStatue
        .default_state()
        .with("copper_golem_pose", "standing")
        .unwrap();
    blocks(
        &region,
        BlockOp::Set {
            changes: vec![(pos, statue)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
    assert_eq!(statue.state_analog_output(), 1);

    let mut hand = place(&player, pos, Item::Stick, 1);
    hand.held = None;
    let outcome = click(&region, &mut events, hand).await;
    assert!(
        matches!(outcome, ActionOutcome::Interacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(pose(&region, pos).await, "sitting");

    click(&region, &mut events, place(&player, pos, Item::Stick, 2)).await;
    assert_eq!(pose(&region, pos).await, "running");
    click(&region, &mut events, place(&player, pos, Item::Stick, 3)).await;
    assert_eq!(pose(&region, pos).await, "star");
    let star = block_at(&region, pos).await.unwrap();
    assert_eq!(star.state_analog_output(), 4);
    click(&region, &mut events, place(&player, pos, Item::Stick, 4)).await;
    assert_eq!(pose(&region, pos).await, "standing");
    world.shutdown().await;
}

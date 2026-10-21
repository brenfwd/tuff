//! Compasses used on a lodestone lock onto it.

use super::*;
use crate::world::messages::{BlockOp, HandEffect};
use tuff_item::item::LodestoneTracker;
use tuff_vanilla_data::BlockState;

const POS: BlockPos = BlockPos::new(96, 200, 96);

async fn set(region: &RegionHandle, pos: BlockPos, state: BlockState) {
    blocks(
        region,
        BlockOp::Set {
            changes: vec![(pos, state)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data: None,
        },
    )
    .await;
}

async fn click(
    region: &RegionHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    mut place: Place,
) -> ActionOutcome {
    let sequence = place.sequence;
    place.item = None;
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

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compass_used_on_a_lodestone_locks_onto_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(6, 6);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    set(&region, POS, Block::Lodestone.default_state()).await;

    let outcome = click(&region, &mut events, place(&player, POS, Item::Compass, 1)).await;
    let ActionOutcome::Used {
        effect: HandEffect::Split(locked),
        ..
    } = outcome
    else {
        panic!("expected a split compass, got {outcome:?}");
    };
    assert_eq!(locked.item, Item::Compass);
    assert_eq!(locked.count, 1);
    let tracker = locked
        .components
        .other
        .get("minecraft:lodestone_tracker")
        .and_then(LodestoneTracker::from_nbt)
        .unwrap();
    assert_eq!(
        tracker,
        LodestoneTracker::locked("minecraft:overworld", [POS.x, POS.y, POS.z])
    );

    let plain = BlockPos::new(POS.x + 2, POS.y, POS.z);
    set(&region, plain, Block::Stone.default_state()).await;
    let outcome = click(
        &region,
        &mut events,
        place(&player, plain, Item::Compass, 2),
    )
    .await;
    assert!(
        !matches!(
            outcome,
            ActionOutcome::Used {
                effect: HandEffect::Split(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    world.shutdown().await;
}

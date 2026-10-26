//! Fall edge cases around honey and fast moves: landing on honey shows
//! its slide, sliding down a honey side resets the fall, and a one-tick
//! move through a cobweb or water is traced.

use super::*;
use crate::world::messages::PlayerUpdate;
use tuff_vanilla_data::registry::SoundEvent;

fn moving(
    player: &PlayerHandle,
    seq: u64,
    position: [f64; 3],
    movement: [f64; 3],
    landed: Option<f64>,
) -> RegionRequest {
    let RegionRequest::Player(mut update) = update(player, seq, position) else {
        unreachable!("update builds a player update");
    };
    update.looks.movement = movement;
    update.looks.height = 1.8;
    update.looks.eye_height = 1.62;
    RegionRequest::Player(PlayerUpdate {
        landed,
        grounded: landed.is_some(),
        ..update
    })
}

async fn resets_fall(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, want: u64) -> bool {
    expect(events, |event| match event {
        PlayerEvent::Surroundings {
            seq, resets_fall, ..
        } if seq == want => Some(resets_fall),
        _ => None,
    })
    .await
}

async fn bench() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(200);
    view_around(&world, &viewer, &mut events, chunk).await;
    present(&region, &viewer, [4.5, f64::from(SKY), 4.5]).await;
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(4, SKY, 4), Block::HoneyBlock.default_state()),
            (BlockPos::new(8, SKY - 3, 8), Block::Cobweb.default_state()),
            (BlockPos::new(8, SKY - 5, 8), Block::Stone.default_state()),
        ],
    )
    .await;
    (dir, world, region, viewer, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landing_on_honey_plays_its_slide_for_the_others() {
    let (_dir, world, region, _viewer, mut viewer_events) = bench().await;
    let (player, _events) = PlayerHandle::new(100);
    let top = [4.5, f64::from(SKY) + 0.9375, 4.5];
    region
        .request(moving(&player, 1, top, [0.0, -0.5, 0.0], Some(2.0)))
        .await;
    world.ticks(1).await;
    let honey_event = framed(&crate::protocol::play::EntityEvent {
        entity_id: 100,
        status: 54,
    });
    let (mut sound, mut event) = (false, false);
    while !(sound && event) {
        let packets = expect(&mut viewer_events, |event| match event {
            PlayerEvent::ChunkPackets { packets, .. } => Some(packets),
            _ => None,
        })
        .await;
        sound |= packets
            .iter()
            .filter_map(decode_sound)
            .any(|s| s.sound_event == SoundEvent::BlockHoneyBlockSlide.id());
        event |= packets.contains(&honey_event);
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_honey_slide_does_not_reach_a_viewer_beyond_the_sound_range() {
    let (_dir, world, region, viewer, mut viewer_events) = bench().await;
    region
        .request(update(&viewer, 1, [4.5, f64::from(SKY) + 1.0, 60.5]))
        .await;
    world.ticks(1).await;
    while viewer_events.try_recv().is_ok() {}
    let (player, _events) = PlayerHandle::new(100);
    let top = [4.5, f64::from(SKY) + 0.9375, 4.5];
    region
        .request(moving(&player, 1, top, [0.0, -0.5, 0.0], Some(2.0)))
        .await;
    world.ticks(2).await;
    while let Ok(event) = viewer_events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            assert!(
                !packets
                    .iter()
                    .filter_map(decode_sound)
                    .any(|s| s.sound_event == SoundEvent::BlockHoneyBlockSlide.id()),
                "the slide reached a viewer 56 blocks away"
            );
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sliding_down_a_honey_side_or_falling_fast_through_a_cobweb_resets_the_fall() {
    let (_dir, world, region, _viewer, _viewer_events) = bench().await;
    let (player, mut events) = PlayerHandle::new(100);
    let beside = [5.29, f64::from(SKY) + 0.2, 4.5];
    region
        .request(moving(&player, 1, beside, [0.0, -0.12, 0.0], None))
        .await;
    assert!(resets_fall(&mut events, 1).await, "sliding down the side");
    region
        .request(moving(&player, 2, beside, [0.0, -0.05, 0.0], None))
        .await;
    assert!(!resets_fall(&mut events, 2).await, "too slow to slide");

    let below_web = [8.5, f64::from(SKY) - 4.5, 8.5];
    region
        .request(moving(&player, 3, below_web, [0.0, -3.0, 0.0], None))
        .await;
    assert!(
        resets_fall(&mut events, 3).await,
        "the fast fall crossed the cobweb"
    );
    let beside_web = [10.5, f64::from(SKY) - 4.5, 8.5];
    region
        .request(moving(&player, 4, beside_web, [0.0, -3.0, 0.0], None))
        .await;
    assert!(!resets_fall(&mut events, 4).await);
    world.shutdown().await;
}

async fn clicks_heard(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> usize {
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            count += packets.iter().filter_map(decode_sound).count();
        }
    }
    count
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_sound_only_reaches_viewers_within_hearing_range() {
    let (_dir, world, region, viewer, mut viewer_events) = bench().await;
    let door = BlockPos::new(2, SKY + 4, 2);
    let tower = |lever_on: bool| {
        [
            (door, Block::IronTrapdoor.default_state()),
            (
                BlockPos::new(3, SKY + 4, 2),
                if lever_on {
                    Block::RedstoneBlock.default_state()
                } else {
                    Block::Air.default_state()
                },
            ),
        ]
    };
    region
        .request(update(&viewer, 1, [15.9, f64::from(SKY) + 12.0, 15.9]))
        .await;
    world.ticks(1).await;
    place_blocks(&world, &region, tower(false)).await;
    world.ticks(2).await;
    clicks_heard(&mut viewer_events).await;
    place_blocks(&world, &region, tower(true)).await;
    world.ticks(4).await;
    assert_eq!(clicks_heard(&mut viewer_events).await, 0, "far viewer");
    region
        .request(update(&viewer, 2, [4.5, f64::from(SKY) + 4.0, 4.5]))
        .await;
    world.ticks(1).await;
    place_blocks(&world, &region, tower(false)).await;
    world.ticks(2).await;
    clicks_heard(&mut viewer_events).await;
    place_blocks(&world, &region, tower(true)).await;
    world.ticks(4).await;
    assert!(clicks_heard(&mut viewer_events).await > 0, "near viewer");
    world.shutdown().await;
}

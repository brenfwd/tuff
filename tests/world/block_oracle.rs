//! Block actions at positions the client could not touch or has not been
//! sent must not answer with the true block state.

use super::anticheat::{Walker, walker};
use super::*;
use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_vanilla_data::packet::play::Clientbound;

const STONE: tuff_vanilla_data::BlockState = tuff_vanilla_data::Block::Stone.default_state();

async fn pump_ids(w: &mut Walker) -> Vec<i32> {
    let wait = std::time::Duration::from_millis(300);
    while let Ok(Some(event)) = tokio::time::timeout(wait, w.session.conn.events.recv()).await {
        w.session.on_event(event).await;
    }
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    let mut ids = Vec::new();
    while let Ok(frame) = w.frames.try_recv() {
        ids.push(framed_payload(&frame).read_var_int().unwrap());
    }
    ids
}

async fn pump(w: &mut Walker) -> usize {
    let single = Clientbound::BlockUpdate.id();
    let section = Clientbound::SectionBlocksUpdate.id();
    let ids = pump_ids(w).await;
    ids.iter()
        .filter(|id| **id == single || **id == section)
        .count()
}

async fn watched_stone(world: &TestWorld, w: &mut Walker, at: BlockPos) {
    let region = world.region(RegionPos::new(1, 1)).await;
    let (viewer, events) = PlayerHandle::new(900);
    let mut events = events;
    let chunk = ChunkPos::of_block(at);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    std::mem::forget(events);
    place_blocks(world, &region, [(at, STONE)]).await;
    pump(w).await;
}

fn action(status: PlayerActionStatus, position: BlockPos) -> Serverbound {
    Serverbound::PlayerAction {
        status,
        position,
        face: 1,
        sequence: 5,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destroy_actions_far_away_or_in_unsent_chunks_reveal_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    let (x, z) = (w.x as i32, w.z as i32);
    let region = world.region(RegionPos::new(1, 1)).await;
    let key = (w.session.dimension, RegionPos::new(1, 1));
    w.session.view.regions.insert(key, region.clone());
    let near = BlockPos::new(x + 2, 100, z);
    place_blocks(&world, &region, [(near, STONE)]).await;
    pump(&mut w).await;
    w.session
        .handle_packet(action(PlayerActionStatus::AbortDestroyBlock, near))
        .await;
    assert_eq!(pump(&mut w).await, 1, "a block in reach is corrected");
    let unsent = BlockPos::new(x + 160, 100, z);
    let unreachable = BlockPos::new(x + 20, 100, z);
    watched_stone(&world, &mut w, unsent).await;
    watched_stone(&world, &mut w, unreachable).await;
    for target in [unsent, unreachable] {
        for status in [
            PlayerActionStatus::AbortDestroyBlock,
            PlayerActionStatus::StopDestroyBlock,
            PlayerActionStatus::StartDestroyBlock,
        ] {
            w.session.handle_packet(action(status, target)).await;
            assert_eq!(pump(&mut w).await, 0, "{status:?} at {target:?}");
        }
        w.session
            .handle_packet(Serverbound::UseItemOn {
                hand: 0,
                position: target,
                face: tuff_world_model::Face::Up,
                cursor: [0.5, 1.0, 0.5],
                sequence: 6,
            })
            .await;
        assert_eq!(pump(&mut w).await, 0, "use at {target:?}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrections_do_not_reveal_enclosed_hidden_blocks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let config = &world.shared.config;
    config
        .set(
            crate::world::anti_xray::ENABLED,
            crate::config::Value::Bool(true),
        )
        .unwrap();
    config
        .set(
            crate::world::anti_xray::MAX_Y,
            crate::config::Value::Int(200),
        )
        .unwrap();
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let key = (w.session.dimension, RegionPos::new(1, 1));
    w.session.view.regions.insert(key, region.clone());
    let ore = BlockPos::new(w.x as i32 + 2, 104, w.z as i32);
    let mut blocks = vec![(ore, tuff_vanilla_data::Block::DiamondOre.default_state())];
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        blocks.push((BlockPos::new(ore.x + dx, ore.y + dy, ore.z + dz), STONE));
    }
    place_blocks(&world, &region, blocks).await;
    pump(&mut w).await;
    let wall = BlockPos::new(ore.x, ore.y + 1, ore.z);
    w.session
        .handle_packet(action(PlayerActionStatus::AbortDestroyBlock, wall))
        .await;
    assert_eq!(pump(&mut w).await, 1, "plain stone is corrected");
    w.session
        .handle_packet(action(PlayerActionStatus::AbortDestroyBlock, ore))
        .await;
    assert_eq!(pump(&mut w).await, 0, "the enclosed ore is not");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_sounds_reach_only_viewers_in_range_of_the_source() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(40, 40)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let key = (w.session.dimension, RegionPos::new(1, 1));
    w.session.view.regions.insert(key, region.clone());
    let (far, mut far_events) = PlayerHandle::new(901);
    subscribe(&region, &far, &mut far_events, &[ChunkPos::new(40, 40)]).await;
    let (spectator, mut spectator_events) = PlayerHandle::new(902);
    subscribe(
        &region,
        &spectator,
        &mut spectator_events,
        &[ChunkPos::new(40, 40)],
    )
    .await;
    let spectator_at = [w.x, 101.0, w.z];
    world.players.send_replace(Arc::new(
        crate::world::nearby::PlayerIndex::default().with_spectator_positions([(
            902,
            w.session.dimension,
            spectator_at,
        )]),
    ));
    w.session.run_tick().await;
    world.ticks(2).await;
    while far_events.try_recv().is_ok() {}
    while spectator_events.try_recv().is_ok() {}
    let position = w.session.pose.position;
    w.session
        .sound_at(
            tuff_vanilla_data::registry::SoundEvent::EntityPlayerBurp,
            position,
        )
        .await;
    assert_eq!(
        pump_sounds(&mut w).await,
        1,
        "the source's own client hears it"
    );
    let mut heard_by_far = 0;
    while let Ok(event) = far_events.try_recv() {
        heard_by_far += usize::from(matches!(event, PlayerEvent::ChunkPackets { .. }));
    }
    assert_eq!(heard_by_far, 0, "a viewer with no known position does not");
    let mut heard_by_spectator = 0;
    while let Ok(event) = spectator_events.try_recv() {
        heard_by_spectator += usize::from(matches!(event, PlayerEvent::ChunkPackets { .. }));
    }
    assert_eq!(
        heard_by_spectator, 1,
        "a spectator is found through the index"
    );
    world.shutdown().await;
}

async fn pump_sounds(w: &mut Walker) -> usize {
    let sound = Clientbound::Sound.id();
    pump_ids(w).await.iter().filter(|id| **id == sound).count()
}

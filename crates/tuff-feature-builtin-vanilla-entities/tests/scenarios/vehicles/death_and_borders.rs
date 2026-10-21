use super::*;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dying_in_a_boat_and_respawning_leaves_the_player_unseated() {
    dying_in_a_boat(false).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dying_in_a_boat_with_a_stale_vehicle_position_still_unseats() {
    dying_in_a_boat(true).await;
}

async fn dying_in_a_boat(stale: bool) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 100);
    let handle = session.conn.handle.clone();
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    load_chunk(&world, &region, &mut session).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    session.stand_at([6.5, SKY as f64 + 1.0, 4.5], true);
    session.publish(true).await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let vehicle = mobs(&region, EntityType::OakBoat).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: handle.clone(),
            target: vehicle,
            interaction: interaction(&handle, None),
        })
        .await;
    for _ in 0..5 {
        world.ticks(1).await;
        pump(&mut session).await;
    }
    assert!(session.ride.riding.is_some(), "mounted");
    if stale {
        session.ride.riding.as_mut().unwrap().position = [5000.0, 64.0, 5000.0];
    }
    session
        .damage(1000.0, &tuff_vanilla_data::damage_type::GENERIC, None)
        .await;
    assert!(session.is_dead());
    assert!(session.ride.riding.is_none(), "dying gets off");
    for _ in 0..5 {
        world.ticks(1).await;
        pump(&mut session).await;
    }
    session.respawn().await;
    for _ in 0..20 {
        world.ticks(1).await;
        pump(&mut session).await;
        session.run_tick().await;
    }
    assert!(!session.is_dead());
    assert!(session.ride.riding.is_none(), "still seated after respawn");
    let boat = mobs(&region, EntityType::OakBoat).await[0].position;
    let apart = (session.pose.position[0] - boat[0]).hypot(session.pose.position[2] - boat[2]);
    assert!(
        apart > 0.5 || session.pose.position[1] > boat[1] + 0.2,
        "respawned on the boat: {:?} {boat:?}",
        session.pose.position
    );
    let (viewer, mut events) = PlayerHandle::new(101);
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: vec![chunk],
        })
        .await;
    let mut listed = false;
    for _ in 0..40 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::Track(track) = event
                && track.kind == EntityType::OakBoat
            {
                listed |= track.spawn.iter().any(|frame| {
                    tuff_test_support::framed_packet_id(frame)
                        == <crate::protocol::play::SetPassengers as crate::protocol::Encode>::ID
                });
            }
        }
    }
    assert!(!listed, "a new viewer is told the boat still has a rider");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_that_throws_its_rider_stops_listing_it_to_new_viewers() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut session, _frames) = session_with_outbound(&world, 100);
    let handle = session.conn.handle.clone();
    session.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    load_chunk(&world, &region, &mut session).await;
    let mut blocks = pen(2, 13);
    for y in 1..=4 {
        for (x, z) in [(6, 6), (6, 7), (7, 6), (7, 7), (5, 6), (6, 5)] {
            blocks.push((
                BlockPos::new(x, SKY + y, z),
                tuff_vanilla_data::Block::Water.default_state(),
            ));
        }
    }
    place_blocks(&world, &region, blocks).await;
    session.stand_at([6.5, SKY as f64 + 1.0, 4.5], true);
    session.publish(true).await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let vehicle = mobs(&region, EntityType::OakBoat).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: handle.clone(),
            target: vehicle,
            interaction: interaction(&handle, None),
        })
        .await;
    for _ in 0..3 {
        world.ticks(1).await;
        pump(&mut session).await;
    }
    assert!(session.ride.riding.is_some(), "mounted");
    for _ in 0..120 {
        world.ticks(1).await;
        pump(&mut session).await;
        if session.ride.riding.is_none() {
            break;
        }
    }
    assert!(session.ride.riding.is_none(), "the boat threw its rider");
    let (viewer, mut events) = PlayerHandle::new(101);
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: vec![chunk],
        })
        .await;
    let mut seen = false;
    let mut listed = false;
    for _ in 0..40 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::Track(track) = event
                && track.kind == EntityType::OakBoat
            {
                seen = true;
                listed |= track.spawn.iter().any(|frame| {
                    tuff_test_support::framed_packet_id(frame)
                        == <crate::protocol::play::SetPassengers as crate::protocol::Encode>::ID
                });
            }
        }
    }
    assert!(seen, "the viewer sees the boat");
    assert!(!listed, "a new viewer is told the boat still has a rider");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewed_chest_boat_crossing_a_region_border_keeps_its_menu_open() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&west, &player, &mut events, &[west_chunk]).await;
    subscribe(&east, &player, &mut events, &[east_chunk]).await;
    world.ticks(3).await;
    summon(
        &west,
        EntityType::OakChestBoat,
        Compound::new().with("NoGravity", true).with(
            "Motion",
            Tag::List(vec![Tag::Double(0.8), Tag::Double(0.0), Tag::Double(0.0)]),
        ),
        [511.8, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let cart = mobs(&west, EntityType::OakChestBoat).await[0].id;
    sneak_open(&west, &player, &mut events, cart).await;
    let mut moved = false;
    for _ in 0..120 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            match event {
                PlayerEvent::EntityContainerMoved { entity, .. } => moved |= entity == cart,
                PlayerEvent::EntityContainerClosed { .. } => panic!("the menu closed"),
                _ => {}
            }
        }
        if moved {
            break;
        }
    }
    assert!(moved, "the menu followed the cart into the eastern region");
    world.ticks(5).await;
    assert!(
        !mobs(&east, EntityType::OakChestBoat).await.is_empty(),
        "the cart is in the east"
    );
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, PlayerEvent::EntityContainerClosed { .. }));
    }
    world.shutdown().await;
}

use super::*;

/// The sky and block section masks of every Update Light packet waiting in
/// `events`, with the chunk each was for.
fn light_updates(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Vec<(ChunkPos, u64, u64)> {
    let light_update = tuff_vanilla_data::packet::play::Clientbound::LightUpdate.id();
    let mask = |r: &mut tuff_wire::PacketReader| {
        let bytes = r.read_prefixed_bytes(8).unwrap();
        let mut word = [0u8; 8];
        word[..bytes.len()].copy_from_slice(&bytes);
        u64::from_le_bytes(word)
    };
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        let PlayerEvent::ChunkPackets { pos, packets, .. } = event else {
            continue;
        };
        for packet in packets.iter() {
            let mut r = framed_payload(packet);
            if r.read_var_int().unwrap() != light_update {
                continue;
            }
            let (x, z) = (r.read_var_int().unwrap(), r.read_var_int().unwrap());
            assert_eq!(ChunkPos::new(x, z), pos);
            let sky = mask(&mut r);
            let block = mask(&mut r);
            found.push((pos, sky, block));
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_loaded_together_need_no_light_updates() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunks: Vec<ChunkPos> = (4..9)
        .flat_map(|z| (4..9).map(move |x| ChunkPos::new(x, z)))
        .collect();

    let world = TestWorld::start(dir.path());
    let region = world.region(chunks[0].region()).await;
    subscribe(&region, &viewer, &mut events, &chunks).await;
    world.ticks(3).await;
    assert_eq!(light_updates(&mut events), Vec::new());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn light_changes_are_sent_once_per_tick_with_only_changed_sections() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(5, 5);
    let generator = Generator::new(1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    world.ticks(1).await;
    light_updates(&mut events);

    let mut heights = Vec::new();
    for (sequence, x) in [(1, 82), (2, 86)] {
        let height = generator.height(x, 88);
        heights.push(height);
        region
            .request(RegionRequest::Break {
                player: viewer.clone(),
                pos: BlockPos::new(x, height, 88),
                tool: None,
                mode: BreakMode::Creative,
                sequence,
            })
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: s,
                outcome: ActionOutcome::Broken { .. },
            } if s == sequence => Some(()),
            _ => None,
        })
        .await;
        world.settle(*world.clock.borrow()).await;
    }
    assert_eq!(light_updates(&mut events), Vec::new());

    world.ticks(1).await;
    let updates = light_updates(&mut events);
    let [(at, sky, block)] = updates[..] else {
        panic!("expected one light update, got {updates:?}");
    };
    assert_eq!(at, pos);
    assert_eq!(block, 0, "no block light changed");
    let section = |y: i32| 1u64 << ((y >> 4) + 5);
    for height in heights {
        assert_ne!(
            sky & section(height),
            0,
            "sky mask {sky:#b} misses y {height}"
        );
    }
    assert_ne!(sky, (1 << 26) - 1, "only the changed sky sections are sent");

    world.ticks(2).await;
    assert_eq!(light_updates(&mut events), Vec::new());
    world.shutdown().await;
}

/// Saves chunk (5, 5) with glowstone against its east edge, lighting the
/// west edge of (6, 5).
async fn glowing_border(dir: &Path) -> (ChunkPos, ChunkPos) {
    let (lamp, lit) = (ChunkPos::new(5, 5), ChunkPos::new(6, 5));
    let (viewer, mut events) = PlayerHandle::new(100);
    let height = Generator::new(1).height(95, 88);
    let world = TestWorld::start(dir);
    let region = world.region(lamp.region()).await;
    subscribe(&region, &viewer, &mut events, &[lamp]).await;
    let glowstone = tuff_vanilla_data::Block::Glowstone.default_state();
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(95, height + 1, 88), glowstone)],
    )
    .await;
    world.shutdown().await;
    (lamp, lit)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_wait_for_neighbours_that_light_them() {
    let dir = TestDir::new();
    let (lamp, lit) = glowing_border(dir.path()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let region = world.region(lit.region()).await;
    subscribe(&region, &viewer, &mut events, &[lit, lamp]).await;
    world.ticks(3).await;
    assert_eq!(light_updates(&mut events), Vec::new());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_neighbour_sends_only_the_light_it_changed() {
    let dir = TestDir::new();
    let (lamp, lit) = glowing_border(dir.path()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let region = world.region(lit.region()).await;
    subscribe(&region, &viewer, &mut events, &[lit]).await;
    world.ticks(1).await;
    assert_eq!(light_updates(&mut events), Vec::new());
    subscribe(&region, &viewer, &mut events, &[lamp]).await;
    world.ticks(2).await;
    let updates = light_updates(&mut events);
    let [(at, sky, block)] = updates[..] else {
        panic!("expected one light update, got {updates:?}");
    };
    assert_eq!(at, lit);
    assert_eq!(sky, 0, "no sky light changed");
    let height = Generator::new(1).height(95, 88) + 1;
    assert_ne!(block & 1 << ((height >> 4) + 5), 0);
    world.shutdown().await;
}

/// A session flying west one chunk at a time towards the glowing border:
/// the lamp chunk is loaded and lit as part of the ring beyond the view
/// before the chunk it lights reaches the view's edge, so that chunk is
/// sent with its final light and no Update Light follows.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flying_viewer_is_sent_no_light_updates() {
    let dir = TestDir::new();
    let (lamp, lit) = glowing_border(dir.path()).await;
    let world = TestWorld::start(dir.path());
    let (mut session, _frames) = session_with_outbound(&world, 1000);
    let mut received = HashSet::new();
    let mut updates = Vec::new();
    for step in 0..5 {
        let chunk = ChunkPos::new(12 - step, lit.z);
        session.stand_at(
            [(chunk.x * 16 + 8) as f64, 120.0, (chunk.z * 16 + 8) as f64],
            false,
        );
        session.update_view().await;
        assert!(
            session
                .view
                .lit_ring
                .iter()
                .all(|c| !session.view.subscribed.contains(c))
        );
        while !session.view.subscribed.iter().all(|c| received.contains(c)) {
            let event = tokio::time::timeout(Duration::from_secs(20), session.conn.events.recv())
                .await
                .expect("chunks arrive")
                .expect("region events");
            if let PlayerEvent::Chunk { pos, .. } = &event {
                received.insert(*pos);
            }
            updates.extend(light_update_of(&event));
            session.on_event(event).await;
        }
        world.ticks(2).await;
        while let Ok(event) = session.conn.events.try_recv() {
            updates.extend(light_update_of(&event));
            session.on_event(event).await;
        }
    }
    assert!(received.contains(&lit) && received.contains(&lamp));
    assert_eq!(updates, Vec::new());
    world.shutdown().await;
}

fn light_update_of(event: &PlayerEvent) -> Option<ChunkPos> {
    let PlayerEvent::ChunkPackets { pos, packets, .. } = event else {
        return None;
    };
    let light_update = tuff_vanilla_data::packet::play::Clientbound::LightUpdate.id();
    packets
        .iter()
        .any(|packet| framed_payload(packet).read_var_int().unwrap() == light_update)
        .then_some(*pos)
}

async fn glowing_region_border(dir: &Path) -> (ChunkPos, ChunkPos) {
    let (lamp, lit) = (ChunkPos::new(32, 5), ChunkPos::new(31, 5));
    let (viewer, mut events) = PlayerHandle::new(100);
    let height = Generator::new(1).height(512, 88);
    let world = TestWorld::start(dir);
    let region = world.region(lamp.region()).await;
    subscribe(&region, &viewer, &mut events, &[lamp]).await;
    let glowstone = tuff_vanilla_data::Block::Glowstone.default_state();
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(512, height + 1, 88), glowstone)],
    )
    .await;
    world.shutdown().await;
    (lamp, lit)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_wait_for_neighbour_regions_that_light_them() {
    let dir = TestDir::new();
    let (lamp, lit) = glowing_region_border(dir.path()).await;
    assert_ne!(lamp.region(), lit.region());
    let (viewer, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let lit_region = world.region(lit.region()).await;
    let lamp_region = world.region(lamp.region()).await;
    for (region, chunk) in [(&lamp_region, lamp), (&lit_region, lit)] {
        region
            .request(RegionRequest::Subscribe {
                player: viewer.clone(),
                chunks: vec![chunk],
            })
            .await;
    }
    world.ticks(6).await;
    let (tx, mut others) = mpsc::unbounded_channel();
    let mut sent = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::Chunk { pos, .. } => sent.push(pos),
            other => {
                let _ = tx.send(other);
            }
        }
    }
    sent.sort_by_key(|pos| pos.x);
    assert_eq!(sent, vec![lit, lamp]);
    assert_eq!(light_updates(&mut others), Vec::new());
    world.shutdown().await;
}

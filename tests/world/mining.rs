//! Mining and right-click harvesting: instant-break blocks, tool wear,
//! destroy progress, and creative/spectator break behaviour.

use super::*;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survival_start_breaks_only_instantly_mined_blocks() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let ground = BlockPos::new(1, height, 1);
    let above = BlockPos::new(1, height + 1, 1);
    let start = BreakMode::Start {
        eye: [1.5, height as f64 + 2.62, 2.5],
        miner: player::Miner::plain(true),
    };
    let outcome = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32| {
        expect(events, |event| match event {
            PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
            _ => None,
        })
        .await
    };

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: ground,
            tool: None,
            mode: start,
            sequence: 1,
        })
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Mining
    ));

    region
        .request(RegionRequest::Place(place(&player, ground, Item::Torch, 2)))
        .await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: above,
            tool: None,
            mode: start,
            sequence: 3,
        })
        .await;
    assert!(matches!(
        outcome(&mut events, 3).await,
        ActionOutcome::Broken { state, .. } if state.block() == Block::Torch
    ));
    world.ticks(2).await;
    let drop = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.player.is_none() => Some(track),
        _ => None,
    })
    .await;
    assert_eq!(drop.chunk, pos);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn right_clicking_cake_eats_a_slice_before_placing() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let ground = BlockPos::new(1, height, 1);
    let cake = BlockPos::new(1, height + 1, 1);
    let outcome = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32| {
        expect(events, |event| match event {
            PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
            _ => None,
        })
        .await
    };

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Place(place(&player, ground, Item::Cake, 1)))
        .await;
    assert!(matches!(
        outcome(&mut events, 1).await,
        ActionOutcome::Placed { .. }
    ));

    let mut bite = place(&player, cake, Item::Torch, 2);
    bite.item = None;
    region.request(RegionRequest::Place(bite)).await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Ate {
            nutrition: 2,
            saturation
        } if (saturation - 0.4).abs() < 1e-6
    ));
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::BlockAt { pos: cake, reply })
        .await;
    let state = answer.await.unwrap().unwrap();
    assert_eq!(state.block(), Block::Cake);
    assert_eq!(state.get("bites"), Some("1"));

    let mut full = place(&player, cake, Item::Torch, 3);
    full.can_eat = false;
    region.request(RegionRequest::Place(full)).await;
    assert!(
        !matches!(outcome(&mut events, 3).await, ActionOutcome::Ate { .. }),
        "a full player does not eat"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn right_clicking_a_grown_sweet_berry_bush_harvests_it() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let bush = BlockPos::new(1, height + 1, 1);
    let age = |n: &str| {
        Block::SweetBerryBush
            .default_state()
            .with("age", n)
            .unwrap()
    };

    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "random_tick_speed",
                crate::world::game_rules::RuleValue::Int(0),
            )
            .expect("a known rule");
    });
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(&world, &region, [(bush, age("3"))]).await;

    region
        .request(RegionRequest::Place(place(&player, bush, Item::Stick, 1)))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Interacted { .. }),
        "expected a harvest interaction, got {outcome:?}"
    );

    let state = block_at(&region, bush).await.expect("bush still there");
    assert_eq!(state.block(), Block::SweetBerryBush);
    assert_eq!(state.get("age"), Some("1"));

    world.shutdown().await;

    let entities = stored_chunk(dir.path(), pos, true).expect("drops saved");
    let mut total = 0i64;
    for entity in entities.get_list("Entities").unwrap() {
        let entity = entity.as_compound().unwrap();
        assert_eq!(entity.get_str("id"), Some("minecraft:item"));
        let item = entity.get_compound("Item").unwrap();
        assert_eq!(item.get_str("id"), Some("minecraft:sweet_berries"));
        total += item.get_int("count").unwrap() as i64;
    }
    assert!(
        (2..=3).contains(&total),
        "expected 2-3 berries, got {total}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn right_clicking_an_unripe_sweet_berry_bush_does_nothing() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let bush = BlockPos::new(1, height + 1, 1);
    let age1 = Block::SweetBerryBush
        .default_state()
        .with("age", "1")
        .unwrap();

    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "random_tick_speed",
                crate::world::game_rules::RuleValue::Int(0),
            )
            .expect("a known rule");
    });
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(&world, &region, [(bush, age1)]).await;

    region
        .request(RegionRequest::Place(place(&player, bush, Item::Stick, 1)))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        !matches!(outcome, ActionOutcome::Interacted { .. }),
        "an unripe bush must not harvest"
    );

    let state = block_at(&region, bush).await.expect("bush still there");
    assert_eq!(state.get("age"), Some("1"), "age must be unchanged");

    world.shutdown().await;

    assert!(
        stored_chunk(dir.path(), pos, true).is_none(),
        "nothing should have dropped"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destroy_progress_reaches_the_breaker_and_ends_at_air() {
    let dir = TestDir::new();
    let (breaker, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let target = BlockPos::new(1, height, 1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &breaker, &mut events, &[pos]).await;
    region
        .request(destroy_tick(
            &breaker,
            target,
            play::level_event::DESTROY_PROGRESS_SOUND,
            1,
        ))
        .await;
    let expected = framed(&play::LevelEvent {
        event: play::level_event::DESTROY_PROGRESS_SOUND,
        position: target,
        data: 1,
        global: false,
    });
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets {
            pos: at, packets, ..
        } if at == pos => packets.contains(&expected).then_some(()),
        _ => None,
    })
    .await;

    let above = BlockPos::new(1, height + 1, 1);
    region
        .request(destroy_tick(
            &breaker,
            above,
            play::level_event::DESTROY_PROGRESS,
            1,
        ))
        .await;
    let ended = expect(&mut events, |event| match event {
        PlayerEvent::DestroyEnded(at) => Some(at),
        PlayerEvent::ChunkPackets { packets, .. } => {
            assert!(
                !packets
                    .iter()
                    .any(|p| framed_packet_id(p) == framed_packet_id(&expected))
            );
            None
        }
        _ => None,
    })
    .await;
    assert_eq!(ended, above);
    world.shutdown().await;
}

fn destroy_tick(player: &PlayerHandle, pos: BlockPos, event: i32, data: i32) -> RegionRequest {
    destroy_tick_after(player, pos, event, data, 0)
}

fn destroy_tick_after(
    player: &PlayerHandle,
    pos: BlockPos,
    event: i32,
    data: i32,
    ticks: u64,
) -> RegionRequest {
    RegionRequest::DestroyProgress(Box::new(tuff_core::world::messages::DestroyTick {
        player: player.clone(),
        pos,
        event,
        data,
        ticks,
        tool: None,
        miner: player::Miner::plain(true),
        eye: [0.5, 300.0, 0.5],
    }))
}

fn crack(breaker: &PlayerHandle, pos: BlockPos, stage: i8) -> bytes::Bytes {
    framed(&play::BlockDestruction {
        id: breaker.id,
        position: pos,
        stage,
    })
}

fn cracks_received(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Vec<bytes::Bytes> {
    let id = framed_packet_id(&crack(&PlayerHandle::new(0).0, BlockPos::new(0, 0, 0), 0));
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Packets(packets) = event {
            found.extend(
                packets
                    .iter()
                    .filter(|packet| framed_packet_id(packet) == id)
                    .cloned(),
            );
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crack_stages_reach_nearby_players_but_not_the_breaker() {
    let dir = TestDir::new();
    let (breaker, mut breaker_events) = PlayerHandle::new(100);
    let (watcher, mut watcher_events) = PlayerHandle::new(101);
    let (distant, mut distant_events) = PlayerHandle::new(102);
    let pos = ChunkPos::new(0, 0);
    let target = BlockPos::new(4, 200, 4);
    let stone = Block::Stone.default_state();

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &breaker, &mut breaker_events, &[pos]).await;
    place_blocks(&world, &region, [(target, stone)]).await;
    region.request(update(&breaker, 1, [4.5, 201.0, 6.5])).await;
    region
        .request(update(&watcher, 1, [10.5, 201.0, 10.5]))
        .await;
    region
        .request(update(&distant, 1, [4.5, 201.0, 40.5]))
        .await;
    let start = BreakMode::Start {
        eye: [4.5, 202.62, 6.5],
        miner: player::Miner::plain(true),
    };
    region
        .request(RegionRequest::Break {
            player: breaker.clone(),
            pos: target,
            tool: None,
            mode: start,
            sequence: 1,
        })
        .await;
    let per_tick = player::destroy_progress(stone, None, &player::Miner::plain(true), false);
    let first_change = (1..)
        .find(|ticks| player::crack_stage(per_tick, *ticks) == 1)
        .unwrap();
    for ticks in 1..=first_change {
        region
            .request(destroy_tick_after(
                &breaker,
                target,
                play::level_event::DESTROY_PROGRESS,
                0,
                ticks,
            ))
            .await;
    }
    region
        .request(RegionRequest::DestroyAborted {
            player: breaker.id,
            pos: target,
        })
        .await;
    block_at(&region, target).await;

    assert_eq!(
        cracks_received(&mut watcher_events),
        [
            crack(&breaker, target, 0),
            crack(&breaker, target, 1),
            crack(&breaker, target, -1)
        ]
    );
    assert!(cracks_received(&mut breaker_events).is_empty());
    assert!(cracks_received(&mut distant_events).is_empty());

    region
        .request(RegionRequest::Break {
            player: breaker.clone(),
            pos: target,
            tool: None,
            mode: start,
            sequence: 2,
        })
        .await;
    region
        .request(RegionRequest::Break {
            player: breaker.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 3,
        })
        .await;
    assert_eq!(
        block_at(&region, target).await,
        Some(Block::Air.default_state())
    );
    assert_eq!(
        cracks_received(&mut watcher_events),
        [crack(&breaker, target, 0), crack(&breaker, target, -1)]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survival_tools_wear_on_blocks_with_hardness_only() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(3, 3);
    let ground = BlockPos::new(3, height, 3);
    let below = BlockPos::new(3, height - 1, 3);
    let deeper = BlockPos::new(3, height - 2, 3);
    let pickaxe = || Some(ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test));
    let outcome = async |events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32| {
        expect(events, |event| match event {
            PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
            _ => None,
        })
        .await
    };
    let tool_damage = |outcome: ActionOutcome| match outcome {
        ActionOutcome::Broken { tool_damage, .. } => tool_damage,
        other => panic!("expected a break, got {other:?}"),
    };

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: ground,
            tool: pickaxe(),
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    assert_eq!(tool_damage(outcome(&mut events, 1).await), 1);

    region
        .request(RegionRequest::Place(place(&player, below, Item::Torch, 2)))
        .await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Placed { .. }
    ));
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: ground,
            tool: pickaxe(),
            mode: BreakMode::Start {
                eye: [3.5, height as f64 + 1.62, 4.5],
                miner: player::Miner::plain(true),
            },
            sequence: 3,
        })
        .await;
    assert_eq!(tool_damage(outcome(&mut events, 3).await), 0);

    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: below,
            tool: pickaxe(),
            mode: BreakMode::Creative,
            sequence: 4,
        })
        .await;
    assert_eq!(tool_damage(outcome(&mut events, 4).await), 0);
    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: deeper,
            tool: Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)),
            mode: BreakMode::Finish,
            sequence: 5,
        })
        .await;
    assert_eq!(tool_damage(outcome(&mut events, 5).await), 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_breaks_drop_nothing_and_spectators_collect_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let height = Generator::new(1).height(40, 40);
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(40, height, 40),
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { outcome, .. } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Broken { .. }));

    let at = [40.5, height as f64, 40.5];
    let spectating = |seq| {
        RegionRequest::Player(PlayerUpdate {
            looks: Default::default(),
            spectator: true,
            ..match update(&viewer, seq, at) {
                RegionRequest::Player(update) => update,
                _ => unreachable!(),
            }
        })
    };
    region.request(spectating(1)).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    world.ticks(5).await;
    let mut tracked_items = HashSet::new();
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::OfferItem { .. } => panic!("a spectator was offered an item"),
            PlayerEvent::Track(track) if track.player.is_none() => {
                tracked_items.insert(track.entity);
            }
            _ => {}
        }
    }
    assert_eq!(tracked_items.len(), 1, "only the spawned item exists");
    region.request(update(&viewer, 2, at)).await;
    world.ticks(3).await;
    let stack = expect(&mut events, |event| match event {
        PlayerEvent::OfferItem { stack, .. } => Some(stack),
        _ => None,
    })
    .await;
    assert_eq!(stack, ItemStack::mint(Item::Stone, 1, MintReason::Test));
    world.shutdown().await;
}

/// Vanilla plays the fire extinguish sound only when a player breaks a fire
/// block directly (`BaseFireBlock`'s destroy hook); rain and natural
/// burnout remove the block without it (see `growth.rs`'s
/// `fire_burns_a_wooden_floor_until_rain_puts_it_out` and
/// `fire_burns_out_on_its_own_without_playing_a_sound`). The level event is
/// queued before the break's `ActionResult`, so it must be looked for first.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_a_fire_block_plays_the_extinguish_sound() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let fire = BlockPos::new(1, 80, 1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &player, &mut events, &[pos]).await;
    place_blocks(&world, &region, [(fire, Block::Fire.default_state())]).await;

    region
        .request(RegionRequest::Break {
            player: player.clone(),
            pos: fire,
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;

    let expected = framed(&play::LevelEvent {
        event: play::level_event::SOUND_EXTINGUISH_FIRE,
        position: fire,
        data: 0,
        global: false,
    });
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets {
            pos: at, packets, ..
        } if at == pos => packets.contains(&expected).then_some(()),
        _ => None,
    })
    .await;

    assert!(matches!(
        expect(&mut events, |event| match event {
            PlayerEvent::ActionResult { outcome, .. } => Some(outcome),
            _ => None,
        })
        .await,
        ActionOutcome::Broken { .. }
    ));
    world.shutdown().await;
}

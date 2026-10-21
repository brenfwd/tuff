//! Container menus: chests, hoppers, ender chests, shulker boxes,
//! crafting/smithing/stonecutter stations, furnaces and campfires.

use super::*;

fn lid_event(pos: BlockPos, block: Block, viewers: u8) -> bytes::Bytes {
    framed(&play::BlockEvent {
        position: pos,
        action: play::block_event::CONTAINER_OPEN_COUNT,
        parameter: viewers,
        block: block.id(),
    })
}

async fn expect_chunk_packet(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    chunk: ChunkPos,
    expected: bytes::Bytes,
) {
    expect(events, |event| match event {
        PlayerEvent::ChunkPackets { pos, packets, .. } if pos == chunk => {
            packets.contains(&expected).then_some(())
        }
        _ => None,
    })
    .await
}

async fn expect_slots(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    container: BlockPos,
) -> Vec<(usize, Option<ItemStack>)> {
    expect(events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. } if pos == container => Some(slots),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chest_under_glass_opens() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let chest = BlockPos::new(1, Generator::new(1).height(1, 1) + 1, 1);
    let above = BlockPos::new(chest.x, chest.y + 1, chest.z);
    place_blocks(
        &world,
        &region,
        [
            (chest, Block::Chest.default_state()),
            (above, Block::Glass.default_state()),
        ],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Torch, 1)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 3 });
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chest_viewers_share_its_contents_and_a_broken_chest_spills_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    subscribe(&region, &bob, &mut bob_events, &[pos]).await;
    let chest = BlockPos::new(1, Generator::new(1).height(1, 1) + 1, 1);
    place_blocks(&world, &region, [(chest, Block::Chest.default_state())]).await;

    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Torch, 1)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 3 });
    assert_eq!(open.title, TextComponent::translate("container.chest", []));
    let MenuSource::Blocks {
        parts, contents, ..
    } = open.source
    else {
        panic!("a chest is a block container");
    };
    assert_eq!(parts, vec![(chest, 27)]);
    assert_eq!(
        contents,
        (0..27).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );
    expect_chunk_packet(&mut bob_events, pos, lid_event(chest, Block::Chest, 1)).await;

    region
        .request(RegionRequest::Place(place(&bob, chest, Item::Torch, 1)))
        .await;
    expect(&mut bob_events, container_menu).await;
    expect_chunk_packet(&mut alice_events, pos, lid_event(chest, Block::Chest, 2)).await;

    let stone = ItemStack::mint(Item::Stone, 5, MintReason::Test);
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(chest, 0, None, Some(stone.dup_unchecked())),
            took_result: false,
        })
        .await;
    let (accepted, contents) = expect(&mut alice_events, transaction_answer).await;
    assert!(accepted);
    assert!(contents.is_empty());
    for events in [&mut alice_events, &mut bob_events] {
        assert_eq!(
            expect_slots(events, chest).await,
            vec![(0, Some(stone.dup_unchecked()))]
        );
    }

    let dirt = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
    region
        .request(RegionRequest::ContainerTransaction {
            player: bob.clone(),
            window_id: 1,
            changes: set_slot(chest, 0, None, Some(dirt.dup_unchecked())),
            took_result: false,
        })
        .await;
    let (accepted, contents) = expect(&mut bob_events, transaction_answer).await;
    assert!(!accepted, "a stale expectation is rejected");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].0, chest);
    assert_eq!(contents[0].1[0], Some(stone.dup_unchecked()));

    let (carol, mut carol_events) = PlayerHandle::new(102);
    region
        .request(RegionRequest::ContainerTransaction {
            player: carol.clone(),
            window_id: 1,
            changes: set_slot(chest, 1, None, Some(dirt.dup_unchecked())),
            took_result: false,
        })
        .await;
    let (accepted, _) = expect(&mut carol_events, transaction_answer).await;
    assert!(!accepted, "only viewers may change a container");

    region
        .request(RegionRequest::CloseContainer {
            player: bob.id,
            positions: vec![chest],
        })
        .await;
    expect_chunk_packet(&mut alice_events, pos, lid_event(chest, Block::Chest, 1)).await;

    region
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: chest,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 2,
        })
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerClosed { pos, .. } if pos == chest => Some(()),
        _ => None,
    })
    .await;
    let mut dropped = HashSet::new();
    for _ in 0..2 {
        let track = expect(&mut alice_events, |event| match event {
            PlayerEvent::Track(track) if track.player.is_none() => Some(track),
            _ => None,
        })
        .await;
        assert_eq!(track.chunk, pos);
        dropped.insert(track.entity);
    }
    assert_eq!(dropped.len(), 2, "the chest and its stone stack");
    assert_eq!(
        block_at(&region, chest).await,
        Some(Block::Air.default_state())
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_locked_chest_refuses_the_wrong_key_and_opens_with_the_right_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let ground = BlockPos::new(1, generator.height(1, 1), 1);
    let chest = BlockPos::new(1, ground.y + 1, 1);

    let mut placing = place(&alice, ground, Item::Chest, 1);
    placing.components.other = Compound::new().with(
        "minecraft:lock",
        Compound::new().with("items", "minecraft:stick"),
    );
    region.request(RegionRequest::Place(placing)).await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;

    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Torch, 2)))
        .await;
    let title = TextComponent::translate("container.chest", []);
    let locked_bar = framed(&play::SetActionBarText(&TextComponent::translate(
        "container.isLocked",
        [title],
    )));
    expect(&mut alice_events, |event| match event {
        PlayerEvent::MenuOpen(_) => panic!("a locked chest does not open for the wrong key"),
        PlayerEvent::Packets(packets) if packets.contains(&locked_bar) => Some(()),
        _ => None,
    })
    .await;

    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Stick, 3)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 3 });
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    assert_eq!(
        contents,
        (0..27).map(|_| None).collect::<Vec<Option<ItemStack>>>(),
        "the right key opens it, empty"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loot_chest_rolls_its_table_once_on_first_open() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let ground = BlockPos::new(1, generator.height(1, 1), 1);
    let chest = BlockPos::new(1, ground.y + 1, 1);

    let mut placing = place(&alice, ground, Item::Chest, 1);
    placing.components.other = Compound::new().with(
        "minecraft:container_loot",
        Compound::new()
            .with("loot_table", "minecraft:chests/simple_dungeon")
            .with("seed", 7i64),
    );
    region.request(RegionRequest::Place(placing)).await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;

    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Torch, 2)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    let expected = tuff_feature_builtin_vanilla_loot::container_loot(
        "minecraft:chests/simple_dungeon",
        27,
        &mut StdRng::seed_from_u64(7),
    )
    .unwrap();
    assert_eq!(contents, expected, "the seed rolls a deterministic layout");
    assert!(
        contents.iter().any(Option::is_some),
        "the table actually rolled something"
    );

    world.shutdown().await;
    let root = stored_chunk(dir.path(), pos, false).expect("chunk saved");
    let stored = root
        .get_list("block_entities")
        .unwrap()
        .iter()
        .filter_map(Tag::as_compound)
        .find(|nbt| nbt.get_int("x") == Some(chest.x) && nbt.get_int("y") == Some(chest.y))
        .expect("chest saved");
    assert!(
        stored.get_str("LootTable").is_none(),
        "LootTable is cleared after unpacking"
    );
    assert!(stored.get_long("LootTableSeed").is_none());
    assert!(!stored.get_list("Items").unwrap_or(&[]).is_empty());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loot_chest_explorer_map_is_finished_or_dropped() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let ground = BlockPos::new(1, generator.height(1, 1), 1);
    let chest = BlockPos::new(1, ground.y + 1, 1);

    let mut placing = place(&alice, ground, Item::Chest, 1);
    placing.components.other = Compound::new().with(
        "minecraft:container_loot",
        Compound::new()
            .with("loot_table", "minecraft:chests/shipwreck_map")
            .with("seed", 7i64),
    );
    region.request(RegionRequest::Place(placing)).await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    region
        .request(RegionRequest::Place(place(&alice, chest, Item::Torch, 2)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    let waiting = |stack: &ItemStack| {
        tuff_feature_builtin_vanilla_loot::exploration::pending(stack).is_some()
    };
    let slot = contents
        .iter()
        .position(|stack| stack.as_ref().is_some_and(waiting))
        .expect("the table rolled an explorer map waiting for its map");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut finished = None;
    while finished.is_none() && tokio::time::Instant::now() < deadline {
        world.ticks(1).await;
        while let Ok(event) = alice_events.try_recv() {
            if let PlayerEvent::ContainerSlots { slots, .. } = event {
                finished = finished.or(slots.into_iter().find(|(changed, _)| *changed == slot));
            }
        }
    }
    let finished = finished.expect("the map slot was finished or emptied");
    if let Some(map) = finished.1 {
        assert!(!waiting(&map));
        assert!(map.components.map_id.is_some());
    }

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn double_chests_join_with_the_right_half_first_and_persist() {
    use tuff_vanilla_data::block::Type as ChestType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    subscribe(&region, &bob, &mut bob_events, &[pos]).await;
    let generator = Generator::new(1);
    let y = generator.height(1, 1).max(generator.height(2, 1)) + 2;
    let left = BlockPos::new(1, y, 1);
    let right = BlockPos::new(2, y, 1);
    let floor = BlockPos::new(2, y - 1, 1);
    place_blocks(
        &world,
        &region,
        [
            (left, Block::Chest.default_state()),
            (floor, Block::Stone.default_state()),
        ],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&alice, floor, Item::Chest, 1)))
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        block_at(&region, right).await.unwrap().r#type(),
        Some(ChestType::Right)
    );
    assert_eq!(
        block_at(&region, left).await.unwrap().r#type(),
        Some(ChestType::Left)
    );

    region
        .request(RegionRequest::Place(place(&alice, left, Item::Torch, 2)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 6 });
    assert_eq!(
        open.title,
        TextComponent::translate("container.chestDouble", [])
    );
    let MenuSource::Blocks {
        parts, contents, ..
    } = open.source
    else {
        panic!("a chest is a block container");
    };
    assert_eq!(parts, vec![(right, 27), (left, 27)]);
    assert_eq!(contents.len(), 54);

    let apple = ItemStack::mint(Item::Apple, 3, MintReason::Test);
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(right, 0, None, Some(apple.dup_unchecked())),
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    assert_eq!(
        expect_slots(&mut alice_events, right).await,
        vec![(0, Some(apple.dup_unchecked()))]
    );

    region
        .request(RegionRequest::Place(place(&bob, right, Item::Torch, 1)))
        .await;
    let open = expect(&mut bob_events, container_menu).await;
    let MenuSource::Blocks {
        parts, contents, ..
    } = open.source
    else {
        panic!("a chest is a block container");
    };
    assert_eq!(parts, vec![(right, 27), (left, 27)]);
    assert_eq!(contents[0], Some(apple.dup_unchecked()));
    world.shutdown().await;

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    region
        .request(RegionRequest::Place(place(&alice, left, Item::Torch, 3)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    let MenuSource::Blocks {
        parts, contents, ..
    } = open.source
    else {
        panic!("a chest is a block container");
    };
    assert_eq!(parts, vec![(right, 27), (left, 27)]);
    assert_eq!(contents[0], Some(apple));
    assert_eq!(
        contents[1..],
        (0..53).map(|_| None).collect::<Vec<Option<ItemStack>>>()[..]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoppers_move_one_item_per_cooldown_and_pull_dropped_items() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    subscribe(&region, &bob, &mut bob_events, &[pos]).await;
    let generator = Generator::new(1);
    let y = generator.height(1, 1) + 2;
    let bottom = BlockPos::new(1, y, 1);
    let hopper = BlockPos::new(1, y + 1, 1);
    let top = BlockPos::new(1, y + 2, 1);
    let lone = BlockPos::new(5, generator.height(5, 5) + 2, 5);
    place_blocks(
        &world,
        &region,
        [
            (bottom, Block::Chest.default_state()),
            (hopper, Block::Hopper.default_state()),
            (top, Block::Chest.default_state()),
            (lone, Block::Hopper.default_state()),
        ],
    )
    .await;

    region
        .request(RegionRequest::Place(place(&alice, top, Item::Torch, 1)))
        .await;
    expect(&mut alice_events, container_menu).await;
    let stone = |count| Some(ItemStack::mint(Item::Stone, count, MintReason::Test));
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(top, 0, None, stone(3)),
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    assert_eq!(
        expect_slots(&mut alice_events, top).await,
        vec![(0, stone(3))]
    );

    world.ticks(1).await;
    assert_eq!(
        expect_slots(&mut alice_events, top).await,
        vec![(0, stone(2))]
    );
    world.ticks(7).await;
    while let Ok(event) = alice_events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::ContainerSlots { .. }),
            "nothing moves during the cooldown"
        );
    }
    world.ticks(1).await;
    assert_eq!(
        expect_slots(&mut alice_events, top).await,
        vec![(0, stone(1))]
    );
    world.ticks(8).await;
    assert_eq!(expect_slots(&mut alice_events, top).await, vec![(0, None)]);
    world.ticks(9).await;

    region
        .request(RegionRequest::Place(place(&bob, bottom, Item::Torch, 1)))
        .await;
    let open = expect(&mut bob_events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    assert_eq!(contents[0], stone(3));
    region
        .request(RegionRequest::CloseContainer {
            player: bob.id,
            positions: vec![bottom],
        })
        .await;

    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Apple, 2, MintReason::Test),
            position: [
                lone.x as f64 + 0.5,
                lone.y as f64 + 1.0,
                lone.z as f64 + 0.5,
            ],
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    world.ticks(2).await;
    region
        .request(RegionRequest::Place(place(&bob, lone, Item::Torch, 2)))
        .await;
    let open = expect(&mut bob_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Hopper);
    assert_eq!(open.title, TextComponent::translate("container.hopper", []));
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a hopper is a block container");
    };
    assert_eq!(
        contents,
        vec![
            Some(ItemStack::mint(Item::Apple, 2, MintReason::Test)),
            None,
            None,
            None,
            None
        ]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ender_chests_and_shulker_boxes_open_through_the_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    let (bob, mut bob_events) = PlayerHandle::new(101);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    subscribe(&region, &bob, &mut bob_events, &[pos]).await;
    let generator = Generator::new(1);
    let ender = BlockPos::new(1, generator.height(1, 1) + 2, 1);
    place_blocks(
        &world,
        &region,
        [(ender, Block::EnderChest.default_state())],
    )
    .await;

    region
        .request(RegionRequest::Place(place(&alice, ender, Item::Torch, 1)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::Generic { rows: 3 });
    assert_eq!(
        open.title,
        TextComponent::translate("container.enderchest", [])
    );
    assert!(matches!(open.source, MenuSource::EnderChest { pos } if pos == ender));
    expect_chunk_packet(&mut bob_events, pos, lid_event(ender, Block::EnderChest, 1)).await;
    region
        .request(RegionRequest::CloseContainer {
            player: alice.id,
            positions: vec![ender],
        })
        .await;
    expect_chunk_packet(&mut bob_events, pos, lid_event(ender, Block::EnderChest, 0)).await;

    let above = BlockPos::new(ender.x, ender.y + 1, ender.z);
    place_blocks(&world, &region, [(above, Block::Stone.default_state())]).await;
    region
        .request(RegionRequest::Place(place(&alice, ender, Item::Torch, 2)))
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::MenuOpen(_) => panic!("a covered ender chest does not open"),
        PlayerEvent::ActionResult { sequence: 2, .. } => Some(()),
        _ => None,
    })
    .await;

    let floor = BlockPos::new(5, generator.height(5, 5), 5);
    let shulker = BlockPos::new(5, floor.y + 1, 5);
    let apple = ItemStack::mint(Item::Apple, 4, MintReason::Test);
    let mut placing = place(&alice, floor, Item::ShulkerBox, 3);
    placing.components.container = Some(vec![Some(apple.dup_unchecked())]);
    region.request(RegionRequest::Place(placing)).await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 3,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    region
        .request(RegionRequest::Place(place(&alice, shulker, Item::Torch, 4)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::ShulkerBox);
    assert_eq!(
        open.title,
        TextComponent::translate("container.shulkerBox", [])
    );
    let MenuSource::Blocks {
        parts, contents, ..
    } = open.source
    else {
        panic!("a shulker box is a block container");
    };
    assert_eq!(parts, vec![(shulker, 27)]);
    assert_eq!(contents[0], Some(apple.dup_unchecked()));
    expect_chunk_packet(
        &mut bob_events,
        pos,
        lid_event(shulker, Block::ShulkerBox, 1),
    )
    .await;

    region
        .request(RegionRequest::Break {
            player: alice.clone(),
            pos: shulker,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 5,
        })
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerClosed { pos, .. } if pos == shulker => Some(()),
        _ => None,
    })
    .await;
    let at = [
        shulker.x as f64 + 0.5,
        shulker.y as f64,
        shulker.z as f64 + 0.5,
    ];
    region.request(update(&bob, 1, at)).await;
    world.ticks(items::BLOCK_DROP_PICKUP_DELAY as u64 + 3).await;
    let offered = expect(&mut bob_events, |event| match event {
        PlayerEvent::OfferItem { stack, .. } => Some(stack),
        _ => None,
    })
    .await;
    assert_eq!(offered.item, Item::ShulkerBox);
    assert_eq!(offered.components.container, Some(vec![Some(apple)]));
    while let Ok(event) = bob_events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::OfferItem { .. }),
            "a shulker box keeps its contents instead of spilling them"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crafting_stations_open_session_local_menus() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let table = BlockPos::new(1, generator.height(1, 1) + 1, 1);
    let cutter = BlockPos::new(3, generator.height(3, 3) + 1, 3);
    let smith = BlockPos::new(5, generator.height(5, 5) + 1, 5);
    let atlas = BlockPos::new(7, generator.height(7, 7) + 1, 7);
    let anvil = BlockPos::new(9, generator.height(9, 9) + 1, 9);
    place_blocks(
        &world,
        &region,
        [
            (table, Block::CraftingTable.default_state()),
            (cutter, Block::Stonecutter.default_state()),
            (smith, Block::SmithingTable.default_state()),
            (atlas, Block::CartographyTable.default_state()),
            (anvil, Block::ChippedAnvil.default_state()),
        ],
    )
    .await;
    let stations = [
        (table, MenuKind::Crafting, "container.crafting", 1),
        (cutter, MenuKind::Stonecutter, "container.stonecutter", 2),
        (smith, MenuKind::Smithing, "container.upgrade", 3),
        (
            atlas,
            MenuKind::Cartography,
            "container.cartography_table",
            4,
        ),
        (anvil, MenuKind::Anvil, "container.repair", 5),
    ];
    for (block, kind, title, sequence) in stations {
        region
            .request(RegionRequest::Place(place(
                &alice,
                block,
                Item::Torch,
                sequence,
            )))
            .await;
        let open = expect(&mut alice_events, container_menu).await;
        assert_eq!(open.kind, kind);
        assert_eq!(open.title, TextComponent::translate(title, []));
        assert!(matches!(open.source, MenuSource::Station { pos, .. } if pos == block));
        expect(&mut alice_events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: seq,
                outcome: ActionOutcome::Interacted { .. },
            } if seq == sequence => Some(()),
            _ => None,
        })
        .await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn furnaces_smelt_with_fuel_and_award_experience_when_the_result_is_taken() {
    use crate::world::block_entity::furnace;
    use tuff_vanilla_data::registry::EntityType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let block = BlockPos::new(1, generator.height(1, 1) + 1, 1);
    place_blocks(
        &world,
        &region,
        [(block, Block::BlastFurnace.default_state())],
    )
    .await;
    let feet = [block.x as f64 + 0.5, block.y as f64, block.z as f64 + 1.5];
    region.request(update(&alice, 1, feet)).await;

    region
        .request(RegionRequest::Place(place(&alice, block, Item::Torch, 1)))
        .await;
    let open = expect(&mut alice_events, container_menu).await;
    assert_eq!(open.kind, MenuKind::BlastFurnace);
    assert_eq!(
        open.title,
        TextComponent::translate("container.blast_furnace", [])
    );
    let MenuSource::Blocks {
        parts,
        contents,
        data,
        ..
    } = open.source
    else {
        panic!("a furnace is a block container");
    };
    assert_eq!(parts, vec![(block, 3)]);
    assert_eq!(
        contents,
        (0..3).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );
    assert_eq!(data, vec![0; 4]);

    let ore = |count| Some(ItemStack::mint(Item::DiamondOre, count, MintReason::Test));
    let coal = Some(ItemStack::mint(Item::Coal, 1, MintReason::Test));
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: vec![
                ContainerChange {
                    pos: block,
                    slot: furnace::FUEL,
                    expected: None,
                    new: coal.dup_unchecked(),
                },
                ContainerChange {
                    pos: block,
                    slot: furnace::INPUT,
                    expected: None,
                    new: ore(2),
                },
            ],
            took_result: false,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    let placed = expect_slots(&mut alice_events, block).await;
    assert!(placed.contains(&(furnace::FUEL, coal.dup_unchecked())));
    assert!(placed.contains(&(furnace::INPUT, ore(2))));
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, block).await.and_then(|s| s.get("lit")),
        Some("true")
    );
    assert_eq!(
        expect_slots(&mut alice_events, block).await,
        vec![(furnace::FUEL, None)],
        "the coal is consumed when the furnace lights"
    );
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerData { pos, data, .. }
            if pos == block && data.contains(&(furnace::DATA_LIT_DURATION, 800)) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    world.ticks(100).await;
    let diamond = |count| Some(ItemStack::mint(Item::Diamond, count, MintReason::Test));
    let first = expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. }
            if pos == block && slots.iter().any(|(slot, _)| *slot == furnace::RESULT) =>
        {
            Some(slots)
        }
        _ => None,
    })
    .await;
    assert!(first.contains(&(furnace::RESULT, diamond(1))));
    assert!(first.contains(&(furnace::INPUT, ore(1))));
    world.ticks(100).await;
    let second = expect(&mut alice_events, |event| match event {
        PlayerEvent::ContainerSlots { pos, slots, .. }
            if pos == block && slots.iter().any(|(slot, _)| *slot == furnace::RESULT) =>
        {
            Some(slots)
        }
        _ => None,
    })
    .await;
    assert!(second.contains(&(furnace::RESULT, diamond(2))));
    assert!(second.contains(&(furnace::INPUT, None)));

    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: set_slot(block, furnace::RESULT, diamond(2), None),
            took_result: true,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    let orb = expect(&mut alice_events, |event| match event {
        PlayerEvent::Track(track) if track.player.is_none() => Some(track),
        _ => None,
    })
    .await;
    let awarded = expect(&mut alice_events, |event| match event {
        PlayerEvent::RecipesAwarded(recipes) => Some(recipes),
        _ => None,
    })
    .await;
    assert_eq!(awarded, vec!["minecraft:diamond_from_blasting_diamond_ore"]);
    assert_eq!(
        spawned_entity_kind(&orb),
        EntityType::ExperienceOrb.id() as i32
    );
    assert!(
        crate::world::distance_sq(orb.position, feet) < 1.0,
        "experience appears at the player"
    );
    let mut collected = 0;
    while collected < 2 {
        world.ticks(1).await;
        while let Ok(event) = alice_events.try_recv() {
            if let PlayerEvent::Experience { amount } = event {
                collected += amount;
            }
        }
    }
    assert_eq!(collected, 2, "two diamonds are worth one point each");
    region
        .request(RegionRequest::ContainerTransaction {
            player: alice.clone(),
            window_id: 1,
            changes: Vec::new(),
            took_result: true,
        })
        .await;
    assert!(expect(&mut alice_events, transaction_answer).await.0);
    world.ticks(3).await;
    while let Ok(event) = alice_events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::Experience { .. }),
            "taking again awards nothing"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joining_viewers_receive_block_entities_in_the_chunk_packet() {
    use tuff_vanilla_data::registry::BlockEntityType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let chest = BlockPos::new(1, generator.height(1, 2) + 1, 2);
    let campfire = BlockPos::new(4, generator.height(4, 5) + 1, 5);
    place_blocks(
        &world,
        &region,
        [
            (chest, Block::Chest.default_state()),
            (campfire, Block::Campfire.default_state()),
        ],
    )
    .await;
    let campfire_items = |listed: &[(u8, i16, i32, Option<Tag>)]| {
        listed
            .iter()
            .find(|(_, _, kind, _)| *kind == BlockEntityType::Campfire.id() as i32)
            .and_then(|(.., nbt)| nbt.as_ref()?.as_compound()?.get_list("Items"))
            .map(<[Tag]>::len)
    };

    let (bob, mut bob_events) = PlayerHandle::new(101);
    let listed = chunk_block_entities(&chunk_packet_for(&region, &bob, &mut bob_events, pos).await);
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed.contains(&(
        1 << 4 | 2,
        chest.y as i16,
        BlockEntityType::Chest.id() as i32,
        None
    )));
    assert_eq!(campfire_items(&listed), Some(0));

    region
        .request(RegionRequest::Place(place(&alice, campfire, Item::Beef, 1)))
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Placed { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    let (carol, mut carol_events) = PlayerHandle::new(102);
    let listed =
        chunk_block_entities(&chunk_packet_for(&region, &carol, &mut carol_events, pos).await);
    assert_eq!(campfire_items(&listed), Some(1));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn campfires_cook_food_placed_on_them() {
    use tuff_vanilla_data::registry::EntityType;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let generator = Generator::new(1);
    let block = BlockPos::new(2, generator.height(2, 2) + 1, 2);
    place_blocks(&world, &region, [(block, Block::Campfire.default_state())]).await;
    assert_eq!(
        block_at(&region, block).await.and_then(|s| s.get("lit")),
        Some("true")
    );

    region
        .request(RegionRequest::Place(place(&alice, block, Item::Stick, 1)))
        .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome: ActionOutcome::Rejected(_),
        } => Some(()),
        _ => None,
    })
    .await;
    region
        .request(RegionRequest::Place(place(&alice, block, Item::Beef, 2)))
        .await;
    let data_id = tuff_vanilla_data::packet::play::Clientbound::BlockEntityData.id();
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ChunkPackets {
            pos: at, packets, ..
        } if at == pos && packets.iter().any(|p| framed_packet_id(p) == data_id) => Some(()),
        _ => None,
    })
    .await;
    expect(&mut alice_events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome: ActionOutcome::Placed {
                item: Item::Beef, ..
            },
        } => Some(()),
        _ => None,
    })
    .await;
    world.ticks(599).await;
    while let Ok(event) = alice_events.try_recv() {
        assert!(
            !matches!(&event, PlayerEvent::Track(track) if track.player.is_none()),
            "nothing pops before 600 ticks"
        );
    }
    world.ticks(2).await;
    let popped = expect(&mut alice_events, |event| match event {
        PlayerEvent::Track(track) if track.player.is_none() => Some(track),
        _ => None,
    })
    .await;
    assert_eq!(spawned_entity_kind(&popped), EntityType::Item.id() as i32);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limited_crafting_hides_results_of_recipes_the_player_has_not_unlocked() {
    use crate::world::game_rules::RuleValue;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session_with_outbound(&world, 1000).0;
    player
        .inventory_mut()
        .set_slot(1, Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test)));
    player.refresh_inventory_result();
    assert!(
        player.inventory().slots()[0].is_some(),
        "unlimited by default"
    );

    world.game_rules.send_modify(|rules| {
        rules
            .set("limited_crafting", RuleValue::Bool(true))
            .expect("a boolean rule");
    });
    player.refresh_inventory_result();
    assert_eq!(player.inventory().slots()[0], None);

    let recipes = world.shared.recipes();
    let name = recipes
        .grid_recipe(player.inventory().craft_grid(), 2, 2)
        .expect("planks recipe");
    player.award_recipes([name]);
    player.refresh_inventory_result();
    assert!(player.inventory().slots()[0].is_some());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shift_click_craft_stops_at_a_recipe_limited_crafting_hides() {
    use crate::world::menu::{GridHook, MenuKind, ResultHook};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session_with_outbound(&world, 1000).0;
    let craft = |player: &mut crate::world::session::PlayerSession, gate| {
        let inventory = player.inventory_mut();
        inventory.set_slot(1, Some(ItemStack::mint(Item::OakLog, 2, MintReason::Test)));
        let mut hook = GridHook::new(MenuKind::Player, world.shared.recipes()).limited_to(gate);
        hook.taken(&mut [], inventory, &mut Vec::new());
        inventory.slots()[0].dup_unchecked()
    };
    assert!(
        craft(&mut player, None).is_some(),
        "ungated, the next craft shows"
    );
    let book = crate::world::recipe_book::RecipeBook::default();
    assert_eq!(
        craft(&mut player, Some(book)),
        None,
        "a recipe the book lacks gives no next result"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crafting_a_recipe_unlocks_it_in_the_recipe_book() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session_with_outbound(&world, 1000).0;
    player
        .inventory_mut()
        .set_slot(1, Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test)));
    let recipes = world.shared.recipes();
    let name = recipes
        .grid_recipe(player.inventory().craft_grid(), 2, 2)
        .expect("planks recipe");
    assert!(!player.knows_recipe(name));
    let frozen = tuff_vanilla_data::Frozen::from(name.to_string());
    player.crafted_recipes(&[(frozen, Vec::new())]);
    assert!(player.knows_recipe(name));
    world.shutdown().await;
}

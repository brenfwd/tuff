use super::*;

pub(super) async fn chest_boat_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the boat exists here")
}

pub(super) async fn sneak_open(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    boat: i32,
) -> Vec<Option<ItemStack>> {
    let mut sneaking = interaction(player, None);
    sneaking.sneaking = true;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: boat,
            interaction: sneaking,
        })
        .await;
    let open = expect(events, |event| match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    })
    .await;
    assert_eq!(open.kind, crate::world::menu::MenuKind::Generic { rows: 3 });
    match open.source {
        MenuSource::Entity { entity, contents } => {
            assert_eq!(entity, boat);
            contents
        }
        other => panic!("not an entity menu: {other:?}"),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chest_boats_serve_their_items_as_a_menu_and_spill_them_when_broken() {
    use crate::world::messages::EntitySlotChange;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    let (other, mut other_events) = PlayerHandle::new(101);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    subscribe(&region, &other, &mut other_events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let diamond = ItemStack::mint(Item::Diamond, 3, MintReason::Test);
    let saved = diamond.to_nbt().with("Slot", 4i8);
    summon(
        &region,
        EntityType::OakChestBoat,
        Compound::new().with("Items", Tag::List(vec![Tag::Compound(saved)])),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let boat = mobs(&region, EntityType::OakChestBoat).await[0].id;

    let contents = sneak_open(&region, &player, &mut events, boat).await;
    assert_eq!(contents.len(), 27);
    assert_eq!(contents[4], Some(diamond.dup_unchecked()));
    sneak_open(&region, &other, &mut other_events, boat).await;

    let take = vec![
        EntitySlotChange {
            slot: 4,
            expected: Some(diamond.dup_unchecked()),
            new: None,
        },
        EntitySlotChange {
            slot: 0,
            expected: None,
            new: Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)),
        },
    ];
    region
        .request(RegionRequest::EntityTransaction {
            player: player.clone(),
            window_id: 1,
            entity: boat,
            changes: take.clone(),
        })
        .await;
    let accepted = expect(&mut events, |event| match event {
        PlayerEvent::EntityTransaction { accepted, .. } => Some(accepted),
        _ => None,
    })
    .await;
    assert!(accepted);
    let slots = expect(&mut other_events, |event| match event {
        PlayerEvent::EntitySlots { entity, slots, .. } if entity == boat => Some(slots),
        _ => None,
    })
    .await;
    assert_eq!(
        slots,
        vec![
            (4, None),
            (0, Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)))
        ]
    );

    region
        .request(RegionRequest::EntityTransaction {
            player: player.clone(),
            window_id: 1,
            entity: boat,
            changes: take,
        })
        .await;
    let (accepted, contents) = expect(&mut events, |event| match event {
        PlayerEvent::EntityTransaction {
            accepted, contents, ..
        } => Some((accepted, contents)),
        _ => None,
    })
    .await;
    assert!(!accepted, "a stale click is refused");
    assert_eq!(
        contents[0],
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test))
    );
    assert_eq!(contents[4], None);

    let data = chest_boat_data(&region, boat).await;
    let items = data.get_list("Items").expect("saved items");
    assert_eq!(items.len(), 1);
    let entry = items[0].as_compound().unwrap();
    assert_eq!(entry.get_byte("Slot"), Some(0));
    assert_eq!(entry.get_str("id"), Some("minecraft:stick"));

    for _ in 0..5 {
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target: boat,
                attack: punch(1.0),
                sweep: None,
            })
            .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::OakChestBoat)
    })
    .await;
    let items: Vec<&str> = found
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .map(|s| s.name.as_str())
        .collect();
    assert!(items.contains(&"minecraft:oak_chest_boat"), "{items:?}");
    assert!(items.contains(&"minecraft:stick"), "{items:?}");
    for events in [&mut events, &mut other_events] {
        expect(events, |event| match event {
            PlayerEvent::EntityContainerClosed { entity, .. } if entity == boat => Some(()),
            _ => None,
        })
        .await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chest_boat_rolls_its_loot_table_on_first_open_and_riders_open_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::BambooChestRaft,
        Compound::new()
            .with("LootTable", "minecraft:chests/simple_dungeon")
            .with("LootTableSeed", 42i64),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let raft = mobs(&region, EntityType::BambooChestRaft).await[0].id;
    let data = chest_boat_data(&region, raft).await;
    assert_eq!(
        data.get_str("LootTable"),
        Some("minecraft:chests/simple_dungeon")
    );
    assert_eq!(data.get_long("LootTableSeed"), Some(42));

    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: raft,
            interaction: interaction(&player, None),
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) if riding.vehicle == raft => Some(()),
        _ => None,
    })
    .await;
    region
        .request(RegionRequest::Vehicle(VehicleRequest::OpenInventory {
            player: player.clone(),
            vehicle: raft,
        }))
        .await;
    let table = expect(&mut events, |event| match event {
        PlayerEvent::Advancement(event) => match *event {
            crate::world::messages::AdvancementEvent::GeneratedContainerLoot(table) => Some(table),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(table, "minecraft:chests/simple_dungeon");
    let open = expect(&mut events, |event| match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    })
    .await;
    let MenuSource::Entity { contents, .. } = open.source else {
        panic!("not an entity menu");
    };
    assert!(contents.iter().any(Option::is_some), "the loot rolled");

    let data = chest_boat_data(&region, raft).await;
    assert!(data.get("LootTable").is_none());
    assert!(data.get("LootTableSeed").is_none());
    assert!(!data.get_list("Items").unwrap_or(&[]).is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_shift_clicks_items_out_of_a_chest_boat() {
    use crate::protocol::play::Serverbound;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(999);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let saved = ItemStack::mint(Item::Diamond, 3, MintReason::Test)
        .to_nbt()
        .with("Slot", 4i8);
    summon(
        &region,
        EntityType::SpruceChestBoat,
        Compound::new().with("Items", Tag::List(vec![Tag::Compound(saved)])),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(1).await;
    let track = expect(&mut events, track_of(EntityType::SpruceChestBoat)).await;
    let boat = track.entity;

    let (mut player, _outbound) = session_with_outbound(&world, 1000);
    player.stand_at([9.5, SKY as f64 + 1.0, 7.5], true);
    player.see_entity(track);
    player
        .handle_packet(Serverbound::Interact {
            entity: boat,
            off_hand: false,
            location: [0.0; 3],
            sneaking: true,
        })
        .await;
    assert!(
        player
            .handle_events_until(|e| matches!(e, PlayerEvent::MenuOpen(_)))
            .await
    );
    let window = player.window_id().expect("the chest is open");
    assert_eq!(player.menu_container().unwrap().len(), 27);
    player
        .handle_packet(Serverbound::ContainerClick {
            window_id: window,
            state_id: 0,
            slot: 4,
            button: 0,
            mode: 1,
            changed: Vec::new(),
            carried: None,
        })
        .await;
    assert!(
        player
            .handle_events_until(|e| matches!(
                e,
                PlayerEvent::EntityTransaction { accepted: true, .. }
            ))
            .await
    );
    assert_eq!(player.menu_container().unwrap()[4], None);
    assert!(
        player
            .inventory()
            .slots()
            .iter()
            .any(|s| s.as_ref() == Some(&ItemStack::mint(Item::Diamond, 3, MintReason::Test)))
    );
    let data = chest_boat_data(&region, boat).await;
    assert!(data.get_list("Items").unwrap_or(&[]).is_empty());
    world.shutdown().await;
}

async fn rail_line(world: &TestWorld, region: &RegionHandle, from: i32, to: i32) {
    let blocks: Vec<_> = (from..=to)
        .map(|x| {
            (
                BlockPos::new(x, SKY + 1, 6),
                Block::Rail
                    .default_state()
                    .with_shape(tuff_vanilla_data::block::Shape::EastWest)
                    .unwrap(),
            )
        })
        .collect();
    place_blocks(world, region, blocks).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_moving_minecart_shoves_one_it_runs_into() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    rail_line(&world, &region, 3, 12).await;
    let y = SKY as f64 + 1.0 + 0.0625;
    summon(
        &region,
        EntityType::Minecart,
        Compound::new(),
        [8.5, y, 6.5],
    )
    .await;
    summon(
        &region,
        EntityType::Minecart,
        Compound::new().with(
            "Motion",
            vec![Tag::Double(0.3), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        [6.5, y, 6.5],
    )
    .await;
    world.ticks(12).await;
    let carts = mobs(&region, EntityType::Minecart).await;
    let still = carts.iter().find(|c| c.position[0] > 8.0).unwrap();
    assert!(
        still.position[0] > 8.9,
        "the struck cart was shoved east: {:?}",
        still.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_furnace_minecart_burns_coal_and_drives_itself_away_from_the_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    rail_line(&world, &region, 3, 12).await;
    let y = SKY as f64 + 1.0 + 0.0625;
    summon(
        &region,
        EntityType::FurnaceMinecart,
        Compound::new(),
        [6.5, y, 6.5],
    )
    .await;
    let id = mobs(&region, EntityType::FurnaceMinecart).await[0].id;
    let mut click = interaction(&player, Some(Item::Coal));
    click.position = [4.5, y, 6.5];
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: id,
            interaction: click,
        })
        .await;
    let effect = expect(&mut events, |event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
    .await;
    assert!(matches!(effect, crate::messages::HandEffect::Consume(1)));
    world.ticks(20).await;
    let cart = mobs(&region, EntityType::FurnaceMinecart).await.remove(0);
    assert!(
        cart.position[0] > 7.0,
        "pushed along the rail away from the player: {:?}",
        cart.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_moving_minecart_pushes_a_mob_in_its_path() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    rail_line(&world, &region, 3, 12).await;
    let y = SKY as f64 + 1.0 + 0.0625;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", Tag::Byte(1)),
        [8.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    summon(
        &region,
        EntityType::Minecart,
        Compound::new().with(
            "Motion",
            vec![Tag::Double(0.3), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        [6.5, y, 6.5],
    )
    .await;
    world.ticks(12).await;
    let cow = mobs(&region, EntityType::Cow).await.remove(0);
    assert!(
        cow.position[0] > 8.6,
        "the cow was pushed east by the cart: {:?}",
        cow.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killing_a_chest_boat_scatters_its_stacks_in_pieces() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let saved = ItemStack::mint(Item::Stone, 64, MintReason::Test)
        .to_nbt()
        .with("Slot", 0i8);
    summon(
        &region,
        EntityType::OakChestBoat,
        Compound::new().with("Items", Tag::List(vec![Tag::Compound(saved)])),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let boat = mobs(&region, EntityType::OakChestBoat).await[0].id;
    let killed = region
        .ask(move |world| {
            let ids: Vec<i32> = vec![boat];
            ids.into_iter().filter(|id| world.kill_entity(*id)).count()
        })
        .await
        .unwrap();
    assert_eq!(killed, 1);
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::OakChestBoat)
    })
    .await;
    let mut pieces: Vec<u8> = Vec::new();
    for entity in mobs(&region, EntityType::Item).await {
        let data = chest_boat_data(&region, entity.id).await;
        let stack = data
            .get("Item")
            .and_then(|tag| tag.as_compound())
            .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
            .expect("a saved stack");
        if stack.item == Item::Stone {
            pieces.push(stack.count());
        }
    }
    assert_eq!(pieces.iter().map(|c| u32::from(*c)).sum::<u32>(), 64);
    assert!(pieces.len() >= 3, "{pieces:?}");
    assert!(pieces.iter().rev().skip(1).all(|c| (10..=30).contains(c)));
    assert!(
        !found.iter().any(|s| s.name == "minecraft:oak_chest_boat"),
        "a kill drops no boat item"
    );
    world.shutdown().await;
}

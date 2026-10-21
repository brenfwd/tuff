//! Pick block and pick entity (middle click) on a real session, with the
//! block and its block entity read from the region.

use super::*;
use crate::protocol::play::{GameMode, Serverbound};
use crate::world::inventory::Inventory;

async fn stone_floor(world: &TestWorld) -> RegionHandle {
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    std::mem::forget(events);
    let floor: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, 150, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    place_blocks(world, &region, floor).await;
    region
}

fn pick_block(x: i32, y: i32, z: i32, include_data: bool) -> Serverbound {
    Serverbound::PickItemFromBlock {
        position: BlockPos::new(x, y, z),
        include_data,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survival_pick_block_moves_the_item_into_the_hotbar_within_reach() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let _region = stone_floor(&world).await;
    let mut player = session_with_outbound(&world, 1000).0;
    player.stand_at([4.5, 151.0, 6.5], true);
    player.run_tick().await;
    let hotbar = |slot| Inventory::hotbar_slot(slot);
    player.inventory_mut().set_slot(
        hotbar(0),
        Some(ItemStack::mint(Item::Dirt, 1, MintReason::Test)),
    );
    player
        .inventory_mut()
        .set_slot(20, Some(ItemStack::mint(Item::Stone, 7, MintReason::Test)));

    player.handle_packet(pick_block(4, 150, 15, false)).await;
    assert_eq!(player.inventory().selected(), 0, "out of reach");

    player.handle_packet(pick_block(4, 150, 4, false)).await;
    assert_eq!(player.inventory().selected(), 1);
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::Stone, 7, MintReason::Test))
    );
    assert_eq!(player.inventory().slots()[20], None);

    player.handle_packet(pick_block(4, 150, 4, true)).await;
    assert_eq!(player.inventory().selected(), 1);

    player.inventory_mut().set_selected(0);
    player.handle_packet(pick_block(5, 150, 5, false)).await;
    assert_eq!(
        player.inventory().selected(),
        1,
        "stone is already in the hotbar"
    );
    player.handle_packet(pick_block(5, 151, 5, false)).await;
    assert_eq!(player.inventory().selected(), 1, "air picks nothing");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creative_pick_block_creates_the_item_and_copies_block_entity_data() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = stone_floor(&world).await;
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(5, 151, 5), Block::Spawner.default_state())],
    )
    .await;
    let mut player = session_with_outbound(&world, 1000).0;
    player.stand_at([4.5, 151.0, 6.5], true);
    player.run_tick().await;
    assert!(player.set_game_mode(GameMode::Creative).await);

    player.handle_packet(pick_block(5, 151, 5, false)).await;
    assert_eq!(player.inventory().selected(), 0);
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::Spawner, 1, MintReason::Test))
    );

    player.handle_packet(pick_block(5, 151, 5, true)).await;
    assert_eq!(
        player.inventory().selected(),
        1,
        "data makes it a different stack"
    );
    let picked = player.inventory().hand(false).unwrap().test_copy();
    assert_eq!(picked.item, Item::Spawner);
    let stored = picked.components.to_nbt();
    let data = stored
        .get_compound("minecraft:block_entity_data")
        .expect("block entity data copied");
    assert_eq!(data.get_str("id"), Some("minecraft:mob_spawner"));
    assert!(data.get("x").is_none());

    player.handle_packet(pick_block(5, 150, 5, false)).await;
    assert_eq!(player.inventory().selected(), 2);
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::Stone, 1, MintReason::Test))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pick_entity_selects_or_gives_its_spawn_egg() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, 150, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &region, floor).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:cow")
                .with("NoAI", true),
            position: [4.5, 151.0, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let track = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.kind == tuff_vanilla_data::registry::EntityType::Cow => {
            Some(track)
        }
        _ => None,
    })
    .await;
    let cow = track.entity;
    let mut player = session_with_outbound(&world, 1000).0;
    player.stand_at([4.5, 151.0, 6.5], true);
    player.see_entity(track);
    player.run_tick().await;
    let pick = Serverbound::PickItemFromEntity {
        entity: cow,
        include_data: false,
    };

    player.handle_packet(pick.clone()).await;
    assert_eq!(
        player.inventory().hand(false),
        None,
        "survival creates nothing"
    );

    player.inventory_mut().set_slot(
        20,
        Some(ItemStack::mint(Item::CowSpawnEgg, 2, MintReason::Test)),
    );
    player.inventory_mut().set_selected(3);
    player.handle_packet(pick.clone()).await;
    assert_eq!(player.inventory().selected(), 3);
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::CowSpawnEgg, 2, MintReason::Test))
    );

    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(3), None);
    assert!(player.set_game_mode(GameMode::Creative).await);
    player.handle_packet(pick).await;
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::CowSpawnEgg, 1, MintReason::Test))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn special_blocks_pick_their_seeds_level_and_honey() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = stone_floor(&world).await;
    let state =
        |block: Block, name: &str, value: &str| block.default_state().with(name, value).unwrap();
    place_blocks(
        &world,
        &region,
        [
            (BlockPos::new(6, 150, 5), Block::Farmland.default_state()),
            (BlockPos::new(5, 151, 5), state(Block::Light, "level", "7")),
            (BlockPos::new(6, 151, 5), state(Block::Wheat, "age", "7")),
            (
                BlockPos::new(7, 151, 5),
                state(Block::Beehive, "honey_level", "5"),
            ),
        ],
    )
    .await;
    let mut player = session_with_outbound(&world, 1000).0;
    player.stand_at([5.5, 151.0, 7.5], true);
    player.run_tick().await;
    assert!(player.set_game_mode(GameMode::Creative).await);
    let block_state = |stack: &ItemStack| {
        stack
            .components
            .to_nbt()
            .get_compound("minecraft:block_state")
            .cloned()
    };

    player.handle_packet(pick_block(5, 151, 5, false)).await;
    let light = player.inventory().hand(false).unwrap().test_copy();
    assert_eq!(light.item, Item::Light);
    assert_eq!(block_state(&light).unwrap().get_str("level"), Some("7"));

    player.handle_packet(pick_block(6, 151, 5, false)).await;
    assert_eq!(
        player.inventory().hand(false).unwrap().item,
        Item::WheatSeeds
    );

    player.handle_packet(pick_block(7, 151, 5, false)).await;
    let hive = player.inventory().hand(false).unwrap().test_copy();
    assert_eq!(hive.item, Item::Beehive);
    assert_eq!(block_state(&hive), None);

    player.handle_packet(pick_block(7, 151, 5, true)).await;
    let hive = player.inventory().hand(false).unwrap().test_copy();
    assert_eq!(
        block_state(&hive).unwrap().get_str("honey_level"),
        Some("5")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn picking_an_item_frame_gives_the_framed_item() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, 150, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    place_blocks(&world, &region, floor).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:item_frame")
                .with("Facing", tuff_nbt::Tag::Byte(1))
                .with("TileX", 4)
                .with("TileY", 150)
                .with("TileZ", 8)
                .with(
                    "Item",
                    ItemStack::mint(Item::Diamond, 1, MintReason::Test).to_nbt(),
                ),
            position: [4.5, 150.97, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let track = expect(&mut events, |event| match event {
        PlayerEvent::Track(track)
            if track.kind == tuff_vanilla_data::registry::EntityType::ItemFrame =>
        {
            Some(track)
        }
        _ => None,
    })
    .await;
    let frame = track.entity;
    let mut player = session_with_outbound(&world, 1000).0;
    player.stand_at([4.5, 151.0, 6.5], true);
    player.see_entity(track);
    player.run_tick().await;
    assert!(player.set_game_mode(GameMode::Creative).await);
    player
        .handle_packet(Serverbound::PickItemFromEntity {
            entity: frame,
            include_data: false,
        })
        .await;
    assert_eq!(
        player.inventory().hand(false),
        Some(&ItemStack::mint(Item::Diamond, 1, MintReason::Test))
    );
    world.shutdown().await;
}

fn packets_of(outbound: &mut tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>, id: i32) -> usize {
    let mut found = 0;
    while let Ok(frame) = outbound.try_recv() {
        if framed_payload(&frame).read_var_int().unwrap() == id {
            found += 1;
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_tag_query_is_answered_only_for_gamemasters() {
    use tuff_storage::admin::PermissionLevel;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let _region = stone_floor(&world).await;
    let (mut player, mut out) = session_with_outbound(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    player.run_tick().await;
    let id = tuff_vanilla_data::packet::play::Clientbound::TagQuery.id();
    let query = Serverbound::BlockEntityTagQuery {
        transaction: 4,
        position: BlockPos::new(4, 150, 6),
    };
    player.mode.permission = PermissionLevel::All;
    player.handle_packet(query.clone()).await;
    assert_eq!(packets_of(&mut out, id), 0);
    player.mode.permission = PermissionLevel::Gamemasters;
    player.handle_packet(query).await;
    assert_eq!(packets_of(&mut out, id), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_is_wrapped_in_delimiters_and_split_at_4096() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut out) = session_with_outbound(&world, 1000);
    let id = tuff_vanilla_data::packet::play::Clientbound::BundleDelimiter.id();
    while out.try_recv().is_ok() {}
    player.send(&crate::protocol::play::BlockChangedAck(1));
    let packet = out.try_recv().unwrap();
    player.send_bundle(std::iter::repeat_n(packet, 4097));
    assert_eq!(packets_of(&mut out, id), 4);
    world.shutdown().await;
}

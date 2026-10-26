//! Chest and hopper minecarts: their menus, saving, spilling, item
//! collection and hoppers moving items into and out of them.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::menu::MenuKind;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Shape;
use tuff_vanilla_data::registry::EntityType;

fn punch(damage: f32) -> EntityAttack {
    EntityAttack {
        attacker: 100,
        attacker_uuid: 77,
        attacker_position: [3.5, SKY as f64 + 1.0, 3.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

fn rail(block: Block) -> BlockState {
    block.default_state().with_shape(Shape::EastWest).unwrap()
}

async fn entity_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the cart exists here")
}

async fn open_cart(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    cart: i32,
) -> (MenuKind, Vec<Option<ItemStack>>) {
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: cart,
            interaction: interaction(player, None),
        })
        .await;
    let open = expect(events, |event| match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    })
    .await;
    match open.source {
        MenuSource::Entity { entity, contents } => {
            assert_eq!(entity, cart);
            (open.kind, stacks(&contents))
        }
        other => panic!("not an entity menu: {other:?}"),
    }
}

fn items_nbt(stacks: &[(i8, ItemStack)]) -> Compound {
    Compound::new().with(
        "Items",
        Tag::List(
            stacks
                .iter()
                .map(|(slot, stack)| Tag::Compound(stack.to_nbt().with("Slot", *slot)))
                .collect(),
        ),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn container_minecarts_place_on_rails_open_as_menus_and_spill_when_broken() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let track = BlockPos::new(6, SKY + 1, 6);
    place_blocks(&world, &region, vec![(track, rail(Block::Rail))]).await;
    region
        .request(RegionRequest::Place(place(
            &player,
            track,
            Item::ChestMinecart,
            1,
        )))
        .await;
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::ChestMinecart)
    })
    .await;
    let chest = mobs(&region, EntityType::ChestMinecart).await[0].id;
    let (kind, contents) = open_cart(&region, &player, &mut events, chest).await;
    assert_eq!(kind, MenuKind::Generic { rows: 3 });
    assert_eq!(
        contents,
        (0..27).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );

    let gold = ItemStack::mint(Item::GoldIngot, 7, MintReason::Test);
    summon(
        &region,
        EntityType::HopperMinecart,
        items_nbt(&[(2, gold.test_copy())]).with("Enabled", false),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    let hopper = mobs(&region, EntityType::HopperMinecart).await[0].id;
    let (kind, contents) = open_cart(&region, &player, &mut events, hopper).await;
    assert_eq!(kind, MenuKind::Hopper);
    assert_eq!(
        contents,
        vec![None, None, Some(gold.test_copy()), None, None]
    );
    let data = entity_data(&region, hopper).await;
    assert_eq!(data.get_byte("Enabled"), Some(0));
    assert_eq!(data.get_list("Items").map(<[_]>::len), Some(1));

    for _ in 0..5 {
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target: hopper,
                attack: punch(1.0),
                sweep: None,
            })
            .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::HopperMinecart)
    })
    .await;
    let names: Vec<&str> = found
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains(&"minecraft:hopper_minecart"), "{names:?}");
    assert!(names.contains(&"minecraft:gold_ingot"), "{names:?}");
    expect(&mut events, |event| match event {
        PlayerEvent::EntityContainerClosed { entity, .. } if entity == hopper => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_flow_from_a_chest_through_a_hopper_minecart_into_a_hopper_below() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let bottom = BlockPos::new(6, SKY - 1, 6);
    let hopper = BlockPos::new(6, SKY, 6);
    let track = BlockPos::new(6, SKY + 1, 6);
    let top = BlockPos::new(6, SKY + 2, 6);
    place_blocks(
        &world,
        &region,
        vec![
            (bottom, Block::Chest.default_state()),
            (hopper, Block::Hopper.default_state()),
            (track, rail(Block::Rail)),
            (top, Block::Chest.default_state()),
        ],
    )
    .await;
    region
        .request(RegionRequest::Place(place(&player, top, Item::Torch, 1)))
        .await;
    expect(&mut events, container_menu).await;
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            changes: set_slot(
                top,
                0,
                None,
                Some(ItemStack::mint(Item::Stone, 3, MintReason::Test)),
            ),
            pool: pool_for(&set_slot(
                top,
                0,
                None,
                Some(ItemStack::mint(Item::Stone, 3, MintReason::Test)),
            )),
            took_result: false,
        })
        .await;
    assert!(expect(&mut events, transaction_answer).await.0);
    region
        .request(RegionRequest::CloseContainer {
            player: player.id,
            positions: vec![top],
        })
        .await;

    summon(
        &region,
        EntityType::HopperMinecart,
        Compound::new(),
        [6.5, SKY as f64 + 1.0625, 6.5],
    )
    .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Apple, 2, MintReason::Test),
            position: [6.5, SKY as f64 + 1.3, 6.5],
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    world.ticks(60).await;

    region
        .request(RegionRequest::Place(place(&player, bottom, Item::Torch, 2)))
        .await;
    let open = expect(&mut events, container_menu).await;
    let MenuSource::Blocks { contents, .. } = open.source else {
        panic!("a chest is a block container");
    };
    let contents = stacks(&contents);
    let moved: Vec<ItemStack> = contents.into_iter().flatten().collect();
    assert!(
        moved.contains(&ItemStack::mint(Item::Stone, 3, MintReason::Test)),
        "the chest emptied into the cart and on down: {moved:?}"
    );
    assert!(
        moved.contains(&ItemStack::mint(Item::Apple, 2, MintReason::Test)),
        "the cart picked up the dropped apples: {moved:?}"
    );
    let cart = mobs(&region, EntityType::HopperMinecart).await[0].id;
    let data = entity_data(&region, cart).await;
    assert!(data.get_list("Items").unwrap_or(&[]).is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_powered_activator_rail_stops_a_hopper_minecart_collecting() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let track = BlockPos::new(6, SKY + 1, 6);
    place_blocks(
        &world,
        &region,
        vec![
            (
                BlockPos::new(6, SKY + 1, 7),
                Block::RedstoneBlock.default_state(),
            ),
            (
                track,
                rail(Block::ActivatorRail).with("powered", "true").unwrap(),
            ),
        ],
    )
    .await;
    summon(
        &region,
        EntityType::HopperMinecart,
        Compound::new(),
        [6.5, SKY as f64 + 1.0625, 6.5],
    )
    .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Apple, 1, MintReason::Test),
            position: [6.5, SKY as f64 + 1.1, 6.5],
            velocity: [0.0; 3],
            pickup_delay: 40,
        })
        .await;
    world.ticks(10).await;
    let found = mobs(&region, EntityType::Item).await;
    assert_eq!(found.len(), 1, "the disabled cart leaves the apple");
    let cart = mobs(&region, EntityType::HopperMinecart).await[0].id;
    assert_eq!(
        entity_data(&region, cart).await.get_byte("Enabled"),
        Some(0)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_container_minecart_keeps_its_contents_without_entity_drops() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "entity_drops",
                crate::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
    });
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::HopperMinecart,
        items_nbt(&[(2, ItemStack::mint(Item::GoldIngot, 7, MintReason::Test))])
            .with("Enabled", false),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    let hopper = mobs(&region, EntityType::HopperMinecart).await[0].id;
    for _ in 0..5 {
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target: hopper,
                attack: punch(1.0),
                sweep: None,
            })
            .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::HopperMinecart)
    })
    .await;
    world.ticks(5).await;
    assert!(
        mobs(&region, EntityType::Item).await.is_empty(),
        "{found:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_block_minecart_runs_its_command_on_a_powered_activator_rail() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let target = BlockPos::new(10, SKY + 4, 10);
    place_blocks(
        &world,
        &region,
        vec![
            (
                BlockPos::new(6, SKY + 1, 7),
                Block::RedstoneBlock.default_state(),
            ),
            (
                BlockPos::new(6, SKY + 1, 6),
                rail(Block::ActivatorRail).with("powered", "true").unwrap(),
            ),
        ],
    )
    .await;
    summon(
        &region,
        EntityType::CommandBlockMinecart,
        Compound::new().with(
            "Command",
            format!(
                "setblock {} {} {} minecraft:gold_block",
                target.x, target.y, target.z
            ),
        ),
        [6.5, SKY as f64 + 1.0625, 6.5],
    )
    .await;
    for _ in 0..40 {
        world.ticks(1).await;
        if block_at(&region, target).await == Some(Block::GoldBlock.default_state()) {
            world.shutdown().await;
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the command never ran");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_chest_minecart_is_heard_by_a_sculk_sensor() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let sensor = BlockPos::new(9, SKY + 1, 9);
    place_blocks(
        &world,
        &region,
        vec![(sensor, Block::SculkSensor.default_state())],
    )
    .await;
    summon(
        &region,
        EntityType::ChestMinecart,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let cart = mobs(&region, EntityType::ChestMinecart).await[0].id;
    let phase = |state: Option<BlockState>| {
        state
            .and_then(|s| s.get("sculk_sensor_phase").map(str::to_owned))
            .unwrap_or_default()
    };
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 5.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(60).await;
    assert_eq!(phase(block_at(&region, sensor).await), "inactive");
    open_cart(&region, &player, &mut events, cart).await;
    let mut heard = false;
    for _ in 0..40 {
        world.ticks(1).await;
        heard |= phase(block_at(&region, sensor).await) == "active";
    }
    assert!(heard, "the sensor heard the cart open");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_region_refuses_a_cart_click_from_a_window_that_was_closed() {
    use crate::world::messages::EntitySlotChange;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let gold = ItemStack::mint(Item::GoldIngot, 7, MintReason::Test);
    summon(
        &region,
        EntityType::ChestMinecart,
        items_nbt(&[(0, gold.test_copy())]),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    let cart = mobs(&region, EntityType::ChestMinecart).await[0].id;
    open_cart(&region, &player, &mut events, cart).await;

    region
        .request(RegionRequest::CloseEntityContainer {
            player: player.id,
            entity: cart,
        })
        .await;
    for change in [
        EntitySlotChange {
            slot: 0,
            expected: snap(Some(gold.test_copy())),
            new: None,
        },
        EntitySlotChange {
            slot: 1,
            expected: None,
            new: snap(Some(gold.test_copy())),
        },
    ] {
        let changes = vec![change];
        let pool = entity_pool_for(&changes);
        let given = pool.iter().map(ItemStack::count).sum::<u8>();
        region
            .request(RegionRequest::EntityTransaction {
                player: player.clone(),
                window_id: 1,
                entity: cart,
                changes,
                pool,
            })
            .await;
        let (accepted, pool) = expect(&mut events, |event| match event {
            PlayerEvent::EntityTransaction { accepted, pool, .. } => Some((accepted, pool)),
            _ => None,
        })
        .await;
        assert!(!accepted, "a closed window cannot change the cart");
        assert_eq!(pool.iter().map(ItemStack::count).sum::<u8>(), given);
        for stack in pool {
            stack.burn(tuff_item::item::BurnReason::Test);
        }
    }
    let data = entity_data(&region, cart).await;
    assert_eq!(data.get_list("Items").map(<[_]>::len), Some(1));
    world.shutdown().await;
}

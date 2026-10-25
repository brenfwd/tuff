//! The example's hooks driven through a running world beside vanilla.

#![expect(
    clippy::disallowed_methods,
    reason = "test code is exempt from the door gate"
)]

use tuff_core::protocol::play::Serverbound;
use tuff_core::world::advancement::SessionAdvancements;
use tuff_feature_builtin_example::{
    DIAMOND_FROM_DIRT, ExampleFeature, GOLD_REPORT_TRIGGER, TICK_COUNTER_ID, gold_placed,
};
use tuff_test_support::*;

async fn world_with_a_viewer(
    dir: &TestDir,
) -> (
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(ExampleFeature)]);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(3).await;
    (world, region, player, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placing_a_gold_block_counts_in_the_world_service() {
    let dir = TestDir::new();
    let (world, region, player, mut events) = world_with_a_viewer(&dir).await;
    assert_eq!(gold_placed(&world.shared), 0);

    for (sequence, (x, item)) in [(4, Item::GoldBlock), (6, Item::Stone)]
        .into_iter()
        .enumerate()
    {
        let sequence = sequence as i32 + 1;
        let floor = BlockPos::new(x, Generator::new(1).height(x, 4), 4);
        region
            .request(RegionRequest::Place(place(&player, floor, item, sequence)))
            .await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: seq,
                outcome,
            } if seq == sequence => Some(outcome),
            _ => None,
        })
        .await;
        assert!(
            matches!(outcome, ActionOutcome::Placed { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(gold_placed(&world.shared), 1, "only the gold block counts");

    let advancements = world.shared.advancements();
    let tree = advancements
        .as_any()
        .downcast_ref::<tuff_core::world::advancement::Tree>()
        .expect("the built-in advancements");
    let conditions = serde_json::json!({ "min_gold": 1 });
    let report = tuff_feature_builtin_example::GoldReport {
        gold_placed: gold_placed(&world.shared),
    };
    let event = tuff_core::world::advancement::trigger::Event::Custom {
        trigger: GOLD_REPORT_TRIGGER,
        data: &report,
    };
    assert!(tree.triggers.matches(
        GOLD_REPORT_TRIGGER,
        &conditions,
        &event,
        &Default::default()
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_example_entity_summons_ticks_and_saves_under_its_own_id() {
    let dir = TestDir::new();
    let (world, region, _player, _events) = world_with_a_viewer(&dir).await;

    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", TICK_COUNTER_ID),
            position: [8.5, 150.0, 8.5],
            fresh: true,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "the registered id summons");
    world.ticks(5).await;

    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let counter = answer
        .await
        .unwrap()
        .into_iter()
        .find(|entity| entity.name == TICK_COUNTER_ID)
        .expect("the entity is in the region");
    assert_eq!(
        counter.kind,
        tuff_vanilla_data::registry::EntityType::Interaction
    );
    assert_eq!(
        counter.registered.as_deref(),
        Some(TICK_COUNTER_ID),
        "selectors tell it apart from vanilla interactions"
    );

    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData {
            id: counter.id,
            reply,
        })
        .await;
    let saved = answer.await.unwrap().expect("the entity saves");
    assert_eq!(saved.get_str("id"), Some(TICK_COUNTER_ID));
    let ticks = match saved.get("ticks") {
        Some(Tag::Long(ticks)) => *ticks,
        other => panic!("ticks saved as {other:?}"),
    };
    assert!(ticks >= 5, "counted {ticks} ticks");
    world.shutdown().await;
}

fn session(
    world: &TestWorld,
    id: i32,
) -> (
    tuff_core::world::session::PlayerSession,
    mpsc::UnboundedReceiver<bytes::Bytes>,
) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = tuff_core::auth::GameProfile {
        id: id as u128,
        name: format!("Player{id}"),
        properties: Vec::new(),
    };
    let session = tuff_core::world::session::PlayerSession::new(
        world.shared.clone(),
        id,
        profile,
        tuff_core::protocol::configuration::ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::Owners,
    );
    (session, outbound_rx)
}

fn sent(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>, id: i32) -> usize {
    let mut found = 0;
    while let Ok(frame) = outbound.try_recv() {
        if framed_packet_id(&frame) == id {
            found += 1;
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn picking_up_dirt_teaches_the_dirt_recipe_and_the_book_fills_the_grid() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(ExampleFeature)]);
    let (mut player, mut outbound) = session(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.run_tick().await;
    assert!(!player.knows_recipe(DIAMOND_FROM_DIRT));
    let add = tuff_vanilla_data::packet::play::Clientbound::RecipeBookAdd.id();
    sent(&mut outbound, add);

    player
        .inventory_mut()
        .set_slot(36, Some(ItemStack::mint(Item::Dirt, 3, MintReason::Test)));
    player.run_tick().await;
    assert!(player.knows_recipe(DIAMOND_FROM_DIRT));
    assert!(player.advancement_done("example:recipes/diamond_from_dirt"));
    assert!(sent(&mut outbound, add) > 0, "the client is told");

    let display = world.shared.recipes().displays_for(DIAMOND_FROM_DIRT)[0].id;
    player
        .handle_packet(Serverbound::PlaceRecipe {
            window_id: 0,
            recipe: display as i32,
            use_max_items: false,
        })
        .await;
    let slots = player.inventory().slots();
    assert_eq!(
        slots[1..5]
            .iter()
            .flatten()
            .map(tuff_item::item::ItemStack::test_copy)
            .collect::<Vec<_>>(),
        vec![ItemStack::mint(Item::Dirt, 1, MintReason::Test)],
        "the book moved one dirt into the grid"
    );
    assert_eq!(
        slots[0],
        Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test))
    );
    assert_eq!(
        slots[36],
        Some(ItemStack::mint(Item::Dirt, 2, MintReason::Test))
    );
    world.shutdown().await;
}

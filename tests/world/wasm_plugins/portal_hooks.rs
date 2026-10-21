//! Hooks for plugin portals: bodies inside watched blocks, teleports
//! across dimensions, and click filters that also name the clicked block.

use super::*;
use crate::protocol::play::PlayerActionStatus;
use crate::world::contact::PLUGIN_PORTAL_COOLDOWN;
use crate::world::inventory::Inventory;
use crate::world::messages::{PlayerEvent, PlayerHandle, RegionRequest};
use tuff_item::item::{ItemStack, MintReason};
use tuff_plugin_wasm::portal::{self, Target, Teleport};
use tuff_plugin_wasm::wit::bindings::types::{Rotation, Uuid};
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::pos::RegionPos;
use tuff_world_model::{BlockPos, Face};

const NETHER: &str = "minecraft:the_nether";

/// Broadcasts `inside` when something enters an end gateway it watches.
fn gateway_watcher() -> Vec<u8> {
    block_watcher("gates", "end_gateway", "inside")
}

/// Plugin `id` watches `block` and broadcasts `text` on every event.
fn block_watcher(id: &str, block: &str, text: &str) -> Vec<u8> {
    let mut data = Data::new();
    let watch = data.call("watch-block", &[s(block)]);
    let bad = data.call("watch-block", &[s("not_a_block")]);
    let (ptr, len) = data.string(text);
    let body = format!(
        "(func {} {watch} {bad} {})
         (func {} (call $broadcast (i32.const {ptr}) (i32.const {len}) (i32.const 256)))
         {}",
        export("register"),
        ok_unit(),
        export("on-event"),
        data.wat()
    );
    module(&plain(id), &body)
}

/// Cancels a right click with a stick on glowstone only.
fn glowstone_stick() -> Vec<u8> {
    let mut data = Data::new();
    let filter = ClickFilter {
        kind: ClickKind::Right,
        item: Some("minecraft:stick".into()),
        tag: None,
        block: Some("glowstone".into()),
        scope: super::everywhere(),
    };
    let claim = data.call("intercept-click", &[filter.to_value()]);
    let (ptr, len) = data.string("clicked");
    let body = format!(
        "(func {} {claim} {})
         (func {} (call $broadcast (i32.const {ptr}) (i32.const {len}) (i32.const 256)))
         {}",
        export("register"),
        ok_unit(),
        export("on-event"),
        data.wat()
    );
    module(&plain("glow"), &body)
}

async fn set_blocks(region: &RegionHandle, blocks: &[(BlockPos, tuff_vanilla_data::BlockState)]) {
    let (reply, done) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Set {
                changes: blocks.to_vec(),
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    done.await.unwrap();
}

async fn items_in(region: &RegionHandle) -> Vec<(i32, u128)> {
    region
        .ask(|world| {
            world
                .entity_ids()
                .into_iter()
                .filter_map(|id| {
                    let tracked = world.entity(id)?;
                    let base = tracked.entity.base();
                    (base.kind.name() == "minecraft:item").then_some((id, base.uuid))
                })
                .collect()
        })
        .await
        .unwrap_or_default()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_click_filter_can_name_the_clicked_block() {
    let dir = TestDir::new();
    install(dir.path(), "glow.wasm", &glowstone_stick());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let slot = Inventory::hotbar_slot(client.session.inventory.selected());
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test));
    let click = |pos: BlockPos| Serverbound::UseItemOn {
        hand: 0,
        position: pos,
        face: Face::Up,
        cursor: [0.5; 3],
        sequence: 1,
    };
    let lamp = BlockPos::new(1, 64, 2);
    let stone = BlockPos::new(1, 64, 3);
    let region = world.region(RegionPos::new(0, 0)).await;
    set_blocks(
        &region,
        &[
            (lamp, Block::Glowstone.default_state()),
            (stone, Block::Stone.default_state()),
        ],
    )
    .await;

    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &click(stone))
            .await,
        Verdict::Allow,
        "a stick on stone is left alone"
    );
    assert_eq!(announced(&world).await.len(), 0);
    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &click(lamp))
            .await,
        Verdict::Cancel
    );
    assert_eq!(announced(&world).await, ["clicked".to_owned()]);
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stone, 1, MintReason::Test));
    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &click(lamp))
            .await,
        Verdict::Allow,
        "the item has to match too"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_inside_a_watched_gateway_is_reported_once_per_cooldown() {
    use tuff_core::world::contact::PortalContact as Contact;
    use tuff_feature_builtin_vanilla_dimensions::world::portal::PortalContact;

    let dir = TestDir::new();
    install(dir.path(), "gates.wasm", &gateway_watcher());
    let (player, mut events) = PlayerHandle::new(1000);
    let world = start(dir.path()).await;
    let gate = BlockPos::new(3, 80, 3);
    let region = world.region(RegionPos::new(0, 0)).await;
    set_blocks(&region, &[(gate, Block::EndGateway.default_state())]).await;
    assert!(host(&world).watching(), "the plugin claimed the block");

    let mut client = Client::new(&world);
    let mut portals: Vec<Option<Contact>> = Vec::new();
    for seq in 1..=2 {
        region.request(update(&player, seq, [3.5, 79.6, 3.5])).await;
        portals.push(
            expect(&mut events, |event| match event {
                PlayerEvent::Surroundings { portal, .. } => Some(portal),
                _ => None,
            })
            .await,
        );
    }
    let contact = portals[0].expect("the region reports the claimed gateway");
    assert!(contact.claimed && contact.pos == gate, "{contact:?}");
    assert!(
        PortalContact::of(contact).is_none(),
        "vanilla leaves a claimed block alone"
    );

    use tuff_core::world::session::hooks::SessionHooks;
    portal_session_touch(&mut client, Some(contact)).await;
    assert_eq!(count(&world, "inside").await, 1);
    assert_eq!(
        client.session.actions.portal_cooldown,
        PLUGIN_PORTAL_COOLDOWN
    );
    portal_session_touch(&mut client, Some(contact)).await;
    assert_eq!(count(&world, "inside").await, 1, "still cooling down");

    client.session.actions.portal_cooldown = 0;
    portal_session_touch(&mut client, Some(contact)).await;
    assert_eq!(count(&world, "inside").await, 2);
    assert!(
        !tuff_feature_builtin_vanilla_dimensions::world::session::portals::PortalHooks
            .wants_reports(&client.session),
        "the built-in portal timer never started"
    );

    console(&world, "tuff plugin unload gates").await;
    region.request(update(&player, 3, [3.5, 79.6, 3.5])).await;
    let after = expect(&mut events, |event| match event {
        PlayerEvent::Surroundings { seq: 3, portal, .. } => Some(portal),
        _ => None,
    })
    .await;
    assert!(
        after.is_some_and(|contact| !contact.claimed),
        "unloaded, the gateway is the built-in one again"
    );
    world.shutdown().await;
}

async fn portal_session_touch(
    client: &mut Client,
    contact: Option<tuff_core::world::contact::PortalContact>,
) {
    use tuff_core::world::session::hooks::SessionHooks;
    portal::Contacts
        .surroundings(&mut client.session, contact)
        .await;
    tuff_feature_builtin_vanilla_dimensions::world::session::portals::PortalHooks
        .surroundings(&mut client.session, contact)
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_in_a_watched_gateway_is_reported_and_can_be_teleported() {
    let dir = TestDir::new();
    install(dir.path(), "gates.wasm", &gateway_watcher());
    let world = start(dir.path()).await;
    let gate = BlockPos::new(4, 80, 4);
    let region = world.region(RegionPos::new(0, 0)).await;
    set_blocks(&region, &[(gate, Block::EndGateway.default_state())]).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [4.5, 81.5, 4.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    world.ticks(10).await;
    assert_eq!(count(&world, "inside").await, 1, "once, then it waits");
    world.ticks(10).await;
    assert_eq!(count(&world, "inside").await, 1);

    let (_, uuid) = items_in(&region).await.remove(0);
    let nether = world.dimension(NETHER);
    let arrival = [100.5, 70.0, 100.5];
    portal::apply(
        &world.shared,
        vec![Teleport {
            target: Target::Entity(uuid),
            dimension: nether,
            position: arrival,
            rotation: Some([90.0, 0.0]),
        }],
    )
    .await;
    let there = world
        .region_in(
            NETHER,
            tuff_world_model::pos::ChunkPos::of_position(arrival).region(),
        )
        .await;
    let mut arrived = Vec::new();
    for _ in 0..50 {
        world.ticks(2).await;
        arrived = items_in(&there).await;
        if !arrived.is_empty() && items_in(&region).await.is_empty() {
            break;
        }
    }
    assert!(items_in(&region).await.is_empty(), "it left the overworld");
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].1, uuid);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_teleport_moves_a_player_across_dimensions_and_starts_the_cooldown() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let nether = world.dimension(NETHER);
    client
        .session
        .on_event(PlayerEvent::PluginTeleport(
            tuff_core::world::messages::Teleport {
                dimension: nether,
                position: [50.5, 70.0, -20.5],
                relative: [false; 3],
                rotation: Some([45.0, 10.0]),
                relative_rotation: [false; 2],
                look_at: None,
            },
        ))
        .await;
    assert_eq!(client.session.dimension, nether);
    assert_eq!(client.session.pose.position, [50.5, 70.0, -20.5]);
    assert_eq!(client.session.pose.yaw, 45.0);
    assert_eq!(
        client.session.actions.portal_cooldown,
        PLUGIN_PORTAL_COOLDOWN
    );
    world.shutdown().await;
}

/// `warp` returns 100 * (unknown dimension refused) + 10 * (player teleport
/// queued) + (entity teleport queued, 0 for ok).
fn teleporter() -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["warp"], "");
    let who = Uuid { high: 0, low: 77 }.to_value().into_owned();
    let at = WitVec3 {
        x: 1.0,
        y: 70.0,
        z: 2.0,
    }
    .to_value()
    .into_owned();
    let turn = Some(Rotation {
        yaw: 90.0,
        pitch: 0.0,
    })
    .to_value()
    .into_owned();
    let nowhere = data.call(
        "teleport-player",
        &[who.clone(), s("nowhere"), at.clone(), turn.clone()],
    );
    let player = data.call(
        "teleport-player",
        &[who.clone(), s(NETHER), at.clone(), turn.clone()],
    );
    let entity = data.call(
        "teleport-entity",
        &[
            who,
            s("overworld"),
            at,
            None::<Rotation>.to_value().into_owned(),
        ],
    );
    let load = format!("(i32.load8_u (i32.const {RET}))");
    let code = ok_code(
        "(i32.add (i32.add (i32.mul (local.get $a) (i32.const 100)) (i32.mul (local.get $b) (i32.const 10))) (local.get $c))",
    );
    let body = format!(
        "{register}
         (func {} (local $a i32) (local $b i32) (local $c i32)
           {nowhere} (local.set $a {load})
           {player} (local.set $b {load})
           {entity} (local.set $c {load})
           {code})
         {}",
        export("run-command"),
        data.wat()
    );
    module(&plain("tp"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_imports_check_the_dimension_and_queue() {
    let dir = TestDir::new();
    install(dir.path(), "tp.wasm", &teleporter());
    let world = start(dir.path()).await;

    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "warp".into())
        .await
        .expect("the plugin owns /warp");
    assert_eq!(outcome.result, Ok(100));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_events_reach_only_the_plugin_that_watches_the_block() {
    let dir = TestDir::new();
    install(
        dir.path(),
        "gates.wasm",
        &block_watcher("gates", "end_gateway", "gate"),
    );
    install(
        dir.path(),
        "glow.wasm",
        &block_watcher("glow", "glowstone", "glow"),
    );
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    set_blocks(
        &region,
        &[(BlockPos::new(4, 80, 4), Block::EndGateway.default_state())],
    )
    .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [4.5, 81.5, 4.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    world.ticks(10).await;
    assert_eq!(count(&world, "gate").await, 1);
    assert_eq!(count(&world, "glow").await, 0, "it watches another block");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entity_resting_in_a_watched_block_is_reported_once() {
    let dir = TestDir::new();
    install(dir.path(), "gates.wasm", &gateway_watcher());
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    set_blocks(
        &region,
        &[
            (BlockPos::new(4, 80, 4), Block::EndGateway.default_state()),
            (BlockPos::new(4, 79, 4), Block::Stone.default_state()),
        ],
    )
    .await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [4.5, 80.0, 4.5],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    world.ticks(40).await;
    assert_eq!(count(&world, "inside").await, 1);
    world.shutdown().await;
}

/// `give` returns 100 * (unknown item refused) + 10 * (non-compound data
/// refused) + (valid give queued, 0 for ok).
fn giver() -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["grant"], "");
    let some = |text: &str| Some(text.to_owned()).to_value().into_owned();
    let none = || None::<String>.to_value().into_owned();
    let bad_item = data.call(
        "give-item",
        &[s("minecraft:nope"), WitValue::U32(1), none(), none()],
    );
    let bad_data = data.call(
        "give-item",
        &[s("minecraft:stone"), WitValue::U32(1), some("1"), none()],
    );
    let good = data.call(
        "give-item",
        &[
            s("minecraft:wooden_axe"),
            WitValue::U32(1),
            some("{wand:1b}"),
            some("Wand"),
        ],
    );
    let load = format!("(i32.load8_u (i32.const {RET}))");
    let code = ok_code(
        "(i32.add (i32.add (i32.mul (local.get $a) (i32.const 100)) (i32.mul (local.get $b) (i32.const 10))) (local.get $c))",
    );
    let body = format!(
        "{register}
         (func {} (local $a i32) (local $b i32) (local $c i32)
           {bad_item} (local.set $a {load})
           {bad_data} (local.set $b {load})
           {good} (local.set $c {load})
           {code})
         {}",
        export("run-command"),
        data.wat()
    );
    module(&plain("giver"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn give_item_import_validates_and_queues() {
    let dir = TestDir::new();
    install(dir.path(), "giver.wasm", &giver());
    let world = start(dir.path()).await;
    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "grant".into())
        .await
        .expect("the plugin owns /grant");
    assert_eq!(outcome.result, Ok(110));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_give_lands_in_the_inventory() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![
            tuff_core::world::session::plugin_inventory::InventoryOp::Give {
                stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
                count: 70,
            },
        ]))
        .await;
    let total: u32 = client
        .session
        .inventory
        .slots()
        .iter()
        .flatten()
        .filter(|stack| stack.item == Item::Stone)
        .map(|stack| u32::from(stack.count()))
        .sum();
    assert_eq!(total, 70);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_inventory_ops_take_clear_set_and_keep() {
    use tuff_core::world::session::plugin_inventory::InventoryOp;
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let stone = |slot: &Client| -> u32 {
        slot.session
            .inventory
            .slots()
            .iter()
            .flatten()
            .filter(|stack| stack.item == Item::Stone)
            .map(|stack| u32::from(stack.count()))
            .sum()
    };
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::Give {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            count: 70,
        }]))
        .await;
    let take = |count, all_or_nothing| {
        PlayerEvent::PluginInventory(vec![InventoryOp::Take {
            item: Item::Stone,
            count,
            all_or_nothing,
        }])
    };
    client.session.on_event(take(100, true)).await;
    assert_eq!(stone(&client), 70);
    client.session.on_event(take(100, false)).await;
    assert_eq!(stone(&client), 0);
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::SetSlot {
            slot: 5,
            stack: Some(ItemStack::mint(Item::DiamondHelmet, 1, MintReason::Test)),
        }]))
        .await;
    assert!(client.session.inventory.slots()[5].is_some());
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::Clear]))
        .await;
    assert!(client.session.inventory.slots()[5].is_none());
    assert!(!client.session.keeps_inventory());
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![
            InventoryOp::KeepInventory(Some(true)),
        ]))
        .await;
    assert!(client.session.keeps_inventory());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_all_or_nothing_take_skips_the_give_after_it() {
    use tuff_core::world::session::plugin_inventory::InventoryOp;
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let count = |client: &Client, item: Item| -> u32 {
        client
            .session
            .inventory
            .slots()
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| u32::from(stack.count()))
            .sum()
    };
    let exchange = || {
        PlayerEvent::PluginInventory(vec![
            InventoryOp::Take {
                item: Item::Diamond,
                count: 10,
                all_or_nothing: true,
            },
            InventoryOp::Give {
                stack: ItemStack::mint(Item::Elytra, 1, MintReason::Test),
                count: 1,
            },
        ])
    };
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::Give {
            stack: ItemStack::mint(Item::Diamond, 1, MintReason::Test),
            count: 4,
        }]))
        .await;
    client.session.on_event(exchange()).await;
    assert_eq!(count(&client, Item::Diamond), 4, "too few: nothing taken");
    assert_eq!(count(&client, Item::Elytra), 0, "and nothing given");
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::Give {
            stack: ItemStack::mint(Item::Diamond, 1, MintReason::Test),
            count: 6,
        }]))
        .await;
    client.session.on_event(exchange()).await;
    assert_eq!(count(&client, Item::Diamond), 0);
    assert_eq!(count(&client, Item::Elytra), 1);
    world.shutdown().await;
}

/// `spawn` returns 100 * (unknown kind refused) + 10 * (bad SNBT refused)
/// + (valid spawn queued, 0 for ok).
fn spawner() -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["spawner"], "");
    let some = |text: &str| Some(text.to_owned()).to_value().into_owned();
    let none = || None::<String>.to_value().into_owned();
    let at = || {
        tuff_plugin_wasm::wit::bindings::types::Vec3 {
            x: 3.5,
            y: 80.0,
            z: 3.5,
        }
        .to_value()
        .into_owned()
    };
    let call = |data: &mut Data, kind: &str, snbt: WitValue<'static>| {
        data.call("spawn-entity", &[s("overworld"), s(kind), at(), snbt])
    };
    let bad_kind = call(&mut data, "minecraft:nope", none());
    let bad_data = call(&mut data, "minecraft:cow", some("1"));
    let good = call(&mut data, "minecraft:cow", some("{CustomName:\"Bob\"}"));
    let load = format!("(i32.load8_u (i32.const {RET}))");
    let code = ok_code(
        "(i32.add (i32.add (i32.mul (local.get $a) (i32.const 100)) (i32.mul (local.get $b) (i32.const 10))) (local.get $c))",
    );
    let body = format!(
        "{register}
         (func {} (local $a i32) (local $b i32) (local $c i32)
           {bad_kind} (local.set $a {load})
           {bad_data} (local.set $b {load})
           {good} (local.set $c {load})
           {code})
         {}",
        export("run-command"),
        data.wat()
    );
    module(&plain("spawner"), &body)
}

async fn cows(region: &PlayerlessRegion) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

type PlayerlessRegion = crate::world::messages::RegionHandle;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_spawns_edits_and_removes_an_entity() {
    use tuff_plugin_wasm::entities::{self, EntityOp};
    let dir = TestDir::new();
    install(dir.path(), "spawner.wasm", &spawner());
    let world = start(dir.path()).await;
    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "spawner".into())
        .await
        .expect("the plugin owns /spawner");
    assert_eq!(outcome.result, Ok(110));
    world.ticks(6).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let found = cows(&region).await;
    assert_eq!(found.len(), 1);
    let uuid = found[0].uuid;

    let data = entities::data(world.shared.clone(), uuid).await.unwrap();
    assert!(data.to_string().contains("Bob"));

    entities::apply(
        &world.shared,
        vec![EntityOp::Rename(uuid, Some("Rob".into()))],
    )
    .await;
    let data = entities::data(world.shared.clone(), uuid).await.unwrap();
    assert!(data.to_string().contains("Rob"));
    assert_eq!(cows(&region).await.len(), 1);

    entities::apply(&world.shared, vec![EntityOp::Remove(uuid)]).await;
    assert!(cows(&region).await.is_empty());
    assert!(entities::data(world.shared.clone(), uuid).await.is_none());
    world.shutdown().await;
}

fn spawn_op(
    dimension: crate::world::dimension::DimensionId,
    snbt: &str,
    position: [f64; 3],
) -> (u128, tuff_plugin_wasm::entities::EntityOp) {
    use tuff_nbt::Tag;
    let Ok(Tag::Compound(nbt)) = Tag::from_snbt(snbt) else {
        panic!("bad snbt");
    };
    let uuid = nbt
        .get("UUID")
        .and_then(|t| t.as_uuid())
        .expect("uuid in snbt");
    (
        uuid,
        tuff_plugin_wasm::entities::EntityOp::Spawn {
            dimension,
            nbt,
            position,
            fresh: false,
        },
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_strike_hurts_a_victim_in_reach_and_not_one_beyond() {
    use tuff_plugin_wasm::entities::{self, EntityOp};
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let dimension = Client::new(&world).session.dimension;
    let (bot, spawn_bot) = spawn_op(
        dimension,
        "{id:\"minecraft:mannequin\",UUID:[I;1,2,3,4],equipment:{mainhand:{id:\"minecraft:iron_sword\",count:1}}}",
        [8.5, 70.0, 8.5],
    );
    let (near, spawn_near) = spawn_op(
        dimension,
        "{id:\"minecraft:cow\",UUID:[I;5,6,7,8]}",
        [9.5, 70.0, 8.5],
    );
    let (far, spawn_far) = spawn_op(
        dimension,
        "{id:\"minecraft:cow\",UUID:[I;9,10,11,12]}",
        [8.5, 70.0, 14.5],
    );
    entities::apply(&world.shared, vec![spawn_bot, spawn_near, spawn_far]).await;
    world.ticks(2).await;
    let health = |uuid| {
        let shared = world.shared.clone();
        async move {
            let data = entities::data(shared, uuid).await.unwrap();
            let text = data.to_string();
            let tail = &text[text.find("Health:").unwrap() + 7..];
            tail[..tail.find('f').unwrap()].parse::<f32>().unwrap()
        }
    };
    let before = health(near).await;
    entities::apply(
        &world.shared,
        vec![
            EntityOp::Strike {
                attacker: bot,
                victim: far,
            },
            EntityOp::Strike {
                attacker: bot,
                victim: near,
            },
        ],
    )
    .await;
    world.ticks(2).await;
    assert!(health(near).await < before);
    assert_eq!(health(far).await, before);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_glides_an_entity_and_reads_its_state() {
    use tuff_plugin_wasm::entities::{self, EntityOp, Glide};
    let dir = TestDir::new();
    install(dir.path(), "spawner.wasm", &spawner());
    let world = start(dir.path()).await;
    host(&world)
        .command(Player::new(77, "Steve"), "spawner".into())
        .await
        .expect("the plugin owns /spawner");
    world.ticks(6).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let uuid = cows(&region).await[0].uuid;

    let unknown = uuid ^ 1;
    let before = entities::states(world.shared.clone(), vec![uuid, unknown]).await;
    assert!(before[0].is_some() && before[1].is_none());
    let from = before[0].unwrap().position;

    let to = [from[0] + 1.0, from[1], from[2] + 0.5];
    entities::apply(
        &world.shared,
        vec![EntityOp::Glide(vec![Glide {
            uuid,
            position: to,
            rotation: Some([90.0, 10.0]),
        }])],
    )
    .await;
    let after = entities::states(world.shared.clone(), vec![uuid])
        .await
        .remove(0)
        .unwrap();
    assert_eq!(after.position, to);
    assert_eq!(after.motion, [0.0; 3]);
    assert_eq!((after.yaw, after.pitch), (90.0, 10.0));
    world.shutdown().await;
}

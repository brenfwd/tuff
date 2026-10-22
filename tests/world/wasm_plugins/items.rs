//! Item drop, pickup, swap-hands and crafting events and their cancel
//! filters, end to end through a WAT plugin.

use super::*;
use crate::protocol::play::PlayerActionStatus;
use crate::world::inventory::Inventory;
use crate::world::menu::MenuKind;
use crate::world::messages::{MenuOpen, MenuSource};
use crate::world::session::hooks::SessionHooks;
use tuff_item::item::{ItemStack, MintReason};
use tuff_plugin_wasm::items::{Actions, Pickups};
use tuff_plugin_wasm::wit::bindings::types::{FilterScope, ItemActionKind, ItemFilter};
use tuff_vanilla_data::Item;
use tuff_world_model::BlockPos;

/// Cancels egg drops, egg pickups, every hand swap and every craft, and
/// broadcasts the item of each drop, pickup, swap and craft event.
fn sorter() -> Vec<u8> {
    sorter_in(super::everywhere)
}

fn sorter_in(scope: impl Fn() -> FilterScope) -> Vec<u8> {
    let mut data = Data::new();
    let filter = |kind, item: Option<&str>| {
        ItemFilter {
            kind,
            item: item.map(Into::into),
            tag: None,
            scope: scope(),
        }
        .to_value()
        .into_owned()
    };
    let more = data.call(
        "intercept-item",
        &[filter(ItemActionKind::Drop, Some("minecraft:egg"))],
    ) + &data.call(
        "intercept-item",
        &[filter(ItemActionKind::Pickup, Some("minecraft:egg"))],
    ) + &data.call("intercept-item", &[filter(ItemActionKind::Swap, None)])
        + &data.call("intercept-item", &[filter(ItemActionKind::Craft, None)]);
    let register = register(&mut data, &[], &more);
    let body = format!(
        r#"{register}
        (func {event} (param $ev i32)
          (call $clear)
          (if (i32.ge_u (i32.load8_u (local.get $ev)) (i32.const 20))
            (then
              (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))
              {SHOUT}))
          )
        {data}"#,
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("sorter"), &body)
}

fn action(status: PlayerActionStatus) -> Serverbound {
    Serverbound::PlayerAction {
        status,
        position: BlockPos::new(0, 0, 0),
        face: 0,
        sequence: 0,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drops_swaps_pickups_and_crafts_are_reported_and_cancelled_by_filter() {
    let dir = TestDir::new();
    install(dir.path(), "sorter.wasm", &sorter());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let slot = Inventory::hotbar_slot(client.session.inventory.selected());

    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Egg, 4, MintReason::Test));
    let drop = action(PlayerActionStatus::DropItem);
    assert_eq!(
        Actions.intercept(&mut client.session, &drop).await,
        Verdict::Cancel
    );
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Snowball, 4, MintReason::Test));
    assert_eq!(
        Actions.intercept(&mut client.session, &drop).await,
        Verdict::Allow
    );

    let swap = action(PlayerActionStatus::SwapItemWithOffhand);
    assert_eq!(
        Actions.intercept(&mut client.session, &swap).await,
        Verdict::Cancel
    );

    let egg = ItemStack::mint(Item::Egg, 2, MintReason::Test);
    assert!(!Pickups.allows_item_pickup(&client.session, egg.view()));
    assert!(Pickups.allows_item_pickup(
        &client.session,
        ItemStack::mint(Item::Stick, 2, MintReason::Test).view()
    ));

    *client.session.inventory.slot_mut(0) = Some(ItemStack::mint(Item::Stick, 4, MintReason::Test));
    let take = Serverbound::ContainerClick {
        window_id: 0,
        state_id: 0,
        slot: 0,
        button: 0,
        mode: 0,
        changed: Vec::new(),
        carried: None,
    };
    assert_eq!(
        Actions.intercept(&mut client.session, &take).await,
        Verdict::Cancel
    );

    assert_eq!(
        announced(&world).await,
        [
            "minecraft:egg",
            "minecraft:snowball",
            "minecraft:snowball",
            "minecraft:egg",
            "minecraft:stick",
            "minecraft:stick",
        ]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn station_result_slots_report_and_cancel_crafts() {
    let dir = TestDir::new();
    install(dir.path(), "sorter.wasm", &sorter());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut expected = Vec::new();

    for kind in [
        MenuKind::Stonecutter,
        MenuKind::Smithing,
        MenuKind::Furnace,
        MenuKind::BlastFurnace,
        MenuKind::Smoker,
        MenuKind::Cartography,
    ] {
        client
            .session
            .open_menu(MenuOpen {
                dimension: client.session.dimension,
                region: region.clone(),
                kind,
                title: TextComponent::translate("container.test", []),
                source: MenuSource::Station {
                    pos: BlockPos::new(0, 64, 0),
                    bookcases: 0,
                    data: Vec::new(),
                },
                stat: None,
            })
            .await;
        let window_id = client.session.window_id().expect("the menu opened");
        let result = kind.result_slot().expect("a result slot");
        client.session.menus.menu.as_mut().unwrap().container[result] =
            Some(ItemStack::mint(Item::Stick, 2, MintReason::Test));
        let click = |slot: usize| Serverbound::ContainerClick {
            window_id,
            state_id: 0,
            slot: slot as i16,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        };
        assert_eq!(
            Actions.intercept(&mut client.session, &click(result)).await,
            Verdict::Cancel,
            "{kind:?}"
        );
        assert_eq!(
            Actions
                .intercept(&mut client.session, &click(result + 1))
                .await,
            Verdict::Allow,
            "{kind:?} non-result slot"
        );
        expected.push("minecraft:stick");
    }

    assert_eq!(announced(&world).await, expected);
    world.shutdown().await;
}

fn drop_egg(client: &mut Client) -> Serverbound {
    let slot = Inventory::hotbar_slot(client.session.inventory.selected());
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Egg, 4, MintReason::Test));
    action(PlayerActionStatus::DropItem)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn item_filters_honour_dimension_area_and_bypass_scopes() {
    let corners = |lo: f64, hi: f64| {
        (
            tuff_plugin_wasm::wit::bindings::types::Vec3 {
                x: lo,
                y: -999.0,
                z: lo,
            },
            tuff_plugin_wasm::wit::bindings::types::Vec3 {
                x: hi,
                y: 999.0,
                z: hi,
            },
        )
    };
    let cases: [(&str, FilterScope, Verdict); 6] = [
        (
            "dimension matches",
            FilterScope {
                dimension: Some("minecraft:overworld".into()),
                ..super::everywhere()
            },
            Verdict::Cancel,
        ),
        (
            "dimension differs",
            FilterScope {
                dimension: Some("minecraft:the_nether".into()),
                ..super::everywhere()
            },
            Verdict::Allow,
        ),
        (
            "inside area",
            FilterScope {
                area: Some(corners(-100000.0, 100000.0)),
                ..super::everywhere()
            },
            Verdict::Cancel,
        ),
        (
            "outside area",
            FilterScope {
                area: Some(corners(90000.0, 100000.0)),
                ..super::everywhere()
            },
            Verdict::Allow,
        ),
        (
            "bypass held",
            FilterScope {
                bypass: Some("tuff.test.bypass".into()),
                ..super::everywhere()
            },
            Verdict::Allow,
        ),
        (
            "bypass not held",
            FilterScope {
                bypass: Some("tuff.test.bypass".into()),
                ..super::everywhere()
            },
            Verdict::Cancel,
        ),
    ];
    for (name, scope, expected) in cases {
        let dir = TestDir::new();
        let scope = std::cell::RefCell::new(Some(scope));
        install(
            dir.path(),
            "sorter.wasm",
            &sorter_in(|| scope.borrow().clone().unwrap()),
        );
        let world = start(dir.path()).await;
        let mut client = Client::new(&world);
        client.session.mode.permission = if name == "bypass not held" {
            tuff_storage::admin::PermissionLevel::All
        } else {
            tuff_storage::admin::PermissionLevel::Owners
        };
        let drop = drop_egg(&mut client);
        let got = Actions.intercept(&mut client.session, &drop).await;
        assert_eq!(got, expected, "{name}");
        world.shutdown().await;
    }
}

/// Broadcasts the item of every `items-taken` event that removed something.
fn take_watcher() -> Vec<u8> {
    let mut data = Data::new();
    let more = data.call(
        "subscribe",
        &[tuff_plugin_wasm::wit::bindings::types::EventKinds::ITEMS_TAKEN.to_value()],
    );
    let register = register(&mut data, &[], &more);
    let body = format!(
        r#"{register}
        (func {event} (param $ev i32)
          (call $clear)
          (if (i32.ne (i32.load offset=44 (local.get $ev)) (i32.const 0))
            (then
              (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))
              {SHOUT}))
          )
        {data}"#,
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("taken"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_take_reports_how_many_items_it_removed() {
    use crate::world::messages::PlayerEvent;
    use crate::world::session::plugin_inventory::InventoryOp;
    let dir = TestDir::new();
    install(dir.path(), "taken.wasm", &take_watcher());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    *client.session.inventory.slot_mut(36) =
        Some(ItemStack::mint(Item::Stone, 10, MintReason::Test));
    let take = |count, all_or_nothing| {
        PlayerEvent::PluginInventory(vec![InventoryOp::Take {
            item: Item::Stone,
            count,
            all_or_nothing,
        }])
    };
    client.session.on_event(take(20, true)).await;
    client.session.on_event(take(4, true)).await;
    client.session.on_event(take(20, false)).await;
    client.session.on_event(take(20, false)).await;
    assert_eq!(
        announced(&world).await,
        ["minecraft:stone", "minecraft:stone"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stacks_move_between_a_player_and_a_stash_without_copies() {
    use crate::world::messages::PlayerEvent;
    use crate::world::session::plugin_inventory::{InventoryOp, TakenSink};
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    *client.session.inventory.slot_mut(36) =
        Some(ItemStack::mint(Item::Stone, 10, MintReason::Test));
    let (delivered, mut arrived) = tokio::sync::mpsc::unbounded_channel();
    let mut sink = TakenSink::new(7);
    sink.attach(move |stacks| delivered.send(stacks).map_err(|refused| refused.0));
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::TakeInto {
            item: Item::Stone,
            count: 4,
            all_or_nothing: true,
            sink,
        }]))
        .await;
    let taken = arrived.try_recv().expect("the stash got what was taken");
    assert_eq!(taken.iter().map(ItemStack::count).sum::<u8>(), 4);
    assert_eq!(
        client.session.inventory.slots()[36]
            .as_ref()
            .map(ItemStack::count),
        Some(6)
    );

    let unreachable = TakenSink::new(8);
    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::TakeInto {
            item: Item::Stone,
            count: 2,
            all_or_nothing: true,
            sink: unreachable,
        }]))
        .await;
    let held: u32 = client
        .session
        .inventory
        .slots()
        .iter()
        .flatten()
        .filter(|s| s.item() == Item::Stone)
        .map(|s| u32::from(s.count()))
        .sum();
    assert_eq!(held, 6, "a stash no one receives for hands the items back");

    client
        .session
        .on_event(PlayerEvent::PluginInventory(vec![InventoryOp::GiveStacks(
            taken,
        )]))
        .await;
    let held: u32 = client
        .session
        .inventory
        .slots()
        .iter()
        .flatten()
        .filter(|s| s.item() == Item::Stone)
        .map(|s| u32::from(s.count()))
        .sum();
    assert_eq!(held, 10, "the same items came back");
    world.shutdown().await;
}

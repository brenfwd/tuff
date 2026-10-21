//! Window click, open and close events and the inventory cancel filters,
//! end to end through a WAT plugin.

use super::*;
use crate::world::menu::MenuKind;
use crate::world::session::hooks::SessionHooks;
use tuff_item::item::ItemStack;
use tuff_plugin_wasm::inventory::{Clicks, Windows};
use tuff_plugin_wasm::wit::bindings::types::{InventoryClickKind, InventoryFilter};
use tuff_vanilla_data::Item;

/// Cancels every click on the armor slots and every shift-click, and
/// shouts the container of each inventory click, open and close event.
fn guard() -> Vec<u8> {
    let mut data = Data::new();
    let filter = |first, last, click| {
        InventoryFilter {
            container: Some("player".into()),
            first_slot: first,
            last_slot: last,
            item: None,
            click,
        }
        .to_value()
        .into_owned()
    };
    let furnace = InventoryFilter {
        container: Some("furnace".into()),
        first_slot: None,
        last_slot: None,
        item: None,
        click: None,
    }
    .to_value()
    .into_owned();
    let smoker = InventoryFilter {
        container: Some("smoker".into()),
        first_slot: None,
        last_slot: None,
        item: None,
        click: Some(InventoryClickKind::Close),
    }
    .to_value()
    .into_owned();
    let more = data.call("intercept-inventory", &[filter(Some(5), Some(8), None)])
        + &data.call("intercept-inventory", &[smoker])
        + &data.call("intercept-inventory", &[furnace])
        + &data.call(
            "intercept-inventory",
            &[filter(None, None, Some(InventoryClickKind::QuickMove))],
        )
        + &data.call(
            "subscribe",
            &[(EventKinds::INVENTORY_OPEN | EventKinds::INVENTORY_CLOSE).to_value()],
        );
    let register = register(&mut data, &[], &more);
    let body = format!(
        r#"{register}
        (func {event} (param $ev i32)
          (call $clear)
          (if (i32.ge_u (i32.load8_u (local.get $ev)) (i32.const 27))
            (then
              (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))
              {SHOUT}))
          )
        {data}"#,
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("windowguard"), &body)
}

fn click(slot: i16, button: i8, mode: i32) -> Serverbound {
    Serverbound::ContainerClick {
        window_id: 0,
        state_id: 0,
        slot,
        button,
        mode,
        changed: Vec::new(),
        carried: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clicks_are_reported_and_cancelled_by_slot_and_kind() {
    let dir = TestDir::new();
    install(dir.path(), "windowguard.wasm", &guard());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    *client.session.inventory.slot_mut(36) = Some(ItemStack::new(Item::Stick, 4));

    let armor = click(5, 0, 0);
    assert_eq!(
        Clicks.intercept(&mut client.session, &armor).await,
        Verdict::Cancel
    );
    assert_eq!(
        Clicks
            .intercept(&mut client.session, &click(36, 0, 0))
            .await,
        Verdict::Allow
    );
    assert_eq!(
        Clicks
            .intercept(&mut client.session, &click(36, 0, 1))
            .await,
        Verdict::Cancel
    );

    assert!(Windows.allows_menu_open(&client.session, MenuKind::Hopper));
    assert!(!Windows.allows_menu_open(&client.session, MenuKind::Furnace));
    assert!(!Windows.allows_menu_close(&client.session, MenuKind::Smoker));
    assert!(Windows.allows_menu_close(&client.session, MenuKind::Hopper));
    Windows.menu_opened(&client.session, MenuKind::Hopper);
    Windows.menu_closed(&client.session, MenuKind::Hopper);

    assert_eq!(
        announced(&world).await,
        ["player", "player", "player", "hopper", "hopper"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_close_filter_reopens_the_window_the_client_shut() {
    use tuff_vanilla_data::packet::play::Clientbound as Id;
    let dir = TestDir::new();
    install(dir.path(), "windowguard.wasm", &guard());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let dimension = client.session.dimension;
    let open = |kind| MenuOpen {
        dimension,
        region: region.clone(),
        kind,
        title: TextComponent::translate("container.test", []),
        source: MenuSource::Station {
            pos: BlockPos::new(0, 64, 0),
            bookcases: 0,
            data: Vec::new(),
        },
        stat: None,
    };
    let opens = |frames: &mut mpsc::UnboundedReceiver<bytes::Bytes>| {
        let mut seen = 0;
        while let Ok(frame) = frames.try_recv() {
            if framed_packet_id(&frame) == Id::OpenScreen.id() {
                seen += 1;
            }
        }
        seen
    };

    client.session.open_menu(open(MenuKind::Smoker)).await;
    let window = client.session.window_id().expect("opened");
    opens(&mut client.frames);
    client
        .session
        .handle_packet(Serverbound::ContainerClose(window))
        .await;
    assert_eq!(client.session.window_id(), Some(window), "still open");
    assert_eq!(opens(&mut client.frames), 1, "the screen is shown again");

    client.session.close_menu(true).await;
    client.session.open_menu(open(MenuKind::Stonecutter)).await;
    let window = client.session.window_id().expect("opened");
    client
        .session
        .handle_packet(Serverbound::ContainerClose(window))
        .await;
    assert_eq!(client.session.window_id(), None, "unfiltered kinds close");
    world.shutdown().await;
}

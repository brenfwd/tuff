//! The enchanting table menu: offers from the player's seed and the shelves
//! around it, and buying one with levels and lapis.

use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::menu::{MenuKind, enchantment};
use tuff_core::world::messages::{MenuOpen, MenuSource};
use tuff_core::world::player::Experience;
use tuff_core::world::session::{PlayerSession, Station};
use tuff_item::item::{ItemStack, MintReason};

async fn open_table(world: &TestWorld, session: &mut PlayerSession, bookcases: i32) -> i32 {
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    session
        .open_menu(MenuOpen {
            dimension: session.dimension,
            region,
            kind: MenuKind::Enchantment,
            title: TextComponent::translate("container.enchant", []),
            source: MenuSource::Station {
                pos: BlockPos::new(0, 64, 0),
                bookcases,
                data: Vec::new(),
            },
            stat: None,
        })
        .await;
    session.window_id().expect("the table opened")
}

fn put(session: &mut PlayerSession, item: Option<ItemStack>, lapis: u8) {
    let menu = session.menus.menu.as_mut().unwrap();
    menu.container[enchantment::ITEM] = item;
    menu.container[enchantment::LAPIS] =
        (lapis > 0).then(|| ItemStack::mint(Item::LapisLazuli, lapis, MintReason::Test));
    session.refresh_station();
}

fn costs(session: &PlayerSession) -> [i32; 3] {
    match &session.menus.menu.as_ref().unwrap().station {
        Some(Station::Enchantment { offers, .. }) => offers.costs,
        _ => panic!("an enchanting station"),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_button_charges_levels_and_lapis_and_enchants_the_item() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session.body.experience = Experience::new(30, 0.0, 0, 4242);
    let window = open_table(&world, &mut session, 15).await;
    put(
        &mut session,
        Some(ItemStack::mint(Item::DiamondSword, 1, MintReason::Test)),
        10,
    );
    let offered = costs(&session);
    assert!(offered[2] >= 30, "{offered:?}");

    session
        .handle_packet(Serverbound::ContainerButtonClick {
            window_id: window,
            button: 2,
        })
        .await;
    let container = session.menu_container().unwrap();
    let sword = container[enchantment::ITEM].as_ref().unwrap();
    assert!(sword.components.enchantments.is_some());
    assert_eq!(container[enchantment::LAPIS].as_ref().unwrap().count(), 7);
    assert_eq!(session.body.experience.level, 27);
    assert_ne!(session.body.experience.seed, 4242);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_button_does_nothing_without_enough_levels_or_lapis() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session.body.experience = Experience::new(2, 0.0, 0, 99);
    let window = open_table(&world, &mut session, 15).await;
    put(
        &mut session,
        Some(ItemStack::mint(Item::DiamondSword, 1, MintReason::Test)),
        3,
    );
    for button in [0, 1, 2] {
        session
            .handle_packet(Serverbound::ContainerButtonClick {
                window_id: window,
                button,
            })
            .await;
    }
    let container = session.menu_container().unwrap();
    assert!(
        container[enchantment::ITEM]
            .as_ref()
            .unwrap()
            .components
            .enchantments
            .is_none()
    );
    assert_eq!(session.body.experience.level, 2);
    assert_eq!(session.body.experience.seed, 99);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offers_clear_when_the_item_is_removed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    session.body.experience = Experience::new(5, 0.0, 0, 7);
    open_table(&world, &mut session, 8).await;
    put(
        &mut session,
        Some(ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test)),
        0,
    );
    assert!(costs(&session)[0] > 0);
    put(&mut session, None, 0);
    assert_eq!(costs(&session), [0; 3]);
    world.shutdown().await;
}

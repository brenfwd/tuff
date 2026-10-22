//! The beacon menu: one payment item buys the effects its pyramid allows.

use super::*;
use tuff_core::protocol::play::Serverbound;
use tuff_core::world::beacon;
use tuff_core::world::menu::MenuKind;
use tuff_core::world::messages::{MenuOpen, MenuSource};
use tuff_core::world::session::PlayerSession;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::registry::MobEffect;

async fn open_beacon(world: &TestWorld, session: &mut PlayerSession, levels: i32) {
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    session
        .open_menu(MenuOpen {
            dimension: session.dimension,
            region,
            kind: MenuKind::Beacon,
            title: TextComponent::translate("container.beacon", []),
            source: MenuSource::Station {
                pos: BlockPos::new(0, 64, 0),
                bookcases: 0,
                data: vec![levels, 0, 0],
            },
            stat: None,
        })
        .await;
    assert!(session.window_id().is_some());
}

fn pay(session: &mut PlayerSession) {
    session.menus.menu.as_mut().unwrap().container[0] =
        Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test));
}

fn id(effect: MobEffect) -> Option<i32> {
    Some(effect.id() as i32)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_valid_choice_consumes_the_payment() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    open_beacon(&world, &mut session, 4).await;
    pay(&mut session);
    session
        .handle_packet(Serverbound::SetBeacon {
            primary: id(MobEffect::Strength),
            secondary: id(MobEffect::Regeneration),
        })
        .await;
    assert!(!session.disconnected);
    assert!(session.menu_container().unwrap()[0].is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_effect_above_the_pyramid_or_without_payment_disconnects() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    open_beacon(&world, &mut session, 1).await;
    pay(&mut session);
    session
        .handle_packet(Serverbound::SetBeacon {
            primary: id(MobEffect::Strength),
            secondary: None,
        })
        .await;
    assert!(session.disconnected);
    assert!(session.menu_container().unwrap()[0].is_some());
    world.shutdown().await;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _out) = session_with_outbound(&world, 1);
    open_beacon(&world, &mut session, 4).await;
    session
        .handle_packet(Serverbound::SetBeacon {
            primary: id(MobEffect::Speed),
            secondary: None,
        })
        .await;
    assert!(session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[test]
fn the_menu_data_encodes_effects_after_zero() {
    assert_eq!(beacon::encode_effect(Some(MobEffect::Speed)), 1);
    assert_eq!(beacon::menu_data(None), vec![0, 0, 0]);
}

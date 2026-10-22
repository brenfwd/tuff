//! The anti-cheat item-use timeline through the real interceptor chain.

use super::anticheat::{Walker, walker};
use super::*;
use crate::world::inventory::Inventory;
use tuff_core::protocol::play::{Movement, Serverbound};

async fn walk(w: &mut Walker, dx: f64) {
    let [px, _, pz] = w.session.pose.position;
    w.session
        .handle_packet(Serverbound::Move(Movement {
            position: Some([px + dx, w.floor_y, pz]),
            rotation: None,
            on_ground: true,
            horizontal_collision: false,
        }))
        .await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
}

fn slow_pace(n: usize) -> f64 {
    (0..=n).fold(0.0, |h, _| h * 0.546 + 0.0196)
}

fn shield_up(w: &mut Walker) {
    w.session.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::Shield, 1, MintReason::Test)),
    );
    w.session.hold_up(true, 10);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_slow_resyncs_the_use_first_and_flags_a_repeat() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(60, 60)).await;
    shield_up(&mut w);
    for n in 0..12 {
        walk(&mut w, slow_pace(n)).await;
    }
    assert!(w.session.is_using(), "slow walking keeps the shield up");
    assert!(w.session.sync.pending_teleport.is_none());
    for n in 0..20 {
        walk(&mut w, if n % 2 == 0 { 0.25 } else { -0.25 }).await;
    }
    assert!(!w.session.is_using(), "the use was resynced");
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "desync is not a cheat"
    );
    shield_up(&mut w);
    for n in 0..20 {
        walk(&mut w, if n % 2 == 0 { 0.25 } else { -0.25 }).await;
        if w.session.sync.pending_teleport.is_some() {
            break;
        }
    }
    assert!(
        w.session.sync.pending_teleport.is_some(),
        "a repeat is set back"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_faster_than_eating_allows_is_resynced_then_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(62, 62)).await;
    w.session.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::CookedBeef, 4, MintReason::Test)),
    );
    w.session.body.food.level = 10;
    let eat = Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    };
    w.session.handle_packet(eat.clone()).await;
    assert!(w.session.is_using());
    for n in 0..12 {
        walk(&mut w, slow_pace(n)).await;
    }
    assert!(w.session.is_using(), "eating at the slowed pace is legit");
    assert!(w.session.sync.pending_teleport.is_none());
    for n in 0..15 {
        walk(&mut w, if n % 2 == 0 { 0.13 } else { -0.13 }).await;
        if !w.session.is_using() {
            break;
        }
    }
    assert!(!w.session.is_using(), "the use was resynced");
    assert!(w.session.sync.pending_teleport.is_none());
    w.session.handle_packet(eat).await;
    assert!(w.session.is_using());
    for n in 0..15 {
        walk(&mut w, if n % 2 == 0 { 0.13 } else { -0.13 }).await;
        if w.session.sync.pending_teleport.is_some() {
            break;
        }
    }
    assert!(
        w.session.sync.pending_teleport.is_some(),
        "a repeat is set back"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn packets_do_not_speed_up_eating() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(61, 61)).await;
    w.session.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::CookedBeef, 4, MintReason::Test)),
    );
    w.session.body.food.level = 10;
    w.session
        .handle_packet(Serverbound::UseItem {
            off_hand: false,
            sequence: 1,
        })
        .await;
    assert!(w.session.is_using());
    for _ in 0..200 {
        walk(&mut w, 0.0).await;
    }
    assert!(w.session.is_using(), "only server ticks finish eating");
    assert_eq!(w.session.body.food.level, 10);
    world.shutdown().await;
}

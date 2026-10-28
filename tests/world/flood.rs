//! Packet flood limits: the total cap kicks, the per-type budgets drop.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::session::PlayerSession;
use tuff_storage::admin::PermissionLevel;

fn session(
    world: &TestWorld,
) -> (
    PlayerSession,
    mpsc::Sender<Serverbound>,
    mpsc::UnboundedReceiver<bytes::Bytes>,
) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(2000);
    let (packets, packets_rx) = mpsc::channel(4096);
    let profile = GameProfile {
        id: 2000,
        name: "Flooder".to_owned(),
        properties: Vec::new(),
    };
    let session = PlayerSession::new(
        world.shared.clone(),
        2000,
        profile,
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    (session, packets, outbound_rx)
}

fn recipe() -> Serverbound {
    Serverbound::PlaceRecipe {
        window_id: 0,
        recipe: 1,
        use_max_items: false,
    }
}

fn suggestion() -> Serverbound {
    Serverbound::CommandSuggestion {
        id: 1,
        text: "/gam".to_owned(),
    }
}

fn book() -> Serverbound {
    Serverbound::EditBook {
        slot: 0,
        pages: vec!["page".to_owned()],
        title: None,
    }
}

fn sign() -> Serverbound {
    Serverbound::SignUpdate {
        position: BlockPos::new(0, 64, 0),
        front: true,
        lines: vec![String::new(); 4],
    }
}

fn admitted(session: &mut PlayerSession, make: fn() -> Serverbound, count: usize) -> usize {
    (0..count)
        .filter(|_| session.admit_budgeted(&make()))
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_each_budgeted_type_drops_the_excess_without_a_kick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _packets, _outbound) = session(&world);
    assert_eq!(admitted(&mut session, recipe, 500), 40);
    assert_eq!(admitted(&mut session, suggestion, 500), 40);
    assert_eq!(admitted(&mut session, book, 500), 4);
    assert_eq!(admitted(&mut session, sign, 500), 8);
    assert!(!session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_a_normal_rate_burst_is_untouched() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _packets, _outbound) = session(&world);
    assert_eq!(admitted(&mut session, recipe, 30), 30);
    assert_eq!(admitted(&mut session, suggestion, 30), 30);
    assert_eq!(admitted(&mut session, book, 3), 3);
    assert_eq!(admitted(&mut session, sign, 6), 6);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_budgets_are_off_when_the_rate_limit_is_zero() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _packets, _outbound) = session(&world);
    session.net.rate_limit = 0;
    assert_eq!(admitted(&mut session, sign, 500), 500);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_the_default_rate_limit_kicks_a_sustained_flood() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, packets, mut outbound) = session(&world);
    assert_eq!(session.net.rate_limit, 400);
    let player = tokio::spawn(async move {
        session.play().await;
        session.disconnected
    });
    for _ in 0..3000 {
        let _ = packets.send(Serverbound::ClientTickEnd).await;
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let _ = packets.send(Serverbound::ClientTickEnd).await;
    while let Some(bytes) = outbound.recv().await {
        let _ = bytes;
        if player.is_finished() {
            break;
        }
    }
    assert!(player.await.unwrap());
    world.shutdown().await;
}

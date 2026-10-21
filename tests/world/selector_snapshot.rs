//! Player `nbt=` selectors read a published snapshot instead of asking the
//! other session, so two players testing each other never wait.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::PlayerSnapshot;
use crate::world::session::{PlayerSession, SELECTOR_IDLE_PERIOD, SELECTOR_INTEREST_TICKS};
use bytes::Bytes;
use tuff_storage::admin::PermissionLevel;

struct Peer {
    session: PlayerSession,
    handle: PlayerHandle,
    _frames: mpsc::UnboundedReceiver<Bytes>,
}

fn peer(shared: &Arc<Shared>, id: i32, name: &str) -> Peer {
    let (outbound, frames) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_packets, packets) = mpsc::channel(8);
    let session = PlayerSession::new(
        shared.clone(),
        id,
        GameProfile {
            id: id as u128,
            name: name.to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle.clone(),
        events,
        packets,
        PermissionLevel::Owners,
    );
    Peer {
        session,
        handle,
        _frames: frames,
    }
}

async fn pair() -> (Peer, Peer) {
    let (coordinator, mut requests) = mpsc::channel(64);
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    shared.scoreboard =
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0;
    let shared = Arc::new(shared);
    let mut a = peer(&shared, 1000, "Alpha");
    let mut b = peer(&shared, 1001, "Bravo");
    let (states, mut received) = mpsc::unbounded_channel();
    let handles = [a.handle.clone(), b.handle.clone()];
    tokio::spawn(async move {
        let mut known: Vec<Option<crate::world::messages::PlayerState>> = vec![None, None];
        while let Some(message) = requests.recv().await {
            match message {
                CoordinatorMessage::PlayerState { player, state } => {
                    known[(player - 1000) as usize] = Some(state);
                    let _ = states.send(());
                }
                CoordinatorMessage::Players { reply } => {
                    let names = ["Alpha", "Bravo"];
                    let snapshots = known
                        .iter()
                        .enumerate()
                        .filter_map(|(i, state)| {
                            Some(PlayerSnapshot {
                                id: 1000 + i as i32,
                                uuid: 1000 + i as u128,
                                name: names[i].to_owned(),
                                handle: handles[i].clone(),
                                address: std::net::Ipv4Addr::LOCALHOST.into(),
                                permission: PermissionLevel::Owners,
                                state: state.clone()?,
                                fake: false,
                            })
                        })
                        .collect();
                    let _ = reply.send(snapshots);
                }
                CoordinatorMessage::Difficulty { difficulty, reply } => {
                    let _ = reply.send(match difficulty {
                        Some(set) => crate::world::messages::DifficultyOutcome::Set(set),
                        None => crate::world::messages::DifficultyOutcome::Query(
                            crate::world::player::Difficulty::Normal,
                        ),
                    });
                }
                _ => {}
            }
        }
    });
    a.session.report_state().await;
    b.session.report_state().await;
    received.recv().await;
    received.recv().await;
    a.session.run_tick().await;
    b.session.run_tick().await;
    (a, b)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_players_testing_each_others_nbt_do_not_wait() {
    let (mut a, mut b) = pair().await;
    let test = |name: &str| {
        format!("execute if entity @a[name={name},nbt={{XpLevel:0}}] run xp add @s 5 levels")
    };
    let (to_bravo, to_alpha) = (test("Bravo"), test("Alpha"));
    tokio::time::timeout(Duration::from_millis(1500), async {
        tokio::join!(
            a.session.run_command(&to_bravo),
            b.session.run_command(&to_alpha),
        )
    })
    .await
    .expect("neither command waits on the other session");
    assert_eq!(a.session.experience().level, 5);
    assert_eq!(b.session.experience().level, 5);
}

fn level(peer: &Peer) -> Option<i32> {
    peer.handle.selector_data().unwrap().get_int("XpLevel")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_directive_commands_report_their_result_to_execute_store() {
    let (mut a, _b) = pair().await;
    a.session
        .run_command("scoreboard objectives add out dummy")
        .await;
    for (command, expected) in [
        (
            "execute store result score #r out run xp set @s 5 levels",
            1,
        ),
        (
            "execute store result score #r out run xp query @s levels",
            5,
        ),
        (
            "execute store result score #r out run xp add @s 3 levels",
            1,
        ),
        (
            "execute store result score #r out run xp query @s levels",
            8,
        ),
        ("execute store result score #r out run difficulty normal", 0),
        ("execute store result score #r out run difficulty", 2),
    ] {
        a.session.run_command(command).await;
        let stored = a
            .session
            .shared
            .scoreboard
            .read(|board| board.score("#r", "out").map(|s| s.value))
            .await
            .flatten();
        assert_eq!(stored, Some(expected), "{command}");
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_makes_the_form_publish_every_tick() {
    let (mut a, _b) = pair().await;
    assert_eq!(level(&a), Some(0));
    a.session.run_command("xp add @s 3 levels").await;
    a.session.run_tick().await;
    assert_eq!(level(&a), Some(3));
    a.session.run_command("xp add @s 2 levels").await;
    a.session.run_tick().await;
    assert_eq!(level(&a), Some(5));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unread_form_refreshes_within_the_idle_period() {
    let (mut a, _b) = pair().await;
    a.session.run_command("xp add @s 3 levels").await;
    for _ in 0..SELECTOR_IDLE_PERIOD {
        a.session.run_tick().await;
    }
    assert_eq!(level(&a), Some(3));
    const { assert!(SELECTOR_INTEREST_TICKS > SELECTOR_IDLE_PERIOD) };
}

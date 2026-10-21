//! `/tag` and `/teammsg` on player sessions: tags live in the player's
//! record and reach the coordinator's snapshot; team messages go to every
//! online member of the sender's team.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{Directive, PlayerSnapshot, PlayerState};
use crate::world::scoreboard::Team;
use crate::world::session::PlayerSession;
use bytes::Bytes;
use tuff_storage::admin::PermissionLevel;

const NAMES: [&str; 3] = ["Alpha", "Bravo", "Charlie"];

struct Peer {
    session: PlayerSession,
    handle: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    _frames: mpsc::UnboundedReceiver<Bytes>,
}

fn peer(shared: &Arc<Shared>, index: usize) -> Peer {
    let id = 1000 + index as i32;
    let (outbound, frames) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_unused, session_events) = PlayerHandle::new(id);
    let (_packets, packets) = mpsc::channel(8);
    let session = PlayerSession::new(
        shared.clone(),
        id,
        GameProfile {
            id: id as u128,
            name: NAMES[index].to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle.clone(),
        session_events,
        packets,
        PermissionLevel::Owners,
    );
    Peer {
        session,
        handle,
        events,
        _frames: frames,
    }
}

async fn trio() -> (Vec<Peer>, mpsc::UnboundedReceiver<(i32, PlayerState)>) {
    let (coordinator, mut requests) = mpsc::channel(64);
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    shared.scoreboard =
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0;
    let shared = Arc::new(shared);
    let mut peers: Vec<Peer> = (0..3).map(|i| peer(&shared, i)).collect();
    let handles: Vec<PlayerHandle> = peers.iter().map(|p| p.handle.clone()).collect();
    let (states, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut known: Vec<Option<PlayerState>> = vec![None, None, None];
        while let Some(message) = requests.recv().await {
            match message {
                CoordinatorMessage::PlayerState { player, state } => {
                    known[(player - 1000) as usize] = Some(state.clone());
                    let _ = states.send((player, state));
                }
                CoordinatorMessage::Players { reply } => {
                    let snapshots = known
                        .iter()
                        .enumerate()
                        .filter_map(|(i, state)| {
                            Some(PlayerSnapshot {
                                id: 1000 + i as i32,
                                uuid: 1000 + i as u128,
                                name: NAMES[i].to_owned(),
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
                _ => {}
            }
        }
    });
    for peer in &mut peers {
        peer.session.report_state().await;
    }
    (peers, received)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tags_on_a_player_are_saved_and_published() {
    let (mut peers, mut states) = trio().await;
    while states.try_recv().is_ok() {}
    let alpha = &mut peers[0].session;
    alpha.run_command("tag @s add boss").await;
    let (player, state) = loop {
        let (player, state) = states.recv().await.unwrap();
        if player == 1000 && !state.tags.is_empty() {
            break (player, state);
        }
    };
    assert_eq!(player, 1000);
    assert_eq!(state.tags, ["boss"]);
    let saved = peers[0].session.selector_nbt();
    assert_eq!(
        saved
            .get_list("Tags")
            .map(|tags| tags.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>()),
        Some(vec!["boss"])
    );
    peers[0].session.run_command("tag @s add boss").await;
    peers[0].session.run_command("tag @s remove boss").await;
    let state = loop {
        let (player, state) = states.recv().await.unwrap();
        if player == 1000 && state.tags.is_empty() {
            break state;
        }
    };
    assert!(state.tags.is_empty());
    assert!(peers[0].session.selector_nbt().get("Tags").is_none());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn team_messages_reach_every_member_online() {
    let (mut peers, _states) = trio().await;
    let scoreboard = peers[0].session.shared().scoreboard.clone();
    scoreboard
        .update(|board| {
            board.add_team(Team::new("red"));
            board.join_team("Alpha", "red");
            board.join_team("Bravo", "red");
        })
        .await
        .unwrap();

    peers[0].session.run_command("teammsg hello team").await;
    let mut got = Vec::new();
    while let Ok(event) = peers[1].events.try_recv() {
        if let PlayerEvent::Directive {
            directive: Directive::PrivateMessage(message),
            ..
        } = event
        {
            got.push(message);
        }
    }
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].translate.as_deref(), Some("chat.type.team.text"));
    assert_eq!(got[0].with[1].plain(), "Alpha");
    assert_eq!(got[0].with[2].plain(), "hello team");
    assert!(
        peers[2].events.try_recv().is_err(),
        "a player outside the team hears nothing"
    );

    peers[2].session.run_command("teammsg anyone?").await;
    assert!(peers[0].events.try_recv().is_err());
    assert!(peers[1].events.try_recv().is_err());
}

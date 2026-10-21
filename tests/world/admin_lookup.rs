//! Admin commands that name a never-seen player in online mode wait for
//! the profile API off the session that ran them, unless something needs
//! their result, which then reflects the real outcome.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{ListOp, ListOutcome};
use crate::world::session::PlayerSession;
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;

/// An operator's session in online mode whose coordinator is the
/// returned receiver.
fn admin_session() -> (PlayerSession, mpsc::Receiver<CoordinatorMessage>) {
    let (coordinator, requests) = mpsc::channel(16);
    let mut shared = bare_shared();
    shared.online_mode = true;
    shared.coordinator = coordinator;
    shared.scoreboard =
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0;
    let (outbound, _outbound) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1000);
    let (_packets, packets) = mpsc::channel(8);
    let session = PlayerSession::new(
        Arc::new(shared),
        1000,
        GameProfile {
            id: 1000,
            name: "Admin".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        PermissionLevel::Owners,
    );
    (session, requests)
}

/// Runs `input` on `session` as a command whose result is consumed, the
/// way `execute store` runs one.
async fn result_of(session: &mut PlayerSession, input: &str) -> Result<i32, TextComponent> {
    let source = session.command_source();
    let registry = session.shared().commands.clone();
    let parsed = registry
        .roots()
        .parse(input, source.permission, source.parse_context());
    assert!(parsed.error(input).is_none(), "{input} parses");
    let executor = registry.executor(parsed.nodes[0].node.name).unwrap();
    let mut ctx = commands::Context {
        actor: commands::Actor::Player(session),
        source,
        nodes: &parsed.nodes,
        rest: &[],
        input,
        needs_result: true,
        frame: None,
    };
    executor
        .execute(&mut ctx)
        .await
        .map_err(|error| *error.message)
}

/// Answers the coordinator like a server where `known` is the only name
/// the profile API finds and `opped` is already an operator.
fn answer(
    mut requests: mpsc::Receiver<CoordinatorMessage>,
    known: NameAndId,
    opped: u128,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(message) = requests.recv().await {
            match message {
                CoordinatorMessage::Players { reply } => {
                    let _ = reply.send(Vec::new());
                }
                CoordinatorMessage::LookupProfile { name, reply } => {
                    let found = name
                        .eq_ignore_ascii_case(&known.name)
                        .then(|| known.clone());
                    let _ = reply.send(found);
                }
                CoordinatorMessage::Lists {
                    op: ListOp::Op(users),
                    reply,
                    ..
                } => {
                    let changed = users.into_iter().filter(|u| u.uuid != opped).collect();
                    let _ = reply.send(ListOutcome::Opped(changed));
                }
                _ => {}
            }
        }
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_result_reflects_the_lookup_when_it_is_needed() {
    let (mut session, requests) = admin_session();
    let known = NameAndId {
        uuid: 7,
        name: "Known".to_owned(),
    };
    let _coordinator = answer(requests, known, 8);

    assert_eq!(
        result_of(&mut session, "op Nobody").await,
        Err(TextComponent::translate("argument.player.unknown", [])),
        "an unknown name fails instead of counting one"
    );
    assert_eq!(result_of(&mut session, "op Known").await, Ok(1));

    let (mut session, requests) = admin_session();
    let already = NameAndId {
        uuid: 8,
        name: "Already".to_owned(),
    };
    let _second = answer(requests, already, 8);
    assert_eq!(
        result_of(&mut session, "op Already").await,
        Err(TextComponent::translate("commands.op.failed", [])),
        "nobody changed is the game's failure"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_of_an_unknown_name_does_not_block_the_session() {
    let (mut session, mut requests) = admin_session();

    let pending = tokio::spawn(async move {
        let mut lookup = None;
        while let Some(message) = requests.recv().await {
            match message {
                CoordinatorMessage::Players { reply } => {
                    let _ = reply.send(Vec::new());
                }
                CoordinatorMessage::LookupProfile { name, reply } => {
                    lookup = Some((name, reply));
                    break;
                }
                _ => {}
            }
        }
        (lookup.expect("the name is looked up"), requests)
    });
    tokio::time::timeout(Duration::from_secs(5), session.run_command("op Slow"))
        .await
        .expect("the command returns before the lookup answers");
    let ((name, reply), mut requests) = pending.await.unwrap();
    assert_eq!(name, "Slow");

    let found = NameAndId {
        uuid: 7,
        name: "Slow".to_owned(),
    };
    reply.send(Some(found.clone())).unwrap();
    let (users, reply) = loop {
        match requests.recv().await.expect("the op reaches the lists") {
            CoordinatorMessage::Lists {
                op: ListOp::Op(users),
                reply,
                ..
            } => break (users, reply),
            _ => continue,
        }
    };
    assert_eq!(users, vec![found.clone()]);
    reply.send(ListOutcome::Opped(users)).unwrap();
    loop {
        if let CoordinatorMessage::AdminBroadcast { .. } =
            requests.recv().await.expect("the success is broadcast")
        {
            break;
        }
    }
    drop(session);
}

/// `#holder`'s score in `objective`, once every queued change landed.
async fn score(session: &PlayerSession, holder: &str, objective: &str) -> Option<i32> {
    let (holder, objective) = (holder.to_owned(), objective.to_owned());
    session
        .shared()
        .scoreboard
        .read(move |board| board.score(&holder, &objective).map(|s| s.value))
        .await
        .flatten()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_store_waits_for_a_lookup_and_stores_its_outcome() {
    let (mut session, requests) = admin_session();
    let known = NameAndId {
        uuid: 7,
        name: "Known".to_owned(),
    };
    let _coordinator = answer(requests, known, 8);

    session
        .run_command("scoreboard objectives add out dummy")
        .await;
    session
        .run_command("execute store success score #unknown out run op Nobody")
        .await;
    session
        .run_command("execute store result score #unknown_result out run op Nobody")
        .await;
    session
        .run_command("execute store result score #known out run op Known")
        .await;

    assert_eq!(score(&session, "#unknown", "out").await, Some(0));
    assert_eq!(score(&session, "#unknown_result", "out").await, Some(0));
    assert_eq!(score(&session, "#known", "out").await, Some(1));
}

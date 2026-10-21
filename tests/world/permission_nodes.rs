//! Permission nodes deciding which `/tuff` subcommands and vanilla
//! commands a player's tree lists and which of them run.

use super::*;
use crate::auth::GameProfile;
use crate::permission::{Change, Permissions, Subject};
use crate::protocol::configuration::ClientInformation;
use crate::world::commands::{Actor, run_counted};
use crate::world::session::PlayerSession;
use tuff_storage::admin::PermissionLevel;

const UUID: u128 = 1000;

fn player(level: PermissionLevel) -> (PlayerSession, Permissions) {
    let (coordinator, mut requests) = mpsc::channel(16);
    tokio::spawn(async move { while requests.recv().await.is_some() {} });
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    let nodes = Arc::new(shared.permissions.view().nodes().clone());
    let permissions = Permissions::start(nodes);
    shared.permissions = permissions.clone();
    let (outbound, _outbound) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1000);
    let (_packets, packets) = mpsc::channel(8);
    let session = PlayerSession::new(
        Arc::new(shared),
        1000,
        GameProfile {
            id: UUID,
            name: "Alex".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        level,
    );
    (session, permissions)
}

/// The `/tuff` subcommands the session's command tree lists, or `None`
/// without `/tuff`.
fn tuff_children(session: &PlayerSession) -> Option<Vec<&'static str>> {
    let access = session
        .shared()
        .permissions
        .access(session.permission_subject());
    let (nodes, root) = session.shared().commands.roots().tree(&access);
    let tuff = nodes[root as usize]
        .children
        .iter()
        .map(|c| &nodes[*c as usize])
        .find(|n| n.name == Some("tuff"))?;
    Some(
        tuff.children
            .iter()
            .filter_map(|c| nodes[*c as usize].name)
            .collect(),
    )
}

async fn runs(session: &mut PlayerSession, line: &str) -> bool {
    let source = session.command_source();
    run_counted(Actor::Player(session), source, line).await == 1
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_sees_and_runs_only_the_subcommands_granted() {
    let (mut session, permissions) = player(PermissionLevel::All);
    assert_eq!(tuff_children(&session), None);
    assert!(!runs(&mut session, "tuff config").await);

    permissions
        .set(UUID, "Alex", "tuff.command.config", Some(true))
        .await
        .unwrap();
    assert_eq!(tuff_children(&session), Some(vec!["config"]));
    assert!(runs(&mut session, "tuff config").await);
    assert!(!runs(&mut session, "tuff perm nodes").await);

    permissions
        .set(UUID, "Alex", "tuff.web.access", Some(true))
        .await
        .unwrap();
    let access = permissions.access(Subject::Player {
        uuid: UUID,
        level: PermissionLevel::All,
    });
    let (nodes, _) = session.shared().commands.roots().tree(&access);
    let web: Vec<&str> = nodes
        .iter()
        .find(|n| n.name == Some("web"))
        .unwrap()
        .children
        .iter()
        .filter_map(|c| nodes[*c as usize].name)
        .collect();
    assert_eq!(web, ["login", "logout"]);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_hold_every_subcommand_until_denied() {
    let (mut session, permissions) = player(PermissionLevel::Owners);
    assert_eq!(
        tuff_children(&session),
        Some(vec![
            "backup", "chunk", "config", "execute", "icon", "perm", "restart", "timings",
            "tracing", "web"
        ])
    );
    permissions
        .set(UUID, "Alex", "tuff.command.config", Some(false))
        .await
        .unwrap();
    assert_eq!(
        tuff_children(&session),
        Some(vec![
            "backup", "chunk", "execute", "icon", "perm", "restart", "timings", "tracing", "web"
        ])
    );
    assert!(!runs(&mut session, "tuff config").await);
    assert!(runs(&mut session, "tuff perm nodes").await);
    assert!(!runs(&mut session, "tuff icon set no-such-icon.png").await);

    permissions
        .set(UUID, "Alex", "tuff.*", Some(false))
        .await
        .unwrap();
    assert_eq!(tuff_children(&session), None);
    assert!(!runs(&mut session, "tuff perm nodes").await);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_tuff_subcommand_node_is_registered() {
    let (session, _) = player(PermissionLevel::Owners);
    let view = session.shared().permissions.view();
    for node in [
        "tuff.command.chunk",
        "tuff.command.config",
        "tuff.command.execute",
        "tuff.command.icon",
        "tuff.command.tracing",
        "tuff.command.perm",
        "tuff.command.restart",
        "tuff.command.timings",
        "tuff.web.access",
        "tuff.web.logout-all",
    ] {
        assert!(view.nodes().contains(node), "{node}");
    }
    let tuff = session.shared().commands.roots().command("tuff").unwrap();
    fn walk(node: &crate::command::CommandNode, view: &crate::permission::View) {
        if let Some(json) = node.permissions {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            if let Some(id) = value["node"].as_str() {
                assert!(view.nodes().contains(id), "{id} is not registered");
            }
        }
        for child in node.children {
            walk(child, view);
        }
    }
    walk(tuff.node, &view);
}

/// The top-level commands the session's command tree lists.
fn root_commands(session: &PlayerSession) -> Vec<&'static str> {
    let access = session
        .shared()
        .permissions
        .access(session.permission_subject());
    let (nodes, root) = session.shared().commands.roots().tree(&access);
    nodes[root as usize]
        .children
        .iter()
        .filter_map(|c| nodes[*c as usize].name)
        .collect()
}

/// Whether `line` parses for the session, as it must before it runs.
fn parses(session: &PlayerSession, line: &str) -> bool {
    let access = session
        .shared()
        .permissions
        .access(session.permission_subject());
    let source = session.command_source();
    let parsed = session
        .shared()
        .commands
        .roots()
        .parse(line, &access, source.parse_context());
    parsed.error(line).is_none()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_granted_player_uses_one_vanilla_command_without_being_an_operator() {
    let (session, permissions) = player(PermissionLevel::All);
    let view = permissions.view();
    for node in ["minecraft.command.gamemode", "minecraft.command.teleport"] {
        assert!(view.nodes().contains(node), "{node}");
    }
    assert!(!view.nodes().contains("minecraft.command.tp"));
    assert!(!root_commands(&session).contains(&"gamemode"));
    assert!(!parses(&session, "gamemode creative"));

    permissions
        .set(UUID, "Alex", "minecraft.command.gamemode", Some(true))
        .await
        .unwrap();
    let listed = root_commands(&session);
    assert!(listed.contains(&"gamemode"), "{listed:?}");
    assert!(!listed.contains(&"time"), "{listed:?}");
    assert!(parses(&session, "gamemode creative"));
    assert!(!parses(&session, "time set day"));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operators_lose_a_vanilla_command_denied_to_them_or_their_role() {
    let (session, permissions) = player(PermissionLevel::Owners);
    assert!(root_commands(&session).contains(&"gamemode"));
    assert!(parses(&session, "gamemode creative"));

    for change in [
        Change::CreateRole {
            role: "guests".into(),
            priority: 0,
        },
        Change::Role {
            role: "guests".into(),
            pattern: "minecraft.command.*".into(),
            held: Some(false),
        },
        Change::Member {
            uuid: UUID,
            name: "Alex".into(),
            role: "guests".into(),
            member: true,
        },
    ] {
        assert!(permissions.apply(change).await.unwrap().changed);
    }
    let listed = root_commands(&session);
    assert!(!listed.contains(&"gamemode"), "{listed:?}");
    assert!(listed.contains(&"tuff"), "{listed:?}");
    assert!(!parses(&session, "gamemode survival"));

    permissions
        .set(UUID, "Alex", "minecraft.command.gamemode", Some(true))
        .await
        .unwrap();
    assert!(parses(&session, "gamemode survival"));
    assert!(!parses(&session, "time set day"));
}

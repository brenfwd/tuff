use crate::common;
use crate::options;
use crate::servers;
use crate::watchers;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use common::TempDir;
use options::Mode;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use watchers::Watch;

fn stress(args: &[&str]) -> anyhow::Result<options::Options> {
    let args = std::iter::once("--bench").chain(args.iter().copied());
    match options::parse_from(args.map(String::from))? {
        Mode::Run(options) => Ok(*options),
        _ => panic!("not a run"),
    }
}

#[tuff_core::watchdog]
#[test]
fn watch_defaults_to_an_ephemeral_loopback_port_with_no_ops() {
    let options = stress(&["--players", "2"]).unwrap();
    assert_eq!(options.watch, Watch::default());
    assert_eq!(options.watch.bind(), "127.0.0.1:0");
    assert!(options.watch.ops.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn stress_simulation_distance_follows_view_distance_unless_given() {
    let options = stress(&["--view_distance", "12"]).unwrap();
    assert_eq!(options.simulation_distance, 12);
    let options = stress(&["--view_distance", "10", "--simulation_distance", "32"]).unwrap();
    assert_eq!(
        (options.view_distance, options.simulation_distance),
        (10, 32)
    );
}

#[tuff_core::watchdog]
#[test]
fn stress_parses_ops_host_and_port_beside_other_flags() {
    let options = stress(&[
        "--ops",
        "Alice, bob,alice,,Carol_9",
        "--seed",
        "7",
        "--host",
        "::",
        "--port",
        "25570",
    ])
    .unwrap();
    assert_eq!(options.watch.ops, ["Alice", "bob", "Carol_9"]);
    assert_eq!(options.watch.host, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    assert_eq!(options.watch.port, 25570);
    assert_eq!(options.watch.bind(), "[::]:25570");
    assert_eq!(options.seed, 7);
}

#[tuff_core::watchdog]
#[test]
fn bad_watch_values_are_rejected() {
    for args in [
        &["--ops", ""][..],
        &["--ops", ",, ,"],
        &["--ops", "has space"],
        &["--ops", "Seventeen_chars_x"],
        &["--ops", "dash-name"],
        &["--port", "65536"],
        &["--port", "-1"],
        &["--host", "localhost"],
        &["--ops"],
    ] {
        assert!(stress(args).is_err(), "{args:?} parsed");
    }
}

#[tuff_core::watchdog]
#[test]
fn flag_ignores_other_flags_without_taking_a_value() {
    let mut watch = Watch::default();
    let taken = watch
        .flag("--seed", || panic!("took a value for --seed"))
        .unwrap();
    assert!(!taken);
    assert!(watch.flag("--port", || Ok("1234".into())).unwrap());
    assert_eq!(watch.port, 1234);
}

#[tuff_core::watchdog]
#[test]
fn watchers_may_not_take_bot_names() {
    let watch = Watch {
        ops: vec!["Human".into(), "fly0".into()],
        ..Watch::default()
    };
    assert!(watch.check_bots(["Anchor", "Fly0"]).is_err());
    assert!(watch.check_bots(["Anchor", "Fly1"]).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn join_hint_names_the_address_and_ops() {
    let watch = Watch {
        ops: vec!["Alice".into(), "Bob".into()],
        ..Watch::default()
    };
    let local = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 4000);
    assert_eq!(
        watch.join_hint(local),
        "join with a client at 127.0.0.1:4000 (operators: Alice, Bob)"
    );
    let any = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 4000);
    assert_eq!(
        Watch::default().join_hint(any),
        "join with a client at this machine's address, port 4000"
    );
    assert_eq!(watchers::reachable(any), local);
    let any6 = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 4000);
    assert_eq!(
        watchers::reachable(any6),
        SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 4000)
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ops_apply_as_offline_operators_and_watchers_are_seen_online() {
    let instance = TempDir::new();
    let watch = Watch {
        ops: watchers::names("Watcher,Absent").unwrap(),
        ..Watch::default()
    };
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec![watch.bind()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = watchers::reachable(handle.local_addrs().await.unwrap()[0]);

    watchers::apply_ops(&handle, &watch.ops).await.unwrap();
    watchers::apply_ops(&handle, &watch.ops).await.unwrap();
    let ops: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(instance.path().join("ops.json")).unwrap())
            .unwrap();
    let ops = ops.as_array().unwrap();
    assert_eq!(ops.len(), 2);
    for (entry, name) in ops.iter().zip(["Watcher", "Absent"]) {
        assert_eq!(entry["name"], name);
        assert_eq!(entry["level"], 4);
        assert_eq!(
            tuff::auth::parse_uuid(entry["uuid"].as_str().unwrap()),
            Some(tuff::auth::offline_uuid(name))
        );
    }
    assert!(
        watchers::online(&handle, &watch.ops)
            .await
            .unwrap()
            .is_empty()
    );

    let human = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Watcher")
        },
    )
    .await
    .unwrap();
    let mut seen = Vec::new();
    for _ in 0..100 {
        seen = watchers::online(&handle, &watch.ops).await.unwrap();
        if !seen.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(seen, ["Watcher"]);

    drop(human);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

fn op_uuids(instance: &TempDir) -> Vec<(String, u128)> {
    let Ok(text) = std::fs::read_to_string(instance.path().join("ops.json")) else {
        return Vec::new();
    };
    let ops: serde_json::Value = serde_json::from_str(&text).unwrap();
    ops.as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            assert_eq!(entry["level"], 4);
            (
                entry["name"].as_str().unwrap().to_owned(),
                tuff::auth::parse_uuid(entry["uuid"].as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}

async fn join(addr: SocketAddr, name: &str) -> Bot {
    Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ops_match_players_joining_with_another_case() {
    let instance = TempDir::new();
    let watch = Watch {
        ops: watchers::names("alice,Bob").unwrap(),
        ..Watch::default()
    };
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec![watch.bind()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = watchers::reachable(handle.local_addrs().await.unwrap()[0]);
    watchers::apply_ops(&handle, &watch.ops).await.unwrap();

    let alice = join(addr, "Alice").await;
    let bystander = join(addr, "Carol").await;
    let mut opped = Vec::new();
    let mut fresh = Vec::new();
    for _ in 0..100 {
        fresh = watchers::op_joined(&handle, &watch.ops, &mut opped)
            .await
            .unwrap();
        if !fresh.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(fresh, ["Alice"]);
    assert!(
        watchers::op_joined(&handle, &watch.ops, &mut opped)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(op_uuids(&instance).contains(&("Alice".into(), tuff::auth::offline_uuid("Alice"))));
    assert_eq!(
        watchers::online(&handle, &watch.ops).await.unwrap(),
        ["Alice"]
    );
    drop(alice);

    let keeper = watchers::keep_ops(handle.clone(), watch.ops.clone());
    let shouting = join(addr, "ALICE").await;
    let bob = join(addr, "bOB").await;
    let wanted = [
        ("ALICE".to_owned(), tuff::auth::offline_uuid("ALICE")),
        ("bOB".to_owned(), tuff::auth::offline_uuid("bOB")),
    ];
    let mut ops = Vec::new();
    for _ in 0..100 {
        ops = op_uuids(&instance);
        if wanted.iter().all(|op| ops.contains(op)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(wanted.iter().all(|op| ops.contains(op)), "{ops:?}");
    assert!(!ops.iter().any(|(name, _)| name == "Carol"));
    let mut online = Vec::new();
    for _ in 0..100 {
        online = watchers::online(&handle, &watch.ops).await.unwrap();
        online.sort();
        if online == ["ALICE", "bOB"] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(online, ["ALICE", "bOB"]);
    keeper.abort();

    drop((shouting, bob, bystander));
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[test]
fn stress_takes_the_server_and_vanilla_flags() {
    let options = stress(&[
        "--server",
        "vanilla",
        "--accept-mojang-eula",
        "--players",
        "2",
        "--java_opts",
        "-Xmx2G",
    ])
    .unwrap();
    assert_eq!(options.server, servers::Choice::One(servers::Kind::Vanilla));
    assert!(options.vanilla.accept_eula);
    assert_eq!(options.vanilla.java_opts, ["-Xmx2G"]);
    assert_eq!(options.players, 2);
    let default = stress(&[]).unwrap();
    assert_eq!(default.server, servers::Choice::One(servers::Kind::Tuff));
    assert!(!default.vanilla.accept_eula);
    assert!(stress(&["--server", "paper"]).is_err());
}

#[tuff_core::watchdog]
#[test]
fn stress_lingers_only_when_asked() {
    use std::time::Duration;
    assert_eq!(stress(&[]).unwrap().linger, Duration::ZERO);
    let options = stress(&["--linger", "45s"]).unwrap();
    assert_eq!(options.linger, Duration::from_secs(45));
    assert_eq!(options.to_json()["linger_s"], 45.0);
}

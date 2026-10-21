use crate::common;

use std::time::Duration;

use common::TempDir;
use tuff_core::Server;
use tuff_core::restart;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn said(handle: &tuff_core::ServerHandle, line: &str) -> String {
    let lines = handle.run_command(line).await.unwrap();
    lines
        .iter()
        .map(|l| l.to_value().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[tuff_core::watchdog]
#[tokio::test]
async fn restart_cancel_then_stops_the_server_and_flags_a_restart() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(dir.path().to_path_buf())
        .serve(false)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());

    handle.run_command("tuff restart 600").await.unwrap();
    assert!(!restart::requested());
    assert!(
        said(&handle, "tuff restart 5")
            .await
            .contains("already scheduled")
    );
    assert!(
        said(&handle, "tuff restart cancel")
            .await
            .contains("cancelled")
    );
    assert!(
        said(&handle, "tuff restart cancel")
            .await
            .contains("No restart")
    );
    assert!(!restart::requested());

    handle.run_command("tuff restart").await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("server stops")
        .unwrap()
        .unwrap();
    assert!(restart::requested());
    assert_eq!(restart::EXIT_CODE, 75);
    let plan = restart::Relaunch {
        exe: "/bin/tuff".into(),
        args: vec!["--x".into()],
    };
    assert_eq!(
        restart::action(|| Ok(plan.clone())),
        restart::Action::Relaunch(plan)
    );
    assert_eq!(
        restart::action(|| Err(std::io::Error::other("no exe"))),
        restart::Action::Exit(75)
    );
    restart::mark(restart::Mode::Exit);
    assert_eq!(
        restart::action(|| unreachable!("exit mode never relaunches")),
        restart::Action::Exit(75)
    );
    restart::clear();
    assert_eq!(
        restart::action(restart::Relaunch::current),
        restart::Action::Nothing
    );
}

struct Running {
    handle: tuff_core::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

async fn start(dir: &TempDir, overrides: Vec<(String, String)>) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(dir.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .config_overrides(overrides)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    Running { handle, task, addr }
}

async fn join(server: &Running, name: &str) -> tuff_bot::Bot {
    let bot = tuff_bot::Bot::connect(
        server.addr,
        tuff_bot::BotConfig {
            view_distance: 2,
            ..tuff_bot::BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

fn chats(bot: &tuff_bot::Bot) -> u64 {
    use tuff_vanilla_data::packet::play::Clientbound as Id;
    bot.state()
        .received
        .get(&Id::SystemChat.id())
        .copied()
        .unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn countdown_and_cancel_reach_every_player_only_when_broadcast_is_on() {
    let _serial = SERIAL.lock().await;
    for on in [true, false] {
        let dir = TempDir::new();
        let server = start(
            &dir,
            vec![("tuff.restart.broadcast_countdown".into(), on.to_string())],
        )
        .await;
        let bot = join(&server, "Watcher").await;
        let before = chats(&bot);
        server.handle.run_command("tuff restart 600").await.unwrap();
        server
            .handle
            .run_command("tuff restart cancel")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let got = chats(&bot) - before;
        assert_eq!(got, if on { 2 } else { 0 });
        restart::clear();
        server.handle.shutdown().await.unwrap();
        server.task.await.unwrap().unwrap();
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn restart_disconnects_players_with_the_restart_text_only_when_enabled() {
    let _serial = SERIAL.lock().await;
    for on in [true, false] {
        let dir = TempDir::new();
        let server = start(
            &dir,
            vec![("tuff.restart.disconnect_message".into(), on.to_string())],
        )
        .await;
        let bot = join(&server, "Leaver").await;
        server.handle.run_command("tuff restart").await.unwrap();
        bot.wait_for(|s| s.disconnected.is_some()).await.ok();
        let reason = bot.state().disconnected.clone().unwrap_or_default();
        tokio::time::timeout(Duration::from_secs(60), server.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        restart::clear();
        assert_eq!(reason.contains("restarting"), on, "{reason}");
    }
}

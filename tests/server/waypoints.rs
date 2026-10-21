use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::world::waypoints::{Operation, TrackedWaypoint};

struct Running {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

async fn start(instance: &TempDir) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    Running { handle, task, addr }
}

async fn stop(server: Running) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn connect(server: &Running, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

async fn waypoints_until(
    bot: &Bot,
    done: impl Fn(&[TrackedWaypoint]) -> bool,
) -> Vec<TrackedWaypoint> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bot.wait_for(|state| done(&state.waypoints)),
    )
    .await
    .expect("the locator bar packets arrive")
    .unwrap();
    bot.with_state(|state| state.waypoints.clone())
}

fn last(packets: &[TrackedWaypoint]) -> &TrackedWaypoint {
    packets.last().unwrap()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn players_track_each_other_and_icons_rule_and_leaving_update_the_bar() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let alice = connect(&server, "Alice").await;
    let bob = connect(&server, "Bob").await;

    let seen = waypoints_until(&alice, |p| !p.is_empty()).await;
    assert_eq!(seen[0].operation, Operation::Track);
    let bob_uuid = seen[0].uuid;
    assert_eq!(seen[0].icon.color, None);
    waypoints_until(&bob, |p| !p.is_empty()).await;

    server
        .handle
        .run_command("waypoint modify Bob color hex ff0000")
        .await
        .unwrap();
    let seen = waypoints_until(&alice, |p| p.len() >= 2).await;
    assert_eq!(last(&seen).operation, Operation::Update);
    assert_eq!(last(&seen).uuid, bob_uuid);
    assert_eq!(last(&seen).icon.color, Some([255, 0, 0]));

    server
        .handle
        .run_command("waypoint modify Bob style set minecraft:bowtie")
        .await
        .unwrap();
    let seen = waypoints_until(&alice, |p| p.len() >= 3).await;
    assert_eq!(last(&seen).icon.style.as_deref(), Some("minecraft:bowtie"));

    server
        .handle
        .run_command("waypoint modify Bob color reset")
        .await
        .unwrap();
    server.handle.run_command("team add reds").await.unwrap();
    server
        .handle
        .run_command("team modify reds color red")
        .await
        .unwrap();
    server
        .handle
        .run_command("team join reds Bob")
        .await
        .unwrap();
    let seen = waypoints_until(&alice, |p| {
        p.last()
            .is_some_and(|w| w.icon.color == Some([0xFF, 0x55, 0x55]))
    })
    .await;
    assert_eq!(last(&seen).operation, Operation::Update);

    server
        .handle
        .run_command("gamerule locator_bar false")
        .await
        .unwrap();
    let seen = waypoints_until(&alice, |p| {
        p.last().is_some_and(|w| w.operation == Operation::Untrack)
    })
    .await;
    assert_eq!(last(&seen).uuid, bob_uuid);
    waypoints_until(&bob, |p| {
        p.last().is_some_and(|w| w.operation == Operation::Untrack)
    })
    .await;

    let count = alice.with_state(|s| s.waypoints.len());
    server
        .handle
        .run_command("gamerule locator_bar true")
        .await
        .unwrap();
    waypoints_until(&alice, |p| p.len() > count).await;
    assert_eq!(
        alice.with_state(|s| s.waypoints.last().unwrap().operation),
        Operation::Track
    );

    let count = alice.with_state(|s| s.waypoints.len());
    drop(bob);
    waypoints_until(&alice, |p| p.len() > count).await;
    assert_eq!(
        alice.with_state(|s| s.waypoints.last().unwrap().operation),
        Operation::Untrack
    );
    stop(server).await;
}

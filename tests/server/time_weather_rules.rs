//! The `advance_time` and `advance_weather` rules: sleeping skips the day
//! only while time advances and ends a storm only while weather advances,
//! `/weather` still works when frozen, and the rules persist.

use crate::common;

use common::{TempDir, TestServer};
use std::time::Duration;
use tuff::Server;
use tuff_core::world::fake::FakeSpec;
use tuff_core::world::fake::actions::FakeAction;
use tuff_world_model::{BlockPos, Face};

async fn start() -> TestServer {
    start_in(TempDir::new()).await
}

async fn start_in(instance: TempDir) -> TestServer {
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
    TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    }
}

async fn say(server: &TestServer, line: &str) -> String {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|c| c.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn until<F: std::future::Future<Output = bool>>(mut check: impl FnMut() -> F) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("condition not reached");
}

async fn raining(server: &TestServer) -> bool {
    say(
        server,
        r#"execute if predicate {type:"minecraft:weather_check",raining:true}"#,
    )
    .await
    .contains("passed")
}

async fn daytime(server: &TestServer) -> i64 {
    let lines = server.handle.run_command("time query time").await.unwrap();
    lines[0].to_value()["with"][1]["text"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn is_sleeping(server: &TestServer) -> bool {
    say(server, "data get entity Sleeper Pos[1]")
        .await
        .contains("150.6875")
}

/// Puts a lone player to bed at night, optionally in a storm, and returns
/// the server after they have woken.
async fn slept(advance_time: bool, advance_weather: bool, storm: bool) -> TestServer {
    let server = start().await;
    let spec = FakeSpec {
        position: Some([0.5, 150.0, 0.5]),
        ..FakeSpec::new("Sleeper")
    };
    let made = server
        .handle
        .spawn_fake("test", spec)
        .await
        .unwrap()
        .unwrap();
    made.observed
        .clone()
        .wait_for(|o| o.teleports >= 1)
        .await
        .unwrap();
    until(|| async {
        say(&server, "fill -10 149 -10 10 149 10 stone")
            .await
            .contains("Successfully")
    })
    .await;
    say(&server, &format!("gamerule advance_time {advance_time}")).await;
    for line in [
        "fill -10 150 -10 10 160 10 air replace air",
        "setblock 2 150 0 red_bed[part=foot,facing=east]",
        "setblock 3 150 0 red_bed[part=head,facing=east]",
        "gamerule spawn_monsters false",
        "gamemode survival Sleeper",
        "tp Sleeper 1.5 150 0.5",
        "time set 13000",
    ] {
        say(&server, line).await;
    }
    if storm {
        say(&server, "weather rain 1000000").await;
        until(|| async { raining(&server).await }).await;
    }
    say(
        &server,
        &format!("gamerule advance_weather {advance_weather}"),
    )
    .await;
    made.act(FakeAction::UseItemOn {
        off_hand: false,
        pos: BlockPos::new(2, 150, 0),
        face: Face::Up,
        cursor: [0.5, 0.5, 0.5],
    });
    until(|| async { is_sleeping(&server).await }).await;
    until(|| async { !is_sleeping(&server).await }).await;
    server
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleeping_skips_the_night_when_time_advances() {
    let server = slept(true, true, false).await;
    assert!((24000..25000).contains(&daytime(&server).await));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleeping_wakes_but_keeps_the_night_when_time_is_frozen() {
    let server = slept(false, true, false).await;
    assert_eq!(daytime(&server).await, 13000);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleeping_ends_a_storm_when_weather_advances() {
    let server = slept(true, true, true).await;
    until(|| async { !raining(&server).await }).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleeping_keeps_a_storm_when_weather_is_frozen() {
    let server = slept(true, false, true).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(raining(&server).await);
    assert!((24000..25000).contains(&daytime(&server).await));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_weather_command_works_with_weather_frozen() {
    let server = start().await;
    say(&server, "gamerule advance_weather false").await;
    say(&server, "weather rain").await;
    until(|| async { raining(&server).await }).await;
    say(&server, "weather clear").await;
    until(|| async { !raining(&server).await }).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_frozen_rules_survive_a_restart() {
    let server = start().await;
    say(&server, "gamerule advance_time false").await;
    say(&server, "gamerule advance_weather false").await;
    say(&server, "time set 5000").await;
    let TestServer {
        handle,
        task,
        _instance,
        ..
    } = server;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let server = start_in(_instance).await;
    assert!(
        say(&server, "gamerule advance_time")
            .await
            .contains("false")
    );
    assert!(
        say(&server, "gamerule advance_weather")
            .await
            .contains("false")
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(daytime(&server).await, 5000);
    stop(server).await;
}

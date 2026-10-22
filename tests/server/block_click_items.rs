//! A block that keeps an item from a click (a lectern's book) takes the
//! player's own stack, components and all, and the hand loses exactly one.

use crate::common;

use common::{TempDir, TestServer};
use std::time::Duration;
use tuff::Server;
use tuff_core::world::fake::FakeSpec;
use tuff_core::world::fake::actions::FakeAction;
use tuff_world_model::{BlockPos, Face};

async fn start() -> TestServer {
    let instance = TempDir::new();
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

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lectern_keeps_the_held_book_and_the_hand_loses_one() {
    let server = start().await;
    let spec = FakeSpec {
        position: Some([0.5, 150.0, 0.5]),
        ..FakeSpec::new("Reader")
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
    for line in [
        "fill -10 150 -10 10 160 10 air replace air",
        "setblock 2 150 0 lectern[facing=west]",
        "gamemode survival Reader",
        "tp Reader 0.5 150 0.5",
        r#"give Reader writable_book[custom_name="Notes"] 2"#,
    ] {
        say(&server, line).await;
    }
    until(|| async {
        say(&server, "clear Reader writable_book 0")
            .await
            .contains("Found 2")
    })
    .await;
    made.act(FakeAction::UseItemOn {
        off_hand: false,
        pos: BlockPos::new(2, 150, 0),
        face: Face::West,
        cursor: [0.0, 0.5, 0.5],
    });
    until(|| async {
        say(&server, "data get block 2 150 0 Book")
            .await
            .contains("Notes")
    })
    .await;
    until(|| async {
        say(&server, "clear Reader writable_book 0")
            .await
            .contains("Found 1")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let left = say(&server, "clear Reader writable_book 0").await;
    assert!(left.contains("Found 1"), "{left}");
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

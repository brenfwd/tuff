//! A water bucket used on the ground in survival becomes an empty bucket
//! and leaves a water source; using the empty bucket on that source turns
//! it back into a water bucket. Neither click drops or keeps a bucket.

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

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// How many of `item` the player holds anywhere in their inventory.
async fn held(server: &TestServer, item: &str) -> String {
    say(server, &format!("clear Bucketeer {item} 0")).await
}

/// A right click with the held item on the top of `ground`, as a client
/// sends it: the click on the block, then the use of the item.
fn right_click(fake: &tuff_core::world::fake::FakeHandle, ground: BlockPos) {
    fake.act(FakeAction::UseItemOn {
        off_hand: false,
        pos: ground,
        face: Face::Up,
        cursor: [0.5, 1.0, 0.5],
    });
    fake.act(FakeAction::UseItem { off_hand: false });
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_water_bucket_empties_onto_the_ground_and_fills_again_in_survival() {
    let server = start().await;
    let spec = FakeSpec {
        position: Some([0.5, 150.0, 0.5]),
        ..FakeSpec::new("Bucketeer")
    };
    let fake = server
        .handle
        .spawn_fake("test", spec)
        .await
        .unwrap()
        .unwrap();
    fake.observed
        .clone()
        .wait_for(|o| o.teleports >= 1)
        .await
        .unwrap();
    until("the floor is filled", || async {
        say(&server, "fill -10 149 -10 10 149 10 stone")
            .await
            .contains("Successfully")
    })
    .await;
    for line in [
        "fill -10 150 -10 10 160 10 air",
        "gamemode survival Bucketeer",
        "tp Bucketeer 0.5 150 0.5 0 39",
        "kill @e[type=item]",
        "clear Bucketeer",
        "give Bucketeer water_bucket 1",
    ] {
        say(&server, line).await;
    }
    until("the player holds the water bucket", || async {
        held(&server, "water_bucket").await.contains("Found 1")
    })
    .await;
    fake.act(FakeAction::Look {
        yaw: 0.0,
        pitch: 39.0,
    });
    let ground = BlockPos::new(0, 149, 2);
    let source = "execute if block 0 150 2 water[level=0]";

    right_click(&fake, ground);
    until("water is placed and the bucket is empty", || async {
        say(&server, source).await.contains("passed")
            && held(&server, "bucket").await.contains("Found 1")
    })
    .await;
    assert!(
        !held(&server, "water_bucket").await.contains("Found"),
        "the water bucket was kept after placing water"
    );
    assert!(
        !say(&server, "kill @e[type=item]").await.contains("Killed"),
        "placing water dropped an item"
    );

    right_click(&fake, ground);
    until("the source is taken and the bucket is full", || async {
        !say(&server, source).await.contains("passed")
            && held(&server, "water_bucket").await.contains("Found 1")
    })
    .await;
    assert!(
        !held(&server, "bucket").await.contains("Found"),
        "the empty bucket was kept after picking water up"
    );
    assert!(
        !say(&server, "kill @e[type=item]").await.contains("Killed"),
        "picking water up dropped an item"
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

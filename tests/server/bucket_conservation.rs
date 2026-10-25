//! A fluid or mob bucket spends exactly one bucket: stacked buckets keep
//! the rest of the stack, and a click followed at once by the stack
//! leaving the hand never places the fluid or mob and keeps the bucket.

use crate::common;

use common::{TempDir, TestServer};
use std::time::Duration;
use tuff::Server;
use tuff_core::world::fake::actions::FakeAction;
use tuff_core::world::fake::{FakeHandle, FakeSpec};
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

async fn found(server: &TestServer, item: &str) -> u32 {
    let answer = say(server, &format!("clear Bucketeer {item} 0")).await;
    answer
        .split("Found ")
        .nth(1)
        .and_then(|n| n.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

async fn ready() -> (TestServer, FakeHandle) {
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
        "clear Bucketeer",
    ] {
        say(&server, line).await;
    }
    fake.act(FakeAction::Look {
        yaw: 0.0,
        pitch: 39.0,
    });
    (server, fake)
}

fn right_click(fake: &FakeHandle) {
    fake.act(FakeAction::UseItemOn {
        off_hand: false,
        pos: BlockPos::new(0, 149, 2),
        face: Face::Up,
        cursor: [0.5, 1.0, 0.5],
    });
    fake.act(FakeAction::UseItem { off_hand: false });
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stack_of_water_buckets_keeps_the_rest_of_the_stack() {
    let (server, fake) = ready().await;
    say(&server, "give Bucketeer water_bucket[max_stack_size=16] 3").await;
    until("the stack arrives", || async {
        found(&server, "water_bucket").await == 3
    })
    .await;
    right_click(&fake);
    until("the water is placed", || async {
        say(&server, "execute if block 0 150 2 water[level=0]")
            .await
            .contains("passed")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        found(&server, "water_bucket").await,
        2,
        "two buckets stay filled"
    );
    assert_eq!(
        found(&server, "bucket").await,
        1,
        "one empty bucket came back"
    );
    assert!(
        !say(&server, "kill @e[type=item]").await.contains("Killed"),
        "nothing was dropped"
    );
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

const TRIALS: usize = 25;

/// Clicks `give` onto the floor and drops the stack at once, `TRIALS`
/// times; counts the clicks that placed (`placed` command passes) and
/// still left a filled bucket in the hand or on the ground.
async fn kept_after_pouring(give: &str, filled: &str, placed: &str, cleanup: &str) -> usize {
    let (server, fake) = ready().await;
    let mut kept = 0;
    for trial in 0..TRIALS {
        for line in cleanup
            .split(';')
            .chain(["kill @e[type=item]", "clear Bucketeer"])
        {
            say(&server, line).await;
        }
        until("the last trial is gone", || async {
            !say(&server, placed).await.contains("passed")
        })
        .await;
        say(&server, give).await;
        until("the bucket arrives", || async {
            found(&server, filled).await == 1
        })
        .await;
        right_click(&fake);
        fake.act(FakeAction::Drop { stack: true });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let did = say(&server, placed).await.contains("passed");
        let held = found(&server, filled).await;
        let dropped = say(
            &server,
            &format!("kill @e[type=item,nbt={{Item:{{id:\"minecraft:{filled}\"}}}}]"),
        )
        .await;
        if did && (held > 0 || dropped.contains("Killed")) {
            kept += 1;
            eprintln!("trial {trial}: poured and kept the bucket: held {held} dropped {dropped}");
        }
    }
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    kept
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lava_poured_from_a_bucket_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_pouring(
            "give Bucketeer lava_bucket 1",
            "lava_bucket",
            "execute if block 0 150 2 lava",
            "setblock 0 150 2 air",
        )
        .await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_poured_from_a_bucket_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_pouring(
            "give Bucketeer water_bucket 1",
            "water_bucket",
            "execute if block 0 150 2 water[level=0]",
            "setblock 0 150 2 air",
        )
        .await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fish_released_from_a_bucket_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_pouring(
            "give Bucketeer cod_bucket 1",
            "cod_bucket",
            "execute if entity @e[type=cod]",
            "kill @e[type=cod];setblock 0 150 2 air",
        )
        .await,
        0
    );
}

//! An entity created from an item spends that item exactly once, even when
//! the click is followed at once by the stack leaving the hand.

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

const TRIALS: usize = 25;

#[derive(Clone, Copy)]
enum Click {
    OnFloor,
    LookingDown,
}

/// Uses `item` on the floor and drops the stack at once, `TRIALS` times;
/// counts the clicks that made an entity matching `selector` and still left
/// `item` in the hand or on the ground.
async fn kept_after_creating(item: &str, selector: &str, click: Click) -> usize {
    let server = start().await;
    let spec = FakeSpec {
        position: Some([0.5, 150.0, 0.5]),
        ..FakeSpec::new("Builder")
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
        "gamemode survival Builder",
        "tp Builder 0.5 150 0.5",
    ] {
        say(&server, line).await;
    }
    if matches!(click, Click::LookingDown) {
        made.act(FakeAction::Look {
            yaw: 0.0,
            pitch: 90.0,
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let target = BlockPos::new(3, 149, 0);
    let mut kept = 0;
    let mut made_any = 0;
    for trial in 0..TRIALS {
        say(&server, &format!("kill @e[{selector}]")).await;
        say(&server, "kill @e[type=item]").await;
        say(&server, "clear Builder").await;
        say(&server, &format!("give Builder {item} 1")).await;
        until(|| async {
            say(&server, &format!("clear Builder {item} 0"))
                .await
                .contains("Found 1")
        })
        .await;
        match click {
            Click::OnFloor => made.act(FakeAction::UseItemOn {
                off_hand: false,
                pos: target,
                face: Face::Up,
                cursor: [0.5, 1.0, 0.5],
            }),
            Click::LookingDown => made.act(FakeAction::UseItem { off_hand: false }),
        }
        made.act(FakeAction::Drop { stack: true });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let made_one = say(&server, &format!("execute if entity @e[{selector}]")).await;
        let held = say(&server, &format!("clear Builder {item} 0")).await;
        let dropped = say(&server, "kill @e[type=item]").await;
        let has_item = held.contains("Found 1") || dropped.contains(item);
        if made_one.contains("passed") {
            made_any += 1;
        }
        if made_one.contains("passed") && has_item {
            kept += 1;
            eprintln!("trial {trial}: made the entity and kept the item: {held} / {dropped}");
        }
    }
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    assert!(made_any > 0, "the clicks never made a {selector}");
    kept
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_armor_stand_placed_from_the_last_item_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_creating("armor_stand", "type=armor_stand", Click::OnFloor).await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_placed_from_the_last_item_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_creating("oak_boat", "type=oak_boat", Click::LookingDown).await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawn_egg_used_from_the_last_item_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_creating("pig_spawn_egg", "type=pig", Click::OnFloor).await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_bucket_emptied_from_the_last_item_spends_it_even_if_the_stack_is_dropped_at_once() {
    assert_eq!(
        kept_after_creating("axolotl_bucket", "type=axolotl", Click::LookingDown).await,
        0
    );
}

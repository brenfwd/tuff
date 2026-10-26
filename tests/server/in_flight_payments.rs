//! An item that pays for a click is spent exactly once whatever the client
//! does next: leaving in the same write, or switching hotbar slots before
//! the region answers.

use crate::common;

use common::{TempDir, TestServer};
use std::time::Duration;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_core::world::fake::FakeSpec;
use tuff_core::world::fake::actions::FakeAction;
use tuff_world_model::{BlockPos, Face};

async fn start() -> TestServer {
    let instance = TempDir::new();
    let mut features = tuff::vanilla_features::all();
    features.retain(|feature| !feature.id().contains("anticheat"));
    let server = Server::builder()
        .features(features)
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

async fn found(server: &TestServer, who: &str, item: &str) -> i64 {
    say(server, &format!("clear {who} {item} 0"))
        .await
        .split("Found ")
        .nth(1)
        .and_then(|n| n.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

async fn killed(server: &TestServer, selector: &str) -> i64 {
    let answer = say(server, &format!("kill {selector}")).await;
    answer
        .split("Killed ")
        .nth(1)
        .map(|n| {
            n.split(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1)
        })
        .unwrap_or(0)
}

async fn floor(server: &TestServer, block: &str) {
    say(server, "gamemode survival Payer").await;
    say(server, "tp Payer 0.5 150 0.5 0 90").await;
    until("the floor is filled", || async {
        say(server, &format!("fill -10 149 -10 10 149 10 {block}"))
            .await
            .contains("Successfully")
    })
    .await;
    for line in [
        "fill -10 150 -10 10 160 10 air replace air",
        "gamemode survival Payer",
        "tp Payer 0.5 150 0.5 0 90",
    ] {
        say(server, line).await;
    }
}

async fn join(server: &TestServer) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Payer")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

#[derive(Clone, Copy)]
enum Click {
    OnFloor,
    InAir,
}

const COUNT: i64 = 32;
const TRIALS: usize = 8;

/// Sends `COUNT` clicks with `item` and leaves in the same write; the
/// item, the entities it made and what the player holds on return must not
/// add up to more than was given.
async fn leaving_after_clicks(item: &str, selector: &str, click: Click) {
    let server = start().await;
    let mut bot = join(&server).await;
    floor(&server, "obsidian").await;
    let mut made_any = false;
    for trial in 0..TRIALS {
        say(&server, &format!("kill @e[{selector}]")).await;
        say(&server, "kill @e[type=item]").await;
        say(&server, "clear Payer").await;
        say(&server, "gamemode survival Payer").await;
        say(&server, "tp Payer 0.5 150 0.5 0 90").await;
        say(&server, &format!("give Payer {item} {COUNT}")).await;
        until("the stack arrives", || async {
            found(&server, "Payer", item).await == COUNT
        })
        .await;
        bot.wait_for(|state| state.teleports > 0).await.unwrap();
        bot.send(Serverbound::Move(Movement {
            position: None,
            rotation: Some((0.0, 90.0)),
            on_ground: true,
            horizontal_collision: false,
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        for sequence in 1..=COUNT as i32 {
            bot.send(match click {
                Click::OnFloor => Serverbound::UseItemOn {
                    hand: 0,
                    position: BlockPos::new(3, 149, 0),
                    face: Face::Up,
                    cursor: [0.5, 1.0, 0.5],
                    sequence,
                },
                Click::InAir => Serverbound::UseItem {
                    off_hand: false,
                    sequence,
                },
            });
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        drop(bot);
        until("the player has left", || async {
            say(&server, "data get entity Payer Health")
                .await
                .contains("No entity")
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        bot = join(&server).await;
        let held = found(&server, "Payer", item).await;
        let made = killed(&server, &format!("@e[{selector}]")).await;

        let dropped = killed(
            &server,
            &format!("@e[type=item,nbt={{Item:{{id:\"minecraft:{item}\"}}}}]"),
        )
        .await;
        made_any |= made > 0;
        assert!(
            held + made + dropped <= COUNT,
            "trial {trial}: gave {COUNT} {item}, found {held} held, {made} made, {dropped} dropped"
        );
    }
    assert!(made_any, "the clicks never made a {selector}");
    drop(bot);
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_crystals_are_not_refunded_by_leaving_with_clicks_in_flight() {
    leaving_after_clicks("end_crystal", "type=end_crystal", Click::OnFloor).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn armor_stands_are_not_refunded_by_leaving_with_clicks_in_flight() {
    leaving_after_clicks("armor_stand", "type=armor_stand", Click::OnFloor).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_eggs_are_not_refunded_by_leaving_with_clicks_in_flight() {
    leaving_after_clicks(
        "pig_spawn_egg",
        "type=pig,nbt={DeathTime:0s}",
        Click::OnFloor,
    )
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boats_are_not_refunded_by_leaving_with_clicks_in_flight() {
    leaving_after_clicks("oak_boat", "type=oak_boat", Click::InAir).await;
}

const SWAPS: usize = 20;

/// Uses `item` on the block `setup` makes at the target and selects another
/// slot at once; counts the clicks that took effect (`landed` passes) and
/// still left `item` in the inventory.
async fn kept_after_switching_slots(item: &str, setup: &str, landed: &str) -> usize {
    let server = start().await;
    let spec = FakeSpec {
        position: Some([0.5, 150.0, 0.5]),
        ..FakeSpec::new("Payer")
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
    floor(&server, "stone").await;
    let mut kept = 0;
    let mut landed_any = false;
    for trial in 0..SWAPS {
        for line in ["kill @e[type=item]", "clear Payer"] {
            say(&server, line).await;
        }
        say(&server, setup).await;
        made.act(FakeAction::SelectSlot(0));
        say(&server, &format!("give Payer {item} 1")).await;
        until("the item arrives", || async {
            found(&server, "Payer", item).await == 1
        })
        .await;
        made.act(FakeAction::UseItemOn {
            off_hand: false,
            pos: BlockPos::new(3, 150, 0),
            face: Face::Up,
            cursor: [0.5, 1.0, 0.5],
        });
        made.act(FakeAction::SelectSlot(1));
        tokio::time::sleep(Duration::from_millis(150)).await;
        let did = say(&server, landed).await.contains("passed");
        landed_any |= did;
        let held = found(&server, "Payer", item).await;
        let dropped = killed(
            &server,
            &format!("@e[type=item,nbt={{Item:{{id:\"minecraft:{item}\"}}}}]"),
        )
        .await;
        if did && (held > 0 || dropped > 0) {
            kept += 1;
            eprintln!("trial {trial}: used the item and kept it: {held} held, {dropped} dropped");
        }
    }
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    assert!(landed_any, "the clicks never had an effect");
    kept
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lava_poured_into_a_cauldron_is_spent_when_the_slot_changes_at_once() {
    assert_eq!(
        kept_after_switching_slots(
            "lava_bucket",
            "setblock 3 150 0 cauldron",
            "execute if block 3 150 0 lava_cauldron"
        )
        .await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_poured_into_a_cauldron_is_spent_when_the_slot_changes_at_once() {
    assert_eq!(
        kept_after_switching_slots(
            "water_bucket",
            "setblock 3 150 0 cauldron",
            "execute if block 3 150 0 water_cauldron"
        )
        .await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flower_put_in_a_pot_is_spent_when_the_slot_changes_at_once() {
    assert_eq!(
        kept_after_switching_slots(
            "poppy",
            "setblock 3 150 0 flower_pot",
            "execute if block 3 150 0 potted_poppy"
        )
        .await,
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bottle_filled_from_a_cauldron_is_spent_when_the_slot_changes_at_once() {
    assert_eq!(
        kept_after_switching_slots(
            "glass_bottle",
            "setblock 3 150 0 water_cauldron[level=3]",
            "execute if block 3 150 0 water_cauldron[level=2]"
        )
        .await,
        0
    );
}

//! A world save that fails or is cut short never leaves items twice (or
//! nowhere) after a restart: everything a save touched lands together or
//! the world restarts from the last complete save.

use crate::common;
use crate::save_ordering::{Running, copy_dir, join, newest, place_chest, said, started};

use std::path::Path;
use std::time::Duration;

use common::TempDir;
use tuff_storage::journal::{Faults, SaveJournal};

const CHEST: &str = "0 100 0";

fn count_in(text: &str) -> i64 {
    text.rsplit(|c: char| !c.is_ascii_digit())
        .find_map(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Diamonds in the chest plus diamonds Steve holds.
async fn diamonds(server: &Running) -> i64 {
    let chest = said(
        &server.handle,
        &format!("data get block {CHEST} Items[0].count"),
    )
    .await;
    let in_chest = if chest.contains("commands.data.block.query") {
        count_in(&chest)
    } else {
        0
    };
    let held = said(&server.handle, "clear Steve diamond 0").await;
    let on_player = if held.contains("clear.failed") {
        0
    } else {
        count_in(&held)
    };
    in_chest + on_player
}

async fn journal(server: &Running) -> std::sync::Arc<SaveJournal> {
    server
        .handle
        .world_shared()
        .await
        .unwrap()
        .backend
        .save_journal()
        .expect("a world on disk keeps a save journal")
}

/// Five diamonds saved in a chest, then moved onto Steve without a save.
async fn moved_out_of_the_chest(dir: &Path) -> (Running, tuff_bot::Bot) {
    let server = started(dir).await;
    let bot = join(&server, "Steve").await;
    let handle = &server.handle;
    place_chest(handle).await;
    handle
        .run_command(&format!(
            "item replace block {CHEST} container.0 with diamond 5"
        ))
        .await
        .unwrap();
    let chest = said(handle, &format!("data get block {CHEST} Items")).await;
    assert!(chest.contains("diamond"), "{chest}");
    saved(&server).await;
    for line in [
        "item replace entity Steve hotbar.0 with diamond 5",
        &format!("item replace block {CHEST} container.0 with air"),
    ] {
        handle.run_command(line).await.unwrap();
    }
    let chest = said(handle, &format!("data get block {CHEST} Items")).await;
    assert!(!chest.contains("diamond"), "{chest}");
    assert_eq!(diamonds(&server).await, 5);
    (server, bot)
}

/// Saves until a save commits (a save whose snapshot took too long is
/// completed by the next one).
async fn saved(server: &Running) {
    let journal = journal(server).await;
    let before = journal.committed();
    for _ in 0..20 {
        server.handle.run_command("save-all flush").await.unwrap();
        if journal.committed() > before {
            return;
        }
    }
    panic!("no save committed");
}

async fn stop(server: Running, bot: tuff_bot::Bot) {
    drop(bot);
    server.handle.shutdown().await.unwrap();
    let _ = server.task.await;
}

/// Starts the world in `dir`, joins Steve and counts the diamonds.
async fn diamonds_after_restart(dir: &Path) -> i64 {
    let server = started(dir).await;
    let bot = join(&server, "Steve").await;
    server
        .handle
        .run_command("forceload add 0 0")
        .await
        .unwrap();
    for _ in 0..600 {
        let chest = said(&server.handle, &format!("data get block {CHEST}")).await;
        if !chest.contains("unloaded") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let found = diamonds(&server).await;
    stop(server, bot).await;
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_io_error_during_a_save_keeps_the_item_count_after_a_restart() {
    let dir = TempDir::new();
    let (server, bot) = moved_out_of_the_chest(dir.path()).await;
    let journal = journal(&server).await;
    journal.set_faults(Faults {
        fail_blob_writes: true,
        ..Faults::default()
    });
    let committed = journal.committed();
    server.handle.run_command("save-all flush").await.unwrap();
    server.handle.run_command("save-all flush").await.unwrap();
    assert_eq!(
        journal.committed(),
        committed,
        "a save with a failed chunk write never commits"
    );
    let crashed = TempDir::new();
    copy_dir(dir.path(), crashed.path());
    journal.set_faults(Faults::default());
    stop(server, bot).await;

    assert_eq!(diamonds_after_restart(crashed.path()).await, 5);
    assert_eq!(diamonds_after_restart(dir.path()).await, 5);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_between_the_player_write_and_the_chunk_write_keeps_the_item_count() {
    let dir = TempDir::new();
    let (server, bot) = moved_out_of_the_chest(dir.path()).await;
    let journal = journal(&server).await;
    let players = dir.path().join("world/players/data");
    let before = newest(&players, "dat");
    journal.set_faults(Faults {
        hold_blob_writes: true,
        ..Faults::default()
    });
    server.handle.run_command("save-all").await.unwrap();
    let mut written = false;
    for _ in 0..600 {
        if newest(&players, "dat") > before {
            written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(written, "the save wrote the player");
    let crashed = TempDir::new();
    copy_dir(dir.path(), crashed.path());
    journal.set_faults(Faults::default());
    stop(server, bot).await;

    assert_eq!(diamonds_after_restart(crashed.path()).await, 5);
    assert_eq!(diamonds_after_restart(dir.path()).await, 5);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_whose_check_fails_is_refused_and_keeps_the_item_count() {
    use tuff::protocol::play::{PlayerActionStatus, Serverbound};
    use tuff_core::world::conservation::Transfer;

    let dir = TempDir::new();
    let server = started(dir.path()).await;
    let bot = join(&server, "Steve").await;
    server
        .handle
        .run_command("item replace entity Steve hotbar.0 with diamond 5")
        .await
        .unwrap();
    assert_eq!(diamonds(&server).await, 5);
    saved(&server).await;
    let shared = server.handle.world_shared().await.unwrap();
    shared.conservation.fail_next(Transfer::Drop);
    bot.send(Serverbound::PlayerAction {
        status: PlayerActionStatus::DropAllItems,
        position: tuff_world_model::BlockPos::new(0, 0, 0),
        face: 0,
        sequence: 0,
    });
    let kicked = tokio::time::timeout(Duration::from_secs(30), bot.wait_for(|_| false)).await;
    assert!(
        matches!(kicked, Ok(Err(_))),
        "the player whose drop was refused is disconnected"
    );
    let items = said(&server.handle, "execute if entity @e[type=minecraft:item]").await;
    assert!(
        items.contains("fail"),
        "no item entity was spawned: {items}"
    );
    stop(server, bot).await;

    assert_eq!(diamonds_after_restart(dir.path()).await, 5);
}

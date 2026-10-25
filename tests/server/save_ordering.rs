use crate::common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::TempDir;
use tuff_core::Server;

pub(crate) struct Running {
    pub(crate) handle: tuff_core::ServerHandle,
    pub(crate) task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

pub(crate) fn start(dir: &Path) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(dir.to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    Running {
        handle,
        task,
        addr: "0.0.0.0:0".parse().unwrap(),
    }
}

pub(crate) async fn started(dir: &Path) -> Running {
    let mut running = start(dir);
    running.addr = running.handle.local_addrs().await.unwrap()[0];
    running
}

pub(crate) async fn join(server: &Running, name: &str) -> tuff_bot::Bot {
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

pub(crate) async fn said(handle: &tuff_core::ServerHandle, line: &str) -> String {
    handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|l| l.to_value().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn newest(dir: &Path, ext: &str) -> Option<SystemTime> {
    let mut best = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let found = if path.is_dir() {
            newest(&path, ext)
        } else if path.extension().is_some_and(|e| e == ext) {
            entry.metadata().ok()?.modified().ok()
        } else {
            None
        };
        best = best.max(found);
    }
    best
}

/// Places a chest at 0 100 0 once its forceloaded chunk is in.
pub(crate) async fn place_chest(handle: &tuff_core::ServerHandle) {
    handle.run_command("forceload add 0 0").await.unwrap();
    for _ in 0..600 {
        handle.run_command("setblock 0 100 0 chest").await.unwrap();
        if said(handle, "data get block 0 100 0")
            .await
            .contains("commands.data.block.query")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the chest never appeared");
}

/// The items of the chest at 0 100 0 once its chunk is loaded again; a
/// restarted world loads its forced chunks after it starts listening.
async fn loaded_chest_items(handle: &tuff_core::ServerHandle) -> String {
    handle.run_command("forceload add 0 0").await.unwrap();
    for _ in 0..600 {
        let items = said(handle, "data get block 0 100 0 Items").await;
        if !items.contains("argument.pos.unloaded") {
            return items;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the chest's chunk never loaded");
}

pub(crate) fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target: PathBuf = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "session.lock" {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

async fn gametime(handle: &tuff_core::ServerHandle) -> u64 {
    said(handle, "time query gametime")
        .await
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .max()
        .unwrap()
}

async fn sprint_to(handle: &tuff_core::ServerHandle, target: u64) {
    let now = gametime(handle).await;
    assert!(now < target, "already at {now}");
    handle
        .run_command(&format!("tick sprint {}", target - now))
        .await
        .unwrap();
    for _ in 0..1200 {
        if gametime(handle).await >= target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the sprint never reached {target}");
}

/// The newest world save whose writes all landed, as its save journal
/// records it.
fn committed_save(world: &Path) -> u64 {
    std::fs::read_to_string(world.join("tuff-journal").join("committed"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn next_autosave(now: u64) -> u64 {
    (now / 6000 + 1) * 6000
}

/// Joins a player half an autosave period in, so a timer counted from the
/// join would not line up with the world's autosave, then moves its stack
/// into a chest.
async fn moved_into_chest(dir: &Path) -> (Running, tuff_bot::Bot) {
    let server = started(dir).await;
    sprint_to(&server.handle, 3000).await;
    let bot = join(&server, "Steve").await;
    let handle = &server.handle;
    place_chest(handle).await;
    handle.run_command("give Steve diamond 5").await.unwrap();
    handle.run_command("save-all flush").await.unwrap();
    handle
        .run_command("item replace block 0 100 0 container.0 with diamond 5")
        .await
        .unwrap();
    handle.run_command("clear Steve diamond").await.unwrap();
    (server, bot)
}

/// A stack moved from a player into a chest, then a crash right after the
/// periodic autosave: the files left behind hold the stack once.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_after_the_autosave_never_leaves_an_item_in_both_chest_and_player() {
    let dir = TempDir::new();
    let (server, bot) = moved_into_chest(dir.path()).await;
    let handle = &server.handle;
    let world = dir.path().join("world");
    let before = committed_save(&world);
    let target = next_autosave(gametime(handle).await) + 5;
    sprint_to(handle, target).await;
    let mut written = false;
    for attempt in 0..600 {
        if committed_save(&world) > before {
            written = true;
            break;
        }
        if attempt == 100 {
            handle.run_command("save-all").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(written, "the autosave wrote both the player and the chunks");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let crashed = TempDir::new();
    copy_dir(dir.path(), crashed.path());
    drop(bot);
    server.handle.shutdown().await.unwrap();
    let _ = server.task.await;

    let after = started(crashed.path()).await;
    let in_chest = loaded_chest_items(&after.handle).await;
    let _bot = join(&after, "Steve").await;
    let on_player = said(&after.handle, "clear Steve diamond 0").await;
    assert!(in_chest.contains("diamond"), "{in_chest}");
    assert!(on_player.contains("clear.failed"), "{on_player}");
    after.handle.shutdown().await.unwrap();
    let _ = after.task.await;
}

/// With `/save-off`, the periodic autosave leaves player files alone too.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_off_stops_player_autosaves() {
    let dir = TempDir::new();
    let (server, bot) = moved_into_chest(dir.path()).await;
    let handle = &server.handle;
    handle.run_command("save-off").await.unwrap();
    let players = dir.path().join("world/players/data");
    let before = newest(&players, "dat");
    let target = next_autosave(gametime(handle).await) + 3500;
    sprint_to(handle, target).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(newest(&players, "dat"), before);
    drop(bot);
    server.handle.shutdown().await.unwrap();
    let _ = server.task.await;
}

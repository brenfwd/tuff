//! The bots plugin and a test consumer talking through the typed `control`
//! and `events` interfaces on a real server: a bot is an online player,
//! removal and owner unload take it away, and a consumer survives bots
//! being absent.

use std::future::Future;
use std::sync::OnceLock;

use super::*;
use crate::feature::{Feature, LoadContext};
use crate::services::{ServiceFactory, WorldService};
use tuff::Server;
use tuff_item::load::{LoadError, Phase};
use tuff_plugin_wasm::sandbox::Player;
use tuff_plugin_wasm::{WasmHost, WasmPluginsFeature};
use tuff_storage::admin::PermissionLevel;

pub(super) fn built(dir: &str, wasm: &str) -> Option<Vec<u8>> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let dir = root.join(dir);
    let ok = std::process::Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()))
        .args([
            "build",
            "--release",
            "--quiet",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .current_dir(&dir)
        .status()
        .is_ok_and(|status| status.success());
    let out = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| dir.join("target"), Into::into);
    let path = out.join("wasm32-unknown-unknown/release").join(wasm);
    if !ok || !path.exists() {
        assert!(
            std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() != Ok("1"),
            "building {wasm} failed (is the wasm32-unknown-unknown target installed?)"
        );
        eprintln!("skipping: {wasm} is not built");
        return None;
    }
    Some(std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display())))
}

pub(super) type Cell = Arc<OnceLock<Arc<Shared>>>;

struct Capture(Cell);

struct CaptureService(Cell);

impl ServiceFactory for Capture {
    fn create(&self, _: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(CaptureService(self.0.clone()))
    }
}

impl WorldService for CaptureService {
    fn start(&self, shared: &Arc<Shared>) {
        let _ = self.0.set(shared.clone());
    }
}

pub(super) struct CaptureShared(pub(super) Cell);

impl Feature for CaptureShared {
    fn id(&self) -> &'static str {
        "capture-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(Capture(self.0.clone())));
        }
        Ok(())
    }
}

/// Server ticks a wait may take: 30 seconds at full speed.
const PATIENCE: u64 = 600;

struct Run {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    shared: Arc<Shared>,
    _dir: TestDir,
}

impl Run {
    async fn start(with_bots: bool) -> Option<Run> {
        let consumer = built(
            "crates/tuff-plugin-wasm/tests/guests/rust-bots-consumer",
            "rust_bots_consumer.wasm",
        )?;
        let bots = built("examples/rust-bots", "bots.wasm")?;
        let dir = TestDir::new();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("bots_consumer.wasm"), &consumer).unwrap();
        if with_bots {
            std::fs::write(plugins.join("bots.wasm"), &bots).unwrap();
        }
        let backend = Arc::new(
            tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
        );
        let config = backend.config().expect("sqlite keeps the config");
        config
            .write_plugin_grant("bots", Some("cross-plugin = all\nplayer-read = all"))
            .unwrap();
        config
            .write_plugin_grant(
                "bots-consumer",
                Some("cross-plugin = all\nplayer-read = all"),
            )
            .unwrap();
        let cell = Cell::default();
        let mut all = tuff::vanilla_features::all();
        all.push(Box::new(CaptureShared(cell.clone())));
        all.push(Box::new(WasmPluginsFeature::at(plugins.clone())));
        let server = Server::builder()
            .features(all)
            .storage(backend)
            .instance_dir(dir.path().to_path_buf())
            .binds(vec!["127.0.0.1:0".into()])
            .online_mode(false)
            .config_overrides(vec![("tuff.offlineOps".into(), "true".into())])
            .whitelist(false)
            .view_distance(2)
            .seed(1)
            .build();
        let handle = server.handle();
        let task = tokio::spawn(server.run());
        handle.local_addrs().await.unwrap();
        let shared = cell.get().expect("the world started").clone();
        let run = Run {
            handle,
            task,
            shared,
            _dir: dir,
        };
        run.host().sync().await;
        Some(run)
    }

    /// Asks the server to stop and waits for the server task to exit.
    async fn stop(self) {
        self.handle.shutdown().await.unwrap();
        drop(self.shared);
        tokio::time::timeout(std::time::Duration::from_secs(20), self.task)
            .await
            .expect("the server task exits after shutdown")
            .unwrap()
            .unwrap();
    }

    fn host(&self) -> impl std::ops::Deref<Target = WasmHost> {
        self.shared.services.get::<WasmHost>().unwrap()
    }

    async fn say(&self, line: &str) -> String {
        let player = Player {
            uuid: 77,
            name: "Steve".to_owned(),
            level: Some(PermissionLevel::Gamemasters),
            position: None,
            items: None,
        };
        let line = format!("botuser {line}");
        self.within(PATIENCE, || async {
            let outcome = self.host().command(player.clone(), line.clone()).await?;
            Some(outcome.replies.join("\n"))
        })
        .await
        .expect("the consumer does not own its command")
    }

    /// Runs `line` and waits until the consumer's answer satisfies `want`.
    async fn ask(&self, line: &str, want: impl Fn(&str) -> bool) -> String {
        self.say(line).await;
        let end = self.ticks() + PATIENCE;
        loop {
            let last = self.say("answer").await;
            if want(&last) {
                return last;
            }
            assert!(self.ticks() < end, "`{line}` answered {last:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn online(&self) -> String {
        self.say("online").await
    }

    fn ticks(&self) -> u64 {
        *self.shared.server_ticks.borrow()
    }

    async fn ticks_pass(&self, ticks: u64) {
        let end = self.ticks() + ticks;
        let mut clock = self.shared.server_ticks.clone();
        drop(
            clock
                .wait_for(|now| *now >= end)
                .await
                .expect("the server ticks"),
        );
    }

    /// Polls `probe` every 50 ms until it answers or `ticks` server ticks
    /// pass.
    async fn within<T, F: Future<Output = Option<T>>>(
        &self,
        ticks: u64,
        mut probe: impl FnMut() -> F,
    ) -> Option<T> {
        let end = self.ticks() + ticks;
        loop {
            if let Some(found) = probe().await {
                return Some(found);
            }
            if self.ticks() >= end {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn eventually<T, F: Future<Output = Option<T>>>(&self, probe: impl FnMut() -> F) -> T {
        match self.within(PATIENCE, probe).await {
            Some(found) => found,
            None => panic!("the condition did not hold within {PATIENCE} server ticks"),
        }
    }

    /// The consumer's events once bot `bot`'s process ended, or as they
    /// stand after `ticks` server ticks.
    async fn process_end(&self, bot: u32, ticks: u64) -> String {
        let (done, failed) = (format!("done {bot}"), format!("failed {bot}"));
        let ended = self
            .within(ticks, || async {
                let events = self.say("events").await;
                (events.contains(&done) || events.contains(&failed)).then_some(events)
            })
            .await;
        match ended {
            Some(events) => events,
            None => self.say("events").await,
        }
    }

    /// Runs `fill` once the forceloaded chunks it covers have loaded.
    async fn fill_loaded(&self, fill: &str) {
        loop {
            let reply = self.server(fill).await;
            if reply.contains("Successfully") {
                return;
            }
            assert!(reply.contains("loaded"), "{fill}: {reply}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits until `Botty` is online (or not) as `players::online` sees it.
    async fn botty_online(&self, online: bool) {
        self.eventually(|| async {
            (self.online().await.contains("Botty") == online).then_some(())
        })
        .await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_bot_is_a_player_and_removal_takes_it_away() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    assert!(!run.online().await.contains("Botty"));
    run.ask("spawn Botty", |a| a.contains("spawned 1")).await;
    run.botty_online(true).await;
    run.ask("state 1", |a| a.contains("alive=true")).await;
    run.ask("owned", |a| a.contains("[1]")).await;
    run.ask("remove 1", |a| a.contains("removed 1")).await;
    run.botty_online(false).await;
    let events = run
        .eventually(|| async {
            let events = run.say("events").await;
            events.contains("removed 1").then_some(events)
        })
        .await;
    assert!(events.contains("spawned 1"), "{events}");
    run.ask("remove 1", |a| a.contains("UnknownBot")).await;
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unloading_the_owner_removes_its_bots() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.ask("spawn Botty", |a| a.contains("spawned 1")).await;
    run.botty_online(true).await;
    run.host().unload("bots-consumer").await.unwrap();
    run.eventually(|| async {
        (!run
            .server("execute if entity @a[name=Botty]")
            .await
            .contains("passed"))
        .then_some(())
    })
    .await;
    run.host().load("bots_consumer.wasm").await.unwrap();
    run.host().sync().await;
    run.botty_online(false).await;
    run.ask("owned", |a| a.contains("[]")).await;
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_consumer_survives_bots_being_absent() {
    let Some(run) = Run::start(false).await else {
        return;
    };
    run.ask("spawn Botty", |a| a.contains("bots is not installed"))
        .await;
    run.stop().await;
}

impl Run {
    async fn server(&self, line: &str) -> String {
        self.handle
            .run_command(line)
            .await
            .unwrap()
            .iter()
            .map(|c| c.plain())
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn block_is(&self, x: i32, y: i32, z: i32, block: &str) -> bool {
        self.server(&format!("execute if block {x} {y} {z} {block}"))
            .await
            .contains("passed")
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goto_climbs_steps_and_a_hurdle_takes_a_drop_and_flags_nothing() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 47").await;
    run.fill_loaded("fill -3 140 -3 3 149 12 stone").await;
    for line in [
        "fill -3 150 -3 3 165 30 air",
        "fill -3 150 5 3 150 12 stone",
        "fill -3 151 9 3 151 12 stone",
        "fill -3 140 13 3 148 30 stone",
        "fill -3 149 20 3 149 30 stone",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.ask("goto 1 0 150 26", |a| a.contains("going 1")).await;
    let mut events = String::new();
    for _ in 0..600 {
        events = run.say("events").await;
        if events.contains("done 1") || events.contains("failed 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let state = run.ask("state 1", |a| a.contains("alive")).await;
    assert!(events.contains("done 1"), "{events} {state}");
    assert!(
        state.contains("z=26.") && state.contains("y=150"),
        "{state}"
    );
    let flags = run.server("anticheat violations Botty").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goto_swims_a_pool_climbs_a_ladder_and_jumps_a_three_block_gap() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 47").await;
    run.fill_loaded("fill -6 140 -3 6 149 34 stone").await;
    for line in [
        "fill -6 150 -3 6 170 34 air",
        "fill -6 150 -3 -6 170 34 stone",
        "fill 6 150 -3 6 170 34 stone",
        "fill -5 145 5 5 149 9 water",
        "fill -5 150 14 5 155 20 stone",
        "fill 0 150 13 0 155 13 ladder[facing=north]",
        "fill -5 141 21 5 155 23 air",
        "fill -5 140 24 5 155 34 stone",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    run.ask("goto 1 0 156 28", |a| a.contains("going 1")).await;
    let events = run.process_end(1, 1200).await;
    let state = run.ask("state 1", |a| a.contains("alive")).await;
    assert!(events.contains("done 1"), "{events} {state}");
    assert!(
        state.contains("z=28.") && state.contains("y=156"),
        "{state}"
    );
    let flags = run.server("anticheat violations Botty").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

fn position(state: &str) -> Option<(f64, f64, f64)> {
    let coord = |key: &str| {
        state
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))?
            .parse::<f64>()
            .ok()
    };
    Some((coord("x=")?, coord("y=")?, coord("z=")?))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_follows_a_moving_player_for_200_ticks_within_range() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 47").await;
    run.fill_loaded("fill -3 140 -3 3 149 30 stone").await;
    run.server("fill -3 150 -3 3 165 30 air").await;
    run.ask("spawnat Leader 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.ask("spawnat Botty 0 150 -2", |a| a.contains("spawned 2"))
        .await;
    run.eventually(|| async {
        let online = run.online().await;
        (online.contains("Leader") && online.contains("Botty")).then_some(())
    })
    .await;
    run.ask("follow 2 Leader 4", |a| a.contains("following 2"))
        .await;
    run.ask("goto 1 0 150 26", |a| a.contains("going 1")).await;
    let end = run.ticks() + 200;
    let mut worst = 0.0f64;
    let mut moved = 0.0f64;
    let mut follower = String::new();
    while run.ticks() < end {
        run.ticks_pass(5).await;
        let leader = run.ask("state 1", |a| a.starts_with("state 1 ")).await;
        follower = run.ask("state 2", |a| a.starts_with("state 2 ")).await;
        let (Some(l), Some(f)) = (position(&leader), position(&follower)) else {
            panic!("{leader} {follower}");
        };
        let gap = ((l.0 - f.0).powi(2) + (l.1 - f.1).powi(2) + (l.2 - f.2).powi(2)).sqrt();
        worst = worst.max(gap);
        moved = moved.max(l.2);
    }
    assert!(moved > 15.0, "the leader barely moved: z={moved}");
    assert!(worst <= 9.0, "the follower fell {worst} blocks behind");
    assert!(
        position(&follower).is_some_and(|f| f.2 > 15.0),
        "{follower}"
    );
    run.stop().await;
}

impl Run {
    async fn op(&self, uuid: u128, line: &str) -> String {
        let player = Player {
            uuid,
            name: "Op".to_owned(),
            level: Some(PermissionLevel::Gamemasters),
            position: None,
            items: None,
        };
        let outcome = self
            .host()
            .command(player, line.to_owned())
            .await
            .expect("the bots plugin owns /bots");
        outcome.replies.join("\n")
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operator_spawns_a_bot_by_command_and_goto_reaches_the_target() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 47").await;
    run.fill_loaded("fill -3 140 -3 3 149 30 stone").await;
    run.server("fill -3 150 -3 3 165 30 air").await;
    run.ask("spawnat Op 0 150 0", |a| a.contains("spawned 1"))
        .await;
    let op = tuff_core::world::fake::derive_uuid("bots", "Op", tuff_core::world::fake::Model::Wide);
    run.eventually(|| async {
        run.op(op, "bots spawn Mate")
            .await
            .contains("Spawned Mate")
            .then_some(())
    })
    .await;
    let reply = run.op(op, "bots goto Mate 0 150 20").await;
    assert!(reply.contains("on its way"), "{reply}");
    let list = run
        .eventually(|| async {
            let list = run.op(op, "bots list").await;
            list.contains("Mate: idle at 0 150 20").then_some(list)
        })
        .await;
    assert!(list.contains("Mate"), "{list}");
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_mines_three_iron_ore_and_holds_raw_iron() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -6 140 -6 6 149 6 stone").await;
    for line in [
        "fill -6 150 -6 6 160 6 air",
        "setblock 3 150 0 iron_ore",
        "setblock -3 150 2 iron_ore",
        "setblock 0 150 -4 iron_ore",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.server("give Botty iron_pickaxe").await;
    run.ask("mine 1 iron_ore 3", |a| a.contains("mining 1"))
        .await;
    let mut held = String::new();
    for _ in 0..600 {
        held = run.server("clear Botty raw_iron 0").await;
        if held.contains("Found 3") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let state = run.ask("state 1", |a| a.contains("alive")).await;
    assert!(
        held.contains("Found 3"),
        "{held} | {state} | {}",
        run.say("events").await
    );
    assert!(
        !run.server("execute if block 3 150 0 iron_ore")
            .await
            .contains("passed")
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_kills_a_zombie_within_reach_and_flags_nothing() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -16 140 -16 31 149 31 stone").await;
    for line in [
        "fill -16 150 -16 31 160 31 air",
        "gamerule spawn_mobs false",
        "gamerule spawn_monsters false",
        "kill @e[type=!minecraft:player]",
        "time set midnight",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    run.server("give Botty minecraft:iron_sword").await;
    run.server(
        "summon minecraft:zombie 5.5 150 0.5 {UUID:[I;0,0,0,1],NoAI:1b,PersistenceRequired:1b}",
    )
    .await;
    let summoned = run
        .within(PATIENCE, || async {
            run.server("execute if entity @e[type=minecraft:zombie]")
                .await
                .contains("passed")
                .then_some(())
        })
        .await;
    assert!(summoned.is_some(), "the zombie never appeared");
    run.ask("attack 1 00000000000000000000000000000001", |a| {
        a.contains("attacking 1")
    })
    .await;
    let events = run.process_end(1, 400).await;
    assert!(events.contains("done 1"), "{events}");
    let killed = run
        .within(PATIENCE, || async {
            (!run
                .server("execute if entity @e[type=minecraft:zombie]")
                .await
                .contains("passed"))
            .then_some(())
        })
        .await;
    assert!(
        killed.is_some(),
        "the attack ended but the zombie lives: {events} | {}",
        run.server("data get entity @e[type=minecraft:zombie,limit=1] Health")
            .await
    );
    let flags = run.server("anticheat violations Botty").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_shoots_a_stationary_zombie_dead_at_twenty_blocks_and_flags_nothing() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -16 140 -16 31 149 31 stone").await;
    for line in [
        "fill -16 150 -16 31 160 31 air",
        "gamerule spawn_mobs false",
        "gamerule spawn_monsters false",
        "kill @e[type=!minecraft:player]",
        "time set midnight",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    run.server("give Botty minecraft:bow").await;
    run.server("give Botty minecraft:arrow 64").await;
    run.server(
        "summon minecraft:zombie 20.5 150 0.5 {UUID:[I;0,0,0,1],NoAI:1b,PersistenceRequired:1b}",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.ask("shoot 1 00000000000000000000000000000001", |a| {
        a.contains("shooting 1")
    })
    .await;
    let mut events = String::new();
    for _ in 0..800 {
        events = run.say("events").await;
        if events.contains("done 1") || events.contains("failed 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(events.contains("done 1"), "{events}");
    run.eventually(|| async {
        (!run
            .server("execute if entity @e[type=minecraft:zombie]")
            .await
            .contains("passed"))
        .then_some(())
    })
    .await;
    let flags = run.server("anticheat violations Botty").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn use_item_throws_a_snowball_toward_an_entity_and_flags_nothing() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -16 140 -16 31 149 31 stone").await;
    for line in [
        "fill -16 150 -16 31 160 31 air",
        "gamerule spawn_mobs false",
        "gamerule spawn_monsters false",
        "kill @e[type=!minecraft:player]",
        "time set midnight",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    run.server("give Botty minecraft:snowball 16").await;
    run.server(
        "summon minecraft:zombie 8.5 150 0.5 {UUID:[I;0,0,0,1],NoAI:1b,PersistenceRequired:1b}",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.ask("useitem 1 snowball 00000000000000000000000000000001", |a| {
        a.contains("using 1")
    })
    .await;
    let mut events = String::new();
    for _ in 0..800 {
        events = run.say("events").await;
        if events.contains("done 1") || events.contains("failed 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(events.contains("done 1"), "{events}");
    let cleared = run.server("clear Botty minecraft:snowball 0").await;
    assert!(cleared.contains("15"), "{cleared}");
    let flags = run.server("anticheat violations Botty").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_flies_an_elytra_200_blocks_over_flat_ground_and_lands_at_the_goal() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -32 -16 31 255").await;
    run.fill_loaded("fill -24 149 -8 24 149 230 stone").await;
    for line in [
        "fill -24 150 -8 24 190 230 air",
        "gamerule spawn_mobs false",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Botty 0.5 150 0.5", |a| a.contains("spawned 1"))
        .await;
    run.botty_online(true).await;
    run.server("item replace entity Botty armor.chest with elytra")
        .await;
    run.server(
        "item replace entity Botty hotbar.0 with firework_rocket[fireworks={flight_duration:1}] 16",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.ask("fly 1 0 200", |a| a.contains("flying 1")).await;
    let mut events = String::new();
    for _ in 0..1600 {
        events = run.say("events").await;
        if events.contains("done 1") || events.contains("failed 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(events.contains("done 1"), "{events}");
    let state = run.ask("state 1", |a| a.starts_with("state 1 ")).await;
    let at = position(&state).unwrap_or_else(|| panic!("{state}"));
    assert!((at.2 - 200.5).abs() < 3.0 && at.0.abs() < 6.0, "{state}");
    let flags = run.server("anticheat violations").await;
    assert!(
        flags.contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_bridges_a_void_gap_and_breaks_the_wool_wall_in_its_way() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -3 149 -3 3 149 3 stone").await;
    for line in [
        "gamerule fall_damage false",
        "fill -3 150 -3 18 156 3 air",
        "fill 4 100 -3 9 149 3 air",
        "fill 10 149 -3 16 149 3 stone",
        "fill 12 150 -3 12 153 3 white_wool",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Builder 0 150 0", |a| a.contains("spawned 1"))
        .await;
    run.eventually(|| async { run.online().await.contains("Builder").then_some(()) })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    for line in ["clear Builder", "give Builder cobblestone 8"] {
        run.server(line).await;
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    run.ask("state 1", |a| {
        a.contains("alive=true") && a.contains("y=150")
    })
    .await;
    run.ask("goto 1 14 150 0", |a| a.contains("going 1")).await;
    let mut events = String::new();
    for _ in 0..1200 {
        events = run.say("events").await;
        if events.contains("done 1") || events.contains("failed 1") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(events.contains("done 1"), "{events}");
    for x in 4..=9 {
        assert!(run.block_is(x, 149, 0, "cobblestone").await, "x {x}");
    }
    assert!(run.block_is(12, 150, 0, "air").await);
    assert!(run.block_is(12, 151, 0, "air").await);
    assert!(run.block_is(12, 152, 0, "white_wool").await);
    let left = run.server("clear Builder cobblestone 0").await;
    assert!(left.contains('2'), "{left}");
    let flags = run.server("anticheat violations Builder").await;
    assert!(
        run.server("anticheat violations")
            .await
            .contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_harvests_and_replants_a_wheat_field() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.server("forceload add -16 -16 31 31").await;
    run.fill_loaded("fill -6 140 -6 6 149 6 stone").await;
    for line in [
        "fill -6 150 -6 6 160 6 air",
        "fill 1 149 1 3 149 3 farmland[moisture=7]",
        "setblock 0 149 2 water",
        "fill 1 150 1 3 150 3 wheat[age=7]",
        "gamerule natural_health_regeneration false",
    ] {
        run.server(line).await;
    }
    run.ask("spawnat Op 2 150 0", |a| a.contains("spawned 1"))
        .await;
    let op = tuff_core::world::fake::derive_uuid("bots", "Op", tuff_core::world::fake::Model::Wide);
    run.eventually(|| async {
        run.server("execute if entity @a[name=Op,x=2,y=149,z=0,distance=..3]")
            .await
            .contains("passed")
            .then_some(())
    })
    .await;
    run.eventually(|| async {
        run.op(op, "bots spawn Mate")
            .await
            .contains("Spawned Mate")
            .then_some(())
    })
    .await;
    run.eventually(|| async { run.online().await.contains("Mate").then_some(()) })
        .await;
    let given = run.server("give Mate wheat_seeds 16").await;
    assert!(given.contains("Gave"), "{given}");
    let said = run.op(op, "bots farm Mate 1 150 1 3 150 3").await;
    assert!(said.contains("farming"), "{said}");
    run.eventually(|| async {
        let list = run.op(op, "bots list").await;
        (list.contains("Mate: idle")
            && run
                .server("execute if block 2 150 2 wheat[age=7]")
                .await
                .contains("Test failed"))
        .then_some(())
    })
    .await;
    let mut replanted = 0;
    let mut ripe = 0;
    for (x, z) in (1..=3).flat_map(|x| (1..=3).map(move |z| (x, z))) {
        let ground = run
            .server(&format!("execute if block {x} 149 {z} farmland"))
            .await
            .contains("passed");
        let wheat = run
            .server(&format!("execute if block {x} 150 {z} wheat"))
            .await
            .contains("passed");
        let old = run
            .server(&format!("execute if block {x} 150 {z} wheat[age=7]"))
            .await
            .contains("passed");
        ripe += usize::from(old);
        replanted += usize::from(ground && wheat);
    }
    assert_eq!(ripe, 0, "ripe wheat left");
    assert!(replanted >= 5, "replanted {replanted} of 9");
    run.stop().await;
}

impl Run {
    async fn build_arena(&self) {
        self.server("forceload add -16 -16 31 31").await;
        self.fill_loaded("fill -3 149 -3 12 149 12 stone").await;
        self.server("fill -3 150 -3 12 160 12 air").await;
    }

    async fn operator_bot(&self, op_name: &str, at_z: i32, ordinal: u32, bot: &str) -> u128 {
        self.ask(&format!("spawnat {op_name} 0 150 {at_z}"), |a| {
            a.contains(&format!("spawned {ordinal}"))
        })
        .await;
        let op = tuff_core::world::fake::derive_uuid(
            "bots",
            op_name,
            tuff_core::world::fake::Model::Wide,
        );
        self.eventually(|| async {
            self.op(op, &format!("bots spawn {bot}"))
                .await
                .contains("Spawned")
                .then_some(())
        })
        .await;
        self.eventually(|| async { self.online().await.contains(bot).then_some(()) })
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.server(&format!("clear {bot}")).await;
        op
    }

    async fn build_and_wait(&self, op: u128, bot: &str, line: &str) {
        let reply = self.op(op, line).await;
        assert!(reply.contains("is building"), "{reply}");
        let idle = format!("{bot}: idle");
        let list = self
            .eventually(|| async {
                let list = self.op(op, "bots list").await;
                list.contains(&idle).then_some(list)
            })
            .await;
        assert!(!list.contains("failed"), "{list}");
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bots_build_a_hollow_cube_from_inventory_blocks() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.build_arena().await;
    let op = run.operator_bot("Op", 0, 1, "Mate").await;
    run.server("give Mate cobblestone 40").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.build_and_wait(op, "Mate", "bots build Mate box 3 3 3 cobblestone hollow")
        .await;
    for x in 2..=4 {
        for y in 150..=152 {
            for z in 0..=2 {
                let shell = (x, y, z) != (3, 151, 1);
                let want = if shell { "cobblestone" } else { "air" };
                assert!(run.block_is(x, y, z, want).await, "{x} {y} {z} {want}");
            }
        }
    }
    let left = run.server("clear Mate cobblestone 0").await;
    assert!(left.contains("14"), "{left}");
    run.stop().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bots_build_honours_stair_and_log_orientation() {
    let Some(run) = Run::start(true).await else {
        return;
    };
    run.build_arena().await;
    run.server("setblock 3 150 8 stone").await;
    let op = run.operator_bot("Op", 0, 1, "Mate").await;
    let other = run.operator_bot("Op2", 8, 3, "Mate2").await;
    run.server("give Mate oak_stairs 4").await;
    run.server("give Mate2 oak_log 4").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.build_and_wait(
        op,
        "Mate",
        "bots build Mate line z 1 oak_stairs[facing=east,half=bottom]",
    )
    .await;
    assert!(
        run.block_is(2, 150, 0, "oak_stairs[facing=east,half=bottom]")
            .await
    );
    run.build_and_wait(other, "Mate2", "bots build Mate2 line x 1 oak_log[axis=x]")
        .await;
    assert!(run.block_is(2, 150, 8, "oak_log[axis=x]").await);
    let flags = run.server("anticheat violations").await;
    assert!(
        flags.contains("No player has anti-cheat violations"),
        "{flags}"
    );
    run.stop().await;
}

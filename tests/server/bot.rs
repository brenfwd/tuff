use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::walk::Wander;
use tuff_bot::{Bot, BotConfig, FlyLine, Idle};
use tuff_world_model::pos::ChunkPos;

#[tuff_core::watchdog]
#[tokio::test]
async fn bot_joins_receives_chunks_and_follows_teleports() {
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

    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    handle
        .run_command("gamemode spectator Tester")
        .await
        .unwrap();

    let start = bot.state().position;
    bot.pursue(&mut FlyLine::new([0.0, 1.0], 120.0, 4.0, 1.0))
        .await
        .unwrap();
    bot.pursue(&mut Idle { ticks: 2 }).await.unwrap();
    let flown = bot.state().position;
    assert_eq!(flown, [start[0], 120.0, start[2] + 4.0]);

    let teleports = bot.state().teleports;
    handle.run_command("tp Tester 40 100 -40").await.unwrap();
    bot.wait_for(|state| state.teleports > teleports)
        .await
        .unwrap();
    assert_eq!(bot.state().position, [40.5, 100.0, -39.5]);
    bot.wait_loaded(2).await.unwrap();
    assert!(bot.state().loaded.contains(&ChunkPos::new(2, -3)));
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_metrics_follow_a_connected_player() {
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
    let metrics = handle.metrics().await.unwrap();
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();

    let mut updates = metrics.subscribe();
    let snapshot = loop {
        let snapshot = updates.borrow_and_update().clone();
        let listed = snapshot
            .tick
            .as_ref()
            .is_some_and(|tick| tick.players.iter().any(|p| p.name == "Tester"));
        let series = |name: &str| snapshot.series.get(name).map_or(0, |s| s.value);
        if listed
            && series("net.connections.play") == 1
            && series("net.bytes_out") > 0
            && series("net.packets_in") > 0
            && snapshot.world.chunks > 0
            && snapshot.manager.is_some()
            && snapshot.chunk_io.is_some()
        {
            break snapshot;
        }
        updates.changed().await.unwrap();
    };
    let tick = snapshot.tick.as_ref().unwrap();
    assert!(tick.ticks > 0 && tick.tps > 0.0, "{tick:?}");
    assert_eq!(tick.target_rate, 20.0);
    assert!(!snapshot.tick_stale);
    assert!(snapshot.world.regions >= 1);
    assert!(snapshot.manager.as_ref().unwrap().regions >= 1);
    let slowest = &snapshot.slowest_regions[0];
    assert_eq!(slowest.dimension, "minecraft:overworld");
    assert!(slowest.ticks > 0 && slowest.chunks > 0, "{slowest:?}");
    assert_eq!(snapshot.dropped_reports, 0);
    assert!(
        metrics
            .history("tick.tps", std::time::Duration::from_secs(60))
            .await
            .is_some_and(|points| !points.is_empty())
    );

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

async fn game_time(handle: &tuff::ServerHandle) -> i64 {
    let lines = handle.run_command("time query gametime").await.unwrap();
    lines[0].to_value()["with"][0]["text"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_server_still_loads_chunks_and_runs_commands() {
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
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    handle.run_command("tick freeze").await.unwrap();
    let frozen_at = game_time(&handle).await;

    let teleports = bot.state().teleports;
    handle.run_command("tp Tester 200 100 -200").await.unwrap();
    bot.wait_for(|state| state.teleports > teleports)
        .await
        .unwrap();
    bot.wait_loaded(2).await.unwrap();
    assert!(bot.state().loaded.contains(&ChunkPos::new(12, -13)));
    let listed = handle.run_command("list").await.unwrap();
    assert_eq!(listed[0].to_value()["with"][0]["text"], "1");
    assert_eq!(
        game_time(&handle).await,
        frozen_at,
        "the game stayed frozen"
    );
    assert!(bot.state().disconnected.is_none());

    handle.run_command("tick unfreeze").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while game_time(&handle).await == frozen_at {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unfreezing resumes the game");
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_and_walking_bots_survive_the_floating_kick() {
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
    let connect = |name: &'static str| {
        Bot::connect(
            addr,
            BotConfig {
                view_distance: 2,
                ..BotConfig::new(name)
            },
        )
    };
    let idler = connect("Idler").await.unwrap();
    let walker = connect("Walker").await.unwrap();
    idler.wait_loaded(2).await.unwrap();
    walker.wait_loaded(2).await.unwrap();

    let mut idle_goal = Idle { ticks: 150 };
    let mut wander = Wander::new([0.0, 0.0], 8.0, 7);
    let (idle, walk) = tokio::join!(
        idler.pursue(&mut idle_goal),
        tokio::time::timeout(
            std::time::Duration::from_secs(9),
            walker.pursue(&mut wander)
        ),
    );
    idle.unwrap();
    assert!(walk.is_err() || walk.unwrap().is_ok());
    assert!(idler.state().disconnected.is_none());
    assert!(walker.state().disconnected.is_none());

    drop((idler, walker));
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn spectate_command_checks_mode_and_target() {
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
    let mut bots = Vec::new();
    for name in ["Tester", "Other"] {
        let bot = Bot::connect(
            addr,
            BotConfig {
                view_distance: 2,
                ..BotConfig::new(name)
            },
        )
        .await
        .unwrap();
        bot.wait_for(|state| state.chunks > 0).await.unwrap();
        bots.push(bot);
    }
    let key = |lines: Vec<tuff_item::text::TextComponent>| {
        lines[0].to_value()["translate"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };

    let lines = handle.run_command("spectate Other Tester").await.unwrap();
    assert_eq!(key(lines), "commands.spectate.not_spectator");
    handle
        .run_command("gamemode spectator Tester")
        .await
        .unwrap();
    let lines = handle.run_command("spectate Tester Tester").await.unwrap();
    assert_eq!(key(lines), "commands.spectate.self");
    let lines = handle.run_command("spectate Other Tester").await.unwrap();
    assert_eq!(key(lines), "commands.spectate.success.started");
    let lines = handle.run_command("spectate").await.unwrap();
    assert_eq!(key(lines), "permissions.requires.player");

    let lines = handle.run_command("posteffect list Tester").await.unwrap();
    assert_eq!(key(lines), "commands.posteffect.list.empty");
    let lines = handle
        .run_command("posteffect add Tester minecraft:invert")
        .await
        .unwrap();
    assert_eq!(key(lines), "commands.posteffect.add.success.single");
    let lines = handle
        .run_command("posteffect add Tester minecraft:invert")
        .await
        .unwrap();
    assert_eq!(key(lines), "commands.posteffect.add.failed");
    let lines = handle.run_command("posteffect list Tester").await.unwrap();
    assert_eq!(key(lines), "commands.posteffect.list.success");
    let lines = handle.run_command("posteffect clear Tester").await.unwrap();
    assert_eq!(key(lines), "commands.posteffect.clear.success.single");
    let lines = handle.run_command("posteffect clear Tester").await.unwrap();
    assert_eq!(key(lines), "commands.posteffect.clear.failed");

    let lines = handle
        .run_command("dialog show Tester minecraft:server_links")
        .await
        .unwrap();
    assert_eq!(key(lines), "commands.dialog.show.single");
    let lines = handle
        .run_command("dialog show Tester minecraft:no_such_dialog")
        .await
        .unwrap();
    assert_eq!(key(lines), "argument.resource_or_id.no_such_element");
    let lines = handle.run_command("dialog clear Tester").await.unwrap();
    assert_eq!(key(lines), "commands.dialog.clear.single");

    drop(bots);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_shows_team_decorated_names() {
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
    let bot = Bot::connect(addr, BotConfig::new("Tester")).await.unwrap();
    loop {
        let listed = handle.run_command("list").await.unwrap();
        if listed[0].with[2].plain().contains("Tester") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    for line in [
        "team add crew",
        "team modify crew color red",
        "team modify crew prefix \"[C] \"",
        "team join crew Tester",
    ] {
        handle.run_command(line).await.unwrap();
    }
    let listed = handle.run_command("list").await.unwrap();
    let value = listed[0].to_value();
    let names = &value["with"][2];
    assert_eq!(listed[0].with[2].plain(), "[C] Tester");
    assert_eq!(names["color"], "red", "{value}");

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

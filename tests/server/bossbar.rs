use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::packet::play::Clientbound as Id;

struct Running {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

async fn start(instance: &TempDir) -> Running {
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
    Running { handle, task, addr }
}

async fn stop(server: Running) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn run(server: &Running, line: &str) -> Vec<TextComponent> {
    server.handle.run_command(line).await.unwrap()
}

fn key(lines: &[TextComponent]) -> &str {
    lines
        .first()
        .and_then(|line| line.translate.as_deref())
        .unwrap_or_default()
}

fn arg(lines: &[TextComponent], index: usize) -> String {
    lines[0].with[index].plain()
}

async fn connect(server: &Running, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

fn boss_events(bot: &Bot) -> u64 {
    bot.with_state(|state| {
        state
            .received
            .get(&Id::BossEvent.id())
            .copied()
            .unwrap_or(0)
    })
}

async fn wait_boss_events(bot: &Bot, count: u64) {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bot.wait_for(|state| {
            state
                .received
                .get(&Id::BossEvent.id())
                .copied()
                .unwrap_or(0)
                >= count
        }),
    )
    .await
    .expect("the boss events arrive")
    .unwrap();
}

/// The value a command returns, read back through `execute store`.
async fn result_of(server: &Running, line: &str) -> String {
    run(server, "scoreboard objectives add r dummy").await;
    run(
        server,
        &format!("execute store result score #r r run {line}"),
    )
    .await;
    arg(&run(server, "scoreboard players get #r r").await, 1)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bossbar_commands_answer_like_the_game_and_reach_their_players() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let bot = connect(&server, "Tester").await;

    assert_eq!(
        key(&run(&server, "bossbar list").await),
        "commands.bossbar.list.bars.none"
    );
    let added = run(&server, "bossbar add fight \"Fight\"").await;
    assert_eq!(key(&added), "commands.bossbar.create.success");
    assert_eq!(arg(&added, 0), "[Fight]");
    assert_eq!(
        key(&run(&server, "bossbar add minecraft:fight \"Again\"").await),
        "commands.bossbar.create.failed"
    );
    assert_eq!(result_of(&server, "bossbar add other \"Other\"").await, "2");
    let listed = run(&server, "bossbar list").await;
    assert_eq!(key(&listed), "commands.bossbar.list.bars.some");
    assert_eq!(arg(&listed, 0), "2");
    assert_eq!(arg(&listed, 1), "[Fight], [Other]");
    assert_eq!(
        key(&run(&server, "bossbar get nope value").await),
        "commands.bossbar.unknown"
    );

    let shown = run(&server, "bossbar set fight players Tester").await;
    assert_eq!(key(&shown), "commands.bossbar.set.players.success.some");
    assert_eq!(arg(&shown, 1), "1");
    assert_eq!(arg(&shown, 2), "Tester");
    wait_boss_events(&bot, 1).await;
    assert_eq!(
        key(&run(&server, "bossbar set fight players @a").await),
        "commands.bossbar.set.players.unchanged"
    );
    let players = run(&server, "bossbar get fight players").await;
    assert_eq!(key(&players), "commands.bossbar.get.players.some");
    assert_eq!(result_of(&server, "bossbar get fight players").await, "1");

    let value = run(&server, "bossbar set fight value 30").await;
    assert_eq!(key(&value), "commands.bossbar.set.value.success");
    assert_eq!(arg(&value, 1), "30");
    wait_boss_events(&bot, 2).await;
    assert_eq!(
        key(&run(&server, "bossbar set fight value 30").await),
        "commands.bossbar.set.value.unchanged"
    );
    assert_eq!(result_of(&server, "bossbar get fight value").await, "30");
    assert_eq!(result_of(&server, "bossbar set fight max 60").await, "60");
    assert_eq!(result_of(&server, "bossbar get fight max").await, "60");
    assert_eq!(
        key(&run(&server, "bossbar set fight color red").await),
        "commands.bossbar.set.color.success"
    );
    assert_eq!(
        key(&run(&server, "bossbar set fight color red").await),
        "commands.bossbar.set.color.unchanged"
    );
    assert_eq!(
        key(&run(&server, "bossbar set fight style notched_6").await),
        "commands.bossbar.set.style.success"
    );
    assert_eq!(
        key(&run(&server, "bossbar set fight name \"Final\"").await),
        "commands.bossbar.set.name.success"
    );
    assert_eq!(
        key(&run(&server, "bossbar set fight name \"Final\"").await),
        "commands.bossbar.set.name.unchanged"
    );
    let named = run(&server, "bossbar get fight visible").await;
    assert_eq!(key(&named), "commands.bossbar.get.visible.visible");
    assert_eq!(arg(&named, 0), "[Final]");
    wait_boss_events(&bot, 6).await;

    assert_eq!(
        result_of(&server, "bossbar set fight visible false").await,
        "0"
    );
    wait_boss_events(&bot, 7).await;
    assert_eq!(
        key(&run(&server, "bossbar set fight visible false").await),
        "commands.bossbar.set.visibility.unchanged.hidden"
    );
    assert_eq!(result_of(&server, "bossbar get fight visible").await, "0");
    let hidden_changes = boss_events(&bot);
    run(&server, "bossbar set fight value 5").await;
    run(&server, "bossbar set fight visible true").await;
    wait_boss_events(&bot, hidden_changes + 1).await;
    run(&server, "scoreboard players set #r r 0").await;
    assert_eq!(
        boss_events(&bot),
        hidden_changes + 1,
        "hidden bars stay quiet"
    );

    run(&server, "scoreboard players set #seven r 7").await;
    run(
        &server,
        "execute store result bossbar fight value run scoreboard players get #seven r",
    )
    .await;
    assert_eq!(result_of(&server, "bossbar get fight value").await, "7");
    run(
        &server,
        "execute store success bossbar minecraft:fight max run scoreboard players get #seven r",
    )
    .await;
    assert_eq!(result_of(&server, "bossbar get fight max").await, "1");
    assert_eq!(
        key(&run(&server, "execute store result bossbar nope max run seed").await),
        "commands.bossbar.unknown"
    );
    let emptied = run(&server, "bossbar set fight players").await;
    assert_eq!(key(&emptied), "commands.bossbar.set.players.success.none");
    wait_boss_events(&bot, hidden_changes + 4).await;
    assert_eq!(
        key(&run(&server, "bossbar get fight players").await),
        "commands.bossbar.get.players.none"
    );
    let removed = run(&server, "bossbar remove fight").await;
    assert_eq!(key(&removed), "commands.bossbar.remove.success");
    assert_eq!(arg(&removed, 0), "[Final]");
    assert_eq!(result_of(&server, "bossbar remove other").await, "0");
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bars_are_saved_and_shown_again_when_their_players_return() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let bot = connect(&server, "Tester").await;
    run(&server, "bossbar add custom:kept \"Kept\"").await;
    run(&server, "bossbar set custom:kept max 10").await;
    run(&server, "bossbar set custom:kept value 4").await;
    run(&server, "bossbar set custom:kept players Tester").await;
    wait_boss_events(&bot, 1).await;
    drop(bot);
    stop(server).await;
    assert!(
        instance
            .path()
            .join("world/data/minecraft/custom_boss_events.dat")
            .is_file()
    );

    let server = start(&instance).await;
    let value = run(&server, "bossbar get custom:kept value").await;
    assert_eq!(arg(&value, 1), "4");
    assert_eq!(
        key(&run(&server, "bossbar get custom:kept players").await),
        "commands.bossbar.get.players.none"
    );
    let bot = connect(&server, "Tester").await;
    wait_boss_events(&bot, 1).await;
    let players = run(&server, "bossbar get custom:kept players").await;
    assert_eq!(key(&players), "commands.bossbar.get.players.some");
    let other = connect(&server, "Other").await;
    run(&server, "bossbar set custom:kept value 5").await;
    wait_boss_events(&bot, 2).await;
    assert_eq!(boss_events(&other), 0);
    drop(other);
    drop(bot);
    stop(server).await;
}

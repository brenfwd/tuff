use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::protocol::play::Serverbound;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::packet::play::Clientbound as Id;

struct Running {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
}

async fn start(instance: &TempDir, overrides: &[(&str, &str)]) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(3)
        .seed(1)
        .config_overrides(
            overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .chain([("tuff.offlineOps".to_string(), "true".to_string())])
                .collect(),
        )
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    Running { handle, task, addr }
}

async fn run(server: &Running, line: &str) -> Vec<TextComponent> {
    server.handle.run_command(line).await.unwrap()
}

async fn connect(server: &Running) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 3,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot.wait_loaded(1).await.unwrap();
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

async fn filled(server: &Running, block: &str) {
    let line = format!("execute at Tester if block ~ 100 ~ {block}");
    for _ in 0..200 {
        let answer = run(server, &line).await;
        if answer.first().and_then(|l| l.translate.as_deref())
            == Some("commands.execute.conditional.pass")
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the fill never landed");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bossbar_shows_only_above_the_vanilla_size_and_the_limit_is_configurable() {
    let instance = TempDir::new();
    let server = start(&instance, &[]).await;
    run(&server, "gamerule max_block_modifications 2147483647").await;
    let bot = connect(&server).await;
    run(&server, "op Tester").await;

    bot.send(Serverbound::ChatCommand(
        "fill ~-15 100 ~-15 ~16 131 ~16 glass".into(),
    ));
    filled(&server, "glass").await;
    assert_eq!(boss_events(&bot), 0, "32768 blocks is the vanilla limit");

    bot.send(Serverbound::ChatCommand(
        "fill ~-16 100 ~-16 ~16 131 ~16 gold_block".into(),
    ));
    filled(&server, "gold_block").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bot.wait_for(|state| {
            state
                .received
                .get(&Id::BossEvent.id())
                .copied()
                .unwrap_or(0)
                >= 3
        }),
    )
    .await
    .expect("the bar is added, advanced and removed")
    .unwrap();

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_limit_follows_the_game_rule() {
    let instance = TempDir::new();
    let server = start(&instance, &[]).await;
    let big = run(&server, "fill 0 0 0 100 100 100 stone").await;
    assert_eq!(big[0].translate.as_deref(), Some("commands.fill.toobig"));
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clone_rejects_above_the_default_limit() {
    let instance = TempDir::new();
    let server = start(&instance, &[]).await;
    let big = run(&server, "clone 0 0 0 100 100 100 200 0 0").await;
    assert_eq!(big[0].translate.as_deref(), Some("commands.clone.toobig"));
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clone_bar_shows_only_above_the_vanilla_size_when_the_limit_is_lifted() {
    let instance = TempDir::new();
    let server = start(&instance, &[]).await;
    run(&server, "gamerule max_block_modifications 2147483647").await;
    let bot = connect(&server).await;
    run(&server, "op Tester").await;

    bot.send(Serverbound::ChatCommand(
        "clone ~-15 100 ~-15 ~16 131 ~16 ~-15 140 ~-15".into(),
    ));
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(boss_events(&bot), 0, "32768 blocks is the vanilla limit");

    bot.send(Serverbound::ChatCommand(
        "clone ~-16 100 ~-16 ~16 131 ~16 ~-16 140 ~-16".into(),
    ));
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bot.wait_for(|state| {
            state
                .received
                .get(&Id::BossEvent.id())
                .copied()
                .unwrap_or(0)
                >= 3
        }),
    )
    .await
    .expect("the bar is added, advanced and removed")
    .unwrap();
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fill_across_regions_needs_loaded_chunks_then_changes_every_block() {
    let instance = TempDir::new();
    let server = start(&instance, &[]).await;
    let rejected = run(&server, "fill -300 100 -2 300 100 2 stone").await;
    assert_eq!(
        rejected[0].translate.as_deref(),
        Some("argument.pos.unloaded")
    );
    run(&server, "forceload add -300 -2 300 2").await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let answer = run(&server, "fill -300 100 -2 300 100 2 stone").await;
    assert_eq!(
        answer[0].translate.as_deref(),
        Some("commands.fill.success")
    );
    let count = answer[0].with[0].plain();
    assert_eq!(count, (601 * 5).to_string());
    for x in [-300, -257, -1, 0, 256, 300] {
        let line = format!("execute if block {x} 100 2 stone");
        let check = run(&server, &line).await;
        assert_eq!(
            check[0].translate.as_deref(),
            Some("commands.execute.conditional.pass"),
            "x={x}"
        );
    }
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

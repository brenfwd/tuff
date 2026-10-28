use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_vanilla_data::packet::play::Clientbound;

async fn join(
    offline_ops: bool,
) -> (
    tuff_core::ServerHandle,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    Bot,
    TempDir,
) {
    let instance = TempDir::new();
    let overrides = if offline_ops {
        vec![("tuff.offlineOps".to_string(), "true".to_string())]
    } else {
        Vec::new()
    };
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .config_overrides(overrides)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            keep: vec![Clientbound::Commands.id()],
            ..BotConfig::new("Smoke")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    (handle, task, bot, instance)
}

fn lists_gamemode(bot: &Bot) -> bool {
    let state = bot.state();
    let tree = state.last.get(&Clientbound::Commands.id()).unwrap();
    tree.windows(8).any(|w| w == b"gamemode")
}

fn commands_received(bot: &Bot) -> u64 {
    bot.state()
        .received
        .get(&Clientbound::Commands.id())
        .copied()
        .unwrap_or(0)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn an_operator_gets_the_gamemode_command_in_a_resent_tree() {
    let (handle, task, bot, _instance) = join(true).await;
    assert!(!lists_gamemode(&bot));
    let before = commands_received(&bot);
    handle.run_command("op Smoke").await.unwrap();
    bot.wait_for(|state| {
        state
            .received
            .get(&Clientbound::Commands.id())
            .copied()
            .unwrap_or(0)
            > before
    })
    .await
    .unwrap();
    assert!(lists_gamemode(&bot));
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn offline_ops_stay_unprivileged_without_the_offline_ops_setting() {
    let (handle, task, bot, _instance) = join(false).await;
    handle.run_command("op Smoke").await.unwrap();
    let chats = |bot: &Bot| {
        bot.state()
            .received
            .get(&Clientbound::SystemChat.id())
            .copied()
            .unwrap_or(0)
    };
    let before = chats(&bot);
    handle.run_command("tellraw Smoke \"sync\"").await.unwrap();
    bot.wait_for(|state| {
        state
            .received
            .get(&Clientbound::SystemChat.id())
            .copied()
            .unwrap_or(0)
            > before
    })
    .await
    .unwrap();
    assert!(!lists_gamemode(&bot));
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

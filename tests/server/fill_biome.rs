use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::packet::play::Clientbound as Id;

struct Running {
    handle: tuff::ServerHandle,
    addr: std::net::SocketAddr,
}

async fn start(instance: &TempDir) -> Running {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(3)
        .seed(1)
        .build();
    let handle = server.handle();
    tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    Running { handle, addr }
}

async fn run(server: &Running, line: &str) -> Vec<TextComponent> {
    server.handle.run_command(line).await.unwrap()
}

fn key(lines: &[TextComponent]) -> Option<&str> {
    lines.first().and_then(|l| l.translate.as_deref())
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fillbiome_replaces_filters_and_rejects_unloaded() {
    let instance = TempDir::new();
    let server = start(&instance).await;
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
    let seen = |bot: &Bot| {
        bot.with_state(|state| {
            state
                .received
                .get(&Id::ChunksBiomes.id())
                .copied()
                .unwrap_or(0)
        })
    };

    let far = run(
        &server,
        "fillbiome 100000 64 0 100003 64 0 minecraft:desert",
    )
    .await;
    assert_eq!(key(&far), Some("argument.pos.unloaded"));

    let skip = run(
        &server,
        "execute at Tester run fillbiome ~ 64 ~ ~7 70 ~7 minecraft:desert replace minecraft:the_void",
    )
    .await;
    assert_eq!(key(&skip), Some("commands.fillbiome.no_changes"));
    assert_eq!(seen(&bot), 0);

    let done = run(
        &server,
        "execute at Tester run fillbiome ~ 64 ~ ~7 70 ~7 minecraft:desert",
    )
    .await;
    assert_eq!(key(&done), Some("commands.fillbiome.success.count"));
    for (biome, expected) in [
        ("minecraft:desert", "commands.execute.conditional.pass"),
        (
            "#minecraft:is_overworld",
            "commands.execute.conditional.pass",
        ),
        ("minecraft:the_void", "commands.execute.conditional.fail"),
    ] {
        let line = format!("execute at Tester if biome ~ 64 ~ {biome}");
        assert_eq!(key(&run(&server, &line).await), Some(expected), "{biome}");
    }
    let far = run(&server, "execute if biome 100000 64 0 minecraft:desert").await;
    assert_eq!(key(&far), Some("commands.execute.conditional.fail"));
    bot.wait_for(|state| {
        state
            .received
            .get(&Id::ChunksBiomes.id())
            .copied()
            .unwrap_or(0)
            > 0
    })
    .await
    .unwrap();

    let again = run(
        &server,
        "execute at Tester run fillbiome ~ 64 ~ ~7 70 ~7 minecraft:desert",
    )
    .await;
    assert_eq!(key(&again), Some("commands.fillbiome.no_changes"));
}

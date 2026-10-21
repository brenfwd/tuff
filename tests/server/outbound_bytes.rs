use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};

fn server(instance: &TempDir, view_distance: i32, overrides: Vec<(String, String)>) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(view_distance)
        .seed(1)
        .config_overrides(overrides)
        .build()
}

fn noise(len: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            char::from(b'a' + ((state >> 59) as u8) % 26)
        })
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_view_distance_join_fits_the_default_outbound_cap() {
    let instance = TempDir::new();
    let server = server(&instance, 32, Vec::new());
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 32,
            ..BotConfig::new("Tester")
        },
    )
    .await
    .unwrap();
    bot.wait_loaded(32).await.unwrap();
    let total = bot.state().bytes;
    eprintln!("join at view distance 32 sent {total} bytes");
    let cap = tuff::config::DEFAULT_MAX_OUTBOUND_BYTES as u64;
    assert!(
        total * 2 <= cap,
        "{total} bytes leaves under 2x margin of {cap}"
    );
    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_client_is_removed_through_the_leave_path() {
    let instance = TempDir::new();
    let server = server(
        &instance,
        8,
        vec![("tuff.net.maxOutboundBytes".into(), "65536".into())],
    );
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 8,
            ..BotConfig::new("Stalled")
        },
    )
    .await
    .unwrap();
    bot.stop_reading();
    let spam = format!("tellraw Stalled \"{}\"", noise(20_000));
    let started = std::time::Instant::now();
    for step in 0..2000 {
        let _ = handle.run_command(spam.clone()).await;
        if step % 5 != 0 {
            continue;
        }
        let listed = handle.run_command("list").await.unwrap();
        if listed[0].with[0].text == "0" {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "left by keep-alive timeout, not the outbound cap"
            );
            drop(bot);
            handle.shutdown().await.unwrap();
            task.await.unwrap().unwrap();
            return;
        }
    }
    panic!("the stalled client was never disconnected");
}

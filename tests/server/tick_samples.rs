use crate::common;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_item::text::TextComponent;

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
        .config_overrides(vec![("tuff.offlineOps".into(), "true".into())])
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

async fn subscriber(
    server: &Running,
    name: &str,
) -> (
    Bot,
    tokio::sync::mpsc::UnboundedReceiver<tuff_bot::TickSample>,
) {
    let (samples, received) = tokio::sync::mpsc::unbounded_channel();
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            tick_samples: Some(samples),
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    (bot, received)
}

async fn next_sample(
    samples: &mut tokio::sync::mpsc::UnboundedReceiver<tuff_bot::TickSample>,
) -> tuff_bot::TickSample {
    tokio::time::timeout(std::time::Duration::from_secs(10), samples.recv())
        .await
        .expect("a tick sample arrives")
        .unwrap()
}

async fn none_within_a_second(
    samples: &mut tokio::sync::mpsc::UnboundedReceiver<tuff_bot::TickSample>,
) {
    while samples.try_recv().is_ok() {}
    let late = tokio::time::timeout(std::time::Duration::from_secs(1), samples.recv()).await;
    assert!(late.is_err(), "no samples without the permission");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tick_time_samples_reach_only_permitted_subscribers() {
    let instance = TempDir::new();
    let server = start(&instance).await;
    let (_op, mut op_samples) = subscriber(&server, "Op").await;
    let (_plain, mut plain_samples) = subscriber(&server, "Plain").await;
    none_within_a_second(&mut op_samples).await;

    run(&server, "op Op").await;
    let sample = next_sample(&mut op_samples).await;
    assert!(sample.full_ns > 0);
    assert!(sample.tick_ns <= sample.full_ns);
    assert_eq!(
        sample.full_ns,
        sample.tick_ns + sample.tasks_ns + sample.idle_ns
    );
    none_within_a_second(&mut plain_samples).await;

    run(&server, "tuff perm grant Plain tuff.debug.tick_time").await;
    next_sample(&mut plain_samples).await;
    run(&server, "tuff perm deny Op tuff.debug.tick_time").await;
    none_within_a_second(&mut op_samples).await;
    run(&server, "deop Op").await;
    run(&server, "tuff perm revoke Op tuff.debug.tick_time").await;
    none_within_a_second(&mut op_samples).await;
    next_sample(&mut plain_samples).await;
    stop(server).await;
}

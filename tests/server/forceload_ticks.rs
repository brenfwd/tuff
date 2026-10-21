//! A forceloaded chunk runs its block ticks without loaded neighbours.

use crate::common;

use common::TempDir;
use tuff::Server;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lone_forceloaded_chunk_runs_block_ticks() {
    let instance = TempDir::new();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .binds(vec!["127.0.0.1:0".to_string()])
        .instance_dir(instance.path().to_path_buf())
        .seed(7)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    handle.run_command("forceload add 100 100").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    for line in [
        "setblock 100 249 100 minecraft:stone",
        "setblock 100 250 100 minecraft:cactus",
        "setblock 100 249 100 minecraft:air",
    ] {
        handle.run_command(line).await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let out = handle
        .run_command("execute if block 100 250 100 minecraft:air")
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let text: Vec<String> = out
        .iter()
        .map(tuff_item::text::TextComponent::plain)
        .collect();
    assert!(text.join(" ").contains("Test passed"), "{text:?}");
}

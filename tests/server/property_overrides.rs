use crate::common;

use common::TempDir;
use tuff::Server;

fn server(instance: &TempDir, overrides: &[(&str, &str)]) -> Server {
    server_seeded(instance, overrides, Some(1))
}

fn server_seeded(instance: &TempDir, overrides: &[(&str, &str)], seed: Option<i64>) -> Server {
    let builder = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(3)
        .maybe_seed(seed)
        .config_overrides(
            overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
    builder.build()
}

async fn run_and_stop(server: Server, commands: &[&str]) -> i64 {
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    for line in commands {
        handle.run_command(*line).await.unwrap();
    }
    let seed = handle.world_shared().await.unwrap().seed;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    seed
}

#[tuff_core::watchdog]
#[tokio::test]
async fn property_override_wins_and_is_not_written_back() {
    let instance = TempDir::new();
    let file = instance.path().join("server.properties");
    std::fs::write(&file, "enable-code-of-conduct=true\n").unwrap();
    let before = std::fs::read_to_string(&file).unwrap();
    let server = server(&instance, &[("minecraft.enable-code-of-conduct", "false")]);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bad_property_overrides_refuse_startup() {
    for (key, value) in [("minecraft.hardcore", "maybe"), ("minecraft.nope", "1")] {
        let instance = TempDir::new();
        let result = tokio::spawn(server(&instance, &[(key, value)]).run())
            .await
            .unwrap();
        assert!(result.is_err(), "{key}={value}");
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn an_override_is_not_saved_when_another_property_changes() {
    let instance = TempDir::new();
    let file = instance.path().join("server.properties");
    std::fs::write(&file, "motd=Original\n").unwrap();
    let server = server(&instance, &[("motd", "Temporary")]);
    run_and_stop(server, &["whitelist on"]).await;
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("motd=Original"), "{saved}");
    assert!(!saved.contains("Temporary"), "{saved}");
    assert!(saved.contains("white-list=true"), "{saved}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_creation_option_applies_only_to_a_new_world() {
    let instance = TempDir::new();
    let file = instance.path().join("server.properties");
    let created = run_and_stop(
        server_seeded(&instance, &[("level-seed", "424242")], None),
        &["whitelist on"],
    )
    .await;
    assert_eq!(created, 424242);
    let saved = std::fs::read_to_string(&file).unwrap_or_default();
    assert!(!saved.contains("424242"), "{saved}");
    let reopened = run_and_stop(server_seeded(&instance, &[("level-seed", "7")], None), &[]).await;
    assert_eq!(reopened, 424242);
}

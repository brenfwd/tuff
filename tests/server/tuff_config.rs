use crate::common;

use common::{TempDir, test_png};
use tuff::Server;
use tuff::status::Favicon;
use tuff_storage::sqlite;

fn vanilla_server(instance: &TempDir, overrides: &[(&str, &str)]) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .favicon(Favicon::from_png(&test_png()).unwrap())
        .whitelist(false)
        .config_overrides(
            overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .build()
}

async fn run_once(server: Server, lines: &[&str]) -> Vec<String> {
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    let mut out = Vec::new();
    for line in lines {
        for text in handle.run_command(*line).await.unwrap() {
            out.push(text.plain());
        }
    }
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    out
}

#[tuff_core::watchdog]
#[tokio::test]
async fn settings_persist_across_restarts_and_keep_the_file_tidy() {
    let instance = TempDir::new();
    let path = instance.path().join("tuff.properties");
    std::fs::write(
        &path,
        "# hand written\nother.plugin.key=keep me\ntuff.http.port=7000\n",
    )
    .unwrap();

    let shown = run_once(
        vanilla_server(&instance, &[]),
        &[
            "tuff config tuff.http.port",
            "tuff config tuff.http.port 8123",
            "tuff config tuff.http.host ::",
        ],
    )
    .await;
    assert!(shown[0].contains("tuff.http.port = 7000"), "{shown:?}");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# hand written\n"), "{text}");
    assert!(text.contains("other.plugin.key=keep me\n"), "{text}");
    assert!(text.contains("tuff.http.port=8123\n"), "{text}");
    assert!(text.contains("tuff.http.host=::\n"), "{text}");

    let again = run_once(
        vanilla_server(&instance, &[]),
        &[
            "tuff config tuff.http.port",
            "tuff config tuff.http.host 0.0.0.0",
        ],
    )
    .await;
    assert!(again[0].contains("tuff.http.port = 8123"), "{again:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("tuff.http.host"), "back to default: {text}");
    assert!(text.contains("other.plugin.key=keep me\n"), "{text}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn overrides_apply_for_the_run_without_being_saved() {
    let instance = TempDir::new();
    let path = instance.path().join("tuff.properties");
    std::fs::write(&path, "tuff.http.port=7000\n").unwrap();

    let shown = run_once(
        vanilla_server(
            &instance,
            &[("tuff.http.port", "1"), ("tuff.http.port", "9001")],
        ),
        &[
            "tuff config tuff.http.port",
            "tuff config tuff.http.host ::1",
        ],
    )
    .await;
    assert!(shown[0].contains("tuff.http.port = 9001"), "{shown:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("tuff.http.port=7000\n"), "{text}");
    assert!(text.contains("tuff.http.host=::1\n"), "{text}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bad_overrides_fail_startup_before_binding() {
    let instance = TempDir::new();
    for (key, value) in [("tuff.nope", "1"), ("tuff.http.port", "banana")] {
        let err = vanilla_server(&instance, &[(key, value)])
            .run()
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains(key), "{message}");
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_server_file_keeps_settings_in_its_own_table() {
    let dir = TempDir::new();
    let file = dir.path().join("main.mcserver");
    let build = || {
        Server::builder()
            .features(tuff::vanilla_features::all())
            .binds(vec!["127.0.0.1:0".to_string()])
            .storage_backend(sqlite::NAME)
            .world_path(file.clone())
            .whitelist(false)
            .build()
    };
    run_once(build(), &["tuff config tuff.http.port 4242"]).await;
    let shown = run_once(build(), &["tuff config tuff.http.port"]).await;
    assert!(shown[0].contains("tuff.http.port = 4242"), "{shown:?}");
    assert!(!dir.path().join("tuff.properties").exists());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_shipped_server_accepts_and_never_saves_its_features() {
    let instance = TempDir::new();
    let enabled = vec![tuff_feature_builtin_moving_block_entities::ID.to_owned()];
    let enabled_key = tuff::features::enabled_key(tuff_feature_builtin_moving_block_entities::ID);
    let server = Server::builder()
        .features(tuff::features::shipped(&enabled).unwrap())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .favicon(Favicon::from_png(&test_png()).unwrap())
        .whitelist(false)
        .config_overrides(vec![(enabled_key.clone(), "true".to_owned())])
        .build();
    let shown = run_once(
        server,
        &[
            &format!("tuff config {enabled_key}"),
            "tuff config tuff.http.port 8123",
        ],
    )
    .await;
    assert!(
        shown[0].contains(&format!(
            "{} = true",
            tuff::features::enabled_key(tuff_feature_builtin_moving_block_entities::ID)
        )),
        "{shown:?}"
    );
    let saved = std::fs::read_to_string(instance.path().join("tuff.properties")).unwrap();
    assert!(saved.contains("tuff.http.port=8123"), "{saved}");
    assert!(!saved.contains(&enabled_key), "{saved}");
}

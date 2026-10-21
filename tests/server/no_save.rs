use crate::common;

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::TempDir;
use tuff::Server;
use tuff_storage::{sqlite, vanilla};

const EDITS: &[&str] = &[
    "setblock 0 100 0 minecraft:gold_block",
    "forceload add 64 64",
    "op Someone",
    "whitelist add Someone",
    "tuff config tuff.http.port 4242",
    "save-all flush",
];

fn server(instance: &Path, backend: &str, world: Option<PathBuf>, no_save: bool) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .whitelist(false)
        .storage_backend(backend)
        .maybe_world_path(world)
        .seed(7)
        .no_save(no_save)
        .build()
}

async fn run(server: Server, lines: &[&str]) -> Vec<String> {
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    if handle.local_addrs().await.is_err() {
        panic!("the server stopped: {:#}", task.await.unwrap().unwrap_err());
    }
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

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                pending.push(path.clone());
            }
            let bytes = if meta.is_file() {
                let mut hasher = DefaultHasher::new();
                std::fs::read(&path).unwrap().hash(&mut hasher);
                hasher.finish()
            } else {
                0
            };
            files.insert(
                path.strip_prefix(dir).unwrap().to_path_buf(),
                (bytes, meta.modified().unwrap()),
            );
        }
    }
    files
}

async fn existing_world_is_untouched(backend: &str, world: Option<PathBuf>) {
    let instance = TempDir::new();
    let world = world.map(|name| instance.path().join(name));
    run(
        server(instance.path(), backend, world.clone(), false),
        EDITS,
    )
    .await;
    let before = snapshot(instance.path());
    assert!(!before.is_empty());
    let out = run(server(instance.path(), backend, world, true), EDITS).await;
    assert!(
        out.iter().any(|line| line.contains("tuff.http.port")),
        "{out:?}"
    );
    assert_eq!(snapshot(instance.path()), before);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_existing_vanilla_world_is_read_but_never_written() {
    existing_world_is_untouched(vanilla::NAME, None).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_existing_server_file_is_read_but_never_written() {
    existing_world_is_untouched(sqlite::NAME, Some("world.mcserver".into())).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_world_nothing_is_created() {
    for (backend, world) in [
        (vanilla::NAME, None),
        (sqlite::NAME, Some(PathBuf::from("world.mcserver"))),
    ] {
        let instance = TempDir::new();
        let world = world.map(|name| instance.path().join(name));
        let out = run(server(instance.path(), backend, world, true), EDITS).await;
        assert!(
            out.iter().any(|line| line.contains("tuff.http.port")),
            "{out:?}"
        );
        assert_eq!(snapshot(instance.path()), BTreeMap::new(), "{backend}");
    }
}

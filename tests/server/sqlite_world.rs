use crate::common;

use std::fs;
use std::path::Path;
use std::time::Duration;
use tuff::storage::codec::WorldBackendExt as _;

use common::{TempDir, ping, test_png};
use tuff::Server;
use tuff::storage::BackendOptions;
use tuff::storage::Storage;
use tuff::storage::level::DimensionSettings;
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;
use tuff_storage::sqlite;
use tuff_storage::sqlite::{Locking, SqliteBackend};
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::chunk_meta::{ChunkMeta, Decoded};
use tuff_world_model::pos::ChunkPos;

const OVERWORLD: &str = "minecraft:overworld";
const THE_NETHER: &str = "minecraft:the_nether";

fn server(bind: &str, file: &Path, seed: i64) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .binds(vec![bind.to_string()])
        .storage_backend(sqlite::NAME)
        .backend_options(BackendOptions {
            sqlite_locking: Locking::Exclusive,
        })
        .world_path(file.to_path_buf())
        .seed(seed)
        .build()
}

fn storage() -> Storage {
    Storage::Named(sqlite::NAME.into())
}

fn stored_seed(file: &Path) -> Option<i64> {
    let backend = storage().open(file).unwrap();
    let settings = backend
        .documents()
        .read(Document::SavedData("minecraft:world_gen_settings"))
        .unwrap()?;
    settings.get_compound("data")?.get_long("seed")
}

#[tuff_core::watchdog]
#[tokio::test]
async fn world_lives_in_one_file_and_survives_restarts() {
    let dir = TempDir::new();
    let file = dir.path().join("worlds").join("main.db");
    let first = server("127.0.0.1:0", &file, 11);
    let handle = first.handle();
    let task = tokio::spawn(first.run());
    handle.local_addrs().await.unwrap();
    assert!(file.is_file());
    assert!(!sqlite::lock_path(&file).exists());

    let second = server("127.0.0.1:0", &file, 11).run().await;
    let err = format!("{:#}", second.unwrap_err());
    assert!(err.contains("already in use"), "{err}");

    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();

    let entries: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for entry in &entries {
        let name = entry.file_name().into_string().unwrap();
        assert!(name.starts_with("main.db"), "unexpected {name}");
        assert!(entry.file_type().unwrap().is_file(), "{name} is not a file");
    }
    assert!(!dir.path().join("world").exists());
    assert!(!dir.path().join("worlds/world").exists());
    assert_eq!(stored_seed(&file), Some(11));

    let again = server("127.0.0.1:0", &file, 12);
    let handle = again.handle();
    let task = tokio::spawn(again.run());
    handle.local_addrs().await.unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(stored_seed(&file), Some(11));
    let backend = storage().open(&file).unwrap();
    assert!(
        backend
            .documents()
            .exists(Document::SavedData("minecraft:world_clocks"))
    );
}

#[tuff_core::watchdog]
#[test]
fn backend_round_trips_generated_chunks_and_entities() {
    let dir = TempDir::new();
    let file = dir.path().join("world.db");
    let pos = ChunkPos::new(3, -2);
    let shape = ChunkShape::OVERWORLD;
    let overworld = DimensionSettings::vanilla().remove(0);
    let chunk = tuff_feature_builtin_vanilla_worldgen::chunk_generator::for_settings(&overworld, 1)
        .generate(pos, shape);
    let pig = Compound::new().with("id", "minecraft:pig").with(
        "Pos",
        Tag::List(vec![
            Tag::Double(48.5),
            Tag::Double(70.0),
            Tag::Double(-20.5),
        ]),
    );
    {
        let backend = storage().open(&file).unwrap();
        let codec = backend.chunk_codec();
        let blob = codec
            .encode_chunk(&chunk, &ChunkMeta::generated(), pos, 5)
            .unwrap();
        let entities = codec
            .encode_entities(pos, std::slice::from_ref(&pig))
            .unwrap()
            .expect("a pig is worth storing");
        assert!(codec.encode_entities(pos, &[]).unwrap().is_none());
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        store.write_chunk(pos, &blob).unwrap();
        store.write_entities(pos, Some(&entities)).unwrap();
        store.flush().unwrap();
    }

    let backend = storage().open(&file).unwrap();
    let codec = backend.chunk_codec();
    let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    let blob = store.read_chunk(pos).unwrap().expect("chunk persisted");
    let Decoded::Chunk(loaded, meta) = codec.decode_chunk(&blob, pos, shape).unwrap() else {
        panic!("the chunk has terrain");
    };
    assert_eq!(*loaded, chunk);
    assert_eq!(meta.status, tuff_world_model::chunk_meta::STATUS_FULL);

    let entities = store
        .read_entities(pos)
        .unwrap()
        .expect("entities persisted");
    let decoded = codec.decode_entities(&entities, pos).unwrap();
    assert_eq!(decoded, vec![pig]);

    store.write_entities(pos, None).unwrap();
    assert!(store.read_entities(pos).unwrap().is_none());
    let mut nether = backend.open_region(THE_NETHER, pos.file_region()).unwrap();
    assert!(nether.read_chunk(pos).unwrap().is_none());
}

fn server_in(instance: &Path, locking: Locking) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .binds(vec!["127.0.0.1:0".to_string()])
        .instance_dir(instance.to_path_buf())
        .storage_backend(sqlite::NAME)
        .backend_options(BackendOptions {
            sqlite_locking: locking,
        })
        .world_path(instance.join("world.mcserver"))
        .online_mode(false)
        .seed(3)
        .build()
}

async fn run(handle: &tuff::ServerHandle, line: &str) -> serde_json::Value {
    let feedback = handle.run_command(line).await.unwrap();
    feedback[0].to_value()
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_whole_server_lives_in_one_file() {
    let instance = TempDir::new();
    let file = instance.path().join("world.mcserver");
    SqliteBackend::open_with(&file, Locking::Exclusive)
        .unwrap()
        .config()
        .unwrap()
        .write_icon(Some(&test_png()))
        .unwrap();

    let server = server_in(instance.path(), Locking::Exclusive);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = ping(addr).await;
            if status.get("favicon").is_some() {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the icon in the server file is shown");
    assert!(
        status["favicon"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    let banned = run(&handle, "ban-ip 10.1.2.3 spam").await;
    assert_eq!(banned["translate"], "commands.banip.success");
    let off = run(&handle, "whitelist off").await;
    assert_eq!(off["translate"], "commands.whitelist.disabled");
    assert_eq!(
        file_names(instance.path()),
        ["world.mcserver", "world.mcserver-wal"]
    );
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();

    assert_eq!(file_names(instance.path()), ["world.mcserver"]);
    {
        let backend = SqliteBackend::open_with(&file, Locking::Exclusive).unwrap();
        let config = backend.config().unwrap();
        let properties = config.read_properties().unwrap().unwrap();
        assert!(!properties.white_list);
        let bans = config.read_ip_bans().unwrap();
        assert_eq!(bans.len(), 1);
        assert_eq!(bans[0].key, "10.1.2.3");
        assert_eq!(bans[0].reason.as_deref(), Some("spam"));
        assert!(
            backend
                .documents()
                .exists(Document::SavedData("minecraft:world_clocks"))
        );
    }

    let server = server_in(instance.path(), Locking::Exclusive);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    let listing = handle.run_command("banlist ips").await.unwrap();
    assert_eq!(listing[1].to_value()["with"][0]["text"], "10.1.2.3");
    let off = run(&handle, "whitelist off").await;
    assert_eq!(off["translate"], "commands.whitelist.alreadyOff");
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(file_names(instance.path()), ["world.mcserver"]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn normal_locking_lets_tools_edit_the_whitelist_while_running() {
    let instance = TempDir::new();
    let file = instance.path().join("world.mcserver");
    let server = server_in(instance.path(), Locking::Normal);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();

    let second = server_in(instance.path(), Locking::Exclusive).run().await;
    let err = format!("{:#}", second.unwrap_err());
    assert!(err.contains("already in use"), "{err}");

    let empty = run(&handle, "whitelist list").await;
    assert_eq!(empty["translate"], "commands.whitelist.none");
    tuff_test_support::sqlite_execute(
        &file,
        "INSERT INTO whitelist(position, uuid, name) \
         VALUES (0, '069a79f4-44e9-4726-a5be-fca90e38aaf5', 'Notch')",
    )
    .unwrap();
    let reloaded = run(&handle, "whitelist reload").await;
    assert_eq!(reloaded["translate"], "commands.whitelist.reloaded");
    let listing = run(&handle, "whitelist list").await;
    assert_eq!(listing["with"][1]["text"], "Notch");

    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        file_names(instance.path()),
        ["world.mcserver", "world.mcserver.lock"]
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn forced_chunks_are_held_again_after_a_restart() {
    let dir = TempDir::new();
    let file = dir.path().join("forced.db");
    let command = |handle: tuff::ServerHandle, line: &'static str| async move {
        handle
            .run_command(line)
            .await
            .unwrap()
            .iter()
            .map(|line| {
                line.to_value()["translate"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };

    let first = server("127.0.0.1:0", &file, 5);
    let handle = first.handle();
    let task = tokio::spawn(first.run());
    handle.local_addrs().await.unwrap();
    assert_eq!(
        command(handle.clone(), "forceload add 0 0 20 20").await,
        ["commands.forceload.added.multiple"]
    );
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();

    let backend = storage().open(&file).unwrap();
    let stored = backend
        .documents()
        .read(Document::DimensionData {
            dimension: OVERWORLD,
            id: "minecraft:chunk_tickets",
        })
        .unwrap()
        .expect("chunk tickets saved");
    let tickets = stored
        .get_compound("data")
        .unwrap()
        .get_list("tickets")
        .unwrap();
    assert_eq!(tickets.len(), 4);
    drop(backend);

    let again = server("127.0.0.1:0", &file, 5);
    let handle = again.handle();
    let task = tokio::spawn(again.run());
    handle.local_addrs().await.unwrap();
    assert_eq!(
        command(handle.clone(), "forceload query 18 18").await,
        ["commands.forceload.query.success"]
    );
    assert_eq!(
        command(handle.clone(), "setblock 3 300 3 minecraft:stone").await,
        ["commands.setblock.success"]
    );
    assert_eq!(
        command(handle.clone(), "forceload remove all").await,
        ["commands.forceload.removed.all"]
    );
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();

    let again = server("127.0.0.1:0", &file, 5);
    let handle = again.handle();
    let task = tokio::spawn(again.run());
    handle.local_addrs().await.unwrap();
    assert_eq!(
        command(handle.clone(), "forceload query").await,
        ["commands.forceload.added.none"]
    );
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

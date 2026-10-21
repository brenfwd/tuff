//! The web panel's data pack operations on a running world, for every
//! built-in backend: an uploaded pack's functions run once it is enabled
//! or reloaded, and removing a pack disables it first.

use super::*;

use crate::datapack::store::{self, StoreError};
use crate::panel_host::datapacks::{self, PackError};
use tuff_item::text::TextComponent;
use tuff_storage::backend::WorldBackend;
use tuff_storage::memory::MemoryBackend;
use tuff_storage::sqlite::SqliteBackend;
use tuff_zip::{DosTime, write_all};

fn pack(objective: &str) -> Vec<u8> {
    let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
    let meta = format!(
        r#"{{"pack":{{"description":"panel","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
        v.major, v.minor
    );
    let function = format!("scoreboard objectives add {objective} dummy\n");
    write_all(
        &[
            ("pack.mcmeta", meta.as_bytes()),
            ("data/panel/function/hi.mcfunction", function.as_bytes()),
        ],
        DosTime::MIN,
        6,
        u64::MAX,
    )
    .unwrap()
}

async fn console(world: &TestWorld, line: &str) -> String {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn runs(world: &TestWorld) -> bool {
    console(world, "function panel:hi")
        .await
        .contains("Running function panel:hi")
}

async fn stored(world: &TestWorld) -> Vec<String> {
    let backend = world.shared.backend.clone();
    tokio::task::spawn_blocking(move || store::list(&*backend).unwrap())
        .await
        .unwrap()
        .iter()
        .map(store::Entry::id)
        .collect()
}

async fn round_trip(backend: Arc<dyn WorldBackend>) {
    let world = TestWorld::start_with(backend);
    let added = datapacks::add(&world.shared, "panel", pack("a"), false, true)
        .await
        .unwrap();
    assert_eq!(added.id, "file/panel.zip");
    assert!(added.enabled);
    assert!(runs(&world).await);
    assert!(world.shared.datapacks.known().is_selected("file/panel.zip"));

    let removed = datapacks::remove(&world.shared, "file/panel.zip")
        .await
        .unwrap();
    assert_eq!(removed, "file/panel.zip");
    assert!(!runs(&world).await, "removing disables the pack first");
    assert!(!world.shared.datapacks.known().is_selected("file/panel.zip"));
    assert!(stored(&world).await.is_empty());

    let added = datapacks::add(&world.shared, "later", pack("b"), false, false)
        .await
        .unwrap();
    assert!(!added.enabled);
    assert!(!runs(&world).await);
    datapacks::reload(&world.shared).await.unwrap();
    assert!(runs(&world).await, "a reload picks up a new pack");
    datapacks::disable(&world.shared, "file/later.zip")
        .await
        .unwrap();
    assert!(!runs(&world).await);
    assert!(matches!(
        datapacks::disable(&world.shared, "file/later.zip").await,
        Err(PackError::NotEnabled(_))
    ));
    datapacks::enable(&world.shared, "file/later.zip")
        .await
        .unwrap();
    assert!(runs(&world).await);
    assert!(matches!(
        datapacks::disable(&world.shared, "vanilla").await,
        Err(PackError::Required(_))
    ));

    for bad in [b"PK nope".to_vec(), Vec::new()] {
        assert!(matches!(
            datapacks::add(&world.shared, "bad", bad, false, true).await,
            Err(PackError::Store(StoreError::Invalid(_)))
        ));
    }
    assert!(matches!(
        datapacks::add(&world.shared, "../escape", pack("c"), false, true).await,
        Err(PackError::Store(StoreError::Name(_)))
    ));
    assert_eq!(stored(&world).await, ["file/later.zip"]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panel_data_packs_work_in_memory() {
    round_trip(Arc::new(MemoryBackend::default())).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panel_data_packs_work_in_a_folder() {
    let dir = TestDir::new();
    round_trip(crate::storage::Storage::default().open(dir.path()).unwrap()).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panel_data_packs_work_in_a_server_file() {
    let dir = TestDir::new();
    round_trip(Arc::new(
        SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
    ))
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_needing_unknown_features_is_stored_but_not_enabled() {
    let world = TestWorld::start_with(Arc::new(MemoryBackend::default()));
    let meta = {
        let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
        format!(
            r#"{{"pack":{{"description":"panel","min_format":[{0},{1}],"max_format":[{0},{1}]}},"features":{{"enabled":["minecraft:not_a_flag"]}}}}"#,
            v.major, v.minor
        )
    };
    let broken = write_all(
        &[("pack.mcmeta", meta.as_bytes())],
        DosTime::MIN,
        6,
        u64::MAX,
    )
    .unwrap();
    let err = datapacks::add(&world.shared, "flagged", broken, false, true)
        .await
        .unwrap_err();
    assert!(matches!(err, PackError::MissingFeatures(..)), "{err}");
    assert_eq!(stored(&world).await, ["file/flagged.zip"]);
    assert!(
        !world
            .shared
            .datapacks
            .known()
            .is_selected("file/flagged.zip")
    );
    world.shutdown().await;
}

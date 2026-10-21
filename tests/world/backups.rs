//! World backups on a running world: saving is flushed and held only for
//! the snapshot, writes made meanwhile wait and stay out of the backup,
//! and both storage backends back up to something that opens as the same
//! world.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::storage::backup::jobs::CreateOptions;
use crate::storage::backup::{self, jobs::BackupError};
use crate::world::chunk_io::HoldPhase;
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_storage::backend::WorldBackend;
use tuff_storage::backup::world_records;
use tuff_storage::document::Document;
use tuff_storage::sqlite::SqliteBackend;
use tuff_storage::vanilla::VanillaBackend;
use tuff_zip::{Archive, Limits};

const PROBE: &str = "tuff:backup_probe";

fn backups_into(world: &TestWorld, dir: &Path) {
    world
        .shared
        .config
        .set_text(backup::DIRECTORY_KEY, &dir.display().to_string())
        .unwrap();
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

/// A hook that checks saving is held and writes a probe document, which
/// the gate must keep out of the backup.
fn probe_while_held(world: &TestWorld, held: Arc<AtomicBool>) -> CreateOptions {
    let shared = world.shared.clone();
    CreateOptions {
        by: "test".to_owned(),
        include_caches: false,
        while_held: Some(Box::new(move || {
            held.store(
                shared.saves().hold == Some(HoldPhase::Held),
                Ordering::SeqCst,
            );
            shared
                .backend
                .documents()
                .write(Document::SavedData(PROBE), Compound::new().with("x", 1i32))
                .unwrap();
        })),
    }
}

async fn records(
    backend: Arc<dyn WorldBackend>,
) -> std::collections::BTreeMap<String, tuff_storage::backup::Record> {
    tokio::task::spawn_blocking(move || world_records(&*backend).unwrap())
        .await
        .unwrap()
}

async fn has_probe(backend: Arc<dyn WorldBackend>) -> bool {
    tokio::task::spawn_blocking(move || backend.documents().exists(Document::SavedData(PROBE)))
        .await
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_world_backup_holds_saving_only_for_the_snapshot() {
    let dir = TestDir::new();
    let out = TestDir::new();
    let world = TestWorld::start(dir.path());
    backups_into(&world, out.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(2).await;

    let held = Arc::new(AtomicBool::new(false));
    let running = world
        .shared
        .backups
        .create(probe_while_held(&world, held.clone()))
        .await
        .unwrap();
    let finished = world.shared.backups.wait(running.id).await.unwrap();
    assert_eq!(finished.error, None);
    assert!(
        held.load(Ordering::SeqCst),
        "saving was held during the copy"
    );
    assert_eq!(world.shared.saves().hold, None, "saving resumed");
    assert!(
        has_probe(world.shared.backend.clone()).await,
        "the held write landed"
    );

    let unzipped = TestDir::new();
    let file = std::fs::File::open(out.path().join(&finished.name)).unwrap();
    Archive::open(file, Limits::untrusted(1 << 32))
        .unwrap()
        .extract_to(unzipped.path(), |_| Ok(()))
        .unwrap();
    let copy: Arc<dyn WorldBackend> = Arc::new(VanillaBackend::new(unzipped.path().join("world")));
    assert!(
        !has_probe(copy.clone()).await,
        "the held write is not in the backup"
    );
    let mut live = records(world.shared.backend.clone()).await;
    live.remove(&format!(
        "{:?}",
        tuff_storage::document::DocumentId::SavedData(PROBE.to_owned())
    ));
    let copied = records(copy).await;
    assert!(
        copied.keys().any(|k| k.contains("chunk")),
        "flushed chunks are in the backup"
    );
    assert_eq!(copied, live);

    let listed = console(&world, "tuff backup list").await;
    assert!(listed[0].starts_with("1 backups: world-"), "{listed:?}");
    let status = console(&world, "tuff backup status").await;
    assert!(status.iter().any(|l| l.contains("done")), "{status:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_file_backup_opens_as_the_same_world() {
    let dir = TestDir::new();
    let out = TestDir::new();
    let backend = Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
    let world = TestWorld::start_with(backend);
    backups_into(&world, out.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(2).await;

    let held = Arc::new(AtomicBool::new(false));
    let running = world
        .shared
        .backups
        .create(probe_while_held(&world, held.clone()))
        .await
        .unwrap();
    let finished = world.shared.backups.wait(running.id).await.unwrap();
    assert_eq!(finished.error, None);
    assert!(finished.name.ends_with(".mcserver"));
    assert!(held.load(Ordering::SeqCst));
    assert!(has_probe(world.shared.backend.clone()).await);

    let copy: Arc<dyn WorldBackend> =
        Arc::new(SqliteBackend::open(out.path().join(&finished.name)).unwrap());
    assert!(!has_probe(copy.clone()).await);
    let mut live = records(world.shared.backend.clone()).await;
    live.remove(&format!(
        "{:?}",
        tuff_storage::document::DocumentId::SavedData(PROBE.to_owned())
    ));
    let copied = records(copy).await;
    assert!(copied.keys().any(|k| k.contains("chunk")));
    assert_eq!(copied, live);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_backup_at_a_time_cancel_cleans_up_and_retention_applies() {
    let dir = TestDir::new();
    let out = TestDir::new();
    let world = TestWorld::start(dir.path());
    backups_into(&world, out.path());
    world.shared.config.set_text(backup::KEEP_KEY, "1").unwrap();

    let (go, wait) = std::sync::mpsc::channel::<()>();
    let running = world
        .shared
        .backups
        .create(CreateOptions {
            by: "test".to_owned(),
            include_caches: false,
            while_held: Some(Box::new(move || {
                let _ = wait.recv();
            })),
        })
        .await
        .unwrap();
    assert!(matches!(
        world
            .shared
            .backups
            .create(CreateOptions::by("again"))
            .await,
        Err(BackupError::Busy(_))
    ));
    assert_eq!(world.shared.backups.cancel().await.unwrap(), running.name);
    go.send(()).unwrap();
    let finished = world.shared.backups.wait(running.id).await.unwrap();
    assert!(finished.error.unwrap().contains("cancelled"));
    assert_eq!(
        std::fs::read_dir(out.path()).unwrap().count(),
        0,
        "nothing partial left"
    );
    assert_eq!(world.shared.saves().hold, None);
    assert_eq!(
        world.shared.backups.cancel().await,
        Err(BackupError::NotRunning)
    );

    let mut names = Vec::new();
    for _ in 0..2 {
        let started = console(&world, "tuff backup create").await;
        assert!(started[0].starts_with("Backup world-"), "{started:?}");
        let id = world.shared.backups.status().await.running.map(|r| r.id);
        let finished = match id {
            Some(id) => world.shared.backups.wait(id).await.unwrap(),
            None => world.shared.backups.status().await.last.unwrap(),
        };
        assert_eq!(finished.error, None);
        names.push(finished.name);
    }
    let listed = world.shared.backups.list().await.unwrap();
    assert_eq!(listed.len(), 1, "keep 1");
    assert_eq!(listed[0].name, names[1]);
    assert!(
        console(&world, &format!("tuff backup delete {}", names[1])).await[0]
            .starts_with("Deleted")
    );
    assert!(world.shared.backups.list().await.unwrap().is_empty());
    assert!(matches!(
        world.shared.backups.delete("../../etc/passwd").await,
        Err(BackupError::NotFound(_))
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_schedule_backs_up_when_due_and_counts_backups_from_before_a_restart() {
    let dir = TestDir::new();
    let out = TestDir::new();
    let world = TestWorld::start(dir.path());
    backups_into(&world, out.path());
    assert!(
        world.shared.backups.check_schedule().await.is_none(),
        "off by default"
    );
    world
        .shared
        .config
        .set_text(backup::INTERVAL_KEY, "60")
        .unwrap();
    let running = world
        .shared
        .backups
        .check_schedule()
        .await
        .expect("due: there is no backup yet");
    assert_eq!(running.by, "schedule");
    assert!(world.shared.backups.check_schedule().await.is_none());
    let finished = world.shared.backups.wait(running.id).await.unwrap();
    assert_eq!(finished.error, None);
    assert!(
        world.shared.backups.check_schedule().await.is_none(),
        "not due again for an hour"
    );
    world.shutdown().await;

    let again = TestDir::new();
    let restarted = TestWorld::start(again.path());
    backups_into(&restarted, out.path());
    restarted
        .shared
        .config
        .set_text(backup::INTERVAL_KEY, "60")
        .unwrap();
    assert!(
        restarted.shared.backups.check_schedule().await.is_none(),
        "the backup made before the restart is recent"
    );
    restarted.shutdown().await;

    std::fs::rename(
        out.path().join(&finished.name),
        out.path().join("world-20200101-000000.zip"),
    )
    .unwrap();
    let old = TestDir::new();
    let later = TestWorld::start(old.path());
    backups_into(&later, out.path());
    later
        .shared
        .config
        .set_text(backup::INTERVAL_KEY, "60")
        .unwrap();
    let running = later
        .shared
        .backups
        .check_schedule()
        .await
        .expect("the newest backup is years old");
    later.shared.backups.wait(running.id).await.unwrap();
    later.shutdown().await;
}

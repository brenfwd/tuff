use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::jobs::{BackupError, Backups, BackupsConfig};
use super::*;
use crate::storage::codec::WorldBackendExt as _;
use tuff_nbt::Compound;
use tuff_storage::backend::WorldBackend;
use tuff_storage::backup::*;
use tuff_storage::document::Document;
use tuff_storage::sqlite::SqliteBackend;
use tuff_storage::test_dir::TestDir;
use tuff_storage::vanilla::VanillaBackend;
use tuff_world_model::pos::ChunkPos;
use tuff_zip::{Archive, Limits};

const OVERWORLD: &str = "minecraft:overworld";

fn fill(backend: &dyn WorldBackend, players: u128) {
    let documents = backend.documents();
    documents
        .write(Document::Level, Compound::new().with("Data", 1i32))
        .unwrap();
    for uuid in 1..=players {
        documents
            .write(
                Document::Player(uuid),
                Compound::new().with("uuid", uuid as i64),
            )
            .unwrap();
    }
    let codec = backend.chunk_codec();
    for (x, z) in [(0, 0), (3, 40), (-40, -2)] {
        let pos = ChunkPos::new(x, z);
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        let blob = codec
            .encode_entities(pos, &[Compound::new().with("id", "minecraft:cow")])
            .unwrap()
            .unwrap();
        store.write_entities(pos, Some(&blob)).unwrap();
        store.flush().unwrap();
    }
}

fn plan(source: BackupSource, dir: &Path, staged: bool) -> Plan {
    Plan {
        source,
        dir: dir.to_path_buf(),
        level: "world".to_owned(),
        compression: 6,
        staged,
        include_caches: false,
    }
}

fn unzip(zip: &Path, into: &Path) {
    let archive = Archive::open(File::open(zip).unwrap(), Limits::untrusted(1 << 30)).unwrap();
    fs::create_dir_all(into).unwrap();
    archive.extract_to(into, |_| Ok(())).unwrap();
}

#[tuff_core::watchdog]
#[test]
fn names_round_trip_and_ignore_strangers() {
    let secs = 1_791_030_896;
    let name = file_name("world", secs, 1, "zip");
    assert_eq!(name, "world-20261003-123456.zip");
    assert_eq!(parse_name("world", &name), Some(secs));
    assert_eq!(
        parse_name("world", &file_name("world", secs, 3, "mcserver")),
        Some(secs)
    );
    for stranger in [
        "world-20261003-123456.zip.part",
        "world-2026103-123456.zip",
        "world-20261303-123456.zip",
        "other-20261003-123456.zip",
        "world-20261003-123456-.zip",
        "world-20261003-123456",
        "notes.txt",
    ] {
        assert_eq!(parse_name("world", stranger), None, "{stranger}");
    }
    assert_eq!(file_level("my world/../x"), "my_world_.._x");
    assert_eq!(file_level(".."), "world");
}

#[tuff_core::watchdog]
#[test]
fn listing_skips_partial_output_and_retention_keeps_the_newest() {
    let dir = TestDir::new();
    let base = 1_791_030_896;
    for i in 0..5u64 {
        fs::write(
            dir.path()
                .join(file_name("world", base + i * 86_400, 1, "zip")),
            b"x",
        )
        .unwrap();
    }
    fs::write(
        dir.path().join("world-20261003-123456.zip.part"),
        b"partial",
    )
    .unwrap();
    fs::create_dir(dir.path().join(".staging-world-20261003-123457.zip")).unwrap();
    fs::write(dir.path().join("readme.txt"), b"keep me").unwrap();

    let listed = list(dir.path(), "world").unwrap();
    assert_eq!(listed.len(), 5);
    assert_eq!(listed[0].created_unix, base + 4 * 86_400);

    clean_leftovers(dir.path(), "world").unwrap();
    assert!(!dir.path().join("world-20261003-123456.zip.part").exists());
    assert!(
        !dir.path()
            .join(".staging-world-20261003-123457.zip")
            .exists()
    );
    assert!(dir.path().join("readme.txt").exists());

    let newest = listed[0].name.clone();
    let deleted = prune(
        dir.path(),
        "world",
        Retention {
            keep: 3,
            max_age: None,
        },
        base + 5 * 86_400,
        &newest,
    )
    .unwrap();
    assert_eq!(deleted.len(), 2);
    assert_eq!(list(dir.path(), "world").unwrap().len(), 3);
    let deleted = prune(
        dir.path(),
        "world",
        Retention {
            keep: 0,
            max_age: Some(Duration::from_secs(86_400 + 1)),
        },
        base + 5 * 86_400,
        &newest,
    )
    .unwrap();
    assert_eq!(deleted.len(), 2, "{deleted:?}");
    assert_eq!(list(dir.path(), "world").unwrap()[0].name, newest);
    assert!(dir.path().join("readme.txt").exists());
}

#[tuff_core::watchdog]
#[test]
fn folder_backups_unzip_to_the_same_world() {
    for staged in [true, false] {
        let dir = TestDir::new();
        let world = dir.path().join("world");
        let source = VanillaBackend::new(&world);
        let _lock = source.lock().unwrap();
        fill(&source, 20);
        fs::create_dir_all(world.join("datapacks")).unwrap();
        fs::write(world.join("datapacks/pack.zip"), b"not really a pack").unwrap();
        let backups = world.join("backups");
        let (tracker, _) = Tracker::new();
        let (name, size) =
            run_offline(&plan(source.backup_source(), &backups, staged), &tracker).unwrap();
        assert!(name.ends_with(".zip"));
        assert_eq!(fs::metadata(backups.join(&name)).unwrap().len(), size);
        assert_eq!(
            fs::read_dir(&backups).unwrap().count(),
            1,
            "nothing partial is left"
        );

        let archive = Archive::open(
            File::open(backups.join(&name)).unwrap(),
            Limits::untrusted(1 << 30),
        )
        .unwrap();
        let names: Vec<&str> = archive.entries().iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"world/level.dat"));
        assert!(!names.iter().any(|n| n.ends_with("session.lock")));
        assert!(!names.iter().any(|n| n.contains("backups")));

        let restored = dir.path().join("restored");
        unzip(&backups.join(&name), &restored);
        let copy = VanillaBackend::new(restored.join("world"));
        assert_eq!(
            world_records(&copy).unwrap(),
            world_records(&source).unwrap()
        );
        assert_eq!(
            fs::read(restored.join("world/datapacks/pack.zip")).unwrap(),
            b"not really a pack"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn server_file_backups_open_as_the_same_world() {
    let dir = TestDir::new();
    let source = SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
    let _lock = source.lock().unwrap();
    fill(&source, 50);
    let backups = dir.path().join("backups");
    let (tracker, _) = Tracker::new();
    let (name, _) = run_offline(&plan(source.backup_source(), &backups, true), &tracker).unwrap();
    assert!(name.ends_with(".mcserver"));
    assert_eq!(fs::read_dir(&backups).unwrap().count(), 1);
    let copy = SqliteBackend::open(backups.join(&name)).unwrap();
    assert_eq!(
        world_records(&copy).unwrap(),
        world_records(&source).unwrap()
    );
}

#[tuff_core::watchdog]
#[test]
fn a_frozen_server_file_keeps_taking_writes_that_the_copy_lacks() {
    let dir = TestDir::new();
    let source = Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
    let _lock = source.lock().unwrap();
    fill(&*source, 5);
    let before = world_records(&*source).unwrap();
    let BackupSource::File(file) = source.backup_source() else {
        panic!("a server file freezes");
    };
    let frozen = file.freeze().unwrap();
    for uuid in 100..400u128 {
        source
            .documents()
            .write(Document::Player(uuid), Compound::new().with("late", 1i32))
            .unwrap();
    }
    let copy_path = dir.path().join("copy.mcserver");
    let (tracker, _) = Tracker::new();
    copy_file(frozen.path(), &copy_path, &tracker, Some(&*frozen)).unwrap();
    drop(frozen);
    file.verify(&copy_path).unwrap();
    let copy = SqliteBackend::open(&copy_path).unwrap();
    assert_eq!(world_records(&copy).unwrap(), before);
    assert!(
        source
            .documents()
            .read(Document::Player(399))
            .unwrap()
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn a_cancelled_backup_leaves_nothing_behind() {
    let dir = TestDir::new();
    let world = dir.path().join("world");
    let source = VanillaBackend::new(&world);
    fill(&source, 3);
    let backups = dir.path().join("backups");
    for staged in [true, false] {
        let (tracker, _) = Tracker::new();
        tracker.cancel();
        let err =
            run_offline(&plan(source.backup_source(), &backups, staged), &tracker).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read_dir(&backups).unwrap().count(), 0);
    }
    let unsupported = run_offline(
        &plan(BackupSource::Unsupported, &backups, true),
        &Tracker::new().0,
    );
    assert_eq!(unsupported.unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[tuff_core::watchdog]
#[test]
fn progress_counts_files_and_bytes() {
    let dir = TestDir::new();
    let world = dir.path().join("world");
    let source = VanillaBackend::new(&world);
    fill(&source, 3);
    let files = scan_folder(&world, &[]).unwrap();
    let (tracker, progress) = Tracker::new();
    zip_folder(
        &world,
        &files,
        "world/",
        &dir.path().join("out.zip"),
        1,
        &tracker,
    )
    .unwrap();
    let progress = progress.borrow().clone();
    assert_eq!(progress.phase, Phase::Compressing);
    assert_eq!(progress.files_done, files.len() as u64);
    assert_eq!(
        progress.bytes_done,
        files.iter().map(|f| f.size).sum::<u64>()
    );
}

struct Locked {
    path: PathBuf,
    skip: std::ops::Range<u64>,
}

impl FrozenFile for Locked {
    fn path(&self) -> &Path {
        &self.path
    }

    fn interrupted(&self) -> bool {
        false
    }

    fn skip(&self) -> Option<std::ops::Range<u64>> {
        Some(self.skip.clone())
    }
}

#[tuff_core::watchdog]
#[test]
fn copies_never_read_the_bytes_a_lock_may_cover() {
    let dir = TestDir::new();
    let source: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8 + 1).collect();
    let path = dir.path().join("source");
    fs::write(&path, &source).unwrap();
    for skip in [100_000..100_512, 0..512, 299_900..300_400, 10..10] {
        let frozen = Locked {
            path: path.clone(),
            skip: skip.clone(),
        };
        let copy = dir.path().join(format!("copy-{}", skip.start));
        let (tracker, _) = Tracker::new();
        let copied = copy_file(&path, &copy, &tracker, Some(&frozen)).unwrap();
        assert_eq!(copied, source.len() as u64);
        let copy = fs::read(&copy).unwrap();
        let mut expected = source.clone();
        let end = (skip.end as usize).min(expected.len());
        expected[(skip.start as usize).min(end)..end].fill(0);
        assert_eq!(copy, expected, "skip {skip:?}");
    }
    assert_eq!(
        tuff_storage::sqlite::LOCK_BYTES,
        (1 << 30)..(1 << 30) + 512,
        "pending, reserved and the 510 shared bytes"
    );
}

#[tuff_core::watchdog]
#[test]
fn scheduled_backups_fall_due_after_their_interval() {
    use jobs::due;
    assert!(!due(None, 0, 1_000_000), "0 turns the schedule off");
    assert!(due(None, 60, 1_000_000), "never backed up");
    assert!(!due(Some(1_000_000), 60, 1_000_000 + 3599));
    assert!(due(Some(1_000_000), 60, 1_000_000 + 3600));
    assert!(!due(Some(u64::MAX - 10), 60, u64::MAX - 5), "no overflow");
}

fn cache_files(backend: &dyn WorldBackend) -> Vec<String> {
    backend.list_extra_files_under("tuff/world-map/").unwrap()
}

fn backup_with_caches(
    source: &dyn WorldBackend,
    dir: &Path,
    staged: bool,
    include: bool,
) -> PathBuf {
    let (tracker, _) = Tracker::new();
    let mut plan = plan(source.backup_source(), dir, staged);
    plan.include_caches = include;
    let (name, _) = run_offline(&plan, &tracker).unwrap();
    dir.join(name)
}

#[tuff_core::watchdog]
#[test]
fn folder_backups_leave_the_map_cache_out_unless_asked() {
    for (staged, include) in [(true, false), (false, false), (true, true), (false, true)] {
        let dir = TestDir::new();
        let world = dir.path().join("world");
        let source = VanillaBackend::new(&world);
        let _lock = source.lock().unwrap();
        fill(&source, 2);
        source
            .write_extra_file("tuff/world-map/v1/index.0", b"index")
            .unwrap();
        source.write_extra_file("other/keep.txt", b"keep").unwrap();
        let path = backup_with_caches(&source, &dir.path().join("backups"), staged, include);
        let restored = dir.path().join("restored");
        unzip(&path, &restored);
        let copy = VanillaBackend::new(restored.join("world"));
        assert_eq!(cache_files(&copy).len(), usize::from(include));
        assert_eq!(copy.read_extra_file("other/keep.txt").unwrap(), b"keep");
    }
}

#[tuff_core::watchdog]
#[test]
fn server_file_backups_leave_the_map_cache_out_unless_asked() {
    for include in [false, true] {
        let dir = TestDir::new();
        let source = SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
        let _lock = source.lock().unwrap();
        fill(&source, 2);
        source
            .write_extra_file("tuff/world-map/v1/index.0", b"index")
            .unwrap();
        source.write_extra_file("other/keep.txt", b"keep").unwrap();
        let path = backup_with_caches(&source, &dir.path().join("backups"), true, include);
        let copy = SqliteBackend::open(&path).unwrap();
        assert_eq!(cache_files(&copy).len(), usize::from(include));
        assert_eq!(copy.read_extra_file("other/keep.txt").unwrap(), b"keep");
        assert_eq!(cache_files(&source).len(), 1);
    }
}

#[tuff_core::watchdog]
#[test]
fn relative_names_are_slash_separated_and_rejoin_per_platform() {
    let root = std::path::Path::new("root");
    let joined = tuff_storage::backup::join(root, "region/r.0.0.mca");
    let expected: std::path::PathBuf = ["root", "region", "r.0.0.mca"].iter().collect();
    assert_eq!(joined, expected);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_runner() {
    let backups = Backups::spawn(BackupsConfig {
        instance_dir: None,
        level: "world".to_owned(),
        settings: crate::config::ConfigStore::new(),
    });
    backups.panic_for_test().await;
    assert_eq!(backups.cancel().await, Err(BackupError::NotRunning));
}

use super::*;
use crate::test_dir::TestDir;
use crate::vanilla::VanillaBackend;

const OVERWORLD: &str = "minecraft:overworld";
const PLAYER: u128 = 7;

fn region() -> FileRegionPos {
    FileRegionPos::new(0, 0)
}

fn chunk() -> ChunkPos {
    ChunkPos::new(1, 2)
}

fn doc(value: i32) -> Compound {
    Compound::new().with("value", value)
}

fn world(dir: &TestDir) -> Arc<dyn WorldBackend> {
    Arc::new(VanillaBackend::new(dir.path().join("world")))
}

fn journal_dir(dir: &TestDir) -> PathBuf {
    dir.path().join("world").join("tuff-journal")
}

fn open(dir: &TestDir) -> JournaledBackend {
    JournaledBackend::open(world(dir), Box::new(DirStore::new(journal_dir(dir)))).unwrap()
}

fn player(backend: &dyn WorldBackend) -> Option<i32> {
    backend
        .documents()
        .read(Document::Player(PLAYER))
        .unwrap()
        .and_then(|root| root.get_int("value"))
}

fn blob(backend: &dyn WorldBackend) -> Option<String> {
    let bytes = backend
        .open_region(OVERWORLD, region())
        .unwrap()
        .read_chunk(chunk())
        .unwrap()?;
    let root = backend.blob_codec().read_root(&bytes).unwrap();
    root.get_str("v").map(str::to_owned)
}

fn write_blob(backend: &dyn WorldBackend, value: &str) -> io::Result<()> {
    let bytes = backend
        .blob_codec()
        .write_root(Compound::new().with("v", value))
        .map_err(io::Error::other)?;
    let mut store = backend.open_region(OVERWORLD, region())?;
    store.write_chunk(chunk(), &bytes)?;
    store.flush()
}

/// The state every test starts from: one committed save holding the
/// player at 1 and the chunk as `old`.
fn saved_once(dir: &TestDir) {
    let backend = open(dir);
    let barrier = backend.journal().barrier();
    let ticket = barrier.ticket();
    ticket.scope(|| {
        backend
            .documents()
            .write(Document::Player(PLAYER), doc(1))
            .unwrap();
        write_blob(&backend, "old").unwrap();
    });
    barrier.seal();
    ticket.landed();
    assert_eq!(backend.journal().committed(), 1);
}

#[tuff_core::watchdog]
#[test]
fn a_finished_save_commits_and_stays_after_reopening() {
    let dir = TestDir::new();
    saved_once(&dir);
    assert!(epochs_in(&journal_dir(&dir)).unwrap().is_empty());
    let reopened = open(&dir);
    assert_eq!(player(&reopened), Some(1));
    assert_eq!(blob(&reopened).as_deref(), Some("old"));
    assert_eq!(reopened.journal().open_epoch(), 2);
}

#[tuff_core::watchdog]
#[test]
fn a_crash_between_the_player_write_and_the_chunk_write_restores_both() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        let barrier = backend.journal().barrier();
        let player_ticket = barrier.ticket();
        let chunk_ticket = barrier.ticket();
        barrier.seal();
        player_ticket.scope(|| {
            backend
                .documents()
                .write(Document::Player(PLAYER), doc(2))
                .unwrap();
        });
        player_ticket.landed();
        assert_eq!(player(&backend), Some(2));
        std::mem::forget(chunk_ticket);
        assert_eq!(backend.journal().committed(), 1);
    }
    let reopened = open(&dir);
    assert_eq!(player(&reopened), Some(1));
    assert_eq!(blob(&reopened).as_deref(), Some("old"));
}

#[tuff_core::watchdog]
#[test]
fn a_failed_chunk_write_poisons_the_journal_and_a_restart_restores_the_last_save() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        backend.journal().set_faults(Faults {
            fail_blob_writes: true,
            ..Faults::default()
        });
        let barrier = backend.journal().barrier();
        let ticket = barrier.ticket();
        barrier.seal();
        ticket.scope(|| {
            backend
                .documents()
                .write(Document::Player(PLAYER), doc(2))
                .unwrap();
            assert!(write_blob(&backend, "new").is_err());
        });
        ticket.landed();
        assert!(backend.journal().poisoned());
        assert_eq!(backend.journal().committed(), 1);
        backend.journal().set_faults(Faults::default());
        let later = backend.journal().barrier();
        let ticket = later.ticket();
        ticket.scope(|| write_blob(&backend, "newer").unwrap());
        later.seal();
        ticket.landed();
        assert_eq!(
            backend.journal().committed(),
            1,
            "nothing commits after a failure"
        );
    }
    let reopened = open(&dir);
    assert_eq!(player(&reopened), Some(1));
    assert_eq!(blob(&reopened).as_deref(), Some("old"));
}

#[tuff_core::watchdog]
#[test]
fn free_writes_join_the_open_epoch_and_commit_with_its_barrier() {
    let dir = TestDir::new();
    saved_once(&dir);
    let backend = open(&dir);
    write_blob(&backend, "unloaded").unwrap();
    assert_eq!(backend.journal().committed(), 1);
    let barrier = backend.journal().barrier();
    let ticket = barrier.ticket();
    barrier.seal();
    write_blob(&backend, "after the seal").unwrap();
    ticket.landed();
    assert_eq!(backend.journal().committed(), 2);
    drop(backend);
    let reopened = open(&dir);
    assert_eq!(
        blob(&reopened).as_deref(),
        Some("unloaded"),
        "the write after the seal belonged to the next save, which never finished"
    );
}

#[tuff_core::watchdog]
#[test]
fn a_member_lost_before_writing_stops_its_save_from_committing() {
    let dir = TestDir::new();
    saved_once(&dir);
    let backend = open(&dir);
    let barrier = backend.journal().barrier();
    let ticket = barrier.ticket();
    barrier.seal();
    drop(ticket);
    assert!(backend.journal().poisoned());
    assert_eq!(backend.journal().committed(), 1);
}

#[tuff_core::watchdog]
#[test]
fn a_member_that_snapshots_after_the_seal_joins_the_next_save() {
    let dir = TestDir::new();
    saved_once(&dir);
    let backend = open(&dir);
    let barrier = backend.journal().barrier();
    let handle = barrier.handle();
    barrier.seal();
    let late = handle.ticket();
    assert_eq!(late.epoch(), 3);
    assert_eq!(backend.journal().committed(), 2);
    late.landed();
}

#[tuff_core::watchdog]
#[test]
fn a_document_new_since_the_last_save_is_removed_on_rollback() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        backend
            .documents()
            .write(Document::Player(PLAYER + 1), doc(9))
            .unwrap();
    }
    let reopened = open(&dir);
    assert!(!reopened.documents().exists(Document::Player(PLAYER + 1)));
    assert_eq!(player(&reopened), Some(1));
}

#[tuff_core::watchdog]
#[test]
fn a_torn_last_record_is_ignored() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        backend
            .documents()
            .write(Document::Player(PLAYER), doc(2))
            .unwrap();
    }
    let log = epoch_path(&journal_dir(&dir), 2);
    let mut bytes = fs::read(&log).unwrap();
    let whole = bytes.len();
    bytes.extend_from_slice(&encode_record(
        &Key::Document(DocumentId::Level),
        Some(b"never written"),
    ));
    bytes.truncate(whole + 9);
    fs::write(&log, bytes).unwrap();
    let reopened = open(&dir);
    assert_eq!(player(&reopened), Some(1));
}

#[tuff_core::watchdog]
#[test]
fn rolling_back_two_unfinished_saves_restores_the_older_contents() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        for value in [2, 3] {
            let barrier = backend.journal().barrier();
            let ticket = barrier.ticket();
            ticket.scope(|| {
                backend
                    .documents()
                    .write(Document::Player(PLAYER), doc(value))
                    .unwrap();
                write_blob(&backend, &format!("v{value}")).unwrap();
            });
            barrier.seal();
            std::mem::forget(ticket);
        }
        assert_eq!(epochs_in(&journal_dir(&dir)).unwrap(), vec![2, 3]);
    }
    let reopened = open(&dir);
    assert_eq!(player(&reopened), Some(1));
    assert_eq!(blob(&reopened).as_deref(), Some("old"));
    assert_eq!(reopened.journal().open_epoch(), 4);
}

#[tuff_core::watchdog]
#[test]
fn prepared_writes_are_journaled_once_and_roll_back_after_a_crash() {
    let dir = TestDir::new();
    saved_once(&dir);
    {
        let backend = open(&dir);
        let barrier = backend.journal().barrier();
        let ticket = barrier.ticket();
        let epoch = ticket.epoch();
        barrier.seal();
        let bytes = backend
            .blob_codec()
            .write_root(Compound::new().with("v", "new"))
            .unwrap();
        let mut store = backend.open_region(OVERWORLD, region()).unwrap();
        store
            .prepare_writes(&[PreparedWrite {
                pos: chunk(),
                epoch: Some(epoch),
                chunk: true,
                entities: false,
                pois: false,
            }])
            .unwrap();
        let journaled =
            || parse_records(&fs::read(epoch_path(&journal_dir(&dir), epoch)).unwrap()).len();
        assert_eq!(journaled(), 1);
        ticket.scope(|| store.write_chunk(chunk(), &bytes).unwrap());
        store.flush().unwrap();
        assert_eq!(journaled(), 1, "the write found its image journaled");
        assert_eq!(blob(&backend).as_deref(), Some("new"));
        std::mem::forget(ticket);
    }
    let reopened = open(&dir);
    assert_eq!(blob(&reopened).as_deref(), Some("old"));
}

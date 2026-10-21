use std::fs;

use rusqlite::OpenFlags;

use super::*;
use crate::admin::ServerProperties;
use crate::config::fixtures::{round_trips_every_record, sample_lists, user};
use crate::sqlar;
use crate::test_dir::TestDir;

const OVERWORLD: &str = "minecraft:overworld";
const NETHER: &str = "minecraft:the_nether";

fn database(dir: &TestDir) -> PathBuf {
    dir.path().join("nested").join("world.mcserver")
}

fn sidecar(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn raw(file: &Path) -> Connection {
    let conn = Connection::open(file).unwrap();
    conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    conn
}

fn pragma(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

fn tables(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn in_use(result: io::Result<SqliteBackend>) -> bool {
    match result {
        Ok(_) => false,
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
            assert!(err.to_string().contains("already in use"), "{err}");
            true
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn documents_round_trip_by_kind() {
    let dir = TestDir::new();
    let backend = SqliteBackend::open(database(&dir)).unwrap();
    let documents = [
        Document::Level,
        Document::SavedData("minecraft:world_clocks"),
        Document::DimensionData {
            dimension: OVERWORLD,
            id: "minecraft:world_border",
        },
        Document::Player(0x069a79f444e94726a5befca90e38aaf5),
        Document::Stats(0x069a79f444e94726a5befca90e38aaf5),
        Document::Advancements(0x069a79f444e94726a5befca90e38aaf5),
        Document::PlayerData {
            id: "counter",
            uuid: 0x069a79f444e94726a5befca90e38aaf5,
        },
    ];
    for (i, document) in documents.iter().enumerate() {
        assert!(!backend.exists(*document));
        assert!(backend.read(*document).unwrap().is_none());
        backend
            .write(*document, Compound::new().with("n", i as i32))
            .unwrap();
    }
    for (i, document) in documents.iter().enumerate() {
        assert!(backend.exists(*document));
        let root = backend.read(*document).unwrap().unwrap();
        assert_eq!(root.get_int("n"), Some(i as i32));
    }
    backend
        .write(Document::Level, Compound::new().with("n", 99))
        .unwrap();
    assert_eq!(
        backend.read(Document::Level).unwrap().unwrap().get_int("n"),
        Some(99)
    );
    assert!(!backend.exists(Document::Player(1)));
    assert!(!backend.exists(Document::DimensionData {
        dimension: NETHER,
        id: "minecraft:world_border",
    }));
}

#[tuff_core::watchdog]
#[test]
fn blobs_round_trip_per_dimension_and_entities_can_be_removed() {
    let dir = TestDir::new();
    let backend = SqliteBackend::open(database(&dir)).unwrap();
    let pos = ChunkPos::new(-3, 40);
    let mut overworld = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    let mut nether = backend.open_region(NETHER, pos.file_region()).unwrap();
    assert!(overworld.read_chunk(pos).unwrap().is_none());
    overworld.write_chunk(pos, b"overworld chunk").unwrap();
    overworld
        .write_entities(pos, Some(b"overworld ents"))
        .unwrap();
    nether.write_chunk(pos, b"nether chunk").unwrap();
    overworld.flush().unwrap();

    assert_eq!(
        overworld.read_chunk(pos).unwrap().unwrap(),
        b"overworld chunk"
    );
    assert_eq!(
        overworld.read_entities(pos).unwrap().unwrap(),
        b"overworld ents"
    );
    assert_eq!(nether.read_chunk(pos).unwrap().unwrap(), b"nether chunk");
    assert!(nether.read_entities(pos).unwrap().is_none());

    overworld.write_chunk(pos, b"replaced").unwrap();
    assert_eq!(overworld.read_chunk(pos).unwrap().unwrap(), b"replaced");
    overworld.write_entities(pos, None).unwrap();
    assert!(overworld.read_entities(pos).unwrap().is_none());
    overworld.write_entities(pos, None).unwrap();
    assert_eq!(nether.read_chunk(pos).unwrap().unwrap(), b"nether chunk");
    overworld.remove_chunk(pos).unwrap();
    assert!(overworld.read_chunk(pos).unwrap().is_none());
    overworld.remove_chunk(pos).unwrap();
    assert_eq!(nether.read_chunk(pos).unwrap().unwrap(), b"nether chunk");
}

#[tuff_core::watchdog]
#[test]
fn every_config_record_round_trips() {
    for locking in Locking::ALL {
        let dir = TestDir::new();
        let backend = SqliteBackend::open_with(database(&dir), locking).unwrap();
        let config = backend.config().expect("the server file keeps the config");
        assert_eq!(config.name(), NAME);
        round_trips_every_record(&*config);
    }
}

#[tuff_core::watchdog]
#[test]
fn config_tables_are_plain_rows() {
    let dir = TestDir::new();
    let file = database(&dir);
    let lists = sample_lists();
    {
        let backend = SqliteBackend::open(&file).unwrap();
        let config = backend.config().unwrap();
        config
            .write_properties(&ServerProperties::parse("motd=Hi\n"))
            .unwrap();
        config.write_ops(&lists.ops).unwrap();
        config.write_bans(&lists.bans).unwrap();
        config.write_icon(Some(b"icon bytes")).unwrap();
    }
    let conn = raw(&file);
    let motd: String = conn
        .query_row(
            "SELECT value FROM properties WHERE key = 'motd'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(motd, "Hi");
    let op: (String, String, i64, bool) = conn
        .query_row(
            "SELECT uuid, name, level, bypasses_player_limit FROM ops ORDER BY position",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        op,
        (
            crate::admin::format_uuid(user("Admin").uuid),
            "Admin".into(),
            4,
            true
        )
    );
    let ban: (i64, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT created, expires, reason FROM banned_players ORDER BY position",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(ban, (1_700_000_000, None, Some("no".into())));
    let icon: (i64, i64, Vec<u8>) = conn
        .query_row(
            "SELECT mode, sz, data FROM sqlar WHERE name = 'server-icon.png'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(icon, (sqlar::FILE_MODE, 10, b"icon bytes".to_vec()));

    conn.execute(
        "INSERT INTO ops(position, uuid, name, level, bypasses_player_limit) \
         VALUES (99, 'not a uuid', 'Broken', 4, 0)",
        [],
    )
    .unwrap();
    drop(conn);
    let backend = SqliteBackend::open(&file).unwrap();
    assert_eq!(backend.config().unwrap().read_ops().unwrap(), lists.ops);
}

#[tuff_core::watchdog]
#[test]
fn new_files_carry_the_format_ids_and_every_table() {
    let dir = TestDir::new();
    let file = database(&dir);
    drop(SqliteBackend::open(&file).unwrap());
    let conn = raw(&file);
    assert_eq!(pragma(&conn, "application_id"), APPLICATION_ID as i64);
    assert_eq!(pragma(&conn, "user_version"), FORMAT_VERSION as i64);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(
        tables(&conn),
        [
            "banned_ips",
            "banned_players",
            "chunks",
            "documents",
            "entities",
            "ops",
            "pois",
            "properties",
            "sqlar",
            "tuff_settings",
            "whitelist"
        ]
    );
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('sqlar') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(columns, ["name", "mode", "mtime", "sz", "data"]);
}

#[tuff_core::watchdog]
#[test]
fn a_second_server_is_refused_in_either_locking_mode() {
    for first in Locking::ALL {
        for second in Locking::ALL {
            let dir = TestDir::new();
            let file = database(&dir);
            let backend = SqliteBackend::open_with(&file, first).unwrap();
            assert!(
                in_use(SqliteBackend::open_with(&file, second)),
                "{first:?} then {second:?}"
            );
            backend
                .write(Document::Level, Compound::new().with("n", 1))
                .unwrap();
            drop(backend);
            let again = SqliteBackend::open_with(&file, second).unwrap();
            assert_eq!(
                again.read(Document::Level).unwrap().unwrap().get_int("n"),
                Some(1)
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn only_normal_locking_lets_other_programs_read() {
    for locking in Locking::ALL {
        let dir = TestDir::new();
        let file = database(&dir);
        let backend = SqliteBackend::open_with(&file, locking).unwrap();
        backend
            .write(Document::Level, Compound::new().with("n", 5))
            .unwrap();
        let reader = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        reader.busy_timeout(std::time::Duration::ZERO).unwrap();
        let read = reader.query_row("SELECT count(*) FROM documents", [], |row| {
            row.get::<_, i64>(0)
        });
        match locking {
            Locking::Normal => assert_eq!(read.unwrap(), 1),
            Locking::Exclusive => assert_eq!(
                read.unwrap_err().sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            ),
        }
        backend
            .write(Document::Level, Compound::new().with("n", 6))
            .unwrap();
        drop(reader);
    }
}

#[tuff_core::watchdog]
#[test]
fn closing_folds_the_log_back_into_the_file() {
    for locking in Locking::ALL {
        let dir = TestDir::new();
        let file = database(&dir);
        let backend = SqliteBackend::open_with(&file, locking).unwrap();
        let lock = backend.lock().unwrap();
        assert_eq!(
            backend.lock().map(drop).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        backend
            .write(Document::Level, Compound::new().with("n", 3))
            .unwrap();
        assert!(sidecar(&file, "-wal").is_file(), "{locking:?}");
        assert_eq!(
            sidecar(&file, "-shm").exists(),
            locking == Locking::Normal,
            "{locking:?}"
        );
        assert_eq!(lock_path(&file).exists(), locking == Locking::Normal);

        drop(lock);
        assert!(!sidecar(&file, "-wal").exists(), "{locking:?}");
        assert!(!sidecar(&file, "-shm").exists(), "{locking:?}");
        let err = backend.read(Document::Level).unwrap_err();
        assert!(err.to_string().contains("closed"), "{err}");
        let mut names: Vec<String> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        match locking {
            Locking::Exclusive => assert_eq!(names, ["world.mcserver"]),
            Locking::Normal => assert_eq!(names, ["world.mcserver", "world.mcserver.lock"]),
        }
        let reopened = SqliteBackend::open_with(&file, locking).unwrap();
        assert_eq!(
            reopened
                .read(Document::Level)
                .unwrap()
                .unwrap()
                .get_int("n"),
            Some(3)
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn reopening_the_file_sees_the_data() {
    let dir = TestDir::new();
    let file = database(&dir);
    let pos = ChunkPos::new(5, 5);
    {
        let backend = SqliteBackend::open(&file).unwrap();
        backend
            .write(Document::Level, Compound::new().with("seed", 7i64))
            .unwrap();
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        store.write_chunk(pos, b"chunk").unwrap();
        store.write_entities(pos, Some(b"ents")).unwrap();
    }
    let backend = SqliteBackend::open(&file).unwrap();
    assert_eq!(
        backend
            .read(Document::Level)
            .unwrap()
            .unwrap()
            .get_long("seed"),
        Some(7)
    );
    let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    assert_eq!(store.read_chunk(pos).unwrap().unwrap(), b"chunk");
    assert_eq!(store.read_entities(pos).unwrap().unwrap(), b"ents");
    assert!(!dir.path().join("nested/level.dat").exists());
}

#[tuff_core::watchdog]
#[test]
fn files_with_another_application_id_are_refused_untouched() {
    let dir = TestDir::new();
    let file = dir.path().join("other.db");
    {
        let conn = raw(&file);
        conn.execute_batch("PRAGMA application_id = 305419896; CREATE TABLE notes(text TEXT);")
            .unwrap();
    }
    let before = fs::read(&file).unwrap();
    let err = SqliteBackend::open(&file).map(drop).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(
        message.contains("is not a Minecraft server file"),
        "{message}"
    );
    assert!(
        message.contains("application id is 0x12345678"),
        "{message}"
    );
    assert!(message.contains("expected 0x4d435346"), "{message}");
    assert_eq!(fs::read(&file).unwrap(), before);
    assert!(!sidecar(&file, "-wal").exists());
    assert!(SqliteBackend::open_with(&file, Locking::Normal).is_err());
    assert_eq!(fs::read(&file).unwrap(), before);

    let unrelated = dir.path().join("unrelated.db");
    raw(&unrelated)
        .execute_batch("CREATE TABLE notes(text TEXT)")
        .unwrap();
    let err = SqliteBackend::open(&unrelated).map(drop).unwrap_err();
    assert!(err.to_string().contains("no application id"), "{err}");

    let garbage = dir.path().join("garbage.mcserver");
    fs::write(&garbage, vec![0x5a; 4096]).unwrap();
    let err = SqliteBackend::open(&garbage).map(drop).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    assert_eq!(fs::read(&garbage).unwrap(), vec![0x5a; 4096]);
}

#[tuff_core::watchdog]
#[test]
fn newer_formats_are_refused() {
    let dir = TestDir::new();
    let file = database(&dir);
    drop(SqliteBackend::open(&file).unwrap());
    raw(&file)
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();
    let err = SqliteBackend::open(&file)
        .map(drop)
        .unwrap_err()
        .to_string();
    assert!(err.contains("server file format 99"), "{err}");
    assert!(err.contains("supports up to 3"), "{err}");
}

#[tuff_core::watchdog]
#[test]
fn older_formats_without_a_migration_are_refused_untouched() {
    let dir = TestDir::new();
    let file = database(&dir);
    drop(SqliteBackend::open(&file).unwrap());
    raw(&file).execute_batch("PRAGMA user_version = 2").unwrap();
    let err = SqliteBackend::open(&file)
        .map(drop)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot be upgraded"), "{err}");
    assert_eq!(pragma(&raw(&file), "user_version"), 2);
}

#[tuff_core::watchdog]
#[test]
fn bad_documents_fail_to_read() {
    let dir = TestDir::new();
    let file = database(&dir);
    drop(SqliteBackend::open(&file).unwrap());
    raw(&file)
        .execute(
            "INSERT INTO documents(kind, key, data) VALUES ('level', '', X'0102')",
            [],
        )
        .unwrap();
    let backend = SqliteBackend::open(&file).unwrap();
    assert!(backend.read(Document::Level).is_err());
    assert!(backend.exists(Document::Level));
}

#[tuff_core::watchdog]
#[test]
fn opening_a_directory_fails() {
    let dir = TestDir::new();
    assert!(SqliteBackend::open(dir.path()).is_err());
}

#[tuff_core::watchdog]
#[test]
fn entity_index_follows_writes_only_when_enabled() {
    let dir = TestDir::new();
    let file = database(&dir);
    let pos = ChunkPos::new(0, 0);
    drop(SqliteBackend::open(&file).unwrap());
    let count = |file: &Path| -> i64 {
        raw(file)
            .query_row("SELECT count(*) FROM entity_index", [], |row| row.get(0))
            .unwrap()
    };
    {
        let conn = raw(&file);
        tuff_sqlite_nbt::rebuild_entity_index(&conn).unwrap();
        conn.execute(
            "INSERT INTO entity_index(dimension, cx, cz, uuid, type, x, y, z, custom_name) \
             VALUES (?1, 0, 0, 'u', 'minecraft:pig', 0, 0, 0, NULL)",
            [OVERWORLD],
        )
        .unwrap();
    }
    {
        let backend = SqliteBackend::open(&file).unwrap();
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        store.write_entities(pos, None).unwrap();
    }
    assert_eq!(count(&file), 1);
    {
        let backend = SqliteBackend::open(&file).unwrap();
        backend.set_entity_index(true);
        let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
        store.write_entities(pos, None).unwrap();
    }
    assert_eq!(count(&file), 0);
}

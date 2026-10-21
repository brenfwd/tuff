use super::*;

fn world() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE chunks(dimension TEXT, x INTEGER, z INTEGER, data BLOB);
         INSERT INTO chunks VALUES ('minecraft:overworld', 0, 0, x'0102');
         CREATE TABLE notes(id INTEGER PRIMARY KEY, text TEXT);
         INSERT INTO notes(text) VALUES ('a'), ('b'), ('c');",
    )
    .unwrap();
    tuff_sqlite_nbt::register(&conn).unwrap();
    conn
}

fn refused(result: Result<impl std::fmt::Debug>) -> String {
    match result {
        Err(ConsoleError::Sql(text)) => text,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[tuff_core::watchdog]
#[test]
fn queries_page_rows_and_preview_blobs() {
    let conn = world();
    let limits = Limits::default();
    let page = query(
        &conn,
        "SELECT id, text FROM notes ORDER BY id",
        1,
        1,
        limits,
    )
    .unwrap();
    assert_eq!(
        page.rows,
        vec![vec![Cell::Integer(2), Cell::Text("b".into())]]
    );
    assert!(page.has_more);
    assert_eq!(page.columns[1].decl_type.as_deref(), Some("TEXT"));
    let blob = query(&conn, "SELECT data FROM chunks", 0, 10, limits).unwrap();
    assert_eq!(
        blob.rows[0][0],
        Cell::Blob {
            prefix: vec![1, 2],
            size: 2
        }
    );
    let capped = Limits { bytes: 1, ..limits };
    let page = query(&conn, "SELECT text FROM notes", 0, 10, capped).unwrap();
    assert!(page.truncated && page.has_more);
    assert_eq!(page.rows.len(), 1);
}

#[tuff_core::watchdog]
#[test]
fn reads_refuse_every_kind_of_write() {
    let conn = world();
    let limits = Limits::default();
    for statement in [
        "DELETE FROM notes",
        "INSERT INTO notes(text) VALUES ('x')",
        "CREATE TEMP TABLE t(x)",
        "CREATE TEMP TRIGGER t AFTER INSERT ON notes BEGIN SELECT 1; END",
        "ATTACH DATABASE ':memory:' AS other",
        "PRAGMA query_only = 0",
        "PRAGMA journal_mode = DELETE",
        "SELECT tuff_entity_index_rebuild()",
        "SELECT load_extension('x')",
        "BEGIN",
        "SELECT 1; DELETE FROM notes",
    ] {
        refused(query(&conn, statement, 0, 10, limits));
    }
    assert_eq!(count(&conn, "notes"), 3);
    let after: i64 = conn
        .query_row("PRAGMA query_only", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 0);
    conn.execute("INSERT INTO notes(text) VALUES ('ok')", [])
        .unwrap();
    assert!(query(&conn, "PRAGMA table_info(notes)", 0, 10, limits).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn a_runaway_query_stops_at_the_time_limit() {
    let conn = world();
    let limits = Limits {
        timeout: Duration::from_millis(50),
        ..Limits::default()
    };
    let text = refused(query(
        &conn,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
         SELECT count(*) FROM n",
        0,
        10,
        limits,
    ));
    assert!(text.contains("time limit"), "{text}");
    assert!(query(&conn, "SELECT 1", 0, 1, Limits::default()).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn writes_preview_then_commit_only_the_previewed_count() {
    let mut conn = world();
    let limits = Limits::default();
    let sql = "UPDATE notes SET text = 'z' WHERE id < 3";
    let preview = write(&mut conn, sql, WriteAction::Preview, limits).unwrap();
    assert_eq!(preview.affected, 2);
    assert!(!preview.committed);
    assert_eq!(count(&conn, "notes WHERE text = 'z'"), 0);
    assert_eq!(
        write(&mut conn, sql, WriteAction::Commit { expect: 3 }, limits),
        Err(ConsoleError::Changed {
            expected: 3,
            actual: 2
        })
    );
    assert_eq!(count(&conn, "notes WHERE text = 'z'"), 0);
    let done = write(&mut conn, sql, WriteAction::Commit { expect: 2 }, limits).unwrap();
    assert!(done.committed);
    assert_eq!(count(&conn, "notes WHERE text = 'z'"), 2);
    assert!(conn.is_autocommit());
}

#[tuff_core::watchdog]
#[test]
fn a_write_that_times_out_rolls_back() {
    let mut conn = world();
    let limits = Limits {
        timeout: Duration::from_millis(50),
        ..Limits::default()
    };
    let text = refused(write(
        &mut conn,
        "INSERT INTO notes(text) WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
         SELECT 'x' FROM n",
        WriteAction::Commit { expect: 0 },
        limits,
    ));
    assert!(text.contains("time limit"), "{text}");
    assert_eq!(count(&conn, "notes"), 3);
    assert!(conn.is_autocommit());
}

#[tuff_core::watchdog]
#[test]
fn writes_never_touch_server_tables_triggers_or_other_files() {
    let mut conn = world();
    let limits = Limits::default();
    for statement in [
        "DROP TABLE chunks",
        "CREATE INDEX c ON chunks(x)",
        "CREATE TRIGGER t AFTER INSERT ON notes BEGIN DELETE FROM chunks; END",
        "CREATE TRIGGER t AFTER INSERT ON notes BEGIN SELECT 1; END",
        "CREATE TEMP TABLE t(x)",
        "ATTACH DATABASE ':memory:' AS other",
        "PRAGMA user_version = 3",
        "COMMIT",
        "SELECT load_extension('x')",
    ] {
        let text = refused(write(&mut conn, statement, WriteAction::Preview, limits));
        assert!(!text.is_empty());
    }
    let text = refused(write(
        &mut conn,
        "DROP TABLE chunks",
        WriteAction::Commit { expect: 0 },
        limits,
    ));
    assert!(text.contains("running server"), "{text}");
    assert_eq!(count(&conn, "chunks"), 1);
    let made = write(
        &mut conn,
        "CREATE TABLE mine AS SELECT x, z FROM chunks",
        WriteAction::Commit { expect: 0 },
        limits,
    )
    .unwrap();
    assert!(made.committed);
    assert_eq!(count(&conn, "mine"), 1);
}

#[tuff_core::watchdog]
#[test]
fn world_row_edits_report_the_chunks_they_touch_and_commit_only_when_checked() {
    let mut conn = world();
    let limits = Limits::default();
    let key = |x| ChunkKey {
        dimension: "minecraft:overworld".into(),
        x,
        z: 0,
    };
    let preview = write(
        &mut conn,
        "UPDATE chunks SET x = 5 WHERE x = 0",
        WriteAction::Preview,
        limits,
    )
    .unwrap();
    assert_eq!(preview.touched, vec![key(0), key(5)]);
    assert_eq!(count(&conn, "chunks"), 1);
    let none = std::collections::HashSet::from([key(0)]);
    let text = refused(write_checked(
        &mut conn,
        "UPDATE chunks SET x = 5 WHERE x = 0",
        WriteAction::Commit { expect: 1 },
        limits,
        Some(&none),
    ));
    assert!(text.contains("not checked"), "{text}");
    let all = std::collections::HashSet::from([key(0), key(5)]);
    let done = write_checked(
        &mut conn,
        "UPDATE chunks SET x = 5 WHERE x = 0",
        WriteAction::Commit { expect: 1 },
        limits,
        Some(&all),
    )
    .unwrap();
    assert!(done.committed);
    let x: i64 = conn
        .query_row("SELECT x FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(x, 5);
    let temp: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_temp_schema WHERE name LIKE '_tuff_touch%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(temp, 0);
    let text = refused(write(
        &mut conn,
        "CREATE TRIGGER t AFTER INSERT ON notes BEGIN DELETE FROM chunks; END",
        WriteAction::Preview,
        limits,
    ));
    assert!(text.contains("triggers"), "{text}");
}

#[tuff_core::watchdog]
#[test]
fn schema_lists_tables_views_and_columns() {
    let conn = world();
    let objects = schema(&conn).unwrap();
    let notes = objects.iter().find(|o| o.name == "notes").unwrap();
    assert_eq!(notes.kind, "table");
    assert!(notes.columns[0].pk);
    assert!(
        objects
            .iter()
            .any(|o| o.name == "entity_list" && o.kind == "view")
    );
    let plan = explain(&conn, "SELECT * FROM notes WHERE id = 1", Limits::default()).unwrap();
    assert!(!plan.is_empty());
    refused(explain(&conn, "DELETE FROM notes", Limits::default()));
}

#[tuff_core::watchdog]
#[test]
fn huge_values_fail_instead_of_allocating() {
    let conn = world();
    let before = conn
        .limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH)
        .unwrap();
    refused(query(
        &conn,
        "SELECT length(zeroblob(1000000000))",
        0,
        1,
        Limits::default(),
    ));
    refused(query(
        &conn,
        "SELECT length(printf('%.*c', 900000000, 'x'))",
        0,
        1,
        Limits::default(),
    ));
    assert_eq!(
        conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH)
            .unwrap(),
        before
    );
}

#[tuff_core::watchdog]
#[test]
fn vacuum_never_writes_another_file() {
    let mut conn = world();
    let dir = crate::test_dir::TestDir::new();
    let target = dir.path().join("copy.db");
    let sql = format!("VACUUM INTO '{}'", target.display());
    refused(query(&conn, &sql, 0, 1, Limits::default()));
    refused(write(
        &mut conn,
        &sql,
        WriteAction::Preview,
        Limits::default(),
    ));
    refused(write(
        &mut conn,
        "VACUUM",
        WriteAction::Preview,
        Limits::default(),
    ));
    assert!(!target.exists());
}

mod apart {
    use super::*;
    use crate::backend::WorldBackend;
    use crate::sqlite::{Locking, SqliteBackend};
    use crate::test_dir::TestDir;

    fn seeded(dir: &TestDir, locking: Locking) -> (SqliteBackend, SqlConsole) {
        let backend = SqliteBackend::open_with(dir.path().join("world.mcserver"), locking).unwrap();
        let console = backend.sql_console().unwrap();
        let limits = Limits::default();
        console
            .write(
                "CREATE TABLE notes(id INTEGER PRIMARY KEY, text TEXT)".into(),
                WriteAction::Commit { expect: 0 },
                limits,
            )
            .unwrap();
        console
            .write(
                "INSERT INTO notes(text) VALUES ('a'),('b'),('c'),('d'),('e')".into(),
                WriteAction::Commit { expect: 5 },
                limits,
            )
            .unwrap();
        (backend, console)
    }

    fn insert_f(console: &SqlConsole) {
        console
            .write(
                "INSERT INTO notes(text) VALUES ('f')".into(),
                WriteAction::Commit { expect: 1 },
                Limits::default(),
            )
            .unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cursor_pages_a_consistent_view_while_the_server_keeps_writing() {
        let dir = TestDir::new();
        let (_backend, console) = seeded(&dir, Locking::Normal);
        let limits = Limits::default();
        let cursor = console
            .cursor(
                "SELECT id FROM notes ORDER BY id".into(),
                limits,
                Cancel::new(),
            )
            .unwrap();
        let first = cursor.fetch(2, limits).unwrap();
        assert_eq!(first.rows.len(), 2);
        assert!(first.has_more);
        insert_f(&console);
        let rest = cursor.fetch(10, limits).unwrap();
        let ids: Vec<_> = rest.rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(
            ids,
            vec![Cell::Integer(3), Cell::Integer(4), Cell::Integer(5)]
        );
        assert!(!rest.has_more);
        let fresh = console
            .query(
                "SELECT count(*) FROM notes".into(),
                0,
                1,
                limits,
                Cancel::new(),
            )
            .unwrap();
        assert_eq!(fresh.rows[0][0], Cell::Integer(6));
    }

    #[tuff_core::watchdog]
    #[test]
    fn exclusive_locking_queries_a_snapshot_copy_that_a_write_refreshes() {
        let dir = TestDir::new();
        let (_backend, console) = seeded(&dir, Locking::Exclusive);
        let limits = Limits::default();
        let count = |console: &SqlConsole| {
            console
                .query(
                    "SELECT count(*) FROM notes".into(),
                    0,
                    1,
                    limits,
                    Cancel::new(),
                )
                .unwrap()
                .rows[0][0]
                .clone()
        };
        assert_eq!(count(&console), Cell::Integer(5));
        insert_f(&console);
        assert_eq!(count(&console), Cell::Integer(6));
        drop(console);
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".console-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cancelling_stops_a_running_cursor_statement() {
        let dir = TestDir::new();
        let (_backend, console) = seeded(&dir, Locking::Normal);
        let limits = Limits {
            timeout: Duration::from_secs(30),
            ..Limits::default()
        };
        let cancel = Cancel::new();
        let cursor = console
            .cursor(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                 SELECT count(*) FROM n"
                    .into(),
                limits,
                cancel.clone(),
            )
            .unwrap();
        let stopper = std::thread::Builder::new()
            .name("console-test-stopper".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                cancel.cancel();
            })
            .unwrap();
        let text = refused(cursor.fetch(10, limits));
        stopper.join().unwrap();
        assert!(text.contains("cancelled"), "{text}");
    }
}

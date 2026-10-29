use crate::common;

use std::sync::Arc;

use common::{TempDir, TestServer};
use serde_json::{Value, json};
use tuff::Server;
use tuff_test_support::admin_client::{AdminClient, find_code};
use tuff_web_ui as web;

async fn start(instance: TempDir, sqlite: bool) -> TestServer {
    let builder = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1);
    let server = if sqlite {
        let file = instance.path().join("world.mcserver");
        let backend = tuff_storage::sqlite::SqliteBackend::open(file).unwrap();
        builder.storage(Arc::new(backend)).build()
    } else {
        builder.build()
    };
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    }
}

async fn run(server: &TestServer, line: &str) -> String {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|c| c.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn login(server: &TestServer) -> AdminClient {
    run(server, &format!("tuff config {} true", web::ENABLED)).await;
    let reply = run(server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = AdminClient::new(server.handle.http_addrs().await.unwrap()[0]);
    assert_eq!(admin.login(&code).await.status, 200);
    admin
}

fn route(rest: &str) -> String {
    format!("{}/{rest}", web::SQL_ROUTE)
}

async fn post(admin: &AdminClient, rest: &str, body: Value) -> (u16, Value) {
    let resp = admin.send_json("POST", &route(rest), &body).await;
    let status = resp.status;
    (
        status,
        serde_json::from_str(&resp.body).unwrap_or(Value::Null),
    )
}

async fn audited(admin: &AdminClient, action: &str) -> Vec<Value> {
    admin
        .get(&format!("{}?action={action}", web::AUDIT_ROUTE))
        .await
        .json()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn write_attempts(
    admin: &AdminClient,
    statements: &[&'static str],
) -> Vec<(&'static str, &'static str, u16)> {
    let mut out = Vec::new();
    for &sql in statements {
        let (q, _) = post(admin, "query", json!({ "sql": sql })).await;
        out.push((sql, "query", q));
        for action in ["preview", "commit"] {
            let body = json!({ "sql": sql, "action": action, "expect": 0 });
            let (w, _) = post(admin, "write", body).await;
            out.push((sql, action, w));
        }
    }
    out
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_file_world_console_writes_once_allowed() {
    let server = start(TempDir::new(), false).await;
    let admin = login(&server).await;
    let state = admin.get(web::SQL_ROUTE).await.json();
    assert_eq!(state["available"], true, "{state}");
    assert_eq!(state["canWrite"], false, "{state}");
    let (status, page) = post(&admin, "query", json!({ "sql": "SELECT 1" })).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);

    let statements = [
        "INSERT INTO chunks VALUES (1)",
        "UPDATE chunks SET x = 1",
        "DELETE FROM chunks",
        "CREATE TABLE notes(x)",
        "DROP TABLE chunks",
    ];
    for (sql, kind, status) in write_attempts(&admin, &statements).await {
        assert!(status >= 400, "{kind} {sql} got {status}");
    }

    run(
        &server,
        &format!("tuff config {} true", web::sql::ALLOW_WRITE_KEY),
    )
    .await;
    assert_eq!(admin.get(web::SQL_ROUTE).await.json()["canWrite"], true);
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": "CREATE TABLE notes(x)", "action": "commit", "expect": 0 }),
    )
    .await;
    assert!(status >= 400, "a schema change must not be saved: {body}");
    let insert = "INSERT INTO chunks(dimension, x, z, data) VALUES ('minecraft:overworld', 90000, 90000, x'02789c030000000001')";
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "preview" }),
    )
    .await;
    assert_eq!((status, body["chunks"].as_u64()), (200, Some(1)), "{body}");
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "commit", "expect": 1 }),
    )
    .await;
    assert_eq!((status, &body["committed"]), (200, &json!(true)), "{body}");
    let (_, page) = post(
        &admin,
        "query",
        json!({ "sql": "SELECT count(*) FROM chunks WHERE x = 90000" }),
    )
    .await;
    assert_eq!(page["rows"][0][0].as_u64(), Some(1), "{page}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_are_read_only_paged_and_audited() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    let state = admin.get(web::SQL_ROUTE).await.json();
    assert_eq!(state["available"], true, "{state}");
    assert_eq!(state["canWrite"], false);
    let (status, page) = post(
        &admin,
        "query",
        json!({ "sql": "SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name", "limit": 2 }),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["rows"].as_array().unwrap().len(), 2);
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["types"][0][0], "text");
    assert!(page["elapsedMs"].is_u64());
    let (status, refused) = post(&admin, "query", json!({ "sql": "DELETE FROM chunks" })).await;
    assert_eq!(status, 400, "{refused}");
    let (status, _) = post(&admin, "explain", json!({ "sql": "SELECT * FROM chunks" })).await;
    assert_eq!(status, 200);
    let schema = admin.get(&route("schema")).await.json();
    assert!(
        schema["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "entity_list")
    );
    let export = admin
        .send_json(
            "POST",
            &route("export"),
            &json!({ "sql": "SELECT 1 AS a", "format": "csv" }),
        )
        .await;
    assert_eq!(export.status, 200);
    assert_eq!(export.body, "a\r\n1\r\n");
    let entries = audited(&admin, "sql.query").await;
    assert!(
        entries
            .iter()
            .any(|e| e["detail"] == "DELETE FROM chunks" && e["outcome"] != "")
    );
    assert_eq!(audited(&admin, "sql.export").await.len(), 1);
    let saved = admin
        .send_json(
            "PUT",
            &route("saved"),
            &json!({ "queries": [{ "name": "n", "sql": "SELECT 1" }] }),
        )
        .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(
        admin.get(&route("saved")).await.json()["queries"][0]["name"],
        "n"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_need_the_permission_and_the_setting() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    let made = admin
        .send_json(
            "POST",
            web::TOKENS_ROUTE,
            &json!({ "name": "mod", "level": 3 }),
        )
        .await;
    let token = made.json()["token"].as_str().unwrap().to_owned();
    let other = AdminClient::new(server.handle.http_addrs().await.unwrap()[0]);
    let auth = format!("Bearer {token}");
    let denied = other
        .request("GET", web::SQL_ROUTE, &[("Authorization", &auth)], None)
        .await;
    assert_eq!(denied.status, 403);

    let create = json!({ "sql": "CREATE TABLE notes(x)", "action": "preview" });
    let (status, body) = post(&admin, "write", create.clone()).await;
    assert_eq!(status, 403, "{body}");
    assert!(body["error"].as_str().unwrap().contains("allowWrite"));
    assert_eq!(
        audited(&admin, "sql.write.preview").await[0]["outcome"],
        "denied"
    );

    run(
        &server,
        &format!("tuff config {} true", web::sql::ALLOW_WRITE_KEY),
    )
    .await;
    assert_eq!(admin.get(web::SQL_ROUTE).await.json()["canWrite"], true);
    let (status, body) = post(&admin, "write", create).await;
    assert_eq!(
        (status, body["affected"].as_u64()),
        (200, Some(0)),
        "{body}"
    );
    let (status, _) = post(&admin, "query", json!({ "sql": "SELECT * FROM notes" })).await;
    assert_eq!(status, 400, "a preview must roll back");
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": "CREATE TABLE notes(x)", "action": "commit", "expect": 0 }),
    )
    .await;
    assert_eq!((status, &body["committed"]), (200, &json!(true)), "{body}");
    let insert = "INSERT INTO notes VALUES (1), (2)";
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "commit", "expect": 5 }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let (status, _) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "rollback" }),
    )
    .await;
    assert_eq!(status, 200);
    let (_, page) = post(
        &admin,
        "query",
        json!({ "sql": "SELECT count(*) FROM notes" }),
    )
    .await;
    assert_eq!(page["rows"][0][0], 0);
    assert_eq!(audited(&admin, "sql.write.rollback").await.len(), 1);
    assert_eq!(audited(&admin, "sql.write.commit").await.len(), 2);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_world_tables_stay_read_only() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    run(
        &server,
        &format!("tuff config {} true", web::sql::ALLOW_WRITE_KEY),
    )
    .await;
    run(&server, "save-all flush").await;
    let count = || async {
        post(
            &admin,
            "query",
            json!({ "sql": "SELECT count(*) FROM documents" }),
        )
        .await
        .1["rows"][0][0]
            .as_u64()
            .unwrap()
    };
    let before = count().await;
    assert!(before > 0);
    for sql in [
        "UPDATE documents SET data = x''",
        "DELETE FROM ops",
        "CREATE TRIGGER t AFTER INSERT ON entity_index BEGIN DELETE FROM chunks; END",
    ] {
        let (status, body) = post(
            &admin,
            "write",
            json!({ "sql": sql, "action": "commit", "expect": 0 }),
        )
        .await;
        assert_eq!(status, 400, "{sql}: {body}");
    }
    assert_eq!(count().await, before);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_rows_edit_only_while_their_chunks_are_unloaded() {
    chunk_rows_edit_flow(true).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn region_file_chunk_rows_edit_only_while_their_chunks_are_unloaded() {
    chunk_rows_edit_flow(false).await;
}

async fn chunk_rows_edit_flow(sqlite: bool) {
    let server = start(TempDir::new(), sqlite).await;
    let admin = login(&server).await;
    run(
        &server,
        &format!("tuff config {} true", web::sql::ALLOW_WRITE_KEY),
    )
    .await;
    run(&server, "forceload add 0 0").await;
    for _ in 0..200 {
        run(&server, "save-all flush").await;
        let (_, page) = post(
            &admin,
            "query",
            json!({ "sql": "SELECT count(*) FROM chunks" }),
        )
        .await;
        if page["rows"][0][0].as_u64() > Some(0) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": "DELETE FROM chunks", "action": "preview" }),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("loaded"), "{body}");
    let insert = "INSERT INTO chunks(dimension, x, z, data) VALUES ('minecraft:overworld', 90000, 90000, x'02789c030000000001')";
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "preview" }),
    )
    .await;
    assert_eq!((status, body["chunks"].as_u64()), (200, Some(1)), "{body}");
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": insert, "action": "commit", "expect": 1 }),
    )
    .await;
    assert_eq!((status, &body["committed"]), (200, &json!(true)), "{body}");
    let delete = "DELETE FROM chunks WHERE x = 90000";
    let (status, body) = post(
        &admin,
        "write",
        json!({ "sql": delete, "action": "commit", "expect": 1 }),
    )
    .await;
    assert_eq!((status, &body["committed"]), (200, &json!(true)), "{body}");
    let (_, page) = post(
        &admin,
        "query",
        json!({ "sql": "SELECT count(*) FROM chunks WHERE x = 90000" }),
    )
    .await;
    assert_eq!(page["rows"][0][0], 0);
    stop(server).await;
}

fn example_queries() -> Vec<String> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let guide = std::fs::read_to_string(format!("{root}/docs/guides/sqlite-queries.md")).unwrap();
    let mut queries = Vec::new();
    let mut block: Option<String> = None;
    for line in guide.lines() {
        match (&mut block, line.trim_start()) {
            (None, "```sql") => block = Some(String::new()),
            (Some(text), "```") => {
                queries.extend(
                    text.split(';')
                        .map(str::trim)
                        .filter(|q| !q.is_empty())
                        .map(str::to_owned),
                );
                block = None;
            }
            (Some(text), _) => {
                text.push_str(line);
                text.push('\n');
            }
            _ => {}
        }
    }
    let panel =
        std::fs::read_to_string(format!("{root}/crates/tuff-web-ui/app/src/sql.ts")).unwrap();
    for line in panel.lines() {
        if let Some((_, rest)) = line.split_once("sql: \"") {
            queries.push(rest.split_once('"').unwrap().0.to_owned());
        }
    }
    assert!(queries.len() >= 10, "{queries:?}");
    queries
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_example_query_runs_on_a_generated_world() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    run(&server, "save-all flush").await;
    for sql in example_queries() {
        let (status, body) = post(&admin, "query", json!({ "sql": sql })).await;
        if sql.contains("tuff_entity_index_rebuild") {
            assert_eq!(status, 400, "the console must refuse {sql}: {body}");
        } else {
            assert_eq!(status, 200, "{sql}: {body}");
        }
    }
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_requests_without_the_csrf_header_are_refused() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    let resp = admin
        .request(
            "POST",
            &route("query"),
            &[("Content-Type", "application/json")],
            Some(r#"{"sql":"SELECT 1"}"#),
        )
        .await;
    assert_eq!(resp.status, 403, "{}", resp.body);
    let anonymous = AdminClient::new(server.handle.http_addrs().await.unwrap()[0]);
    assert_eq!(anonymous.get(web::SQL_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pages_stream_from_a_cursor_and_a_cancel_stops_a_statement() {
    let server = start(TempDir::new(), true).await;
    let admin = login(&server).await;
    let state = admin.get(web::SQL_ROUTE).await.json();
    assert_eq!(state["limits"]["timeoutMs"], 60_000);

    let (status, first) = post(
        &admin,
        "query",
        json!({ "sql": "SELECT value FROM generate_series(1, 5)", "limit": 2 }),
    )
    .await;
    if status == 200 {
        let cursor = first["cursor"].as_u64().unwrap();
        let (status, second) =
            post(&admin, "query", json!({ "cursor": cursor, "limit": 10 })).await;
        assert_eq!(status, 200, "{second}");
        assert_eq!(second["rows"].as_array().unwrap().len(), 3);
        assert!(second.get("cursor").is_none());
        let (status, _) = post(&admin, "query", json!({ "cursor": cursor })).await;
        assert_eq!(status, 410);
    }

    let (status, page) = post(
        &admin,
        "query",
        json!({ "sql": "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 5) SELECT i FROM n", "limit": 2 }),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    let cursor = page["cursor"].as_u64().unwrap();
    let (status, rest) = post(&admin, "query", json!({ "cursor": cursor, "limit": 10 })).await;
    assert_eq!(status, 200, "{rest}");
    assert_eq!(rest["rows"].as_array().unwrap().len(), 3);
    assert_eq!(rest["hasMore"], false);
    let (status, _) = post(&admin, "query", json!({ "cursor": cursor })).await;
    assert_eq!(status, 410);

    let runaway = post(
        &admin,
        "query",
        json!({ "sql": "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n" }),
    );
    let canceller = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let (status, body) = post(&admin, "cancel", json!({})).await;
            assert_eq!(status, 200);
            if body["cancelled"] == true {
                break;
            }
        }
    };
    let ((status, body), ()) = tokio::join!(runaway, canceller);
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("cancelled"),
        "{body}"
    );
    stop(server).await;
}

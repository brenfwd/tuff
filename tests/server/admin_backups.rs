use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{TempDir, TestServer, start_server_with_features};
use serde_json::{Value, json};
use tuff_test_support::admin_client::{AdminClient, find_code};
use tuff_web_ui as web;
use tuff_zip::{Archive, DosTime, Limits, write_all};

const ROUTE: &str = web::BACKUPS_ROUTE;

async fn start(instance: TempDir) -> TestServer {
    start_server_with_features(
        instance,
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![Box::new(
            tuff_feature_builtin_core_commands::CoreCommandsFeature,
        )],
    )
    .await
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

async fn listing(admin: &AdminClient) -> Value {
    let resp = admin.get(ROUTE).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    resp.json()
}

async fn backed_up(admin: &AdminClient) -> String {
    let created = admin
        .send_json("POST", &format!("{ROUTE}/create"), &json!({}))
        .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let name = created.json()["running"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    for _ in 0..400 {
        let list = listing(admin).await;
        if list["status"]["running"].is_null() && list["status"]["last"]["name"] == name.as_str() {
            assert_eq!(list["status"]["last"]["error"], Value::Null);
            return name;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the backup never finished");
}

async fn upload(
    admin: &AdminClient,
    bytes: &[u8],
) -> tuff_test_support::admin_client::AdminResponse {
    admin
        .request_bytes(
            "PUT",
            &format!("{ROUTE}/upload?filename=up.zip"),
            &[],
            bytes,
            Some(bytes.len()),
        )
        .await
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

async fn audit_actions(admin: &AdminClient, want: &[&str]) -> Vec<String> {
    let mut actions = Vec::new();
    for _ in 0..100 {
        let entries = admin
            .get(&format!("{}?limit=100", web::AUDIT_ROUTE))
            .await
            .json();
        actions = entries["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect();
        if want.iter().all(|w| actions.iter().any(|a| a == w)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    actions
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backups_download_upload_and_restore_at_the_next_start() {
    let instance = TempDir::new();
    let dir = instance.path().to_path_buf();
    let server = start(instance).await;
    let admin = login(&server).await;
    let list = listing(&admin).await;
    assert_eq!(list["level"], "world");
    assert_eq!(list["world"], "folder");
    assert_eq!(list["staged"], Value::Null);

    let name = backed_up(&admin).await;
    let whole = admin.get(&format!("{ROUTE}/{name}")).await;
    assert_eq!(whole.status, 200);
    assert!(
        whole.header("content-disposition").unwrap().contains(&name),
        "{:?}",
        whole.headers
    );
    let zip = whole.raw.clone();
    assert_eq!(zip.len() as u64, list_size(&admin, &name).await);
    let resumed = admin
        .request(
            "GET",
            &format!("{ROUTE}/{name}"),
            &[("Range", "bytes=100-")],
            None,
        )
        .await;
    assert_eq!(resumed.status, 206);
    assert_eq!(resumed.raw, zip[100..]);
    assert_eq!(admin.get(&format!("{ROUTE}/..%2Fworld")).await.status, 404);

    let uploaded = upload(&admin, &zip).await;
    assert_eq!(uploaded.status, 200, "{}", uploaded.body);
    assert_eq!(uploaded.json()["checked"]["kind"], "folder");
    let copy = uploaded.json()["backup"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(copy, name);
    assert_eq!(
        listing(&admin).await["backups"].as_array().unwrap().len(),
        2
    );

    std::fs::write(dir.join("world/after-backup.txt"), "later").unwrap();
    let wrong = admin
        .send_json(
            "POST",
            &format!("{ROUTE}/restore"),
            &json!({ "name": copy, "confirm": "nope" }),
        )
        .await;
    assert_eq!(wrong.status, 400);
    let staged = admin
        .send_json(
            "POST",
            &format!("{ROUTE}/restore"),
            &json!({ "name": copy, "confirm": "world" }),
        )
        .await;
    assert_eq!(staged.status, 200, "{}", staged.body);
    assert_eq!(listing(&admin).await["staged"]["backup"], copy.as_str());
    assert!(
        dir.join("world/after-backup.txt").exists(),
        "the live world is untouched"
    );
    let actions = audit_actions(
        &admin,
        &[
            "backup.create",
            "backup.download",
            "backup.upload",
            "backup.restore",
        ],
    )
    .await;
    for want in [
        "backup.create",
        "backup.download",
        "backup.upload",
        "backup.restore",
    ] {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }

    let stopping = admin
        .send_json("POST", &format!("{ROUTE}/restore/shutdown"), &json!({}))
        .await;
    assert_eq!(stopping.status, 202, "{}", stopping.body);
    let TestServer {
        task, _instance, ..
    } = server;
    tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the server stopped")
        .unwrap()
        .unwrap();

    let server = start(_instance).await;
    assert!(!dir.join("world/after-backup.txt").exists(), "restored");
    assert!(dir.join("world/level.dat").exists());
    let aside: Vec<String> = files_in(&dir)
        .into_iter()
        .filter(|n| n.starts_with("world-before-restore-"))
        .collect();
    assert_eq!(aside.len(), 1, "{:?}", files_in(&dir));
    assert!(dir.join(&aside[0]).join("after-backup.txt").exists());
    assert!(!files_in(&dir).iter().any(|n| n.contains(".restore")));
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn list_size(admin: &AdminClient, name: &str) -> u64 {
    listing(admin).await["backups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == name)
        .unwrap()["size"]
        .as_u64()
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_uploads_are_refused_and_leave_nothing_behind() {
    let instance = TempDir::new();
    let dir = instance.path().join("backups");
    let server = start(instance).await;
    let admin = login(&server).await;
    let name = backed_up(&admin).await;
    let zip = admin.get(&format!("{ROUTE}/{name}")).await.raw;
    let level = Archive::open(&zip[..], Limits::untrusted(1 << 30))
        .unwrap()
        .read("world/level.dat", 1 << 20)
        .unwrap();
    run(
        &server,
        &format!("tuff config {} 1", web::MAX_BACKUP_UPLOAD),
    )
    .await;

    let archive =
        |entries: &[(&str, &[u8])]| write_all(entries, DosTime::MIN, 9, u64::MAX).unwrap();
    let zeros = vec![0u8; 3 << 20];
    let bomb = archive(&[("w/level.dat", &level), ("w/a", &zeros), ("w/b", &zeros)]);
    assert!(bomb.len() < 1 << 20);
    let traversal = archive(&[("w/level.dat", &level), ("w/../../evil", b"x")]);
    let two = archive(&[("a/level.dat", &level), ("b/level.dat", &level)]);
    for (what, bytes, status) in [
        ("bomb", bomb, 422),
        ("traversal", traversal, 422),
        ("two worlds", two, 422),
        ("garbage", b"not a backup".to_vec(), 422),
        ("too large", vec![1u8; (1 << 20) + 1], 413),
    ] {
        let resp = upload(&admin, &bytes).await;
        assert_eq!(resp.status, status, "{what}: {}", resp.body);
        assert_eq!(
            files_in(&dir),
            std::slice::from_ref(&name),
            "{what} left something"
        );
    }

    let short = admin
        .request_bytes(
            "PUT",
            &format!("{ROUTE}/upload"),
            &[],
            b"PK\x03\x04 only part",
            Some(4096),
        )
        .await;
    assert_eq!(short.status, 400, "{}", short.body);
    let long = admin
        .request_bytes("PUT", &format!("{ROUTE}/upload"), &[], &zip, Some(64))
        .await;
    assert_eq!(
        long.status, 422,
        "only the declared bytes arrive: {}",
        long.body
    );
    let chunked = admin
        .request_bytes(
            "PUT",
            &format!("{ROUTE}/upload"),
            &[("Transfer-Encoding", "chunked")],
            b"5\r\nhello\r\n0\r\n\r\n",
            None,
        )
        .await;
    assert_eq!(chunked.status, 411);
    let mut anonymous = AdminClient::new(admin.addr);
    anonymous.csrf = admin.csrf.clone();
    assert_eq!(upload(&anonymous, b"x").await.status, 401);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(files_in(&dir), std::slice::from_ref(&name));

    let deleted = admin
        .request(
            "DELETE",
            &format!("{ROUTE}/{name}"),
            &[("X-Tuff-CSRF", admin.csrf.as_deref().unwrap())],
            None,
        )
        .await;
    assert_eq!(deleted.status, 200, "{}", deleted.body);
    assert!(files_in(&dir).is_empty());
    let actions = audit_actions(&admin, &["backup.delete"]).await;
    assert!(
        actions.iter().filter(|a| *a == "backup.upload").count() >= 5,
        "{actions:?}"
    );
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

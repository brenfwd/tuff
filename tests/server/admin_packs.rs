use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TempDir, TestServer};
use serde_json::{Value, json};
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::protocol::pack::PackStatus;
use tuff_storage::sqlite;
use tuff_test_support::admin_client::{AdminClient, find_code};
use tuff_web_ui as web;
use tuff_zip::{DosTime, write_all};

const DATAPACKS: &str = web::DATAPACKS_ROUTE;
const PACKS: &str = web::RESOURCE_PACKS_ROUTE;

#[derive(Clone, Copy, PartialEq)]
enum Storage {
    Folder,
    ServerFile,
}

async fn start(instance: TempDir, storage: Storage) -> TestServer {
    let builder = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1);
    let server = match storage {
        Storage::Folder => builder.build(),
        Storage::ServerFile => builder
            .storage_backend(sqlite::NAME)
            .world_path(instance.path().join("world.mcserver"))
            .build(),
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

async fn restart(server: TestServer, storage: Storage) -> TestServer {
    let TestServer {
        handle,
        task,
        _instance,
        ..
    } = server;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    start(_instance, storage).await
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
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

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    write_all(entries, DosTime::MIN, 6, u64::MAX).unwrap()
}

fn data_meta() -> Vec<u8> {
    let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
    format!(
        r#"{{"pack":{{"description":"panel pack","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
        v.major, v.minor
    )
    .into_bytes()
}

fn resource_meta() -> Vec<u8> {
    let v = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    format!(
        r#"{{"pack":{{"description":"shiny","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
        v.major, v.minor
    )
    .into_bytes()
}

fn data_pack(command: &str) -> Vec<u8> {
    archive(&[
        ("pack.mcmeta", &data_meta()),
        ("data/panel/function/hi.mcfunction", command.as_bytes()),
    ])
}

async fn put(
    admin: &AdminClient,
    path: &str,
    bytes: &[u8],
) -> tuff_test_support::admin_client::AdminResponse {
    admin
        .request_bytes("PUT", path, &[], bytes, Some(bytes.len()))
        .await
}

async fn post(
    admin: &AdminClient,
    path: &str,
    body: Value,
) -> tuff_test_support::admin_client::AdminResponse {
    admin.send_json("POST", path, &body).await
}

async fn delete(admin: &AdminClient, path: &str) -> tuff_test_support::admin_client::AdminResponse {
    admin
        .request(
            "DELETE",
            path,
            &[("X-Tuff-CSRF", admin.csrf.as_deref().unwrap())],
            None,
        )
        .await
}

async fn audit_actions(admin: &AdminClient, want: &[&str]) -> Vec<String> {
    let mut actions = Vec::new();
    for _ in 0..100 {
        let entries = admin
            .get(&format!("{}?limit=200", web::AUDIT_ROUTE))
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

fn pack<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["packs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
}

async fn data_packs_round_trip(storage: Storage) {
    let server = start(TempDir::new(), storage).await;
    let admin = login(&server).await;
    let list = admin.get(DATAPACKS).await;
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(pack(&list.json(), "vanilla").unwrap()["required"], true);

    let bytes = data_pack("scoreboard objectives add panel dummy");
    let uploaded = put(
        &admin,
        &format!("{DATAPACKS}/upload?filename=Panel%20Pack.zip&enable=1"),
        &bytes,
    )
    .await;
    assert_eq!(uploaded.status, 200, "{}", uploaded.body);
    let id = uploaded.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(id, "file/Panel_Pack.zip");
    assert_eq!(uploaded.json()["enabled"], true);
    assert_eq!(uploaded.json()["meta"]["compatibility"], "compatible");
    assert!(
        run(&server, "function panel:hi")
            .await
            .contains("Running function panel:hi")
    );

    let again = put(
        &admin,
        &format!("{DATAPACKS}/upload?name=Panel_Pack"),
        &bytes,
    )
    .await;
    assert_eq!(again.status, 409, "{}", again.body);
    let replaced = put(
        &admin,
        &format!("{DATAPACKS}/upload?name=Panel_Pack&replace=1"),
        &data_pack("scoreboard objectives add replaced dummy"),
    )
    .await;
    assert_eq!(replaced.status, 200, "{}", replaced.body);
    assert_eq!(replaced.json()["replaced"], true);
    assert_eq!(replaced.json()["enabled"], true);

    let download = admin.get(&format!("{DATAPACKS}/Panel_Pack.zip")).await;
    assert_eq!(download.status, 200);
    assert!(
        download
            .header("content-disposition")
            .unwrap()
            .contains("Panel_Pack.zip")
    );
    assert_eq!(
        download.raw,
        data_pack("scoreboard objectives add replaced dummy")
    );

    let listed = admin.get(DATAPACKS).await.json();
    let entry = pack(&listed, &id).unwrap();
    assert_eq!(entry["selected"], true);
    assert_eq!(entry["kind"], "zip");
    assert_eq!(entry["position"], 1);

    let off = post(&admin, &format!("{DATAPACKS}/disable"), json!({ "id": id })).await;
    assert_eq!(off.status, 200, "{}", off.body);
    assert!(
        run(&server, "function panel:hi")
            .await
            .contains("Unknown function")
    );
    assert_eq!(
        post(&admin, &format!("{DATAPACKS}/disable"), json!({ "id": id }))
            .await
            .status,
        409
    );
    assert_eq!(
        post(
            &admin,
            &format!("{DATAPACKS}/disable"),
            json!({ "id": "vanilla" })
        )
        .await
        .status,
        409
    );
    assert_eq!(
        post(
            &admin,
            &format!("{DATAPACKS}/enable"),
            json!({ "id": "file/missing" })
        )
        .await
        .status,
        404
    );
    let on = post(&admin, &format!("{DATAPACKS}/enable"), json!({ "id": id })).await;
    assert_eq!(on.status, 200, "{}", on.body);
    assert!(
        run(&server, "function panel:hi")
            .await
            .contains("Running function")
    );
    let reload = post(&admin, &format!("{DATAPACKS}/reload"), json!({})).await;
    assert_eq!(reload.status, 200, "{}", reload.body);

    let server = restart(server, storage).await;
    let mut admin = admin;
    admin.addr = server.handle.http_addrs().await.unwrap()[0];
    admin.host = format!("localhost:{}", admin.addr.port());
    assert!(
        run(&server, "function panel:hi")
            .await
            .contains("Running function")
    );
    let removed = delete(&admin, &format!("{DATAPACKS}/Panel_Pack.zip")).await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert!(
        run(&server, "function panel:hi")
            .await
            .contains("Unknown function")
    );
    assert!(pack(&admin.get(DATAPACKS).await.json(), &id).is_none());
    assert_eq!(
        delete(&admin, &format!("{DATAPACKS}/Panel_Pack.zip"))
            .await
            .status,
        404
    );

    let actions = audit_actions(
        &admin,
        &[
            "datapack.upload",
            "datapack.download",
            "datapack.disable",
            "datapack.enable",
            "datapack.reload",
            "datapack.remove",
        ],
    )
    .await;
    for want in [
        "datapack.upload",
        "datapack.download",
        "datapack.disable",
        "datapack.enable",
        "datapack.reload",
        "datapack.remove",
    ] {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_packs_upload_enable_and_remove_in_a_folder_world() {
    data_packs_round_trip(Storage::Folder).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_packs_upload_enable_and_remove_in_a_server_file() {
    data_packs_round_trip(Storage::ServerFile).await;
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.map(Result::unwrap) {
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_pack_uploads_are_refused_and_leave_nothing_behind() {
    let instance = TempDir::new();
    let datapacks = instance.path().join("world/datapacks");
    let resourcepacks = instance.path().join("resourcepacks");
    let server = start(instance, Storage::Folder).await;
    let admin = login(&server).await;
    let meta = data_meta();
    let rmeta = resource_meta();
    let zeros = vec![0u8; 20 << 20];
    for (what, path, bytes, status) in [
        (
            "garbage",
            format!("{DATAPACKS}/upload?name=g"),
            b"PK not a zip".to_vec(),
            422,
        ),
        (
            "no meta",
            format!("{DATAPACKS}/upload?name=n"),
            archive(&[("data/a/x.json", b"{}")]),
            422,
        ),
        (
            "traversal",
            format!("{DATAPACKS}/upload?name=t"),
            archive(&[("pack.mcmeta", &meta), ("../../evil", b"x")]),
            422,
        ),
        (
            "case twins",
            format!("{DATAPACKS}/upload?name=c"),
            archive(&[
                ("pack.mcmeta", &meta),
                ("data/a.json", b"1"),
                ("data/A.json", b"2"),
            ]),
            422,
        ),
        (
            "bomb",
            format!("{DATAPACKS}/upload?name=b"),
            archive(&[("pack.mcmeta", &meta), ("data/big.json", &zeros)]),
            413,
        ),
        (
            "bad name",
            format!("{DATAPACKS}/upload?name=..%2Fescape"),
            data_pack("say hi"),
            422,
        ),
        (
            "garbage",
            format!("{PACKS}/upload?name=g"),
            b"PK not a zip".to_vec(),
            422,
        ),
        (
            "no meta",
            format!("{PACKS}/upload?name=n"),
            archive(&[("assets/x.png", b"x")]),
            422,
        ),
        (
            "traversal",
            format!("{PACKS}/upload?name=t"),
            archive(&[("pack.mcmeta", &rmeta), ("../../evil", b"x")]),
            422,
        ),
        (
            "unreadable meta",
            format!("{PACKS}/upload?name=u"),
            archive(&[("pack.mcmeta", b"{")]),
            422,
        ),
    ] {
        let resp = put(&admin, &path, &bytes).await;
        assert_eq!(resp.status, status, "{what} at {path}: {}", resp.body);
    }
    for path in [DATAPACKS, PACKS] {
        let oversized = admin
            .request_bytes(
                "PUT",
                &format!("{path}/upload?name=big"),
                &[],
                b"PK",
                Some(300 << 20),
            )
            .await;
        assert_eq!(oversized.status, 413, "{path}: {}", oversized.body);
        let chunked = admin
            .request_bytes(
                "PUT",
                &format!("{path}/upload?name=c"),
                &[("Transfer-Encoding", "chunked")],
                b"5\r\nhello\r\n0\r\n\r\n",
                None,
            )
            .await;
        assert_eq!(chunked.status, 411, "{path}");
        let short = admin
            .request_bytes(
                "PUT",
                &format!("{path}/upload?name=s"),
                &[],
                b"PK\x03\x04 only part",
                Some(4096),
            )
            .await;
        assert_eq!(short.status, 400, "{path}: {}", short.body);
        let mut anonymous = AdminClient::new(admin.addr);
        anonymous.csrf = admin.csrf.clone();
        assert_eq!(
            put(&anonymous, &format!("{path}/upload?name=a"), b"x")
                .await
                .status,
            401
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        files_under(&datapacks).is_empty(),
        "{:?}",
        files_under(&datapacks)
    );
    assert!(
        files_under(&resourcepacks).is_empty(),
        "{:?}",
        files_under(&resourcepacks)
    );
    let actions = audit_actions(&admin, &["datapack.upload", "resourcepack.upload"]).await;
    assert!(
        actions.iter().filter(|a| *a == "datapack.upload").count() >= 6,
        "{actions:?}"
    );
    assert!(
        actions
            .iter()
            .filter(|a| *a == "resourcepack.upload")
            .count()
            >= 4,
        "{actions:?}"
    );
    stop(server).await;
}

fn pack_zip() -> Vec<u8> {
    archive(&[
        ("pack.mcmeta", &resource_meta()),
        (
            "assets/minecraft/textures/block/stone.png",
            b"not really a png",
        ),
    ])
}

async fn public_get(
    server: &TestServer,
    path: &str,
) -> tuff_test_support::admin_client::AdminResponse {
    AdminClient::new(server.handle.http_addrs().await.unwrap()[0])
        .get(path)
        .await
}

async fn resource_packs_round_trip(storage: Storage) {
    let server = start(TempDir::new(), storage).await;
    let admin = login(&server).await;
    let bytes = pack_zip();
    let uploaded = put(
        &admin,
        &format!("{PACKS}/upload?filename=Shiny.zip"),
        &bytes,
    )
    .await;
    assert_eq!(uploaded.status, 200, "{}", uploaded.body);
    let body = uploaded.json();
    let id = body["pack"]["id"].as_str().unwrap().to_owned();
    assert_eq!(body["pack"]["name"], "Shiny");
    assert_eq!(body["checked"]["compatibility"], "compatible");
    assert_eq!(body["checked"]["files"], 2);

    let url = format!("/resourcepacks/{id}.zip");
    let served = public_get(&server, &url).await;
    assert_eq!(served.status, 200);
    assert_eq!(served.raw, bytes);
    let panel_copy = admin.get(&format!("{PACKS}/{id}")).await;
    assert_eq!(panel_copy.raw, bytes);

    let list = admin.get(PACKS).await;
    assert_eq!(list.status, 200, "{}", list.body);
    let list = list.json();
    let entry = pack(&list, &id).unwrap();
    assert_eq!(entry["kind"], "uploaded");
    assert_eq!(entry["editable"], true);
    assert_eq!(entry["required"], false);
    assert_eq!(entry["description"], "shiny");
    let sha1 = entry["sha1"].as_str().unwrap().to_owned();
    assert_eq!(sha1.len(), 40);

    let changed = post(
        &admin,
        &format!("{PACKS}/{id}/settings"),
        json!({ "required": true, "prompt": "Please", "order": 7, "name": "Shinier" }),
    )
    .await;
    assert_eq!(changed.status, 200, "{}", changed.body);
    let entry = pack(&admin.get(PACKS).await.json(), &id).unwrap().clone();
    assert_eq!(entry["required"], true);
    assert_eq!(entry["prompt"], "Please");
    assert_eq!(entry["order"], 7);
    assert_eq!(entry["name"], "Shinier");
    let offer = server
        .handle
        .resource_packs()
        .await
        .unwrap()
        .offer(tuff_core::resource_pack::parse_uuid(&id).unwrap())
        .await
        .unwrap();
    assert!(offer.required);
    assert_eq!(offer.prompt.unwrap().plain(), "Please");
    assert_eq!(
        post(
            &admin,
            &format!("{PACKS}/{id}/settings"),
            json!({ "order": 99999 })
        )
        .await
        .status,
        422
    );

    let server = restart(server, storage).await;
    let mut admin = admin;
    admin.addr = server.handle.http_addrs().await.unwrap()[0];
    admin.host = format!("localhost:{}", admin.addr.port());
    let mut entry = Value::Null;
    for _ in 0..200 {
        entry = pack(&admin.get(PACKS).await.json(), &id)
            .cloned()
            .unwrap_or(Value::Null);
        if entry["state"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(entry["state"], "ready", "{entry}");
    assert_eq!(entry["sha1"], sha1.as_str());
    assert_eq!(entry["required"], true);
    assert_eq!(entry["name"], "Shinier");
    assert_eq!(public_get(&server, &url).await.raw, bytes);

    let removed = delete(&admin, &format!("{PACKS}/{id}")).await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert_eq!(public_get(&server, &url).await.status, 404);
    assert!(pack(&admin.get(PACKS).await.json(), &id).is_none());
    assert_eq!(delete(&admin, &format!("{PACKS}/{id}")).await.status, 404);

    let actions = audit_actions(
        &admin,
        &[
            "resourcepack.upload",
            "resourcepack.download",
            "resourcepack.settings",
            "resourcepack.delete",
        ],
    )
    .await;
    for want in [
        "resourcepack.upload",
        "resourcepack.download",
        "resourcepack.settings",
        "resourcepack.delete",
    ] {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }
    let server = restart(server, storage).await;
    admin.addr = server.handle.http_addrs().await.unwrap()[0];
    admin.host = format!("localhost:{}", admin.addr.port());
    assert!(pack(&admin.get(PACKS).await.json(), &id).is_none());
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploaded_resource_packs_survive_restarts_in_a_folder_instance() {
    resource_packs_round_trip(Storage::Folder).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploaded_resource_packs_survive_restarts_in_a_server_file() {
    resource_packs_round_trip(Storage::ServerFile).await;
}

async fn player_packs(admin: &AdminClient, name: &str) -> Value {
    let list = admin.get(PACKS).await.json();
    list["players"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .map_or(Value::Null, |p| p["packs"].clone())
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_get_uploaded_packs_and_their_status_is_listed() {
    let server = start(TempDir::new(), Storage::Folder).await;
    let admin = login(&server).await;
    let uploaded = put(&admin, &format!("{PACKS}/upload?name=Joined"), &pack_zip()).await;
    assert_eq!(uploaded.status, 200, "{}", uploaded.body);
    let id = uploaded.json()["pack"]["id"].as_str().unwrap().to_owned();

    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            pack_status: PackStatus::Loaded,
            ..BotConfig::new("Viewer")
        },
    )
    .await
    .unwrap();
    assert!(bot.state().pack_offers.iter().any(|o| !o.in_play));
    let mut packs = Value::Null;
    for _ in 0..200 {
        packs = player_packs(&admin, "Viewer").await;
        if packs[0]["status"] == "loaded" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(packs[0]["id"], id.as_str(), "{packs}");
    assert_eq!(packs[0]["status"], "loaded");
    assert_eq!(packs[0]["loaded"], true);
    assert_eq!(packs[0]["current"], true);

    let popped = post(
        &admin,
        &format!("{PACKS}/{id}/pop"),
        json!({ "player": "viewer" }),
    )
    .await;
    assert_eq!(popped.status, 200, "{}", popped.body);
    for _ in 0..200 {
        packs = player_packs(&admin, "Viewer").await;
        if packs.as_array().is_some_and(Vec::is_empty) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(packs, json!([]));
    let pushed = post(
        &admin,
        &format!("{PACKS}/{id}/push"),
        json!({ "player": "Viewer" }),
    )
    .await;
    assert_eq!(pushed.status, 200, "{}", pushed.body);
    bot.wait_for(|state| state.pack_offers.iter().any(|o| o.in_play))
        .await
        .unwrap();
    assert_eq!(
        post(
            &admin,
            &format!("{PACKS}/{id}/push"),
            json!({ "player": "Nobody" })
        )
        .await
        .status,
        404
    );
    let everyone = post(&admin, &format!("{PACKS}/{id}/push"), json!({})).await;
    assert_eq!(everyone.status, 200);
    let actions = audit_actions(&admin, &["resourcepack.push", "resourcepack.pop"]).await;
    assert!(
        actions.iter().any(|a| a == "resourcepack.pop"),
        "{actions:?}"
    );
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pack_pages_need_their_scopes() {
    let server = start(TempDir::new(), Storage::Folder).await;
    let admin = login(&server).await;
    for route in [DATAPACKS, PACKS] {
        let anonymous = AdminClient::new(admin.addr);
        assert_eq!(anonymous.get(route).await.status, 401, "{route}");
        assert_eq!(admin.get(route).await.status, 200, "{route}");
        assert_eq!(
            admin.get(&format!("{route}/a/b/c")).await.status,
            404,
            "{route}"
        );
    }
    let session = admin.get(web::SESSION_ROUTE).await.json();
    let scopes: Vec<&str> = session["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for scope in [
        "datapacks.read",
        "datapacks.manage",
        "resourcepacks.read",
        "resourcepacks.manage",
    ] {
        assert!(scopes.contains(&scope), "{scope} in {scopes:?}");
    }
    stop(server).await;
}

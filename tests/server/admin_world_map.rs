//! The world map plugin's panel page and API on a real server: listed and
//! served only with `map.view`, tiles answered with versioned ETags.

use crate::common;

use common::{MockSessionServer, TEST_UUID, TempDir, TestServer, start_server_with_features};
use serde_json::json;
use tuff_test_support::admin_client::{AdminClient, find_code};
use tuff_web_ui as web;
use tuff_web_ui::Principal;

const API: &str = "/admin/api/v1/plugins/world-map";
const MODULE: &str = "/admin/pages/world-map/map.js";

async fn server(instance: TempDir, session_url: &str) -> TestServer {
    start_server_with_features(
        instance,
        "127.0.0.1:0",
        session_url,
        Some(false),
        vec![
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
            Box::new(tuff_feature_builtin_world_map::WorldMapFeature),
        ],
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

async fn client(server: &TestServer) -> AdminClient {
    AdminClient::new(server.handle.http_addrs().await.unwrap()[0])
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_map_page_and_tiles_are_served_behind_the_session() {
    let server = server(TempDir::new(), "http://127.0.0.1:1").await;
    run(&server, &format!("tuff config {} true", web::ENABLED)).await;
    let anonymous = client(&server).await;
    assert_eq!(anonymous.get(MODULE).await.status, 401);
    assert_eq!(
        anonymous
            .get(&format!("{API}/tiles/0/0/0/0.png"))
            .await
            .status,
        401
    );

    let reply = run(&server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 200);

    let pages = admin.get(web::PAGES_ROUTE).await.json();
    let page = pages["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "world-map")
        .unwrap_or_else(|| panic!("{pages}"))
        .clone();
    assert_eq!(page["scope"], "map.view");
    assert_eq!(page["module"], MODULE);
    let module = admin.get(MODULE).await;
    assert_eq!(module.status, 200);
    assert!(
        module.body.contains("__tuffPanel"),
        "uses the shell's React"
    );

    let info = admin.get(API).await;
    assert_eq!(info.status, 200, "{}", info.body);
    let info = info.json();
    assert_eq!(info["tileSize"], 512);
    assert_eq!(info["maxLevel"], 6);
    assert!(info["dimensions"].as_array().unwrap().len() >= 3, "{info}");

    let tile = admin.get(&format!("{API}/tiles/0/0/0/0.png")).await;
    assert_eq!(tile.status, 200, "{}", tile.body);
    assert_eq!(tile.header("content-type"), Some("image/png"));
    assert_eq!(tile.raw[..4], [0x89, b'P', b'N', b'G']);
    let etag = tile.header("etag").unwrap().to_owned();
    assert!(etag.starts_with('"') && etag.contains('.'), "{etag}");
    let again = admin
        .request(
            "GET",
            &format!("{API}/tiles/0/0/0/0.png"),
            &[("If-None-Match", &etag)],
            None,
        )
        .await;
    assert_eq!(again.status, 304);
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    assert_eq!(
        admin.get(&format!("{API}/tiles/0/9/0/0.png")).await.status,
        404
    );
    assert_eq!(
        admin.get(&format!("{API}/tiles/77/0/0/0.png")).await.status,
        404
    );

    let viewport =
        json!({"dim": 0, "level": 0, "minX": -100, "minZ": -100, "maxX": 100, "maxZ": 100});
    let lease = admin
        .send_json("POST", &format!("{API}/viewport"), &viewport)
        .await;
    assert_eq!(lease.status, 200, "{}", lease.body);
    let bad = admin
        .send_json("POST", &format!("{API}/viewport"), &json!({"dim": 0}))
        .await;
    assert_eq!(bad.status, 400);
    let markers = admin.get(&format!("{API}/markers")).await;
    assert_eq!(markers.json(), json!({"players": []}));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_map_needs_operator_level_two() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "op-permission-level=1\n",
    )
    .unwrap();
    let server = server(instance, &session_url).await;
    run(&server, &format!("tuff config {} true", web::ENABLED)).await;
    run(&server, &format!("tuff config {} 1", web::MIN_OP_LEVEL)).await;
    run(&server, "op Alex").await;
    let operator = Principal::Operator {
        uuid: u128::from_str_radix(TEST_UUID, 16).unwrap(),
        name: "Alex".into(),
    };
    let code = server
        .handle
        .web_login_code(operator)
        .await
        .unwrap()
        .unwrap();
    let mut admin = client(&server).await;
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["level"], 1);
    let pages = admin.get(web::PAGES_ROUTE).await.json();
    assert!(
        !pages["pages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "world-map"),
        "{pages}"
    );
    assert_eq!(admin.get(MODULE).await.status, 403);
    assert_eq!(
        admin.get(&format!("{API}/tiles/0/0/0/0.png")).await.status,
        403
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn an_open_map_gets_live_player_markers_and_tiles_of_loaded_chunks() {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(tuff_feature_builtin_world_map::WorldMapFeature));
    let instance = TempDir::new();
    let server = tuff::Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let server = TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    };
    run(&server, &format!("tuff config {} true", web::ENABLED)).await;
    let reply = run(&server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 200);

    let bot = tuff_bot::Bot::connect(
        server.addr,
        tuff_bot::BotConfig {
            view_distance: 2,
            ..tuff_bot::BotConfig::new("Mapper")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();

    let info = admin.get(API).await.json();
    let dim = info["spawn"]["dim"].as_u64().unwrap();
    let (x, z) = (
        info["spawn"]["x"].as_i64().unwrap(),
        info["spawn"]["z"].as_i64().unwrap(),
    );
    let mut events = admin
        .events("plugin.world-map.markers,plugin.world-map.tiles")
        .await;
    assert_eq!(events.status, 200);
    let viewport = json!({"dim": dim, "level": 0, "minX": x - 64, "minZ": z - 64, "maxX": x + 64, "maxZ": z + 64});
    let lease = admin
        .send_json("POST", &format!("{API}/viewport"), &viewport)
        .await;
    assert_eq!(lease.status, 200, "{}", lease.body);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let players = loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = events.next(left).await.expect("a players event arrives");
        if event.event == "players" {
            break event.data;
        }
    };
    let players: serde_json::Value = serde_json::from_str(&players).unwrap();
    let row = &players["players"][0];
    assert_eq!(row["name"], "Mapper", "{players}");
    assert_eq!(row["dim"], dim);
    assert_eq!(admin.get(&format!("{API}/markers")).await.json(), players);

    let (tx, tz) = (x.div_euclid(512), z.div_euclid(512));
    let tile = admin
        .get(&format!("{API}/tiles/{dim}/0/{tx}/{tz}.png"))
        .await;
    assert_eq!(tile.status, 200, "{}", tile.body);
    assert!(
        tile.raw.len() > 1400,
        "the loaded chunks are drawn ({} bytes)",
        tile.raw.len()
    );
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_map_starts_a_pre_generation_job_and_streams_its_progress() {
    let server = server(TempDir::new(), "http://127.0.0.1:1").await;
    run(&server, &format!("tuff config {} true", web::ENABLED)).await;
    let reply = run(&server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 200);

    let idle = admin.get(&format!("{API}/pregen")).await;
    assert_eq!(idle.json(), json!({"running": null, "last": null}));
    let mut events = admin.events("plugin.world-map.pregen").await;
    assert_eq!(events.status, 200);
    let job = json!({"dim": 0, "x": 1000, "z": 1000, "radius": 24, "shape": "square"});
    let started = admin
        .send_json("POST", &format!("{API}/pregen"), &job)
        .await;
    assert_eq!(started.status, 200, "{}", started.body);
    let started = started.json();
    assert_eq!(started["total"], 16);
    assert_eq!(started["shape"], "square");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut saw_progress = false;
    let last = loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = events.next(left).await.expect("status events arrive");
        let status: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        saw_progress |= !status["running"].is_null();
        if status["running"].is_null() && !status["last"].is_null() {
            break status["last"].clone();
        }
    };
    assert!(saw_progress);
    assert_eq!(last["generated"], 16);
    assert_eq!(last["failed"], 0);
    let done = admin.get(&format!("{API}/pregen")).await.json();
    assert_eq!(done["last"]["id"], started["id"]);
    let cancel = admin
        .send_json("POST", &format!("{API}/pregen/cancel"), &json!({}))
        .await;
    assert_eq!(cancel.status, 409);
    let regenerate =
        json!({"dim": 0, "x": 0, "z": 0, "radius": 1, "shape": "circle", "regenerate": true});
    let refused = admin
        .send_json("POST", &format!("{API}/pregen"), &regenerate)
        .await;
    assert_eq!(refused.status, 400, "regenerating needs confirming");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn viewers_below_level_four_see_jobs_but_cannot_start_them() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "op-permission-level=2\n",
    )
    .unwrap();
    let server = server(instance, &session_url).await;
    run(&server, &format!("tuff config {} true", web::ENABLED)).await;
    run(&server, &format!("tuff config {} 2", web::MIN_OP_LEVEL)).await;
    run(&server, "op Alex").await;
    let operator = Principal::Operator {
        uuid: u128::from_str_radix(TEST_UUID, 16).unwrap(),
        name: "Alex".into(),
    };
    let code = server
        .handle
        .web_login_code(operator)
        .await
        .unwrap()
        .unwrap();
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 200);
    assert_eq!(admin.get(&format!("{API}/pregen")).await.status, 200);
    let job = json!({"dim": 0, "x": 0, "z": 0, "radius": 8, "shape": "square"});
    let started = admin
        .send_json("POST", &format!("{API}/pregen"), &job)
        .await;
    assert_eq!(started.status, 403, "{}", started.body);
    let cancel = admin
        .send_json("POST", &format!("{API}/pregen/cancel"), &json!({}))
        .await;
    assert_eq!(cancel.status, 403);
    stop(server).await;
}

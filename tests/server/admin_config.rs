use crate::common;

use std::time::Duration;

use common::{TempDir, TestServer};
use serde_json::{Value, json};
use tuff::Server;
use tuff_core::config::{Key, Value as ConfigValue};
use tuff_test_support::admin_client::{AdminClient, AdminResponse, find_code};
use tuff_web_ui as web;
use tuff_web_ui::Principal;

const CONFIG: &str = web::CONFIG_ROUTE;
const SECRET_KEY: &str = "vault.apiToken";
const LIMIT_KEY: &str = "vault.limit";

struct VaultFeature;

impl tuff_core::feature::Feature for VaultFeature {
    fn id(&self) -> &'static str {
        "vault"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let keys = &mut cx.registries().config_keys;
            keys.push(
                Key::new(
                    SECRET_KEY,
                    ConfigValue::String(String::new()),
                    "Token for the vault",
                )
                .secret(),
            );
            keys.push(
                Key::new(LIMIT_KEY, ConfigValue::Int(5), "Vault size").validated(|v| match v {
                    ConfigValue::Int(n) if (1..=10).contains(n) => Ok(()),
                    _ => Err("must be 1 to 10".into()),
                }),
            );
        }
        Ok(())
    }
}

async fn start(instance: TempDir) -> TestServer {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(VaultFeature));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .config_overrides(vec![("tuff.offlineOps".into(), "true".into())])
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
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

async fn client(server: &TestServer) -> AdminClient {
    AdminClient::new(server.handle.http_addrs().await.unwrap()[0])
}

async fn console_login(server: &TestServer) -> AdminClient {
    run(server, &format!("tuff config {} true", web::ENABLED)).await;
    let reply = run(server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = client(server).await;
    assert_eq!(admin.login(&code).await.status, 200);
    admin
}

async fn put(admin: &AdminClient, path: &str, body: Value) -> AdminResponse {
    admin
        .send_json("PUT", &format!("{CONFIG}{path}"), &body)
        .await
}

fn find<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {list}"))
}

async fn audit_entries(admin: &AdminClient, wanted: usize) -> Vec<Value> {
    let mut entries = Vec::new();
    for _ in 0..100 {
        entries = admin
            .get(&format!("{}?limit=200", web::AUDIT_ROUTE))
            .await
            .json()["entries"]
            .as_array()
            .unwrap()
            .clone();
        let config = entries
            .iter()
            .filter(|e| {
                let action = e["action"].as_str().unwrap_or_default();
                action.starts_with("config.")
                    || action.starts_with("properties.")
                    || action.starts_with("gamerules.")
            })
            .count();
        if config >= wanted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    entries
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_round_trip_by_kind_and_validators_answer_400() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let state = admin.get(CONFIG).await;
    assert_eq!(state.status, 200, "{}", state.body);
    let state = state.json();
    assert_eq!(state["writable"], true);
    let limit = find(&state["settings"], LIMIT_KEY);
    assert_eq!(limit["kind"], "int");
    assert_eq!(limit["value"], "5");
    assert_eq!(limit["default"], "5");
    assert_eq!(limit["feature"], "vault");
    assert_eq!(limit["group"], "vault");
    assert_eq!(find(&state["settings"], web::ENABLED)["feature"], "core");
    assert_eq!(find(&state["settings"], web::ENABLED)["lockout"], true);

    for (key, text, kind) in [
        (LIMIT_KEY, "7", "int"),
        ("tuff.resourcePacks.enabled", "false", "bool"),
        (web::PUBLIC_URL, "https://panel.example/", "string"),
        (
            "tuff.http.resourcePathBaseUrl",
            "http://cdn.example/r",
            "string",
        ),
    ] {
        let resp = put(&admin, &format!("/{key}"), json!({ "value": text })).await;
        assert_eq!(resp.status, 200, "{key}: {}", resp.body);
        assert_eq!(resp.json()["value"], text, "{key}");
        assert_eq!(resp.json()["kind"], kind, "{key}");
        let shown = run(&server, &format!("tuff config {key}")).await;
        assert!(shown.contains(&format!("{key} = {text}")), "{shown}");
    }
    let bad = put(&admin, &format!("/{LIMIT_KEY}"), json!({ "value": "11" })).await;
    assert_eq!(bad.status, 400);
    assert_eq!(bad.json()["error"], "must be 1 to 10");
    let bad = put(&admin, &format!("/{LIMIT_KEY}"), json!({ "value": "many" })).await;
    assert_eq!(bad.status, 400);
    assert_eq!(bad.json()["error"], "'many' is not an integer");
    let bad = put(
        &admin,
        &format!("/{}", web::MIN_OP_LEVEL),
        json!({ "value": "0" }),
    )
    .await;
    assert_eq!(bad.status, 400);
    assert_eq!(bad.json()["error"], "must be 1 to 4");
    assert_eq!(
        put(&admin, "/tuff.nothing", json!({ "value": "1" }))
            .await
            .status,
        404
    );

    let reset = put(&admin, &format!("/{LIMIT_KEY}"), json!({ "reset": true })).await;
    assert_eq!(reset.status, 200);
    assert_eq!(reset.json()["value"], "5");

    let entries = audit_entries(&admin, 6).await;
    let set: Vec<&Value> = entries
        .iter()
        .filter(|e| e["action"] == "config.set" && e["target"] == LIMIT_KEY)
        .collect();
    assert!(
        set.iter()
            .any(|e| e["detail"] == "7" && e["outcome"] == "ok"),
        "{entries:?}"
    );
    assert!(
        set.iter()
            .any(|e| e["detail"] == "11" && e["outcome"] == "must be 1 to 10"),
        "{entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e["action"] == "config.reset" && e["target"] == LIMIT_KEY),
        "{entries:?}"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secret_settings_are_write_only_and_redacted() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let listed = admin.get(CONFIG).await.json();
    let secret = find(&listed["settings"], SECRET_KEY);
    assert_eq!(secret["secret"], true);
    assert_eq!(secret["set"], false);
    assert!(secret.get("value").is_none() && secret.get("default").is_none());

    let resp = put(
        &admin,
        &format!("/{SECRET_KEY}"),
        json!({ "value": "hunter2" }),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert!(!resp.body.contains("hunter2"), "{}", resp.body);
    assert_eq!(resp.json()["set"], true);
    let listed = admin.get(CONFIG).await;
    assert!(!listed.body.contains("hunter2"));
    assert_eq!(find(&listed.json()["settings"], SECRET_KEY)["set"], true);

    let shown = run(&server, &format!("tuff config {SECRET_KEY}")).await;
    assert!(
        shown.contains("<set>") && !shown.contains("hunter2"),
        "{shown}"
    );
    let cleared = put(&admin, &format!("/{SECRET_KEY}"), json!({ "reset": true })).await;
    assert_eq!(cleared.json()["set"], false);
    let ran = admin
        .send_json(
            "POST",
            web::COMMAND_ROUTE,
            &json!({ "line": format!("tuff config {SECRET_KEY} swordfish") }),
        )
        .await;
    assert_eq!(ran.status, 200, "{}", ran.body);
    assert!(!ran.body.contains("swordfish"), "{}", ran.body);
    assert_eq!(
        find(&admin.get(CONFIG).await.json()["settings"], SECRET_KEY)["set"],
        true
    );

    let entries = audit_entries(&admin, 1).await;
    let text = serde_json::to_string(&entries).unwrap();
    assert!(
        !text.contains("hunter2") && !text.contains("swordfish"),
        "{text}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e["action"] == "config.set" && e["detail"] == "<redacted>"),
        "{text}"
    );
    assert!(
        entries.iter().any(|e| e["action"] == "command"
            && e["target"] == format!("tuff config {SECRET_KEY} <redacted>")),
        "{text}"
    );
    let properties = admin.get(CONFIG).await.json();
    let rcon = find(&properties["properties"], "rcon.password");
    assert_eq!(
        (rcon["secret"].as_bool(), rcon["set"].as_bool()),
        (Some(true), Some(false))
    );
    let resp = put(
        &admin,
        "/properties/rcon.password",
        json!({ "value": "letmein" }),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert!(!resp.body.contains("letmein") && resp.json()["set"] == true);
    assert!(!admin.get(CONFIG).await.body.contains("letmein"));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lockout_changes_need_confirmation() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let path = format!("/{}", web::HOSTNAMES);
    let refused = put(&admin, &path, json!({ "value": "panel.example" })).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(refused.json()["confirm"], true);
    assert_eq!(
        find(&admin.get(CONFIG).await.json()["settings"], web::HOSTNAMES)["value"],
        ""
    );
    let done = put(
        &admin,
        &path,
        json!({ "value": "panel.example", "confirm": true }),
    )
    .await;
    assert_eq!(done.status, 200, "{}", done.body);
    assert_eq!(done.json()["value"], "panel.example");
    let lower = put(
        &admin,
        &format!("/{}", web::MIN_OP_LEVEL),
        json!({ "value": 3 }),
    )
    .await;
    assert_eq!(lower.status, 200, "{}", lower.body);
    let raise = put(
        &admin,
        &format!("/{}", web::MIN_OP_LEVEL),
        json!({ "value": 4 }),
    )
    .await;
    assert_eq!(raise.status, 409);
    let off = put(
        &admin,
        &format!("/{}", web::ENABLED),
        json!({ "value": false }),
    )
    .await;
    assert_eq!(off.status, 409);
    assert_eq!(admin.get(CONFIG).await.status, 200);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn properties_and_game_rules_apply_live() {
    let instance = TempDir::new();
    let file = instance.path().join("server.properties");
    let server = start(instance).await;
    let admin = console_login(&server).await;
    let state = admin.get(CONFIG).await.json();
    let whitelist = find(&state["properties"], "white-list");
    assert_eq!(
        (whitelist["value"].as_str(), whitelist["effect"].as_str()),
        (Some("false"), Some("live"))
    );
    assert_eq!(
        find(&state["properties"], "motd")["default"],
        "A Minecraft Server"
    );
    let difficulty = find(&state["properties"], "difficulty");
    assert_eq!(difficulty["kind"], "choice");
    let rcon = find(&state["properties"], "enable-rcon");
    assert_eq!(
        (rcon["effect"].as_str(), rcon["todo"].as_str()),
        (Some("permanent"), None)
    );
    let seed = put(&admin, "/properties/level-seed", json!({ "value": "42" })).await;
    assert_eq!(seed.status, 200, "{}", seed.body);
    let note = seed.json()["note"].as_str().unwrap_or_default().to_owned();
    assert!(note.contains("the current world keeps seed 1"), "{note}");

    let resp = put(&admin, "/properties/white-list", json!({ "value": "true" })).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["value"], "true");
    assert!(run(&server, "whitelist on").await.contains("already"));
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(&file).unwrap_or_default();
        if text.contains("white-list=true") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(text.contains("white-list=true"), "{text}");
    let motd = put(
        &admin,
        "/properties/motd",
        json!({ "value": "hi\nwhite-list=false" }),
    )
    .await;
    assert_eq!(motd.status, 400);
    let level = put(
        &admin,
        "/properties/op-permission-level",
        json!({ "value": "9" }),
    )
    .await;
    assert_eq!(level.status, 400);
    assert_eq!(level.json()["error"], "must be a whole number from 0 to 4");
    assert_eq!(
        put(&admin, "/properties/not-a-thing", json!({ "value": "1" }))
            .await
            .status,
        404
    );
    let reset = put(&admin, "/properties/white-list", json!({ "reset": true })).await;
    assert_eq!(reset.json()["value"], "true");

    let keep = find(&state["gameRules"], "keep_inventory");
    assert_eq!(
        (keep["kind"].as_str(), keep["value"].as_str()),
        (Some("bool"), Some("false"))
    );
    let resp = put(
        &admin,
        "/gamerules/keep_inventory",
        json!({ "value": true }),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["value"], "true");
    assert!(
        run(&server, "gamerule keep_inventory")
            .await
            .contains("true")
    );
    let bad = put(&admin, "/gamerules/respawn_radius", json!({ "value": -1 })).await;
    assert_eq!(bad.status, 400);
    assert_eq!(
        put(&admin, "/gamerules/nope", json!({ "value": 1 }))
            .await
            .status,
        404
    );
    let reset = put(
        &admin,
        "/gamerules/keep_inventory",
        json!({ "reset": true }),
    )
    .await;
    assert_eq!(reset.json()["value"], "false");

    let entries = audit_entries(&admin, 5).await;
    assert!(
        entries
            .iter()
            .any(|e| e["action"] == "properties.set" && e["target"] == "white-list"),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|e| e["action"] == "gamerules.set"
            && e["target"] == "keep_inventory"
            && e["detail"] == "true"),
        "{entries:?}"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_needs_its_scopes() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let resp = put(
        &admin,
        "/properties/op-permission-level",
        json!({ "value": 3 }),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    run(
        &server,
        &format!("tuff config {} true", web::TRUST_OFFLINE_NAMES),
    )
    .await;
    assert!(run(&server, "op Alex").await.contains("Alex"));
    let lower = put(
        &admin,
        &format!("/{}", web::MIN_OP_LEVEL),
        json!({ "value": 3 }),
    )
    .await;
    assert_eq!(lower.status, 200, "{}", lower.body);

    let alex = Principal::Operator {
        uuid: tuff_core::auth::offline_uuid("Alex"),
        name: "Alex".into(),
    };
    let code = server.handle.web_login_code(alex).await.unwrap().unwrap();
    let mut operator = client(&server).await;
    let login = operator.login(&code).await;
    assert_eq!(login.status, 200, "{}", login.body);
    let scopes = login.json()["scopes"].to_string();
    assert!(!scopes.contains("config."), "{scopes}");
    assert_eq!(operator.get(CONFIG).await.status, 403);
    assert_eq!(
        put(&operator, "/tuff.web.hostnames", json!({ "value": "x" }))
            .await
            .status,
        403
    );
    assert_eq!(client(&server).await.get(CONFIG).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn minecraft_keys_read_and_set_server_properties_in_game() {
    let instance = TempDir::new();
    let file = instance.path().join("server.properties");
    let server = start(instance).await;
    let admin = console_login(&server).await;

    assert!(
        run(&server, "tuff config minecraft.hardcore")
            .await
            .contains("minecraft.hardcore = false")
    );
    let reply = run(&server, "tuff config minecraft.hardcore true").await;
    assert!(reply.contains("Set minecraft.hardcore to true"), "{reply}");
    assert!(
        run(&server, "tuff config minecraft.hardcore")
            .await
            .contains("= true")
    );
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(&file).unwrap_or_default();
        if text.contains("hardcore=true") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(text.contains("hardcore=true"), "{text}");
    let reply = run(&server, "tuff config minecraft.hardcore reset").await;
    assert!(reply.contains("to false"), "{reply}");

    let reply = run(&server, "tuff config minecraft.difficulty lava").await;
    assert!(reply.contains("must be one of"), "{reply}");
    assert!(
        run(&server, "tuff config minecraft.difficulty")
            .await
            .contains("= easy")
    );
    let reply = run(&server, "tuff config minecraft.no-such-key 1").await;
    assert!(reply.contains("Unknown server property"), "{reply}");

    let reply = run(&server, "tuff config minecraft.rcon.password hunter2").await;
    assert!(!reply.contains("hunter2"), "{reply}");
    assert!(reply.contains("<set>"), "{reply}");
    assert!(
        !run(&server, "tuff config minecraft.rcon.password")
            .await
            .contains("hunter2")
    );
    let reply = run(&server, "tuff config minecraft.feature-packs vanilla").await;
    assert!(reply.contains("needs a restart"), "{reply}");

    let entries = audit_entries(&admin, 6).await;
    let rendered = serde_json::to_string(&entries).unwrap();
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(
        entries
            .iter()
            .any(|e| e["action"] == "properties.set" && e["target"] == "hardcore")
    );
    stop(server).await;
}

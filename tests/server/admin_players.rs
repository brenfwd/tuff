use crate::common;

use std::time::Duration;

use common::{TempDir, TestServer};
use serde_json::{Value, json};
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_storage::admin::parse_uuid_text;
use tuff_test_support::admin_client::{AdminClient, EventStream, find_code};
use tuff_web_ui as web;
use tuff_web_ui::Principal;

const PLAYERS: &str = web::PLAYERS_ROUTE;

async fn start() -> TestServer {
    start_in(TempDir::new()).await
}

async fn start_in(instance: TempDir) -> TestServer {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
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

async fn join(server: &TestServer, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

async fn act(admin: &AdminClient, body: Value) -> Value {
    let resp = admin
        .send_json("POST", &format!("{PLAYERS}/actions"), &body)
        .await;
    assert_eq!(resp.status, 200, "{body}: {}", resp.body);
    let reply = resp.json();
    assert_eq!(reply["ok"], true, "{body}: {}", resp.body);
    reply["state"].clone()
}

async fn next_event(events: &mut EventStream, name: &str, player: &str) -> Value {
    loop {
        let event = events
            .next(Duration::from_secs(10))
            .await
            .unwrap_or_else(|| panic!("no {name} event for {player}"));
        let data: Value = serde_json::from_str(&event.data).unwrap();
        if event.event == name && data["name"] == player {
            return data;
        }
    }
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn audit_actions(admin: &AdminClient, wanted: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for _ in 0..100 {
        entries = admin
            .get(&format!("{}?limit=100", web::AUDIT_ROUTE))
            .await
            .json()["entries"]
            .as_array()
            .unwrap()
            .clone();
        if entries.iter().any(|e| e["action"] == wanted) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    entries
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn online_players_are_listed_live_and_moderated_through_commands() {
    let server = start().await;
    let admin = console_login(&server).await;
    let mut events = admin.events("players").await;
    assert_eq!(events.status, 200);
    let bot = join(&server, "Viewer").await;

    let joined = next_event(&mut events, "join", "Viewer").await;
    assert_eq!(joined["brand"], "vanilla", "{joined}");
    assert_eq!(joined["gameMode"], "survival");
    assert_eq!(joined["dimension"], "minecraft:overworld");
    assert!(joined.get("address").is_none(), "{joined}");

    let state = admin.get(PLAYERS).await.json();
    let row = &state["online"][0];
    assert_eq!(row["name"], "Viewer", "{state}");
    assert_eq!(row["address"], "127.0.0.1");
    assert_eq!(state["moderate"], true);

    act(
        &admin,
        json!({"action":"gamemode","player":"Viewer","mode":"creative"}),
    )
    .await;
    let updated = next_event(&mut events, "update", "Viewer").await;
    assert_eq!(updated["gameMode"], "creative", "{updated}");
    act(
        &admin,
        json!({"action":"message","player":"Viewer","text":"hello from the panel"}),
    )
    .await;
    act(&admin, json!({"action":"spawn","player":"Viewer"})).await;

    let state = act(&admin, json!({"action":"op","player":"Viewer"})).await;
    assert_eq!(names(&state["ops"]), ["Viewer"]);
    assert_eq!(state["online"][0]["opLevel"], 4);
    let state = act(&admin, json!({"action":"deop","player":"Viewer"})).await;
    assert!(names(&state["ops"]).is_empty());

    let state = act(&admin, json!({"action":"whitelist-add","player":"Viewer"})).await;
    assert_eq!(names(&state["whitelist"]["entries"]), ["Viewer"]);
    let state = act(&admin, json!({"action":"whitelist-on"})).await;
    assert_eq!(state["whitelist"]["enabled"], true);
    let state = act(&admin, json!({"action":"whitelist-off"})).await;
    assert_eq!(state["whitelist"]["enabled"], false);
    let uuid = row["uuid"].as_str().unwrap().to_owned();
    let state = act(&admin, json!({"action":"whitelist-remove","player":uuid})).await;
    assert!(names(&state["whitelist"]["entries"]).is_empty());

    act(
        &admin,
        json!({"action":"kick","player":"Viewer","reason":"see you"}),
    )
    .await;
    let left = next_event(&mut events, "leave", "Viewer").await;
    assert_eq!(left["uuid"], uuid.as_str());
    bot.wait_for(|state| state.disconnected.is_some())
        .await
        .unwrap();

    let state = act(
        &admin,
        json!({"action":"ban","player":"Viewer","reason":"griefing"}),
    )
    .await;
    assert_eq!(state["bans"][0]["name"], "Viewer", "{state}");
    assert_eq!(state["bans"][0]["reason"], "griefing");
    let offline = &state["offline"][0];
    assert_eq!(offline["name"], "Viewer", "{state}");
    assert_eq!(offline["banned"], true);
    let state = act(&admin, json!({"action":"pardon","player":uuid})).await;
    assert!(names(&state["bans"]).is_empty());

    let state = act(
        &admin,
        json!({"action":"ban-ip","target":"10.1.2.3","reason":"alts"}),
    )
    .await;
    assert_eq!(state["ipBans"][0]["ip"], "10.1.2.3", "{state}");
    let state = act(&admin, json!({"action":"pardon-ip","ip":"10.1.2.3"})).await;
    assert_eq!(state["ipBans"], json!([]));

    let actions = format!("{PLAYERS}/actions");
    for (body, status) in [
        (json!({"action":"op","player":"Viewer op"}), 400),
        (json!({"action":"op","player":"@a"}), 400),
        (
            json!({"action":"ban","player":"Viewer","reason":"x\nop Viewer"}),
            400,
        ),
        (
            json!({"action":"message","player":"Viewer","text":"a\u{0}b"}),
            400,
        ),
        (json!({"action":"ban-ip","target":"10.0.0.1 extra"}), 400),
        (json!({"action":"stop"}), 400),
        (
            json!({"action":"op","player":"00000000-0000-0000-0000-00000000beef"}),
            404,
        ),
    ] {
        let resp = admin.send_json("POST", &actions, &body).await;
        assert_eq!(resp.status, status, "{body}: {}", resp.body);
    }
    assert_eq!(admin.get(&actions).await.status, 405);
    let state = admin.get(PLAYERS).await.json();
    assert!(names(&state["ops"]).is_empty(), "{state}");

    let entries = audit_actions(&admin, "players.pardon-ip").await;
    for action in [
        "players.gamemode",
        "players.op",
        "players.deop",
        "players.whitelist-add",
        "players.kick",
        "players.ban",
        "players.pardon",
        "players.ban-ip",
        "players.pardon-ip",
    ] {
        assert!(
            entries
                .iter()
                .any(|e| e["action"] == action && e["outcome"] == "ok"),
            "{action}: {entries:?}"
        );
    }
    let ban = entries
        .iter()
        .find(|e| e["action"] == "players.ban")
        .unwrap();
    assert_eq!(ban["target"], "Viewer");
    assert_eq!(ban["detail"], "griefing");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moderation_needs_the_scopes_and_each_command_keeps_its_level() {
    let server = start().await;
    let console = console_login(&server).await;
    let bot = join(&server, "Helper").await;
    let mut uuid = None;
    for _ in 0..200 {
        let state = console.get(PLAYERS).await.json();
        if let Some(found) = state["online"][0]["uuid"].as_str() {
            uuid = parse_uuid_text(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let helper = Principal::Operator {
        uuid: uuid.expect("Helper is listed"),
        name: "Helper".into(),
    };
    run(&server, "tuff perm grant Helper tuff.web.access").await;
    let code = server.handle.web_login_code(helper).await.unwrap().unwrap();
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 200);
    assert_eq!(admin.get(PLAYERS).await.status, 403);

    run(&server, "tuff perm grant Helper tuff.web.players.read").await;
    let state = admin.get(PLAYERS).await;
    assert_eq!(state.status, 200, "{}", state.body);
    let state = state.json();
    assert_eq!(state["moderate"], false);
    assert_eq!(state["ipBans"], Value::Null);
    assert!(state["online"][0].get("address").is_none(), "{state}");
    let op = json!({"action":"op","player":"Helper"});
    let actions = format!("{PLAYERS}/actions");
    assert_eq!(admin.send_json("POST", &actions, &op).await.status, 403);

    run(&server, "tuff perm grant Helper tuff.web.players.moderate").await;
    let resp = admin.send_json("POST", &actions, &op).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["ok"], false, "{}", resp.body);
    assert!(names(&resp.json()["state"]["ops"]).is_empty());
    let kick = json!({"action":"kick","player":"Helper"});
    let resp = admin.send_json("POST", &actions, &kick).await;
    assert_eq!(resp.json()["ok"], false, "{}", resp.body);
    assert!(bot.state().disconnected.is_none());

    let entries = audit_actions(&console, "players.kick").await;
    let refused = entries
        .iter()
        .find(|e| e["action"] == "players.op")
        .unwrap();
    assert_eq!(refused["principal"], "Helper");
    assert_ne!(refused["outcome"], "ok");

    let anticheat = "/admin/api/v1/plugins/anticheat";
    assert_eq!(admin.get(anticheat).await.status, 403);
    let listed = console.get(anticheat).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(listed.json()["players"].is_array());
    let summary = console.get("/admin/api/v1/plugins/anticheat/summary").await;
    assert_eq!(summary.status, 200, "{}", summary.body);
    assert!(summary.json()["checks"].is_array());
    drop(bot);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_seen_before_a_restart_stay_known_offline() {
    let server = start().await;
    drop(join(&server, "Wanderer").await);
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    let instance = server._instance;
    let cache: Value = serde_json::from_str(
        &std::fs::read_to_string(instance.path().join("usercache.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cache[0]["name"], "Wanderer", "{cache}");

    let server = start_in(instance).await;
    let admin = console_login(&server).await;
    let state = admin.get(PLAYERS).await.json();
    let offline = &state["offline"][0];
    assert_eq!(offline["name"], "Wanderer", "{state}");
    assert_eq!(offline["uuid"], cache[0]["uuid"]);
    assert_eq!(
        run(&server, "op wanderer").await,
        "Made Wanderer a server operator"
    );
    stop(server).await;
}

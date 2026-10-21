use crate::common;

use common::{TempDir, TestServer};
use serde_json::json;
use tuff::Server;
use tuff_test_support::admin_client::{AdminClient, AdminResponse, find_code};
use tuff_web_ui as web;

async fn start(instance: TempDir) -> TestServer {
    let features = tuff::vanilla_features::all();
    let server = Server::builder()
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

async fn bearer(
    admin: &AdminClient,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> AdminResponse {
    let auth = format!("Bearer {token}");
    admin
        .request(method, path, &[("Authorization", &auth)], body)
        .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tokens_authenticate_until_revoked() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let made = admin
        .send_json(
            "POST",
            web::TOKENS_ROUTE,
            &json!({ "name": "ci", "level": 4 }),
        )
        .await;
    assert_eq!(made.status, 200, "{}", made.body);
    let token = made.json()["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("tuff_"));
    let id = made.json()["principal"]["id"].as_str().unwrap().to_owned();
    let anonymous = client(&server).await;
    let me = bearer(&anonymous, "GET", web::SESSION_ROUTE, &token, None).await;
    assert_eq!(me.status, 200, "{}", me.body);
    assert_eq!(me.json()["principal"]["kind"], "token");
    let ran = bearer(
        &anonymous,
        "POST",
        web::COMMAND_ROUTE,
        &token,
        Some(r#"{"line":"say hi"}"#),
    )
    .await;
    assert_eq!(ran.status, 200, "{}", ran.body);
    let listed = admin.get(web::TOKENS_ROUTE).await;
    assert!(!listed.body.contains(&token[20..]), "{}", listed.body);
    let row = &listed.json()["tokens"][0];
    assert_eq!(
        (row["name"].as_str(), row["level"].as_i64()),
        (Some("ci"), Some(4))
    );
    assert!(row["lastUsed"].is_u64(), "{row}");
    let wrong = format!("{}0", &token[..token.len() - 1]);
    let wrong = if wrong == token {
        format!("{}1", &token[..token.len() - 1])
    } else {
        wrong
    };
    assert_eq!(
        bearer(&anonymous, "GET", web::SESSION_ROUTE, &wrong, None)
            .await
            .status,
        401
    );
    let bad = admin
        .send_json(
            "POST",
            web::TOKENS_ROUTE,
            &json!({ "name": "has space", "level": 1 }),
        )
        .await;
    assert_eq!(bad.status, 400);

    let revoked = admin
        .send_json("DELETE", &format!("{}/{id}", web::TOKENS_ROUTE), &json!({}))
        .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    assert_eq!(
        bearer(&anonymous, "GET", web::SESSION_ROUTE, &token, None)
            .await
            .status,
        401
    );
    let audit = admin
        .get(&format!("{}?action=token.", web::AUDIT_ROUTE))
        .await
        .json();
    let actions: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(
        !actions.is_empty() && actions.iter().all(|a| a.starts_with("token.")),
        "{actions:?}"
    );
    stop(server).await;
}

fn token_in(reply: &str) -> String {
    let start = reply
        .find("tuff_")
        .unwrap_or_else(|| panic!("no token in {reply:?}"));
    reply[start..start + 86].to_owned()
}

async fn audited(admin: &AdminClient, action: &str) -> Vec<serde_json::Value> {
    admin
        .get(&format!("{}?action={action}", web::AUDIT_ROUTE))
        .await
        .json()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn console_manages_tokens_and_refusals_are_audited() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let reply = run(&server, "tuff web token create deploy 2 30d").await;
    assert!(reply.contains("expires in 30d"), "{reply}");
    let token = token_in(&reply);
    let anonymous = client(&server).await;
    let me = bearer(&anonymous, "GET", web::SESSION_ROUTE, &token, None).await;
    assert_eq!(me.status, 200, "{}", me.body);
    assert_eq!(me.json()["principal"]["name"], "deploy");

    let listed = admin.get(web::TOKENS_ROUTE).await.json();
    let row = &listed["tokens"][0];
    assert_eq!(row["expired"], false, "{listed}");
    assert!(row["expires"].as_u64().unwrap() > row["created"].as_u64().unwrap());
    let list = run(&server, "tuff web token list").await;
    assert!(
        list.contains("deploy (level 2, by console") && list.contains(", expires in "),
        "{list}"
    );
    assert!(!list.contains(&token[22..]), "{list}");
    assert!(
        run(&server, "tuff web token create boss 4 forever")
            .await
            .contains("Unknown expiry"),
    );

    let mut wrong = token.clone();
    let last = if wrong.ends_with('0') { "1" } else { "0" };
    wrong.replace_range(wrong.len() - 1.., last);
    let refused = bearer(&anonymous, "GET", web::WORLD_ROUTE, &wrong, None).await;
    assert_eq!(refused.status, 401, "{}", refused.body);
    let uses = audited(&admin, "token.use").await;
    assert_eq!(uses.len(), 1, "{uses:?}");
    assert_eq!(uses[0]["outcome"], "wrong secret");
    assert_eq!(uses[0]["principal"], "token:deploy");

    let revoked = run(&server, "tuff web token revoke deploy").await;
    assert!(revoked.contains("Revoked API token"), "{revoked}");
    assert_eq!(
        bearer(&anonymous, "GET", web::WORLD_ROUTE, &token, None)
            .await
            .status,
        401
    );
    assert!(
        run(&server, "tuff web token list")
            .await
            .contains("No API tokens")
    );
    let made = audited(&admin, "token.create").await;
    assert_eq!(made[0]["principal"], "console", "{made:?}");
    assert_eq!(audited(&admin, "token.revoke").await.len(), 1);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_are_listed_and_revoked() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let other = console_login(&server).await;
    let listed = admin.get(web::SESSIONS_ROUTE).await.json();
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{listed}");
    assert!(
        sessions.iter().all(|s| s["remote"] == "127.0.0.1"),
        "{listed}"
    );
    let theirs = sessions.iter().find(|s| s["current"] == false).unwrap()["session"]
        .as_str()
        .unwrap()
        .to_owned();
    let ended = admin
        .send_json(
            "DELETE",
            &format!("{}/{theirs}", web::SESSIONS_ROUTE),
            &json!({}),
        )
        .await;
    assert_eq!(ended.status, 200, "{}", ended.body);
    assert_eq!(other.get(web::SESSION_ROUTE).await.status, 401);
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 200);
    let all = admin
        .send_json("DELETE", web::SESSIONS_ROUTE, &json!({}))
        .await;
    assert_eq!(all.json()["ended"], 1);
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn world_overview_hides_the_seed_from_non_owners() {
    let server = start(TempDir::new()).await;
    let admin = console_login(&server).await;
    let world = admin.get(web::WORLD_ROUTE).await;
    assert_eq!(world.status, 200, "{}", world.body);
    let world = world.json();
    assert_eq!(world["seed"], "1");
    assert!(world["difficulty"].as_str().is_some());
    assert!(
        world["dimensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["key"] == "minecraft:overworld")
    );
    assert!(
        world["dimensions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["loaded"]["chunks"].is_u64() && d["loaded"]["regions"].is_u64()),
        "{world}"
    );
    server
        .handle
        .run_command("worldborder set 500 1d")
        .await
        .unwrap();
    server
        .handle
        .run_command("worldborder center 4 -4")
        .await
        .unwrap();
    let world = admin.get(web::WORLD_ROUTE).await.json();
    let overworld = world["dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "minecraft:overworld")
        .unwrap()
        .clone();
    let border = &overworld["border"];
    assert_eq!(border["target"], 500.0, "{border}");
    assert_eq!(border["center"], json!([4.5, -3.5]), "{border}");
    assert!(border["lerpTicks"].as_i64().unwrap() > 0, "{border}");
    assert!(border["size"].as_f64().unwrap() > 500.0, "{border}");
    assert_eq!(border["warningTicks"], 300, "{border}");
    let saved = admin
        .send_json("POST", &format!("{}/save", web::WORLD_ROUTE), &json!({}))
        .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["ok"], true, "{}", saved.body);

    let made = admin
        .send_json(
            "POST",
            web::TOKENS_ROUTE,
            &json!({ "name": "viewer", "level": 3 }),
        )
        .await;
    let token = made.json()["token"].as_str().unwrap().to_owned();
    let anonymous = client(&server).await;
    let seen = bearer(&anonymous, "GET", web::WORLD_ROUTE, &token, None).await;
    assert_eq!(seen.status, 200, "{}", seen.body);
    assert!(seen.json()["seed"].is_null(), "{}", seen.body);
    assert_eq!(seen.json()["canSave"], false);
    assert_eq!(
        bearer(
            &anonymous,
            "POST",
            &format!("{}/save", web::WORLD_ROUTE),
            &token,
            Some("{}")
        )
        .await
        .status,
        403
    );
    assert_eq!(
        bearer(&anonymous, "GET", web::TOKENS_ROUTE, &token, None)
            .await
            .status,
        403
    );
    assert_eq!(
        bearer(&anonymous, "GET", web::SESSIONS_ROUTE, &token, None)
            .await
            .status,
        403
    );
    stop(server).await;
}

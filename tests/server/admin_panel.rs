use crate::common;

use std::time::Duration;

use common::{MockSessionServer, TEST_UUID, TempDir, TestServer, start_server_with_features};
use serde_json::json;
use tuff_test_support::admin_client::{AdminClient, find_code};
use tuff_web_ui as web;
use tuff_web_ui::Principal;

async fn server(session_url: &str) -> TestServer {
    start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        session_url,
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

async fn client(server: &TestServer) -> AdminClient {
    AdminClient::new(server.handle.http_addrs().await.unwrap()[0])
}

async fn enable(server: &TestServer) {
    run(server, &format!("tuff config {} true", web::ENABLED)).await;
}

async fn console_login(server: &TestServer) -> AdminClient {
    let reply = run(server, "tuff web login").await;
    let code = find_code(&reply).unwrap_or_else(|| panic!("no code in {reply:?}"));
    let mut admin = client(server).await;
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    admin
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn panel_is_off_until_enabled() {
    let server = server("http://127.0.0.1:1").await;
    let admin = client(&server).await;
    assert_eq!(admin.get("/admin/").await.status, 404);
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 404);
    let reply = run(&server, "tuff web login").await;
    assert!(reply.contains("off"), "{reply}");
    assert!(find_code(&reply).is_none());
    enable(&server).await;
    let shell = admin.get("/admin/").await;
    assert_eq!(shell.status, 200);
    assert!(
        shell
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
    assert!(shell.body.contains("<div id=\"root\">"));
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn every_response_carries_security_headers_and_hosts_are_checked() {
    let server = server("http://127.0.0.1:1").await;
    enable(&server).await;
    let mut admin = client(&server).await;
    for path in ["/admin/", "/admin/nothing", web::LOGS_ROUTE] {
        let resp = admin.get(path).await;
        let csp = resp.header("content-security-policy").unwrap_or_default();
        assert!(
            csp.contains("default-src 'none'") && csp.contains("'sha256-"),
            "{path}: {csp}"
        );
        assert!(!csp.contains("unsafe-inline"));
        assert_eq!(resp.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(resp.header("referrer-policy"), Some("no-referrer"));
        assert_eq!(resp.header("x-frame-options"), Some("DENY"));
    }
    assert_eq!(
        admin.get(web::LOGS_ROUTE).await.header("cache-control"),
        Some("no-store")
    );
    let ok_host = admin.host.clone();
    admin.host = "evil.example".into();
    assert_eq!(admin.get("/admin/").await.status, 421);
    run(
        &server,
        &format!("tuff config {} evil.example", web::HOSTNAMES),
    )
    .await;
    assert_eq!(admin.get("/admin/").await.status, 200);
    admin.host = "127.0.0.1".into();
    assert_eq!(admin.get("/admin/").await.status, 200);
    admin.host = ok_host;

    run(
        &server,
        &format!("tuff config {} true", web::TRUST_FORWARDED),
    )
    .await;
    let remote = admin
        .request(
            "GET",
            "/admin/",
            &[("X-Forwarded-For", "203.0.113.9")],
            None,
        )
        .await;
    assert_eq!(remote.status, 403);
    run(&server, &format!("tuff config {} true", web::ALLOW_REMOTE)).await;
    let unacknowledged = admin
        .request(
            "GET",
            "/admin/",
            &[("X-Forwarded-For", "203.0.113.9")],
            None,
        )
        .await;
    assert_eq!(unacknowledged.status, 403);
    let v6 = admin
        .request(
            "GET",
            "/admin/",
            &[("X-Forwarded-For", "2001:db8::9")],
            None,
        )
        .await;
    assert_eq!(v6.status, 403);
    run(&server, &format!("tuff config {} true", web::ACK_PLAINTEXT)).await;
    let remote = admin
        .request(
            "GET",
            "/admin/",
            &[("X-Forwarded-For", "203.0.113.9")],
            None,
        )
        .await;
    assert_eq!(remote.status, 200);
    let unrelated = admin.get("/resourcepacks/none").await;
    assert!(unrelated.header("content-security-policy").is_none());
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn declared_tls_termination_lets_remote_clients_in_without_acknowledgement() {
    let server = server("http://127.0.0.1:1").await;
    enable(&server).await;
    let admin = client(&server).await;
    run(
        &server,
        &format!("tuff config {} true", web::TRUST_FORWARDED),
    )
    .await;
    run(&server, &format!("tuff config {} true", web::ALLOW_REMOTE)).await;
    for addr in ["203.0.113.9", "2001:db8::9"] {
        let resp = admin
            .request("GET", "/admin/", &[("X-Forwarded-For", addr)], None)
            .await;
        assert_eq!(resp.status, 403, "{addr}");
    }
    run(
        &server,
        &format!("tuff config {} true", web::TLS_TERMINATED),
    )
    .await;
    for addr in ["203.0.113.9", "2001:db8::9"] {
        let resp = admin
            .request("GET", "/admin/", &[("X-Forwarded-For", addr)], None)
            .await;
        assert_eq!(resp.status, 200, "{addr}");
    }
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn private_routes_refuse_anonymous_and_foreign_requests() {
    let server = server("http://127.0.0.1:1").await;
    enable(&server).await;
    let admin = client(&server).await;
    for path in web::PRIVATE_ROUTES
        .iter()
        .copied()
        .chain(["/admin/api/v1/unknown", "/admin/api"])
    {
        assert_eq!(admin.get(path).await.status, 401, "GET {path}");
        let post = admin
            .request("POST", path, &[("X-Tuff-CSRF", "x")], Some("{}"))
            .await;
        assert_eq!(post.status, 401, "POST {path}");
    }
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    let bogus = admin
        .request(
            "GET",
            web::LOGS_ROUTE,
            &[("Cookie", &format!("{}=../../x", web::auth::COOKIE))],
            None,
        )
        .await;
    assert_eq!(bogus.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn console_login_runs_commands_with_csrf_and_audit() {
    let server = server("http://127.0.0.1:1").await;
    enable(&server).await;
    let reply = run(&server, "tuff web login").await;
    let code = find_code(&reply).unwrap();
    assert!(reply.contains("/admin/"), "{reply}");
    let mut admin = client(&server).await;
    let wrong_type = admin
        .request(
            "POST",
            web::SESSION_ROUTE,
            &[("Content-Type", "text/plain")],
            None,
        )
        .await;
    assert_eq!(wrong_type.status, 415);
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let cookie = resp.header("set-cookie").unwrap();
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/admin"] {
        assert!(cookie.contains(flag), "{cookie}");
    }
    assert_eq!(resp.json()["principal"]["kind"], "console");
    let mut other = client(&server).await;
    assert_eq!(other.login(&code).await.status, 401, "codes are single use");

    let session = admin.get(web::SESSION_ROUTE).await;
    assert_eq!(session.status, 200);
    assert_eq!(session.json()["csrf"], admin.csrf.clone().unwrap());

    let ran = admin
        .send_json("POST", web::COMMAND_ROUTE, &json!({ "line": "banlist" }))
        .await;
    assert_eq!(ran.status, 200, "{}", ran.body);
    let feedback = ran.json()["feedback"].to_string();
    assert!(feedback.contains("no bans"), "{feedback}");

    let no_csrf = admin
        .request(
            "POST",
            web::COMMAND_ROUTE,
            &[],
            Some(r#"{"line":"banlist"}"#),
        )
        .await;
    assert_eq!(no_csrf.status, 403);
    let csrf = admin.csrf.clone().unwrap();
    let foreign = admin
        .request(
            "POST",
            web::COMMAND_ROUTE,
            &[("X-Tuff-CSRF", &csrf), ("Origin", "http://evil.example")],
            Some(r#"{"line":"banlist"}"#),
        )
        .await;
    assert_eq!(foreign.status, 403);
    let control = admin
        .send_json(
            "POST",
            web::COMMAND_ROUTE,
            &json!({ "line": "say a\nop x" }),
        )
        .await;
    assert_eq!(control.status, 400);
    let long = admin
        .send_json(
            "POST",
            web::COMMAND_ROUTE,
            &json!({ "line": "a".repeat(40_000) }),
        )
        .await;
    assert_eq!(long.status, 400);

    let completed = admin
        .get(&format!("{}?line=ban", web::COMPLETE_ROUTE))
        .await;
    assert_eq!(completed.status, 200, "{}", completed.body);
    let matches = completed.json()["matches"].clone();
    assert!(
        matches.as_array().unwrap().iter().any(|m| m == "banlist"),
        "{matches}"
    );
    assert_eq!(completed.json()["start"], 0);

    let mut entries = serde_json::Value::Null;
    for _ in 0..50 {
        entries = admin
            .get(&format!("{}?limit=10", web::AUDIT_ROUTE))
            .await
            .json();
        if entries["entries"].as_array().is_some_and(|e| e.len() >= 2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let actions: Vec<String> = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap().to_owned())
        .collect();
    assert!(actions.contains(&"command".to_owned()), "{actions:?}");
    assert!(actions.contains(&"login".to_owned()), "{actions:?}");

    let reply = run(&server, "tuff web logout-all").await;
    assert!(reply.contains("Ended all 1"), "{reply}");
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn sessions_survive_a_restart_and_logout_ends_them() {
    let instance = TempDir::new();
    let dir = instance.path().to_path_buf();
    let server = start_server_with_features(
        instance,
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![Box::new(
            tuff_feature_builtin_core_commands::CoreCommandsFeature,
        )],
    )
    .await;
    enable(&server).await;
    let mut admin = console_login(&server).await;
    let id = admin.session.clone().unwrap();
    let mut sessions = String::new();
    for _ in 0..200 {
        sessions = std::fs::read_to_string(dir.join("web/sessions.json")).unwrap_or_default();
        if sessions.contains("\"hash\"") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        sessions.contains("\"hash\"") && !sessions.contains(&id),
        "{sessions}"
    );
    let TestServer {
        handle,
        task,
        _instance,
        ..
    } = server;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let server = start_server_with_features(
        _instance,
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![Box::new(
            tuff_feature_builtin_core_commands::CoreCommandsFeature,
        )],
    )
    .await;
    admin.addr = server.handle.http_addrs().await.unwrap()[0];
    admin.host = format!("localhost:{}", admin.addr.port());
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 200);
    let reply = run(&server, "tuff web logout").await;
    assert!(reply.contains("Ended 1"), "{reply}");
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn operators_lose_access_when_deopped() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = server(&session_url).await;
    enable(&server).await;
    run(&server, "op Alex").await;
    let uuid = u128::from_str_radix(TEST_UUID, 16).unwrap();
    let operator = Principal::Operator {
        uuid,
        name: "Alex".into(),
    };
    let code = server
        .handle
        .web_login_code(operator.clone())
        .await
        .unwrap()
        .unwrap();
    let mut admin = client(&server).await;
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["principal"]["name"], "Alex");
    let ran = admin
        .send_json(
            "POST",
            web::COMMAND_ROUTE,
            &json!({ "line": "tuff web logout" }),
        )
        .await;
    assert!(
        ran.json()["feedback"].to_string().contains("Ended 1"),
        "{}",
        ran.body
    );
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);

    let code = server
        .handle
        .web_login_code(operator)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admin.login(&code).await.status, 200);
    run(&server, "deop Alex").await;
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn panel_commands_run_at_the_principal_level() {
    let server = server("http://127.0.0.1:1").await;
    let feedback = server
        .handle
        .run_command_as(
            "tuff config",
            "Helper",
            tuff_storage::admin::PermissionLevel::Gamemasters,
        )
        .await
        .unwrap();
    let text: String = feedback.iter().map(|c| c.plain()).collect();
    assert!(!text.contains("config keys"), "{text}");
    let feedback = server
        .handle
        .run_command_as(
            "tuff config",
            "Owner",
            tuff_storage::admin::PermissionLevel::Owners,
        )
        .await
        .unwrap();
    let text: String = feedback.iter().map(|c| c.plain()).collect();
    assert!(text.contains("config keys"), "{text}");
    stop(server).await;
}

async fn gold_server(instance: TempDir, session_url: &str) -> TestServer {
    start_server_with_features(
        instance,
        "127.0.0.1:0",
        session_url,
        Some(false),
        vec![
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
            Box::new(tuff_feature_builtin_example::ExampleFeature),
        ],
    )
    .await
}

const GOLD_API: &str = "/admin/api/v1/plugins/example-gold";
const GOLD_MODULE: &str = "/admin/pages/example-gold/gold.js";

#[tuff_core::watchdog]
#[tokio::test]
async fn plugin_pages_are_listed_and_served_behind_the_session() {
    let server = gold_server(TempDir::new(), "http://127.0.0.1:1").await;
    enable(&server).await;
    let anonymous = client(&server).await;
    assert_eq!(anonymous.get(GOLD_MODULE).await.status, 401);
    assert_eq!(anonymous.get(GOLD_API).await.status, 401);
    let post = anonymous
        .request(
            "POST",
            &format!("{GOLD_API}/reset"),
            &[("X-Tuff-CSRF", "x")],
            Some("{}"),
        )
        .await;
    assert_eq!(post.status, 401);

    let admin = console_login(&server).await;
    let pages = admin.get(web::PAGES_ROUTE).await;
    assert_eq!(pages.status, 200, "{}", pages.body);
    let listed = pages.json()["pages"].clone();
    let gold = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "example-gold")
        .unwrap_or_else(|| panic!("{listed}"))
        .clone();
    assert_eq!(gold["module"], GOLD_MODULE);
    assert_eq!(gold["title"], "Gold");
    assert_eq!(gold["scope"], "example.gold.read");

    let module = admin.get(GOLD_MODULE).await;
    assert_eq!(module.status, 200);
    assert!(
        module
            .header("content-type")
            .unwrap()
            .starts_with("text/javascript")
    );
    assert!(module.body.contains("as mount"), "{}", module.body);
    assert!(
        module.body.contains("__tuffPanel") && !module.body.contains("__CLIENT_INTERNALS"),
        "the page uses the shell's React instead of bundling its own"
    );
    assert!(
        module.body.contains(".Ui") && !module.body.contains("Previous page"),
        "the page uses the shell's component kit instead of bundling it"
    );
    assert_eq!(
        gold["styles"],
        json!(["/admin/pages/example-gold/gold.css"])
    );
    let style = admin.get("/admin/pages/example-gold/gold.css").await;
    assert_eq!(style.status, 200);
    assert!(
        style
            .header("content-type")
            .unwrap()
            .starts_with("text/css")
    );
    assert_eq!(module.header("x-content-type-options"), Some("nosniff"));
    let etag = module.header("etag").unwrap().to_owned();
    let again = admin
        .request("GET", GOLD_MODULE, &[("If-None-Match", &etag)], None)
        .await;
    assert_eq!(again.status, 304);
    for missing in [
        "/admin/pages/example-gold/none.js",
        "/admin/pages/none/gold.js",
        "/admin/pages/example-gold",
    ] {
        assert_eq!(admin.get(missing).await.status, 404, "{missing}");
    }
    let traversal = admin
        .get("/admin/pages/example-gold/../example-gold/gold.js")
        .await;
    assert!(
        traversal.status == 400 || traversal.status == 404,
        "{}",
        traversal.status
    );

    let shell_csp = admin
        .get("/admin/")
        .await
        .header("content-security-policy")
        .unwrap()
        .to_owned();
    assert!(!shell_csp.contains("/admin/pages/"), "{shell_csp}");
    assert!(shell_csp.contains("frame-src 'self'"), "{shell_csp}");
    assert!(!shell_csp.contains("unsafe"));
    let frame = admin.get("/admin/frame.html").await;
    assert_eq!(frame.status, 200);
    let frame_csp = frame.header("content-security-policy").unwrap();
    assert!(
        frame_csp.starts_with("sandbox allow-scripts;"),
        "{frame_csp}"
    );
    assert!(frame_csp.contains("connect-src 'none'"), "{frame_csp}");
    assert!(!frame_csp.contains("allow-same-origin") && !frame_csp.contains("unsafe"));
    assert_eq!(frame.header("x-frame-options"), Some("SAMEORIGIN"));
    let api_csp = admin.get(GOLD_API).await;
    assert!(
        !api_csp
            .header("content-security-policy")
            .unwrap()
            .contains("/admin/pages/")
    );

    let count = admin.get(GOLD_API).await;
    assert_eq!(count.status, 200, "{}", count.body);
    assert_eq!(count.json()["goldPlaced"], 0);
    assert_eq!(admin.get(&format!("{GOLD_API}/nothing")).await.status, 404);
    assert_eq!(
        admin.get("/admin/api/v1/plugins/none").await.status,
        404,
        "unknown plugin APIs fall through to the API 404"
    );
    let no_csrf = admin
        .request("POST", &format!("{GOLD_API}/reset"), &[], Some("{}"))
        .await;
    assert_eq!(no_csrf.status, 403);
    let reset = admin
        .send_json("POST", &format!("{GOLD_API}/reset"), &json!({}))
        .await;
    assert_eq!(reset.status, 200, "{}", reset.body);
    let mut actions = Vec::new();
    for _ in 0..50 {
        let entries = admin
            .get(&format!("{}?limit=10", web::AUDIT_ROUTE))
            .await
            .json();
        actions = entries["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect();
        if actions.iter().any(|a| a == "example.gold.reset") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        actions.iter().any(|a| a == "example.gold.reset"),
        "{actions:?}"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn plugin_pages_need_their_scope() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "op-permission-level=2\n",
    )
    .unwrap();
    let server = gold_server(instance, &session_url).await;
    enable(&server).await;
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
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["level"], 2);
    let pages = admin.get(web::PAGES_ROUTE).await;
    assert_eq!(pages.status, 200, "{}", pages.body);
    assert_eq!(pages.json()["pages"], json!([]));
    assert_eq!(admin.get(GOLD_MODULE).await.status, 403);
    assert_eq!(admin.get(GOLD_API).await.status, 403);
    let reset = admin
        .send_json("POST", &format!("{GOLD_API}/reset"), &json!({}))
        .await;
    assert_eq!(reset.status, 403);
    stop(server).await;
}

struct PageFeature(&'static str);

impl tuff_core::feature::Feature for PageFeature {
    fn id(&self) -> &'static str {
        self.0
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        use tuff_web_ui::panel::{Asset, PanelPage};
        if cx.phase() == tuff_item::load::Phase::Content {
            let module = Asset::module("main.js", "export function mount() {}");
            let page = PanelPage::new("shared-id", "Page", web::Scope::Plugin("test.read"), module);
            cx.registries().panel.register_page(self.0, page)?;
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[test]
fn duplicate_page_ids_fail_feature_load_naming_both_features() {
    let features: Vec<Box<dyn tuff_core::feature::Feature>> = vec![
        Box::new(PageFeature("first-pages")),
        Box::new(PageFeature("second-pages")),
    ];
    let mut registries = tuff_core::registry::Registries::new();
    let err = tuff_core::feature::load_all(&features, &mut registries)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("first-pages") && err.contains("second-pages") && err.contains("shared-id"),
        "{err}"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn dashboard_metrics_api_answers_snapshots_history_and_cards() {
    let server = gold_server(TempDir::new(), "http://127.0.0.1:1").await;
    enable(&server).await;
    let admin = console_login(&server).await;
    let mut live = admin.events("metrics").await;
    assert_eq!(live.status, 200);
    let ticked = loop {
        let event = live
            .next(Duration::from_secs(10))
            .await
            .expect("a snapshot event");
        assert_eq!(event.event, "snapshot");
        let data: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert!(data["panel"]["auditFailures"].is_u64(), "{data}");
        if data["snapshot"]["tick"].is_object() {
            break data;
        }
    };
    assert!(ticked["snapshot"]["tick"]["tps"].is_number(), "{ticked}");

    let resp = admin.get(web::METRICS_ROUTE).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = resp.json();
    let snapshot = &body["snapshot"];
    for key in [
        "unix_ms",
        "uptime_s",
        "tick",
        "world",
        "slowest_regions",
        "series",
        "dropped_reports",
    ] {
        assert!(!snapshot[key].is_null(), "{key} missing in {snapshot}");
    }
    assert!(snapshot["series"]["net.bytes_in"]["rate"].is_number());
    assert_eq!(body["history"]["seconds"], 600);
    assert_eq!(body["history"]["minutes"], 1440);
    assert!(body["alerts"].is_array());
    assert_eq!(
        body["cards"],
        json!([{ "series": tuff_feature_builtin_example::GOLD_SERIES, "title": "Gold placed", "unit": "count" }])
    );
    assert!(snapshot["series"][tuff_feature_builtin_example::GOLD_SERIES].is_object());

    let history = admin
        .get(&format!(
            "{}?series=tick.mspt,players,no.such&range=10m",
            web::METRICS_HISTORY_ROUTE
        ))
        .await;
    assert_eq!(history.status, 200, "{}", history.body);
    let history = history.json();
    assert_eq!(history["range"], 600);
    assert_eq!(history["step"], 1);
    let players = history["series"]["players"].as_array().unwrap();
    assert!(!players.is_empty());
    assert_eq!(players[0].as_array().unwrap().len(), 3);
    assert!(history["series"]["tick.mspt"].is_array());
    assert!(history["series"]["no.such"].is_null());
    #[cfg(unix)]
    {
        assert!(snapshot["cpu"]["cores"].is_number(), "{snapshot}");
        let cpu = admin
            .get(&format!(
                "{}?series={}&range=10m",
                web::METRICS_HISTORY_ROUTE,
                tuff_core::metrics::PROCESS_CPU
            ))
            .await
            .json();
        let points = cpu["series"]["process.cpu"].as_array().unwrap();
        assert!(!points.is_empty(), "{cpu}");
        assert!(points[0][1].as_f64().unwrap() >= 0.0);
    }
    let day = admin
        .get(&format!(
            "{}?series=players&range=24h",
            web::METRICS_HISTORY_ROUTE
        ))
        .await
        .json();
    assert_eq!(day["step"], 60);
    assert!(!day["series"]["players"].as_array().unwrap().is_empty());
    let nine = ["players"; 9].join(",");
    for query in [
        String::new(),
        "?series=".to_owned(),
        "?series=players&range=1y".to_owned(),
        "?series=Bad/Name".to_owned(),
        format!("?series={nine}"),
    ] {
        let resp = admin
            .get(&format!("{}{query}", web::METRICS_HISTORY_ROUTE))
            .await;
        assert_eq!(resp.status, 400, "{query}: {}", resp.body);
    }
    assert_eq!(
        admin
            .get(&format!("{}/nothing", web::METRICS_ROUTE))
            .await
            .status,
        404
    );
    let post = admin
        .send_json("POST", web::METRICS_ROUTE, &json!({}))
        .await;
    assert_eq!(post.status, 405);
    let session = admin.get(web::SESSION_ROUTE).await.json();
    assert!(
        session["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("metrics.read"))
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn dashboard_metrics_need_operator_level_four() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "op-permission-level=3\n",
    )
    .unwrap();
    let server = gold_server(instance, &session_url).await;
    enable(&server).await;
    run(&server, &format!("tuff config {} 3", web::MIN_OP_LEVEL)).await;
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
    assert_eq!(resp.json()["level"], 3);
    assert!(
        !resp.json()["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("metrics.read"))
    );
    assert_eq!(admin.get(web::METRICS_ROUTE).await.status, 403);
    assert_eq!(
        admin
            .get(&format!("{}?series=players", web::METRICS_HISTORY_ROUTE))
            .await
            .status,
        403
    );
    let mut live = admin.events("metrics,alerts").await;
    assert_eq!(live.status, 200);
    assert!(
        live.next(Duration::from_millis(2500)).await.is_none(),
        "a level 3 operator must not get snapshots"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn timings_record_stop_list_and_export_over_the_api() {
    let _serial = tuff_timings::serial_async().await;
    let server = server("http://127.0.0.1:1").await;
    enable(&server).await;
    let admin = console_login(&server).await;
    let route = web::TIMINGS_ROUTE;
    let idle = admin.get(route).await;
    assert_eq!(idle.status, 200, "{}", idle.body);
    assert!(idle.json()["recording"].is_null());
    let not_running = admin
        .send_json("POST", &format!("{route}/stop"), &json!({}))
        .await;
    assert_eq!(not_running.status, 409, "{}", not_running.body);
    for bad in [json!({ "seconds": 0 }), json!({ "seconds": "5" })] {
        let resp = admin
            .send_json("POST", &format!("{route}/start"), &bad)
            .await;
        assert_eq!(resp.status, 400, "{bad}: {}", resp.body);
    }
    let no_csrf = admin
        .request("POST", &format!("{route}/start"), &[], Some("{}"))
        .await;
    assert_eq!(no_csrf.status, 403);
    assert_eq!(admin.get(&format!("{route}/start")).await.status, 405);

    let started = admin
        .send_json(
            "POST",
            &format!("{route}/start"),
            &json!({ "seconds": 600 }),
        )
        .await;
    assert_eq!(started.status, 200, "{}", started.body);
    assert_eq!(started.json()["recording"]["stops_after_s"], 600);
    assert_eq!(started.json()["recording"]["by"], "console");
    let again = admin
        .send_json("POST", &format!("{route}/start"), &json!({}))
        .await;
    assert_eq!(again.status, 409, "{}", again.body);
    let running = admin.get(route).await.json();
    assert!(running["recording"]["elapsed_ms"].is_u64(), "{running}");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let stopped = admin
        .send_json("POST", &format!("{route}/stop"), &json!({}))
        .await;
    assert_eq!(stopped.status, 200, "{}", stopped.body);
    let summary = stopped.json()["summary"].clone();
    let id = summary["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("timings-"), "{summary}");
    assert_eq!(summary["file"], format!("timings/{id}.json"));
    let status = admin.get(route).await.json();
    assert!(status["recording"].is_null());
    assert_eq!(status["last"]["id"], id.as_str());

    let reports = admin.get(&format!("{route}/reports")).await;
    assert_eq!(reports.status, 200, "{}", reports.body);
    assert_eq!(reports.json()["reports"][0]["id"], id.as_str());

    let report = admin.get(&format!("{route}/reports/{id}")).await;
    assert_eq!(report.status, 200, "{}", report.body);
    assert_eq!(report.header("content-type"), Some("application/json"));
    assert!(report.header("content-disposition").is_none());
    let parsed = tuff_timings::report::Report::from_json(report.body.as_bytes()).unwrap();
    assert_eq!(parsed.id, id);
    assert!(!parsed.merged.is_empty());

    let folded = admin
        .get(&format!("{route}/reports/{id}?format=folded&download=1"))
        .await;
    assert_eq!(folded.status, 200, "{}", folded.body);
    assert!(
        folded
            .header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    assert_eq!(
        folded.header("content-disposition"),
        Some(format!("attachment; filename=\"{id}.folded\"").as_str())
    );
    assert!(folded.body.lines().all(|l| l.rsplit_once(' ').is_some()));
    let speedscope = admin
        .get(&format!(
            "{route}/reports/{id}?format=speedscope&download=1"
        ))
        .await;
    assert_eq!(speedscope.status, 200);
    assert_eq!(
        speedscope.json()["$schema"],
        "https://www.speedscope.app/file-format-schema.json"
    );
    assert_eq!(
        speedscope.header("content-disposition"),
        Some(format!("attachment; filename=\"{id}.speedscope.json\"").as_str())
    );
    let json_download = admin
        .get(&format!("{route}/reports/{id}?format=json&download=1"))
        .await;
    assert_eq!(json_download.body, report.body);

    for (path, code) in [
        (format!("{route}/reports/{id}?format=svg"), 400),
        (format!("{route}/reports/timings-none"), 404),
        (format!("{route}/reports/.hidden"), 400),
        (format!("{route}/reports/{id}/x"), 404),
        (format!("{route}/nothing"), 404),
    ] {
        assert_eq!(admin.get(&path).await.status, code, "{path}");
    }

    let mut actions = Vec::new();
    for _ in 0..50 {
        let entries = admin
            .get(&format!("{}?limit=10", web::AUDIT_ROUTE))
            .await
            .json();
        actions = entries["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        if actions.contains(&"timings.stop".to_owned()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(actions.contains(&"timings.start".to_owned()), "{actions:?}");
    assert!(actions.contains(&"timings.stop".to_owned()), "{actions:?}");
    let session = admin.get(web::SESSION_ROUTE).await.json();
    assert!(
        session["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("timings"))
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn timings_need_operator_level_four() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "op-permission-level=3\n",
    )
    .unwrap();
    let server = start_server_with_features(
        instance,
        "127.0.0.1:0",
        &session_url,
        Some(false),
        vec![Box::new(
            tuff_feature_builtin_core_commands::CoreCommandsFeature,
        )],
    )
    .await;
    enable(&server).await;
    run(&server, &format!("tuff config {} 3", web::MIN_OP_LEVEL)).await;
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
    assert!(
        !resp.json()["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("timings"))
    );
    let route = web::TIMINGS_ROUTE;
    assert_eq!(admin.get(route).await.status, 403);
    assert_eq!(admin.get(&format!("{route}/reports")).await.status, 403);
    assert_eq!(
        admin
            .send_json("POST", &format!("{route}/start"), &json!({}))
            .await
            .status,
        403
    );
    stop(server).await;
}

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
            let node =
                tuff_core::permission::Node::new("vault.open", "Open the vault").op_bypass(false);
            cx.registries().permissions.register(self.id(), node)?;
        }
        Ok(())
    }
}

async fn vault_server(instance: TempDir, session_url: &str) -> TestServer {
    start_server_with_features(
        instance,
        "127.0.0.1:0",
        session_url,
        Some(false),
        vec![
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
            Box::new(VaultFeature),
        ],
    )
    .await
}

fn alex() -> Principal {
    Principal::Operator {
        uuid: u128::from_str_radix(TEST_UUID, 16).unwrap(),
        name: "Alex".into(),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn plugin_nodes_without_op_bypass_need_a_grant_that_survives_a_restart() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = vault_server(TempDir::new(), &session_url).await;
    run(&server, "op Alex").await;
    let nodes = run(&server, "tuff perm nodes").await;
    assert!(
        nodes.contains("vault.open - Open the vault (default nobody, no operator bypass)"),
        "{nodes}"
    );
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(check.contains("Alex lacks vault.open (default)"), "{check}");
    let check = run(&server, "tuff perm check Alex tuff.command.config").await;
    assert!(
        check.contains("Alex has tuff.command.config (operator level 4"),
        "{check}"
    );
    let unknown = run(&server, "tuff perm grant Alex vault.close").await;
    assert!(unknown.contains("Unknown permission node"), "{unknown}");
    let granted = run(&server, "tuff perm grant Alex vault.open").await;
    assert!(granted.contains("Granted vault.open to Alex"), "{granted}");
    let again = run(&server, "tuff perm grant Alex vault.open").await;
    assert!(again.contains("already has vault.open granted"), "{again}");

    let TestServer {
        handle,
        task,
        _instance,
        ..
    } = server;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let server = vault_server(_instance, &session_url).await;
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(
        check.contains("Alex has vault.open (granted as vault.open)"),
        "{check}"
    );
    let listed = run(&server, "tuff perm list Alex").await;
    assert!(listed.contains("vault.open (granted)"), "{listed}");
    let everyone = run(&server, "tuff perm list").await;
    assert!(everyone.contains("Alex (1)"), "{everyone}");
    let revoked = run(&server, "tuff perm revoke Alex vault.open").await;
    assert!(
        revoked.contains("Cleared Alex's setting for vault.open"),
        "{revoked}"
    );
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(check.contains("lacks"), "{check}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn granted_players_use_the_panel_without_being_operators() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = server(&session_url).await;
    enable(&server).await;
    let code = server.handle.web_login_code(alex()).await.unwrap().unwrap();
    let mut admin = client(&server).await;
    assert_eq!(admin.login(&code).await.status, 403);

    run(&server, "tuff perm grant Alex tuff.web.access").await;
    let code = server.handle.web_login_code(alex()).await.unwrap().unwrap();
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["level"], 0);
    assert_eq!(resp.json()["scopes"], json!([]));
    let line = json!({ "line": "tuff perm nodes" });
    assert_eq!(
        admin
            .send_json("POST", web::COMMAND_ROUTE, &line)
            .await
            .status,
        403
    );

    run(&server, "tuff perm grant Alex tuff.web.console.run").await;
    run(&server, "tuff perm grant Alex tuff.command.perm").await;
    let ran = admin.send_json("POST", web::COMMAND_ROUTE, &line).await;
    assert_eq!(ran.status, 200, "{}", ran.body);
    assert!(
        ran.json()["feedback"]
            .to_string()
            .contains("permission nodes"),
        "{}",
        ran.body
    );
    let config = json!({ "line": "tuff config" });
    let ran = admin.send_json("POST", web::COMMAND_ROUTE, &config).await;
    assert!(
        !ran.json()["feedback"].to_string().contains("config keys"),
        "{}",
        ran.body
    );

    run(&server, "tuff perm deny Alex tuff.web.access").await;
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn operators_keep_the_panel_through_bypass_until_denied() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = server(&session_url).await;
    enable(&server).await;
    run(&server, "op Alex").await;
    let code = server.handle.web_login_code(alex()).await.unwrap().unwrap();
    let mut admin = client(&server).await;
    let resp = admin.login(&code).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let config = json!({ "line": "tuff config" });
    let ran = admin.send_json("POST", web::COMMAND_ROUTE, &config).await;
    assert!(
        ran.json()["feedback"].to_string().contains("config keys"),
        "{}",
        ran.body
    );

    run(&server, "tuff perm deny Alex tuff.command.config").await;
    let ran = admin.send_json("POST", web::COMMAND_ROUTE, &config).await;
    assert!(
        !ran.json()["feedback"].to_string().contains("config keys"),
        "{}",
        ran.body
    );
    run(&server, "tuff perm deny Alex tuff.web.access").await;
    assert_eq!(admin.get(web::SESSION_ROUTE).await.status, 401);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn permission_roles_are_managed_by_command_and_survive_a_restart() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = vault_server(TempDir::new(), &session_url).await;
    for (line, expected) in [
        (
            "tuff perm role create mods 5",
            "Created role mods with priority 5",
        ),
        ("tuff perm role create mods", "already exists"),
        ("tuff perm role create Bad-Name", "a role name is"),
        ("tuff perm role create default", "Created role default"),
        (
            "tuff perm role grant mods vault.open",
            "Granted vault.open to role mods",
        ),
        (
            "tuff perm role grant mods vault.close",
            "Unknown permission node",
        ),
        ("tuff perm role grant nobody vault.open", "no such role"),
        (
            "tuff perm role deny default tuff.command.config",
            "Denied tuff.command.config to role default",
        ),
        ("tuff perm role add mods Alex", "Added Alex to role mods"),
        (
            "tuff perm role add mods Alex",
            "Alex is already in role mods",
        ),
        (
            "tuff perm role add default Alex",
            "every player is in the default role",
        ),
        (
            "tuff perm check Alex vault.open",
            "Alex has vault.open (granted as vault.open by role mods)",
        ),
        (
            "tuff perm check Alex tuff.command.config",
            "Alex lacks tuff.command.config (denied as tuff.command.config by role default)",
        ),
        (
            "tuff perm check Alex tuff.command.timings",
            "Alex lacks tuff.command.timings (default)",
        ),
        (
            "tuff perm role priority mods 7",
            "Set role mods's priority to 7",
        ),
        (
            "tuff perm role list",
            "2 permission role(s): mods (priority 7), default (priority 0)",
        ),
        (
            "tuff perm role list mods",
            "Role mods (priority 7, members: Alex): vault.open (granted)",
        ),
        (
            "tuff perm list Alex",
            "Alex has no permission settings; roles: mods, default",
        ),
        (
            "tuff perm role set mods prefix <gold>[Mod] ",
            "Set role mods's prefix to <gold>[Mod]",
        ),
        (
            "tuff perm role set mods name_color nope",
            "attribute values",
        ),
        (
            "tuff perm role set mods colour red",
            "Unknown role attribute",
        ),
        (
            "tuff perm role clear mods prefix",
            "Cleared role mods's prefix",
        ),
    ] {
        let reply = run(&server, line).await;
        assert!(reply.contains(expected), "{line}: {reply}");
    }

    let TestServer {
        handle,
        task,
        _instance,
        ..
    } = server;
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let server = vault_server(_instance, &session_url).await;
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(check.contains("by role mods"), "{check}");
    run(&server, "tuff perm deny Alex vault.open").await;
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(
        check.contains("Alex lacks vault.open (denied as vault.open)"),
        "{check}"
    );
    let removed = run(&server, "tuff perm role remove mods Alex").await;
    assert!(removed.contains("Removed Alex from role mods"), "{removed}");
    let deleted = run(&server, "tuff perm role delete default").await;
    assert!(deleted.contains("Deleted role default"), "{deleted}");
    let check = run(&server, "tuff perm check Alex tuff.command.config").await;
    assert!(
        check.contains("Alex lacks tuff.command.config (default)"),
        "{check}"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_permissions_api_lists_and_changes_settings_with_audit() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("Alex", TEST_UUID);
    let server = vault_server(TempDir::new(), &session_url).await;
    enable(&server).await;
    let admin = console_login(&server).await;
    let route = web::PERMISSIONS_ROUTE;
    let changes = format!("{route}/changes");
    let state = admin.get(route).await;
    assert_eq!(state.status, 200, "{}", state.body);
    assert!(
        state.json()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "vault.open" && n["op_bypass"] == false),
        "{}",
        state.body
    );
    let session = admin.get(web::SESSION_ROUTE).await.json();
    assert!(
        session["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("permissions"))
    );

    let uuid = alex_uuid();
    for (body, status) in [
        (
            json!({"kind":"create_role","role":"mods","priority":3}),
            200,
        ),
        (json!({"kind":"create_role","role":"mods"}), 409),
        (
            json!({"kind":"role","role":"mods","node":"vault.open","held":true}),
            200,
        ),
        (
            json!({"kind":"role","role":"mods","node":"vault.nope","held":true}),
            400,
        ),
        (
            json!({"kind":"role","role":"gone","node":"vault.open","held":true}),
            404,
        ),
        (
            json!({"kind":"member","player":uuid,"role":"mods","member":true}),
            200,
        ),
        (
            json!({"kind":"player","player":"Nobody","node":"vault.open","held":true}),
            404,
        ),
        (
            json!({"kind":"player","player":uuid,"node":"tuff.web.access","held":true}),
            200,
        ),
        (
            json!({"kind":"attribute","role":"mods","attribute":"prefix","value":"<red>[M] "}),
            200,
        ),
        (
            json!({"kind":"attribute","role":"mods","attribute":"name_color","value":"nocolour"}),
            400,
        ),
        (json!({"kind":"bogus"}), 400),
    ] {
        let resp = admin.send_json("POST", &changes, &body).await;
        assert_eq!(resp.status, status, "{body}: {}", resp.body);
    }
    let check = run(&server, "tuff perm check Alex vault.open").await;
    assert!(check.contains("by role mods"), "{check}");
    let state = admin.get(route).await.json();
    assert_eq!(state["roles"][0]["members"], json!([uuid]));
    assert_eq!(state["roles"][0]["attributes"]["prefix"], "<red>[M] ");
    let preview = admin
        .send_json(
            "POST",
            "/admin/api/v1/text/preview",
            &json!({"text":"<bold>hi","format":"minimessage"}),
        )
        .await;
    assert_eq!(preview.status, 200, "{}", preview.body);
    assert_eq!(
        preview.json()["component"],
        json!({"text":"hi","bold":true})
    );
    assert_eq!(state["players"][0]["nodes"]["tuff.web.access"], true);
    let again = admin
        .send_json(
            "POST",
            &changes,
            &json!({"kind":"player","player":uuid,"node":"tuff.web.access","held":true}),
        )
        .await;
    assert_eq!(again.json()["changed"], false, "{}", again.body);
    assert_eq!(admin.get(&changes).await.status, 405);

    let mut actions = Vec::new();
    for _ in 0..50 {
        let entries = admin
            .get(&format!("{}?limit=20", web::AUDIT_ROUTE))
            .await
            .json();
        actions = entries["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        if actions.contains(&"permissions.player".to_owned()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for action in [
        "permissions.role.create",
        "permissions.role",
        "permissions.member",
        "permissions.player",
    ] {
        assert!(actions.contains(&action.to_owned()), "{actions:?}");
    }

    let code = server.handle.web_login_code(alex()).await.unwrap().unwrap();
    let mut alex_admin = client(&server).await;
    assert_eq!(alex_admin.login(&code).await.status, 200);
    assert_eq!(alex_admin.get(route).await.status, 403);
    run(&server, "tuff perm grant Alex tuff.web.permissions").await;
    assert_eq!(alex_admin.get(route).await.status, 200);
    stop(server).await;
}

fn alex_uuid() -> String {
    let uuid = TEST_UUID;
    format!(
        "{}-{}-{}-{}-{}",
        &uuid[0..8],
        &uuid[8..12],
        &uuid[12..16],
        &uuid[16..20],
        &uuid[20..32]
    )
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_audit_log_panel_api_lists_lookups_behind_the_session() {
    let instance = TempDir::new();
    let database = instance.path().join("audit.sqlite");
    let server = start_server_with_features(
        instance,
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
            Box::new(tuff_feature_builtin_audit_log::AuditLogFeature::at(
                database,
            )),
        ],
    )
    .await;
    enable(&server).await;
    run(&server, "tuff config tuff.features.audit-log.enabled true").await;
    let api = "/admin/api/v1/plugins/audit-log";
    assert_eq!(client(&server).await.get(api).await.status, 401);
    let admin = console_login(&server).await;
    let resp = admin.get(&format!("{api}?a=chat&limit=5")).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json()["total"], 0);
    assert_eq!(resp.json()["rows"], json!([]));
    assert_eq!(admin.get(&format!("{api}?t=nonsense")).await.status, 400);
    assert_eq!(admin.get(&format!("{api}/other")).await.status, 404);
    stop(server).await;
}

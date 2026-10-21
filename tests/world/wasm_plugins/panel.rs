//! Panel pages, APIs and cards a runtime plugin registers: they appear in
//! the live panel while it is loaded, requests reach the guest as route
//! events and conflicts fail the load.

use super::*;
use tuff_http::{Method, Request};
use tuff_plugin_wasm::wit::bindings::types::{
    PanelApi, PanelAsset, PanelCard, PanelPage, PanelUnit,
};
use tuff_storage::admin::PermissionLevel;
use tuff_web_ui::auth::{AuthContext, Principal};
use tuff_web_ui::panel::ApiCall;

fn page(id: &str) -> PanelPage {
    PanelPage {
        id: id.into(),
        title: "Gold".into(),
        section: String::new(),
        icon: String::new(),
        scope: "gold.read".into(),
        module: "main.js".into(),
        styles: Vec::new(),
        assets: vec![PanelAsset {
            path: "main.js".into(),
            content_type: "text/javascript".into(),
            data: b"export function mount() {}".to_vec(),
        }],
    }
}

fn gold(plugin: &str, page_id: &str) -> Vec<u8> {
    let mut data = Data::new();
    let ok = format!("(if (i32.load8_u (i32.const {RET})) (then unreachable))");
    let mut page = page(page_id);
    page.scope = format!("{plugin}.read");
    let page = data.call("register-panel-page", &[page.to_value()]);
    let api = data.call(
        "register-panel-api",
        &[PanelApi {
            id: "stats".into(),
            scope: format!("{plugin}.read"),
        }
        .to_value()],
    );
    let card = data.call(
        "register-panel-card",
        &[PanelCard {
            series: format!("{plugin}.coins"),
            title: "Coins".into(),
            unit: PanelUnit::Count,
            scope: None,
        }
        .to_value()],
    );
    let response = WitValue::Record(vec![
        WitValue::U16(200),
        WitValue::List(Vec::new()),
        WitValue::Variant(0, Some(Box::new(WitValue::Bytes(b"{}".as_slice().into())))),
    ]);
    let respond = data
        .call("net.respond", &[WitValue::U32(7), response])
        .replacen("(i32.const 7)", "(local.get 1)", 1);
    let body = format!(
        "(func {} {page} {ok} {api} {ok} {card} {ok} {})\n(func {} {respond})\n{}",
        export("register"),
        ok_unit(),
        export("on-io"),
        data.wat()
    );
    module(&plain(plugin), &body)
}

fn call(world: &TestWorld, path: &str) -> ApiCall {
    let mut request = Request::new(Method::GET, path, "127.0.0.1:1".parse().unwrap());
    request.rest = path.into();
    let auth = AuthContext {
        principal: Principal::Operator {
            uuid: 1,
            name: "op".into(),
        },
        level: PermissionLevel::Owners,
        session: "s".into(),
        csrf: String::new(),
        permissions: std::sync::Arc::new(crate::permission::View::default()),
    };
    ApiCall {
        server: world.shared.clone(),
        auth: std::sync::Arc::new(auth),
        request,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_panel_entries_live_while_loaded_and_answer_requests() {
    let dir = TestDir::new();
    install(dir.path(), "gold.wasm", &gold("gold", "gold-page"));
    let world = start(dir.path()).await;
    let view = world.shared.web.live.view();
    let now = view.get();
    assert_eq!(
        now.pages["gold-page"].module_url(),
        "/admin/pages/gold-page/main.js"
    );
    assert_eq!(now.cards["gold.coins"].title, "Coins");
    let api = now.apis["stats"].clone();

    let reply = (api.handler)(call(&world, "/x")).await;
    assert_eq!(reply.status.as_u16(), 200);

    console(&world, "tuff plugin unload gold").await;
    let gone = view.get();
    assert!(gone.pages.is_empty() && gone.apis.is_empty() && gone.cards.is_empty());
    console(&world, "tuff plugin load gold").await;
    assert!(view.get().pages.contains_key("gold-page"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_panel_key_conflicts_fail_the_second_load() {
    let dir = TestDir::new();
    install(dir.path(), "gold.wasm", &gold("gold", "shared-page"));
    install(dir.path(), "silver.wasm", &gold("silver", "shared-page"));
    let world = start(dir.path()).await;
    let now = world.shared.web.live.view().get();
    assert_eq!(now.pages.len(), 1);
    world.shutdown().await;
}

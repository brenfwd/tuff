//! Plugin HTTP routes: `/plugins/<id>/...` and granted top-level aliases
//! on the server's own HTTP listener, with grants watched live.

use super::*;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tuff_http::{HttpConfig, MAIN_LISTENER};

#[derive(Clone, Copy)]
enum Mode {
    Bytes,
    File,
    Silent,
}

fn site(id: &str, route: &str, mode: Mode) -> Vec<u8> {
    let mut data = Data::new();
    let register = format!("(func {} {})", export("register"), ok_unit());
    let headers = WitValue::List(vec![
        WitValue::Record(vec![s("x-test"), WitValue::Bytes(b"yes".as_slice().into())]),
        WitValue::Record(vec![
            s("set-cookie"),
            WitValue::Bytes(b"a=b".as_slice().into()),
        ]),
        WitValue::Record(vec![
            s("content-security-policy"),
            WitValue::Bytes(b"default-src *".as_slice().into()),
        ]),
    ]);
    let body = match mode {
        Mode::File => WitValue::Variant(1, Some(Box::new(s("page.html")))),
        _ => WitValue::Variant(0, Some(Box::new(WitValue::Bytes(b"hi".as_slice().into())))),
    };
    let response = WitValue::Record(vec![WitValue::U16(200), headers, body]);
    let respond = data
        .call("net.respond", &[WitValue::U32(7), response])
        .replacen("(i32.const 7)", "(local.get 1)", 1);
    let on_io = match mode {
        Mode::Silent => String::new(),
        _ => respond,
    };
    let body = format!(
        "{register}\n(func {} {on_io})\n{}",
        export("on-io"),
        data.wat()
    );
    module(
        &format!("{}\ncap.route = {route}\ncap.files = 1 MiB", plain(id)),
        &body,
    )
}

async fn serve(world: &TestWorld) -> SocketAddr {
    let config = HttpConfig {
        hosts: vec!["127.0.0.1".into()],
        port: 0,
        ..HttpConfig::default()
    };
    world.shared.http.bind(MAIN_LISTENER, config).await.unwrap()[0]
}

async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let text = format!(
        "{method} {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

async fn get(addr: SocketAddr, path: &str) -> String {
    request(addr, "GET", path, "").await
}

fn status(response: &str) -> &str {
    response.split(' ').nth(1).unwrap_or("")
}

async fn started(dir: &Path) -> (TestWorld, SocketAddr) {
    let world = start_sqlite(dir, open_sqlite(dir)).await;
    let addr = serve(&world).await;
    (world, addr)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_routes_need_a_grant_and_follow_revokes() {
    let dir = TestDir::new();
    install(dir.path(), "site.wasm", &site("site", "/", Mode::Bytes));
    let (world, addr) = started(dir.path()).await;
    assert_eq!(status(&get(addr, "/plugins/site/api/x").await), "404");

    console(&world, "tuff plugin grant site route /api").await;
    let ok = get(addr, "/plugins/site/api/x?q=1").await.to_lowercase();
    assert_eq!(status(&ok), "200", "{ok}");
    assert!(ok.ends_with("hi"), "{ok}");
    assert!(ok.contains("x-test: yes"), "{ok}");
    assert!(ok.contains("x-content-type-options: nosniff"), "{ok}");
    assert!(ok.contains("content-security-policy: sandbox"), "{ok}");
    assert!(!ok.contains("set-cookie"), "{ok}");
    assert!(!ok.contains("default-src"), "{ok}");

    assert_eq!(status(&get(addr, "/plugins/site/other").await), "404");
    assert_eq!(status(&get(addr, "/plugins/ghost/api").await), "404");
    for path in ["/plugins/site/api/../x", "/plugins/site/api/%2e%2e/x"] {
        let answer = get(addr, path).await;
        assert!(["400", "404"].contains(&status(&answer)), "{answer}");
    }

    console(&world, "tuff plugin revoke site route /api").await;
    assert_eq!(status(&get(addr, "/plugins/site/api/x").await), "404");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn top_level_aliases_need_an_owner_grant_and_never_shadow_routes() {
    let dir = TestDir::new();
    install(
        dir.path(),
        "maps.wasm",
        &site("maps", "/map top", Mode::Bytes),
    );
    install(
        dir.path(),
        "rival.wasm",
        &site("rival", "/map top", Mode::Bytes),
    );
    install(
        dir.path(),
        "admin.wasm",
        &site("adminish", "/admin top", Mode::Bytes),
    );
    let (world, addr) = started(dir.path()).await;
    world
        .shared
        .http
        .register_route(
            "/admin",
            tuff_http::handler(|_| async { tuff_http::Response::text("built-in") }),
        )
        .await;
    assert_eq!(status(&get(addr, "/map/x").await), "404");

    let out = console(&world, "tuff plugin grant maps route /map top").await;
    assert!(!out[0].contains("cannot"), "{out:?}");
    let ok = get(addr, "/map/x").await;
    assert_eq!(status(&ok), "200", "{ok}");
    assert!(ok.ends_with("hi"));

    let out = console(&world, "tuff plugin grant rival route /map top").await;
    assert!(out[0].contains("overlaps"), "{out:?}");
    let out = console(&world, "tuff plugin grant adminish route /admin top").await;
    assert!(out[0].contains("overlaps"), "{out:?}");
    assert!(get(addr, "/admin").await.ends_with("built-in"));

    console(&world, "tuff plugin revoke maps route /map top").await;
    assert_eq!(status(&get(addr, "/map/x").await), "404");
    let out = console(&world, "tuff plugin grant rival route /map top").await;
    assert!(!out[0].contains("cannot"), "{out:?}");
    assert_eq!(status(&get(addr, "/map").await), "200");
    console(&world, "tuff plugin unload rival").await;
    assert_eq!(status(&get(addr, "/map").await), "404");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routes_stream_files_from_the_plugins_store() {
    let dir = TestDir::new();
    install(dir.path(), "pages.wasm", &site("pages", "/", Mode::File));
    let store = plugins_dir(dir.path()).join("pages").join("files");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("page.html"), "<p>files</p>").unwrap();
    let (world, addr) = started(dir.path()).await;
    console(&world, "tuff plugin grant pages route /").await;
    console(&world, "tuff plugin grant pages files 1 MiB").await;
    let page = get(addr, "/plugins/pages/").await.to_lowercase();
    assert_eq!(status(&page), "200", "{page}");
    assert!(page.contains("content-type: text/html"), "{page}");
    assert!(page.ends_with("<p>files</p>"), "{page}");

    console(&world, "tuff plugin revoke pages files all").await;
    assert_eq!(status(&get(addr, "/plugins/pages/").await), "404");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_limits_cap_bodies_concurrency_and_time() {
    let dir = TestDir::new();
    install(dir.path(), "quiet.wasm", &site("quiet", "/", Mode::Silent));
    install(dir.path(), "loud.wasm", &site("loud", "/", Mode::Bytes));
    let (world, addr) = started(dir.path()).await;
    console(&world, "tuff plugin grant loud route /").await;
    console(&world, "tuff plugin grant loud limit.route-body-bytes 4").await;
    assert_eq!(
        status(&request(addr, "POST", "/plugins/loud/x", "0123456789").await),
        "413"
    );
    assert_eq!(
        status(&request(addr, "POST", "/plugins/loud/x", "012").await),
        "200"
    );

    console(&world, "tuff plugin grant quiet route /").await;
    console(&world, "tuff plugin grant quiet limit.route-timeout 600ms").await;
    console(&world, "tuff plugin grant quiet limit.route-concurrency 1").await;
    let slow = tokio::spawn(async move { get(addr, "/plugins/quiet/a").await });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(status(&get(addr, "/plugins/quiet/b").await), "503");
    assert_eq!(status(&slow.await.unwrap()), "504");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unloading_a_plugin_ends_its_requests_in_flight() {
    let dir = TestDir::new();
    install(dir.path(), "quiet.wasm", &site("quiet", "/", Mode::Silent));
    let (world, addr) = started(dir.path()).await;
    console(&world, "tuff plugin grant quiet route /").await;
    let waiting = tokio::spawn(async move { get(addr, "/plugins/quiet/a").await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    console(&world, "tuff plugin unload quiet").await;
    assert_eq!(status(&waiting.await.unwrap()), "503");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_routes_answer_while_loaded_and_404_after_unload() {
    let dir = TestDir::new();
    install(dir.path(), "site.wasm", &site("site", "/", Mode::Bytes));
    let (world, addr) = started(dir.path()).await;
    console(&world, "tuff plugin grant site route /").await;
    let ok = get(addr, "/plugins/site/a").await;
    assert_eq!(status(&ok), "200", "{ok}");
    console(&world, "tuff plugin unload site").await;
    assert_eq!(status(&get(addr, "/plugins/site/a").await), "404");
    let posted = request(addr, "POST", "/plugins/site/a", "x").await;
    assert_eq!(status(&posted), "404", "{posted}");
    world.shutdown().await;
}

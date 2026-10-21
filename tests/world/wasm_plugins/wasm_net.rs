//! Plugin network capabilities end to end: a guest granted through the
//! SQLite backend reaches in-process servers on port 0, and live grant
//! changes (revoke, limits) take effect on what it already has open.

use super::*;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tuff_plugin_wasm::wit::bindings::capability_types::{CapabilityKind, HttpRequest};

fn steve() -> Player {
    Player::new(77, "Steve")
}

/// A plugin whose command `go` runs `call` and returns the result tag
/// (0 ok, 1 denied), and which broadcasts `io` for every I/O event.
fn netter(id: &str, caps: &str, op: &str, args: &[WitValue<'_>]) -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["go"], "");
    let call = data.call(op, args);
    let io = text(&mut data, "io");
    let body = format!(
        "{register}
         (func {} (local $tag i32) {call} (local.set $tag (i32.load8_u (i32.const {RET}))) {})
         (func {} (call $broadcast {io} (i32.const {RET})))
         {}",
        export("run-command"),
        ok_code("(local.get $tag)"),
        export("on-io"),
        data.wat()
    );
    module(&format!("{}\n{caps}", plain(id)), &body)
}

async fn revoked(world: &TestWorld, line: &str) {
    let out = console(world, line).await;
    assert!(out[0].starts_with("Revoked"), "{out:?}");
}

async fn go(world: &TestWorld) -> i32 {
    host(world)
        .command(steve(), "go".into())
        .await
        .unwrap()
        .result
        .unwrap()
}

async fn heard(world: &TestWorld, want: usize) {
    for _ in 0..500 {
        if count(world, "io").await >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "expected {want} io events, got {}",
        count(world, "io").await
    );
}

async fn world_with(dir: &TestDir, plugin: &str, bytes: &[u8]) -> TestWorld {
    install(dir.path(), &format!("{plugin}.wasm"), bytes);
    start_sqlite(dir.path(), open_sqlite(dir.path())).await
}

fn get(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
        timeout_ms: None,
        decompress: false,
        max_redirects: 0,
    }
}

async fn web_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_grants_revokes_and_limits_apply_live() {
    let dir = TestDir::new();
    let addr = web_server().await;
    let base = format!("http://127.0.0.1:{}", addr.port());
    let request = get(format!("{base}/"));
    let guest = netter(
        "web",
        &format!("cap.http = {base}"),
        "net.fetch",
        &[request.to_value()],
    );
    let world = world_with(&dir, "web", &guest).await;

    assert_eq!(go(&world).await, 1, "not granted yet");
    console(
        &world,
        &format!("tuff plugin grant web http {base} private"),
    )
    .await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 1).await;

    let out = console(&world, "tuff plugin grant web limit.requests-per-minute 1").await;
    assert!(out[0].contains("is now 1"), "{out:?}");
    assert_eq!(go(&world).await, 0, "one token left after the cap shrinks");
    assert_eq!(go(&world).await, 1, "the new limit applies without reload");
    heard(&world, 2).await;

    revoked(
        &world,
        &format!("tuff plugin revoke web http {base} private"),
    )
    .await;
    assert_eq!(go(&world).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_grant_connects_and_revoke_closes_it() {
    let dir = TestDir::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (gone, mut closed) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let gone = gone.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::text("hello"))
                    .await;
                while ws.next().await.is_some_and(|m| m.is_ok()) {}
                let _ = gone.send(());
            });
        }
    });
    let base = format!("ws://127.0.0.1:{}", addr.port());
    let guest = netter(
        "sock",
        &format!("cap.websocket = {base}"),
        "net.ws-connect",
        &[
            s(&format!("{base}/")),
            WitValue::List(vec![]),
            WitValue::List(vec![]),
        ],
    );
    let world = world_with(&dir, "sock", &guest).await;

    assert_eq!(go(&world).await, 1);
    console(
        &world,
        &format!("tuff plugin grant sock websocket {base} private"),
    )
    .await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 2).await;
    revoked(
        &world,
        &format!("tuff plugin revoke sock websocket {base} private"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), closed.recv())
        .await
        .expect("the server saw the socket close")
        .unwrap();
    heard(&world, 3).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_grant_connects_and_revoke_closes_it() {
    let dir = TestDir::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (gone, mut closed) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let gone = gone.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                let _ = gone.send(());
            });
        }
    });
    let target = format!("127.0.0.1:{port}");
    let guest = netter(
        "raw",
        &format!("cap.tcp = {target}"),
        "net.tcp-connect",
        &[s("127.0.0.1"), WitValue::U16(port), WitValue::Bool(false)],
    );
    let world = world_with(&dir, "raw", &guest).await;

    assert_eq!(go(&world).await, 1);
    console(
        &world,
        &format!("tuff plugin grant raw tcp {target} private"),
    )
    .await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 1).await;
    revoked(
        &world,
        &format!("tuff plugin revoke raw tcp {target} private"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), closed.recv())
        .await
        .expect("the server saw the connection close")
        .unwrap();
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_grant_connects_and_revoke_closes_it() {
    let dir = TestDir::new();
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let target = format!("127.0.0.1:{port}");
    let guest = netter(
        "dgram",
        &format!("cap.udp = {target}"),
        "net.udp-connect",
        &[s("127.0.0.1"), WitValue::U16(port)],
    );
    let world = world_with(&dir, "dgram", &guest).await;

    assert_eq!(go(&world).await, 1);
    console(
        &world,
        &format!("tuff plugin grant dgram udp {target} private"),
    )
    .await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 1).await;
    revoked(
        &world,
        &format!("tuff plugin revoke dgram udp {target} private"),
    )
    .await;
    heard(&world, 2).await;
    world.shutdown().await;
}

/// A port below every OS ephemeral range, so outbound connections of
/// concurrently running tests cannot take it between the probe and the
/// plugin's own bind.
async fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    loop {
        let step = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let port = 20_000 + (std::process::id() % 97) as u16 * 100 + step % 100;
        if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
            return port;
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_listener_grant_accepts_and_revoke_closes_it() {
    let dir = TestDir::new();
    let port = free_port().await;
    let guest = netter(
        "serve",
        &format!("cap.listen-tcp = {port}"),
        "net.listen",
        &[
            CapabilityKind::Tcp.to_value(),
            None::<String>.to_value(),
            WitValue::U16(port),
        ],
    );
    let world = world_with(&dir, "serve", &guest).await;

    assert_eq!(go(&world).await, 1);
    console(
        &world,
        &format!("tuff plugin grant serve listen-tcp {port}"),
    )
    .await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 1).await;
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    heard(&world, 2).await;
    revoked(
        &world,
        &format!("tuff plugin revoke serve listen-tcp {port}"),
    )
    .await;
    let mut buf = [0u8; 8];
    let end = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buf))
        .await
        .expect("the connection ends");
    assert!(matches!(end, Ok(0) | Err(_)), "{end:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_listener_grant_hears_datagrams() {
    let dir = TestDir::new();
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let guest = netter(
        "hear",
        &format!("cap.listen-udp = {port}"),
        "net.listen",
        &[
            CapabilityKind::Udp.to_value(),
            None::<String>.to_value(),
            WitValue::U16(port),
        ],
    );
    let world = world_with(&dir, "hear", &guest).await;

    console(&world, &format!("tuff plugin grant hear listen-udp {port}")).await;
    assert_eq!(go(&world).await, 0);
    heard(&world, 1).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"ping", ("127.0.0.1", port)).await.unwrap();
    heard(&world, 2).await;
    world.shutdown().await;
}

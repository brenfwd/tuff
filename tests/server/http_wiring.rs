use crate::common;

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use common::{TempDir, start_server_with_features};
use tuff_core::feature::{Feature, LoadContext};
use tuff_http::{Response, handler};
use tuff_item::load::{LoadError, Phase};

/// A tiny plugin exercising `Registries::http_routes`: a plugin registers a
/// route beside core's own, with no network code of its own.
struct RouteFeature;

impl Feature for RouteFeature {
    fn id(&self) -> &'static str {
        "http-wiring-test-route"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().http_routes.push((
                "/hello".to_string(),
                handler(|_| async { Response::text("hi from plugin") }),
            ));
        }
        Ok(())
    }
}

/// A minimal blocking HTTP/1.1 GET over a plain `TcpStream`, since this is
/// an in-process test server, not a real client.
async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connect timed out")
        .unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("read timed out")
        .unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn http_server_spawns_binds_and_serves_feature_routes() {
    let server = start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![Box::new(RouteFeature)],
    )
    .await;

    let addrs = server.handle.http_addrs().await.unwrap();
    assert!(!addrs.is_empty(), "http server did not bind");
    assert!(addrs[0].ip().is_loopback() || addrs[0].ip().is_unspecified());

    let (status, body) = get(addrs[0], "/hello").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("hi from plugin"), "{body}");

    let (missing_status, _) = get(addrs[0], "/nope").await;
    assert_eq!(missing_status, 404);

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn http_rebinds_when_config_changes() {
    let server = start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![
            Box::new(RouteFeature),
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
        ],
    )
    .await;

    let first = server.handle.http_addrs().await.unwrap();
    assert!(!first.is_empty());
    let first_port = first[0].port();

    // Pick a free port deterministically rather than relying on the kernel
    // not reusing the same ephemeral port for a second `0` bind.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = probe.local_addr().unwrap().port();
    drop(probe);
    assert_ne!(target_port, first_port);

    server
        .handle
        .run_command(format!("tuff config tuff.http.port {target_port}"))
        .await
        .unwrap();

    // The rebind watcher reacts to the config change asynchronously; poll
    // briefly instead of assuming it is instantaneous.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let rebound = loop {
        let rebound = server.handle.http_addrs().await.unwrap();
        if (!rebound.is_empty() && rebound[0].port() == target_port)
            || tokio::time::Instant::now() >= deadline
        {
            break rebound;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(!rebound.is_empty(), "lost the http listener on rebind");
    assert_eq!(
        rebound[0].port(),
        target_port,
        "expected the listener to move to the configured port"
    );

    let (status, body) = get(rebound[0], "/hello").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("hi from plugin"), "{body}");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn http_shuts_down_cleanly_with_the_world() {
    let server = start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        Vec::new(),
    )
    .await;
    let addrs = server.handle.http_addrs().await.unwrap();
    assert!(!addrs.is_empty());
    let addr = addrs[0];

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();

    // The listener task is aborted on shutdown, so a new connection either
    // fails outright or the peer closes without a response.
    let connect = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(addr)).await;
    if let Ok(Ok(mut stream)) = connect {
        let _ = stream.write_all(b"GET / HTTP/1.1\r\n\r\n").await;
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
        assert!(
            matches!(read, Ok(Ok(0)) | Err(_)),
            "http server still answering after shutdown"
        );
    }
}

struct MockProbeFeature(std::net::SocketAddr);

impl Feature for MockProbeFeature {
    fn id(&self) -> &'static str {
        "public-ip-test-probe"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().public_ip_endpoints = Some(vec![tuff_core::public_ip::Endpoint::new(
                self.0.ip().to_string(),
                self.0.port(),
                tuff_core::public_ip::Family::V4,
            )]);
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn public_base_url_follows_config() {
    let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let probe_addr = probe.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, peer)) = probe.recv_from(&mut buf).await else {
                return;
            };
            let Some(txn) = buf.get(8..20).filter(|_| n == 20) else {
                continue;
            };
            let txn: tuff_core::public_ip::TransactionId = txn.try_into().unwrap();
            let mapped = "203.0.113.5:40000".parse().unwrap();
            let reply = tuff_core::public_ip::binding_response(&txn, mapped);
            let _ = probe.send_to(&reply, peer).await;
        }
    });
    let server = start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        "http://127.0.0.1:1",
        Some(false),
        vec![
            Box::new(MockProbeFeature(probe_addr)),
            Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
        ],
    )
    .await;
    let port = server.handle.http_addrs().await.unwrap()[0].port();
    let base = server.handle.public_base_url().await.unwrap();
    let mut w = base.watch();
    wait(&mut w, format!("http://127.0.0.1:{port}")).await;

    server
        .handle
        .run_command("tuff config tuff.http.detectPublicIp true".to_string())
        .await
        .unwrap();
    wait(&mut w, format!("http://203.0.113.5:{port}")).await;

    server
        .handle
        .run_command(
            "tuff config tuff.http.resourcePathBaseUrl https://packs.example.com".to_string(),
        )
        .await
        .unwrap();
    wait(&mut w, "https://packs.example.com".to_string()).await;

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn wait(w: &mut tokio::sync::watch::Receiver<String>, want: String) {
    tokio::time::timeout(Duration::from_secs(10), w.wait_for(|u| *u == want))
        .await
        .expect("base url did not converge")
        .unwrap();
}

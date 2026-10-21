use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::socket_tests::rig as socket_rig;
use super::*;
use crate::caps::resolve::{Lookup, Resolver};
use crate::wit::bindings::capability_types::IoEvent;

const SERVER_CERT: &[u8] = include_bytes!("testdata/server.der");
const SERVER_KEY: &[u8] = include_bytes!("testdata/server.key.der");

fn acceptor() -> TlsAcceptor {
    let config =
        rustls::ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(SERVER_CERT.to_vec())],
                PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec()).into(),
            )
            .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

struct To(SocketAddr);

impl Resolver for To {
    fn lookup<'a>(&'a self, _name: &'a str, _port: u16) -> Lookup<'a> {
        let found = vec![self.0];
        Box::pin(async move { Ok(found) })
    }
}

async fn tls_server<F, Fut>(serve: F) -> SocketAddr
where
    F: Fn(tokio_rustls::server::TlsStream<tokio::net::TcpStream>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = acceptor();
    let serve = Arc::new(serve);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let serve = serve.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(stream).await {
                    serve(stream).await;
                }
            });
        }
    });
    addr
}

fn http_rig(host: &str, port: u16, target: SocketAddr) -> super::tests::Rig {
    let base = format!("https://{host}:{port}");
    super::tests::rig_with(
        &base,
        &format!("http = {base} private\n"),
        Arc::new(To(target)),
    )
}

#[tuff_core::watchdog]
#[tokio::test]
async fn https_trusts_the_test_ca_and_checks_the_name() {
    let addr = tls_server(|mut stream| async move {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
        let _ = stream.shutdown().await;
    })
    .await;

    let mut rig = http_rig("localhost", addr.port(), addr);
    rig.net
        .fetch(super::tests::get(format!(
            "https://localhost:{}/",
            addr.port()
        )))
        .unwrap();
    let response = rig.done().await.outcome.unwrap();
    assert_eq!(
        (response.status, response.body.as_slice()),
        (200, &b"ok"[..])
    );

    let mut wrong = http_rig("wrong.example", addr.port(), addr);
    wrong
        .net
        .fetch(super::tests::get(format!(
            "https://wrong.example:{}/",
            addr.port()
        )))
        .unwrap();
    assert!(wrong.done().await.outcome.is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_tls_echoes_over_an_encrypted_stream() {
    let addr = tls_server(|mut stream| async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                break;
            }
            let _ = stream.flush().await;
        }
    })
    .await;
    let port = addr.port();
    let target = format!("localhost:{port}");
    let mut rig = socket_rig("cap.tcp", &target, &format!("tcp = {target} private"));
    let socket = rig.net.tcp_connect("localhost", port, true).unwrap();
    match rig.event().await {
        IoEvent::Connected(done) => done.outcome.unwrap(),
        _ => panic!("not a connect event"),
    }
    rig.net.send(socket, b"secret").unwrap();
    match rig.event().await {
        IoEvent::Data(data) => assert_eq!(data.data, b"secret"),
        _ => panic!("not data"),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn wss_echoes_over_tls() {
    let addr = tls_server(|stream| async move {
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        while let Some(Ok(message)) = ws.next().await {
            if message.is_text() && ws.send(message).await.is_err() {
                break;
            }
        }
    })
    .await;
    let base = format!("wss://localhost:{}", addr.port());
    let (policy, watcher) = tokio::sync::watch::channel(super::ws_tests::policy(
        &base,
        &format!("websocket = {base} private\n"),
    ));
    let _keep = policy;
    let (queue, mut events) = tokio::sync::mpsc::unbounded_channel();
    let net = Net::new(watcher, Arc::new(To(addr)), "p", 1, queue);
    let socket = net.ws_connect(&format!("{base}/"), vec![], vec![]).unwrap();
    let mut next = async || {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap()
            .event
    };
    match next().await {
        IoEvent::Connected(done) => assert!(done.outcome.is_ok()),
        _ => panic!("not connected"),
    }
    net.ws_send(socket, WsMessage::Text("hi".into())).unwrap();
    match next().await {
        IoEvent::WsMessage(frame) => assert_eq!(frame.message, WsMessage::Text("hi".into())),
        _ => panic!("not a message"),
    }
}

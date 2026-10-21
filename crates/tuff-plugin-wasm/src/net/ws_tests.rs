use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::*;
use crate::caps::Grants;
use crate::caps::policy::Request;
use crate::caps::resolve::SystemResolver;
use crate::wit::bindings::capability_types::{IoEvent, WsMessage};

#[derive(Clone, Copy)]
enum Script {
    Echo,
    Burst,
    CloseWith,
    Silent,
}

async fn server(script: Script) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                match script {
                    Script::Echo => {
                        while let Some(Ok(message)) = ws.next().await {
                            if message.is_text() || message.is_binary() {
                                let _ = ws.send(message).await;
                            }
                        }
                    }
                    Script::Burst => {
                        let _ = ws.send(Message::binary(vec![0u8; 4096])).await;
                        let _ = ws.next().await;
                    }
                    Script::CloseWith => {
                        let _ = ws
                            .close(Some(CloseFrame {
                                code: CloseCode::Away,
                                reason: "bye".into(),
                            }))
                            .await;
                    }
                    Script::Silent => {
                        let _ = ws.next().await;
                    }
                }
            });
        }
    });
    addr
}

struct Rig {
    net: Net,
    policy: watch::Sender<Arc<Policy>>,
    events: mpsc::UnboundedReceiver<Delivery>,
}

pub(super) fn policy(request: &str, grant: &str) -> Arc<Policy> {
    let requests = Request::parse_line("cap.websocket", request)
        .unwrap()
        .unwrap();
    let grants = Grants::parse(&format!("[p]\n{grant}")).unwrap();
    Arc::new(Policy::new(requests, grants.get("p")))
}

fn rig(addr: SocketAddr, limits: &str) -> Rig {
    let host = format!("ws://127.0.0.1:{}", addr.port());
    let (policy, watcher) = watch::channel(self::policy(
        &host,
        &format!("websocket = {host} private\n{limits}"),
    ));
    let (queue, events) = mpsc::unbounded_channel();
    Rig {
        net: Net::new(watcher, Arc::new(SystemResolver), "p", 1, queue),
        policy,
        events,
    }
}

fn url(addr: SocketAddr) -> String {
    format!("ws://127.0.0.1:{}/", addr.port())
}

impl Rig {
    async fn next(&mut self) -> IoEvent {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("an event")
            .unwrap()
            .event
    }

    async fn open(&mut self, addr: SocketAddr) -> Handle {
        let handle = self.net.ws_connect(&url(addr), vec![], vec![]).unwrap();
        match self.next().await {
            IoEvent::Connected(c) => assert!(c.outcome.is_ok() && c.socket == handle),
            _ => panic!("not connected"),
        }
        handle
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn echoes_text_and_binary() {
    let addr = server(Script::Echo).await;
    let mut rig = rig(addr, "");
    let socket = rig.open(addr).await;
    rig.net
        .ws_send(socket, WsMessage::Text("hi".into()))
        .unwrap();
    rig.net
        .ws_send(socket, WsMessage::Binary(vec![1, 2]))
        .unwrap();
    for want in [WsMessage::Text("hi".into()), WsMessage::Binary(vec![1, 2])] {
        match rig.next().await {
            IoEvent::WsMessage(frame) => assert_eq!(frame.message, want),
            _ => panic!("not a message"),
        }
    }
    rig.net.close(socket);
    assert_eq!(rig.net.open_handles(), 0);
    assert_eq!(
        rig.net.ws_send(socket, WsMessage::Text("x".into())),
        Err(Denied::Closed)
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn peer_close_reports_code_and_reason() {
    let addr = server(Script::CloseWith).await;
    let mut rig = rig(addr, "");
    let socket = rig.open(addr).await;
    match rig.next().await {
        IoEvent::Closed(closed) => {
            assert_eq!(closed.socket, socket);
            assert!(closed.error.is_none());
            assert_eq!(closed.code, Some(1001));
            assert_eq!(closed.reason, "bye");
        }
        _ => panic!("not closed"),
    }
    assert_eq!(rig.net.open_handles(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn oversized_messages_are_refused_both_ways() {
    let addr = server(Script::Burst).await;
    let mut rig = rig(addr, "limit.message-bytes = 1024");
    let socket = rig.open(addr).await;
    match rig.next().await {
        IoEvent::Closed(closed) => assert!(matches!(closed.error, Some(Denied::Quota(_)))),
        _ => panic!("not closed"),
    }
    let addr = server(Script::Silent).await;
    let mut rig = self::rig(addr, "limit.message-bytes = 1024");
    let socket2 = rig.open(addr).await;
    assert!(matches!(
        rig.net.ws_send(socket2, WsMessage::Binary(vec![0; 1025])),
        Err(Denied::Quota(_))
    ));
    let _ = socket;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn denied_hosts_and_connection_cap() {
    let addr = server(Script::Silent).await;
    let rig = rig(addr, "limit.connections = 1");
    assert!(matches!(
        rig.net.ws_connect("ws://127.0.0.1:1/", vec![], vec![]),
        Err(Denied::NotGranted(_) | Denied::NotRequested(_))
    ));
    assert!(matches!(
        rig.net.ws_connect(
            &url(addr),
            vec![Header {
                name: "Upgrade".into(),
                value: b"x".to_vec()
            }],
            vec![]
        ),
        Err(Denied::Malformed(_))
    ));
    rig.net.ws_connect(&url(addr), vec![], vec![]).unwrap();
    assert!(matches!(
        rig.net.ws_connect(&url(addr), vec![], vec![]),
        Err(Denied::Quota(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn revoke_and_unload_close_the_socket() {
    let addr = server(Script::Silent).await;
    let mut rig = rig(addr, "");
    let socket = rig.open(addr).await;
    rig.policy.send_replace(policy("example.org", ""));
    match rig.next().await {
        IoEvent::Closed(closed) => {
            assert_eq!(closed.socket, socket);
            assert!(matches!(closed.error, Some(Denied::NotRequested(_))));
        }
        _ => panic!("not closed"),
    }
    assert_eq!(rig.net.open_handles(), 0);

    let mut rig = self::rig(addr, "");
    rig.open(addr).await;
    let usage = rig.net.shared().usage.clone();
    assert_eq!(usage.open_outbound(), 1);
    drop(rig.net);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(usage.open_outbound(), 0);
    assert!(rig.events.try_recv().is_err());
}

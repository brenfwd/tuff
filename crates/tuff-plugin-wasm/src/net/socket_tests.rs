use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, watch};

use super::*;
use crate::caps::Grants;
use crate::caps::policy::Request;
use crate::caps::resolve::SystemResolver;
use crate::wit::bindings::capability_types::IoEvent;

pub(super) struct Rig {
    pub(super) net: Net,
    pub(super) policy: watch::Sender<Arc<Policy>>,
    pub(super) events: mpsc::UnboundedReceiver<Delivery>,
}

pub(super) fn policy(cap: &str, request: &str, grant: &str) -> Arc<Policy> {
    let requests = Request::parse_line(cap, request).unwrap().unwrap();
    let grants = Grants::parse(&format!("[p]\n{grant}")).unwrap();
    Arc::new(Policy::new(requests, grants.get("p")))
}

pub(super) fn rig(cap: &str, request: &str, grant: &str) -> Rig {
    let (policy, watcher) = watch::channel(policy(cap, request, grant));
    let (queue, events) = mpsc::unbounded_channel();
    Rig {
        net: Net::new(watcher, Arc::new(SystemResolver), "p", 1, queue),
        policy,
        events,
    }
}

fn tcp_rig(host: &str, port: u16, extra: &str) -> Rig {
    let target = format!("{host}:{port}");
    let grant = format!("tcp = {target} private\n{extra}");
    rig("cap.tcp", &target, &grant)
}

impl Rig {
    pub(super) async fn next(&mut self) -> Delivery {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("an event")
            .unwrap()
    }

    pub(super) async fn event(&mut self) -> IoEvent {
        self.next().await.event
    }

    async fn connected(&mut self) -> Result<(), Denied> {
        match self.event().await {
            IoEvent::Connected(done) => done.outcome,
            _ => panic!("not a connect event"),
        }
    }

    async fn data(&mut self) -> Vec<u8> {
        match self.event().await {
            IoEvent::Data(data) => data.data,
            _ => panic!("not a data event"),
        }
    }
}

async fn echo(bind: &str) -> SocketAddr {
    let listener = TcpListener::bind(bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_echoes_to_a_granted_private_address() {
    let addr = echo("127.0.0.1:0").await;
    let mut rig = tcp_rig("127.0.0.1", addr.port(), "");
    let socket = rig
        .net
        .tcp_connect("127.0.0.1", addr.port(), false)
        .unwrap();
    rig.connected().await.unwrap();
    rig.net.send(socket, b"ping").unwrap();
    assert_eq!(rig.data().await, b"ping");
    rig.net.close(socket);
    assert_eq!(rig.net.open_handles(), 0);
    assert!(matches!(rig.net.send(socket, b"x"), Err(Denied::Closed)));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_reaches_an_ipv6_loopback_grant() {
    let Ok(listener) = TcpListener::bind("[::1]:0").await else {
        return;
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"v6").await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut rig = rig(
        "cap.tcp",
        &format!("[::1]:{port}"),
        &format!("tcp = [::1]:{port}"),
    );
    rig.net.tcp_connect("::1", port, false).unwrap();
    rig.connected().await.unwrap();
    assert_eq!(rig.data().await, b"v6");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_private_addresses_need_a_private_grant_or_cidr() {
    let addr = echo("127.0.0.1:0").await;
    let port = addr.port();
    let target = format!("127.0.0.1:{port}");

    let named = format!("localhost:{port}");
    let mut plain = rig("cap.tcp", &named, &format!("tcp = {named}"));
    plain.net.tcp_connect("localhost", port, false).unwrap();
    let err = plain.connected().await.unwrap_err();
    assert!(matches!(err, Denied::Address(_)), "{err:?}");

    let mut cidr = rig("cap.tcp", &target, &format!("tcp = [127.0.0.0/8]:{port}"));
    cidr.net.tcp_connect("127.0.0.1", port, false).unwrap();
    cidr.connected().await.unwrap();

    let mut flagged = tcp_rig("127.0.0.1", port, "");
    flagged.net.tcp_connect("127.0.0.1", port, false).unwrap();
    flagged.connected().await.unwrap();
    assert!(matches!(
        flagged
            .net
            .tcp_connect("127.0.0.1", port.wrapping_add(1), false),
        Err(Denied::NotRequested(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_refuses_ungranted_targets_up_front() {
    let rig = rig("cap.tcp", "db.example.com:5432", "");
    assert!(matches!(
        rig.net.tcp_connect("db.example.com", 5432, false),
        Err(Denied::NotGranted(_))
    ));
    assert!(matches!(
        rig.net.tcp_connect("other.example.com", 5432, false),
        Err(Denied::NotRequested(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_revoking_the_grant_closes_the_socket() {
    let addr = echo("127.0.0.1:0").await;
    let mut rig = tcp_rig("127.0.0.1", addr.port(), "");
    let socket = rig
        .net
        .tcp_connect("127.0.0.1", addr.port(), false)
        .unwrap();
    rig.connected().await.unwrap();
    let target = format!("127.0.0.1:{}", addr.port());
    rig.policy.send(policy("cap.tcp", &target, "")).unwrap();
    match rig.event().await {
        IoEvent::Closed(closed) => {
            assert_eq!(closed.socket, socket);
            assert!(matches!(closed.error, Some(Denied::NotGranted(_))));
        }
        _ => panic!("not a close event"),
    }
    assert_eq!(rig.net.open_handles(), 0);
    assert_eq!(rig.net.shared().usage.open_outbound(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_peer_close_and_unload_release_everything() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, gone) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        first.write_all(b"bye").await.unwrap();
        drop(first);
        let (mut second, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8];
        let n = second.read(&mut buf).await.unwrap_or(0);
        let _ = seen.send(n);
    });
    let mut rig = tcp_rig("127.0.0.1", port, "");
    rig.net.tcp_connect("127.0.0.1", port, false).unwrap();
    rig.connected().await.unwrap();
    assert_eq!(rig.data().await, b"bye");
    match rig.event().await {
        IoEvent::Closed(closed) => assert!(closed.error.is_none()),
        _ => panic!("not a close event"),
    }
    rig.net.tcp_connect("127.0.0.1", port, false).unwrap();
    rig.connected().await.unwrap();
    drop(rig);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), gone)
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_pauses_reading_while_events_are_pending() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        for part in [&b"one"[..], b"two", b"three"] {
            stream.write_all(part).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut rig = tcp_rig("127.0.0.1", port, "limit.pending-events = 1");
    rig.net.tcp_connect("127.0.0.1", port, false).unwrap();
    let connect = rig.next().await;
    assert!(matches!(connect.event, IoEvent::Connected(_)));
    drop(connect);
    let first = rig.next().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(rig.events.try_recv().is_err());
    let mut total = match &first.event {
        IoEvent::Data(data) => data.data.len(),
        other => panic!("not a data event: {other:?}"),
    };
    drop(first);
    while total < 11 {
        let delivery = rig.next().await;
        match &delivery.event {
            IoEvent::Data(data) => total += data.data.len(),
            other => panic!("not a data event: {other:?}"),
        }
    }
    assert_eq!(total, 11);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_send_limits_and_idle_timeout() {
    let addr = echo("127.0.0.1:0").await;
    let mut rig = tcp_rig(
        "127.0.0.1",
        addr.port(),
        "limit.message-bytes = 8\nlimit.idle-timeout = 300ms",
    );
    let socket = rig
        .net
        .tcp_connect("127.0.0.1", addr.port(), false)
        .unwrap();
    rig.connected().await.unwrap();
    assert!(matches!(
        rig.net.send(socket, &[0; 9]),
        Err(Denied::Quota(_))
    ));
    match rig.event().await {
        IoEvent::Closed(closed) => assert!(matches!(closed.error, Some(Denied::TimedOut))),
        _ => panic!("not a close event"),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_tls_to_a_plain_server_fails_the_connect() {
    let addr = echo("127.0.0.1:0").await;
    let mut rig = tcp_rig("127.0.0.1", addr.port(), "");
    rig.net.tcp_connect("127.0.0.1", addr.port(), true).unwrap();
    assert!(matches!(
        rig.connected().await,
        Err(Denied::Failed(_) | Denied::TimedOut)
    ));
    assert_eq!(rig.net.open_handles(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn udp_exchanges_datagrams_with_one_destination() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let target = format!("127.0.0.1:{port}");
    let mut rig = rig("cap.udp", &target, &format!("udp = {target} private"));
    let socket = rig.net.udp_connect("127.0.0.1", port).unwrap();
    rig.connected().await.unwrap();
    rig.net.send(socket, b"hello").unwrap();
    let mut buf = [0u8; 64];
    let (n, from) = tokio::time::timeout(Duration::from_secs(10), server.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"hello");
    server.send_to(b"world", from).await.unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    stranger.send_to(b"spoof", from).await.unwrap();
    match rig.event().await {
        IoEvent::Datagram(datagram) => {
            assert_eq!(datagram.data, b"world");
            assert_eq!(datagram.peer, target);
        }
        _ => panic!("not a datagram event"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rig.events.recv())
            .await
            .is_err()
    );
    rig.net.close(socket);
    assert_eq!(rig.net.open_handles(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn udp_ipv6_and_revocation() {
    let Ok(server) = UdpSocket::bind("[::1]:0").await else {
        return;
    };
    let port = server.local_addr().unwrap().port();
    let target = format!("[::1]:{port}");
    let mut rig = rig("cap.udp", &target, &format!("udp = {target}"));
    let socket = rig.net.udp_connect("::1", port).unwrap();
    rig.connected().await.unwrap();
    rig.net.send(socket, b"v6").unwrap();
    let mut buf = [0u8; 8];
    let (n, _) = tokio::time::timeout(Duration::from_secs(10), server.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"v6");
    rig.policy.send(policy("cap.udp", &target, "")).unwrap();
    match rig.event().await {
        IoEvent::Closed(closed) => assert!(matches!(closed.error, Some(Denied::NotGranted(_)))),
        _ => panic!("not a close event"),
    }
    assert_eq!(rig.net.shared().usage.open_outbound(), 0);
}

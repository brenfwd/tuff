use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use super::listen::Reserved;
use super::socket_tests::{Rig, policy, rig};
use super::*;
use crate::wit::bindings::capability_types::IoEvent;

struct Ports(Vec<u16>);

impl Reserved for Ports {
    fn ports(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<u16>> + Send + '_>> {
        Box::pin(async move { self.0.clone() })
    }
}

/// Tests bind the loopback address explicitly. A wildcard bind on port 0
/// can be handed a port that another process already holds on a specific
/// address such as 127.0.0.1; the kernel then routes our loopback client
/// to that process and the listener never sees it. A specific-address bind
/// is checked against both kinds of holder.
const LOOPBACK: &str = "127.0.0.1";

fn listen_rig(kind: &str, port: u16, extra: &str) -> Rig {
    rig(
        &format!("cap.{kind}"),
        &port.to_string(),
        &format!("{kind} = {port} bind={LOOPBACK}\n{extra}"),
    )
}

async fn listening(rig: &mut Rig) -> u16 {
    match rig.event().await {
        IoEvent::Listening(done) => done.port,
        _ => panic!("not a listening event"),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_listener_accepts_and_echoes() {
    let mut rig = listen_rig("listen-tcp", 0, "");
    let listener = rig
        .net
        .listen(CapabilityKind::Tcp, Some(LOOPBACK), 0)
        .unwrap();
    let port = listening(&mut rig).await;
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let socket = match rig.event().await {
        IoEvent::Accepted(accepted) => {
            assert_eq!(accepted.listener, listener);
            accepted.socket
        }
        _ => panic!("not an accepted event"),
    };
    client.write_all(b"hi").await.unwrap();
    match rig.event().await {
        IoEvent::Data(data) => assert_eq!(data.data, b"hi"),
        _ => panic!("not data"),
    }
    rig.net.send(socket, b"yo").unwrap();
    let mut buf = [0u8; 2];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"yo");
    assert_eq!(rig.net.shared().usage.open_inbound(), 1);
    rig.net.close(listener);
    rig.net.close(socket);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn reserved_port_is_refused_even_when_granted() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    drop(taken);
    let mut rig = listen_rig("listen-tcp", port, "");
    rig.net.set_reserved(Arc::new(Ports(vec![port])));
    rig.net
        .listen(CapabilityKind::Tcp, Some(LOOPBACK), port)
        .unwrap();
    match rig.event().await {
        IoEvent::Closed(closed) => {
            assert!(matches!(closed.error, Some(Denied::ReservedPort(p)) if p == port))
        }
        _ => panic!("not closed"),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bind_restriction_limits_the_address() {
    let rig = rig("cap.listen-tcp", "0", "listen-tcp = 0 bind=127.0.0.1");
    assert!(matches!(
        rig.net.listen(CapabilityKind::Tcp, None, 0),
        Err(Denied::NotGranted(_))
    ));
    assert!(
        rig.net
            .listen(CapabilityKind::Tcp, Some("127.0.0.1"), 0)
            .is_ok()
    );
    assert!(matches!(
        rig.net.listen(CapabilityKind::Udp, Some("127.0.0.1"), 0),
        Err(Denied::NotGranted(_) | Denied::NotRequested(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn inbound_cap_drops_extra_connections() {
    let mut rig = listen_rig("listen-tcp", 0, "limit.inbound-connections = 1");
    rig.net
        .listen(CapabilityKind::Tcp, Some(LOOPBACK), 0)
        .unwrap();
    let port = listening(&mut rig).await;
    let _first = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    assert!(matches!(rig.event().await, IoEvent::Accepted(_)));
    let mut second = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), second.read(&mut buf))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    assert_eq!(rig.net.shared().usage.open_inbound(), 1);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn revoking_the_grant_closes_listener_and_connections() {
    let mut rig = listen_rig("listen-tcp", 0, "");
    let listener = rig
        .net
        .listen(CapabilityKind::Tcp, Some(LOOPBACK), 0)
        .unwrap();
    let port = listening(&mut rig).await;
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    assert!(matches!(rig.event().await, IoEvent::Accepted(_)));
    let revoked = policy("cap.listen-tcp", "0", "");
    rig.policy.send(revoked).unwrap();
    let mut closed = 0;
    while closed < 2 {
        if let IoEvent::Closed(done) = rig.event().await {
            assert!(done.error.is_some());
            closed += 1;
        }
    }
    let mut buf = [0u8; 1];
    assert!(matches!(client.read(&mut buf).await, Ok(0) | Err(_)));
    assert!(!rig.net.shared().handles.contains(listener));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn udp_listener_replies_only_to_heard_peers() {
    let mut rig = listen_rig("listen-udp", 0, "");
    let listener = rig
        .net
        .listen(CapabilityKind::Udp, Some(LOOPBACK), 0)
        .unwrap();
    let port = listening(&mut rig).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer: SocketAddr = client.local_addr().unwrap();
    assert!(
        rig.net
            .reply_to(listener, &peer.to_string(), b"no")
            .is_err()
    );
    client.send_to(b"ping", ("127.0.0.1", port)).await.unwrap();
    match rig.event().await {
        IoEvent::Datagram(datagram) => {
            assert_eq!(datagram.data, b"ping");
            assert_eq!(
                datagram.peer.parse::<SocketAddr>().unwrap().port(),
                peer.port()
            );
        }
        _ => panic!("not a datagram"),
    }
    rig.net
        .reply_to(listener, &peer.to_string(), b"pong")
        .unwrap();
    let mut buf = [0u8; 8];
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"pong");
    let stranger = "127.0.0.1:9";
    assert!(rig.net.reply_to(listener, stranger, b"x").is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn closing_the_net_releases_everything() {
    let mut rig = listen_rig("listen-tcp", 0, "");
    rig.net
        .listen(CapabilityKind::Tcp, Some(LOOPBACK), 0)
        .unwrap();
    listening(&mut rig).await;
    assert_eq!(rig.net.open_handles(), 1);
    let shared = rig.net.shared().clone();
    drop(rig.net);
    assert!(shared.handles.is_empty());
}

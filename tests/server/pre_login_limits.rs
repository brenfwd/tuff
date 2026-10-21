use crate::common;

use std::time::Duration;

use common::{TempDir, connect, ping, start_server_in};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tuff::protocol::PROTOCOL_VERSION;
use tuff::protocol::handshake::Intent;
use tuff::protocol::status::StatusRequest;

async fn closed(socket: &mut TcpStream) -> bool {
    let mut byte = [0u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await,
        Ok(Ok(0) | Err(_))
    )
}

async fn server(properties: &str) -> common::TestServer {
    let instance = TempDir::new();
    std::fs::write(instance.path().join("server.properties"), properties).unwrap();
    start_server_in(instance, "127.0.0.1:0", "http://127.0.0.1:1", Some(false)).await
}

#[tuff_core::watchdog]
#[tokio::test]
async fn per_ip_cap_drops_extra_idle_connections_and_frees_slots() {
    let server = server("pre-login-max-connections-per-ip=2\n").await;
    let mut held = vec![
        TcpStream::connect(server.addr).await.unwrap(),
        TcpStream::connect(server.addr).await.unwrap(),
    ];
    let mut third = TcpStream::connect(server.addr).await.unwrap();
    assert!(closed(&mut third).await);
    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ping(server.addr).await["version"]["protocol"],
        PROTOCOL_VERSION
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn oversized_pre_login_frame_is_rejected() {
    let server = server("").await;
    let mut socket = TcpStream::connect(server.addr).await.unwrap();
    socket.write_all(&[0x80, 0x80, 0x40]).await.unwrap();
    assert!(closed(&mut socket).await);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn repeated_status_requests_are_cut_off() {
    let server = server("").await;
    let mut conn = connect(server.addr, PROTOCOL_VERSION, Intent::Status).await;
    for _ in 0..16 {
        if conn.send(&StatusRequest).await.is_err() {
            return;
        }
    }
    let mut seen = 0;
    while let Ok(Some(_)) = conn.read_packet().await {
        seen += 1;
    }
    assert!(seen <= 4, "answered {seen} status requests");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn silent_connection_times_out_at_the_handshake_deadline() {
    let server = server("pre-login-timeout-seconds=30\n").await;
    let mut socket = TcpStream::connect(server.addr).await.unwrap();
    let start = std::time::Instant::now();
    let mut byte = [0u8; 1];
    let _ = tokio::time::timeout(Duration::from_secs(15), socket.read(&mut byte)).await;
    assert!(start.elapsed() < Duration::from_secs(12));
}

use crate::common;

use std::time::Duration;

use common::{TempDir, connect, ping, read_disconnect};
use tuff::Server;
use tuff::protocol::PROTOCOL_VERSION;
use tuff::protocol::handshake::Intent;
use tuff::protocol::login::{LoginStart, SetCompression};
use tuff_vanilla_data::packet::login;
use tuff_wire::PacketReader;

async fn listed(properties: &str, with_motd: bool) -> serde_json::Value {
    let instance = TempDir::new();
    std::fs::write(instance.path().join("server.properties"), properties).unwrap();
    let builder = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .view_distance(2);
    let server = if with_motd {
        builder.motd("Explicit").build()
    } else {
        builder.build()
    };
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let mut status = ping(addr).await;
    for _ in 0..100 {
        if status["players"]["max"] == 7 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        status = ping(addr).await;
    }
    handle.shutdown().await.unwrap();
    let _ = task.await;
    status
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_server_list_follows_server_properties() {
    let status = listed("max-players=7\nmotd=From File\n", false).await;
    assert_eq!(status["players"]["max"], 7);
    assert_eq!(status["description"]["text"], "From File");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn an_explicit_motd_beats_server_properties() {
    let status = listed("max-players=7\nmotd=From File\n", true).await;
    assert_eq!(status["players"]["max"], 7);
    assert_eq!(status["description"]["text"], "Explicit");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn status_policy_keys_shape_the_server_list() {
    let status = listed(
        "max-players=7\nhide-online-players=true\nenforce-secure-profile=true\n",
        false,
    )
    .await;
    assert!(status["players"].get("sample").is_none());
    assert_eq!(status["enforcesSecureChat"], true);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn server_port_and_ip_come_from_server_properties() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "server-ip=127.0.0.1\nserver-port=0\n",
    )
    .unwrap();
    let server = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .view_distance(2)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addrs = handle.local_addrs().await.unwrap();
    assert_eq!(addrs.len(), 1);
    assert!(addrs[0].ip().is_loopback());
    let status = ping(addrs[0]).await;
    assert!(status["version"].is_object());
    handle.shutdown().await.unwrap();
    let _ = task.await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn online_mode_comes_from_server_properties_unless_overridden() {
    for (explicit, expected) in [(None, false), (Some(true), true)] {
        let instance = TempDir::new();
        std::fs::write(
            instance.path().join("server.properties"),
            "online-mode=false\n",
        )
        .unwrap();
        let server = Server::builder()
            .instance_dir(instance.path().to_path_buf())
            .binds(vec!["127.0.0.1:0".to_string()])
            .view_distance(2)
            .maybe_online_mode(explicit)
            .build();
        let handle = server.handle();
        let task = tokio::spawn(server.run());
        handle.local_addrs().await.unwrap();
        assert_eq!(handle.online_mode(), expected);
        handle.shutdown().await.unwrap();
        let _ = task.await;
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn enable_status_false_refuses_server_list_pings() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "enable-status=false\n",
    )
    .unwrap();
    let server = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .view_distance(2)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let pinged = tokio::time::timeout(Duration::from_secs(5), tokio::spawn(ping(addr))).await;
    assert!(!matches!(pinged, Ok(Ok(_))), "status was served");
    handle.shutdown().await.unwrap();
    let _ = task.await;
}

async fn offline_server(
    properties: &str,
) -> (
    tuff::ServerHandle,
    tokio::task::JoinHandle<()>,
    std::net::SocketAddr,
    TempDir,
) {
    let instance = TempDir::new();
    std::fs::write(instance.path().join("server.properties"), properties).unwrap();
    let server = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .view_distance(2)
        .online_mode(false)
        .whitelist(false)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(async move {
        let _ = server.run().await;
    });
    let addr = handle.local_addrs().await.unwrap()[0];
    (handle, task, addr, instance)
}

async fn login_start(conn: &mut tuff::net::connection::Connection) {
    conn.send(&LoginStart {
        name: "Steve".to_string(),
        uuid: 0,
    })
    .await
    .unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn network_compression_threshold_is_announced_at_login() {
    for threshold in [64, 512] {
        let (handle, task, addr, _dir) =
            offline_server(&format!("network-compression-threshold={threshold}\n")).await;
        let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Login).await;
        login_start(&mut conn).await;
        let compression: SetCompression = conn.expect().await.unwrap();
        assert_eq!(compression.threshold, threshold);
        handle.shutdown().await.unwrap();
        let _ = task.await;
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_negative_compression_threshold_disables_compression() {
    let (handle, task, addr, _dir) = offline_server("network-compression-threshold=-1\n").await;
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Login).await;
    login_start(&mut conn).await;
    let mut first = PacketReader::new(conn.read_packet().await.unwrap().unwrap());
    assert_eq!(
        first.read_var_int().unwrap(),
        login::Clientbound::LoginFinished.id()
    );
    handle.shutdown().await.unwrap();
    let _ = task.await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn transfers_are_refused_unless_accepts_transfers_is_set() {
    let (handle, task, addr, _dir) = offline_server("").await;
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Transfer).await;
    login_start(&mut conn).await;
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(
        reason["translate"],
        "multiplayer.disconnect.transfers_disabled"
    );
    handle.shutdown().await.unwrap();
    let _ = task.await;

    let (handle, task, addr, _dir) = offline_server("accepts-transfers=true\n").await;
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Transfer).await;
    login_start(&mut conn).await;
    let _: SetCompression = conn.expect().await.unwrap();
    handle.shutdown().await.unwrap();
    let _ = task.await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn level_name_comes_from_server_properties() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "level-name=lobby\n",
    )
    .unwrap();
    let server = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .view_distance(2)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    handle.shutdown().await.unwrap();
    let _ = task.await;
    assert!(instance.path().join("lobby").is_dir());
    assert!(!instance.path().join("world").exists());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn enabling_the_code_of_conduct_needs_its_folder() {
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("server.properties"),
        "enable-code-of-conduct=true\n",
    )
    .unwrap();
    let server = Server::builder()
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .view_distance(2)
        .build();
    assert!(server.run().await.is_err());
}

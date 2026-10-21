use crate::common;

use std::time::Duration;

use common::{
    MockSessionServer, TEST_UUID, connect, encrypted_login, login, ping, plain_text,
    read_disconnect, start_server,
};
use tuff::protocol::handshake::Intent;
use tuff::protocol::login::{LoginStart, SetCompression};
use tuff::protocol::{PROTOCOL_VERSION, VERSION_NAME};
use tuff::status::Favicon;
use tuff_vanilla_data::packet::login;
use tuff_wire::PacketReader;

#[tuff_core::watchdog]
#[tokio::test]
async fn server_list_ping_reports_status_and_follows_updates() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let status = ping(server.addr).await;
    assert_eq!(status["version"]["protocol"], PROTOCOL_VERSION);
    assert_eq!(status["version"]["name"], VERSION_NAME);
    assert_eq!(status["description"]["text"], "Test MOTD");
    assert!(
        status["favicon"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );

    server.handle.set_motd("Changed MOTD").await.unwrap();
    server.handle.set_favicon(None).await.unwrap();
    server.handle.set_max_players(7).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = ping(server.addr).await;
            if status["players"]["max"] == 7 {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("status updates were not published");
    assert_eq!(status["description"]["text"], "Changed MOTD");
    assert!(status.get("favicon").is_none());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn server_list_ping_over_ipv6() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("[::1]:0", &session_url).await;
    assert!(server.addr.is_ipv6());
    assert_eq!(ping(server.addr).await["description"]["text"], "Test MOTD");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn authenticated_login_enables_compression_and_succeeds() {
    let (session, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    assert_eq!(compression.threshold, 256);
    conn.enable_compression(compression.threshold as usize);

    let mut success = PacketReader::new(conn.read_packet().await.unwrap().unwrap());
    assert_eq!(
        success.read_var_int().unwrap(),
        login::Clientbound::LoginFinished.id()
    );
    assert_eq!(
        success.read_uuid().unwrap(),
        u128::from_str_radix(TEST_UUID, 16).unwrap()
    );
    assert_eq!(success.read_string().unwrap(), "TestUser");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn login_without_session_join_fails_verification() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let reason = login(server.addr, "Impostor", None).await;
    assert_eq!(plain_text(&reason), "Failed to verify username!");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn login_with_unreachable_session_server_reports_outage() {
    let server = start_server("127.0.0.1:0", "http://127.0.0.1:1").await;

    let reason = login(server.addr, "TestUser", None).await;
    assert!(plain_text(&reason).starts_with("Authentication servers are down"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn login_with_other_protocol_version_is_rejected() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let mut conn = connect(server.addr, PROTOCOL_VERSION - 1, Intent::Login).await;
    conn.send(&LoginStart {
        name: "OldClient".into(),
        uuid: 0,
    })
    .await
    .unwrap();
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(
        reason["translate"],
        "multiplayer.disconnect.outdated_client"
    );
    assert!(reason.to_string().contains(VERSION_NAME));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn shutdown_stops_server_and_handle_reports_stopped() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    assert!(server.handle.local_addrs().await.is_err());
    assert!(server.handle.set_favicon(None::<Favicon>).await.is_err());
}

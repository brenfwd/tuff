//! Core with no features at all: it boots, answers the server list, lets an
//! authenticated player log in, and shuts down cleanly.

use crate::common;

use common::*;
use tuff::protocol::login::SetCompression;
use tuff_vanilla_data::packet::login;
use tuff_wire::PacketReader;

#[tuff_core::watchdog]
#[tokio::test]
async fn core_without_features_boots_serves_logins_and_stops() {
    let (session, session_url) = MockSessionServer::start().await;
    let server = start_server_with_features(
        TempDir::new(),
        "127.0.0.1:0",
        &session_url,
        Some(false),
        Vec::new(),
    )
    .await;

    assert_eq!(ping(server.addr).await["description"]["text"], "Test MOTD");

    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    conn.enable_compression(compression.threshold as usize);
    let mut success = PacketReader::new(conn.read_packet().await.unwrap().unwrap());
    assert_eq!(
        success.read_var_int().unwrap(),
        login::Clientbound::LoginFinished.id()
    );

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

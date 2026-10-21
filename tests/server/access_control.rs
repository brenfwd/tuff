use crate::common;

use common::{
    MockSessionServer, TEST_UUID, TempDir, encrypted_login, read_disconnect, start_server_in,
};
use serde_json::Value;
use tuff::protocol::login::SetCompression;

async fn run(server: &common::TestServer, line: &str) -> Vec<Value> {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|line| line.to_value())
        .collect()
}

fn uuid_text() -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &TEST_UUID[0..8],
        &TEST_UUID[8..12],
        &TEST_UUID[12..16],
        &TEST_UUID[16..20],
        &TEST_UUID[20..32]
    )
}

#[tuff_core::watchdog]
#[tokio::test]
async fn whitelist_is_on_by_default_and_admits_listed_players() {
    let (session, session_url) = MockSessionServer::start().await;
    let instance = TempDir::new();
    let server = start_server_in(instance, "127.0.0.1:0", &session_url, None).await;
    let properties =
        std::fs::read_to_string(server._instance.path().join("server.properties")).unwrap();
    assert!(properties.contains("white-list=true"));

    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(
        reason["translate"],
        "multiplayer.disconnect.not_whitelisted"
    );

    let feedback = run(&server, "whitelist add TestUser").await;
    assert_eq!(feedback[0]["translate"], "commands.whitelist.add.success");
    let listed = std::fs::read_to_string(server._instance.path().join("whitelist.json")).unwrap();
    assert!(listed.contains(&uuid_text()), "{listed}");
    assert!(listed.contains("\"name\": \"TestUser\""));

    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    assert_eq!(compression.threshold, 256);

    let listing = run(&server, "whitelist list").await;
    assert_eq!(listing[0]["translate"], "commands.whitelist.list");
    assert_eq!(listing[0]["with"][1]["text"], "TestUser");
    let off = run(&server, "whitelist off").await;
    assert_eq!(off[0]["translate"], "commands.whitelist.disabled");
    let again = run(&server, "whitelist off").await;
    assert_eq!(again[0]["translate"], "commands.whitelist.alreadyOff");
    assert_eq!(again[0]["color"], "red");
    let properties =
        std::fs::read_to_string(server._instance.path().join("server.properties")).unwrap();
    assert!(properties.contains("white-list=false"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn banned_players_are_told_the_reason_and_expiry() {
    let (session, session_url) = MockSessionServer::start().await;
    let instance = TempDir::new();
    std::fs::write(
        instance.path().join("banned-players.json"),
        format!(
            r#"[{{"uuid": "{}", "name": "TestUser", "created": "2024-01-01 00:00:00 +0000", "source": "Server", "expires": "2100-01-01 12:30:00 +0000", "reason": "Griefing"}}]"#,
            uuid_text()
        ),
    )
    .unwrap();
    let server = start_server_in(instance, "127.0.0.1:0", &session_url, Some(false)).await;
    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(reason["translate"], "multiplayer.disconnect.banned.reason");
    assert_eq!(reason["with"][0]["text"], "Griefing");
    assert_eq!(
        reason["extra"][0]["translate"],
        "multiplayer.disconnect.banned.expiration"
    );
    assert_eq!(
        reason["extra"][0]["with"][0]["text"],
        "2100-01-01 at 12:30:00 UTC"
    );

    let listing = run(&server, "banlist").await;
    assert_eq!(listing[0]["translate"], "commands.banlist.list");
    assert_eq!(listing[1]["translate"], "commands.banlist.entry");
    assert_eq!(listing[1]["with"][0]["text"], "TestUser");
    assert_eq!(listing[1]["with"][1]["text"], "Server");
    let pardon = run(&server, "pardon TestUser").await;
    assert_eq!(pardon[0]["translate"], "commands.pardon.success");
    let none = run(&server, "banlist players").await;
    assert_eq!(none[0]["translate"], "commands.banlist.none");
    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    assert_eq!(compression.threshold, 256);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ip_bans_and_operators_are_written_in_the_games_format() {
    let (session, session_url) = MockSessionServer::start().await;
    let instance = TempDir::new();
    let server = start_server_in(instance, "127.0.0.1:0", &session_url, Some(false)).await;
    let dir = server._instance.path().to_path_buf();
    let mut conn = encrypted_login(server.addr, "Admin", Some(&session)).await;
    let _: SetCompression = conn.expect().await.unwrap();
    drop(conn);
    let unknown = run(&server, "op Nobody").await;
    assert_eq!(unknown[0]["translate"], "argument.player.unknown");
    let banned = run(&server, "ban-ip 10.1.2.3 spam").await;
    assert_eq!(banned[0]["translate"], "commands.banip.success");
    assert_eq!(banned[0]["with"][0]["text"], "10.1.2.3");
    assert_eq!(banned[0]["with"][1]["text"], "spam");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("banned-ips.json")).unwrap())
            .unwrap();
    assert_eq!(file[0]["ip"], "10.1.2.3");
    assert_eq!(file[0]["source"], "Server");
    assert_eq!(file[0]["expires"], "forever");
    assert_eq!(file[0]["reason"], "spam");
    let invalid = run(&server, "ban-ip nobody").await;
    assert_eq!(invalid[0]["translate"], "commands.banip.invalid");
    let pardoned = run(&server, "pardon-ip 10.1.2.3").await;
    assert_eq!(pardoned[0]["translate"], "commands.pardonip.success");
    assert_eq!(
        std::fs::read_to_string(dir.join("banned-ips.json")).unwrap(),
        "[]"
    );

    let opped = run(&server, "op Admin").await;
    assert_eq!(opped[0]["translate"], "commands.op.success");
    let ops: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("ops.json")).unwrap()).unwrap();
    assert_eq!(ops[0]["name"], "Admin");
    assert_eq!(ops[0]["level"], 4);
    assert_eq!(ops[0]["bypassesPlayerLimit"], false);
    let again = run(&server, "op Admin").await;
    assert_eq!(again[0]["translate"], "commands.op.failed");
    let deopped = run(&server, "deop Admin").await;
    assert_eq!(deopped[0]["translate"], "commands.deop.success");
    assert_eq!(std::fs::read_to_string(dir.join("ops.json")).unwrap(), "[]");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn whitelist_reload_reads_the_file_again() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server_in(TempDir::new(), "127.0.0.1:0", &session_url, None).await;
    let dir = server._instance.path().to_path_buf();
    let none = run(&server, "whitelist list").await;
    assert_eq!(none[0]["translate"], "commands.whitelist.none");
    std::fs::write(
        dir.join("whitelist.json"),
        format!(r#"[{{"uuid": "{}", "name": "Edited"}}]"#, uuid_text()),
    )
    .unwrap();
    let reloaded = run(&server, "whitelist reload").await;
    assert_eq!(reloaded[0]["translate"], "commands.whitelist.reloaded");
    let listing = run(&server, "whitelist list").await;
    assert_eq!(listing[0]["with"][1]["text"], "Edited");
    let on = run(&server, "whitelist on").await;
    assert_eq!(on[0]["translate"], "commands.whitelist.alreadyOn");
}

const LOOKED_UP_UUID: &str = "853c80ef3c3749fdaa49938b674adae6";

#[tuff_core::watchdog]
#[tokio::test]
async fn names_nobody_logged_in_with_are_looked_up_in_online_mode() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("jeb_", LOOKED_UP_UUID);
    let server = start_server_in(TempDir::new(), "127.0.0.1:0", &session_url, Some(false)).await;
    let dir = server._instance.path().to_path_buf();

    let opped = run(&server, "op JEB_").await;
    assert_eq!(opped[0]["translate"], "commands.op.success");
    assert_eq!(opped[0]["with"][0]["text"], "jeb_");
    let ops: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("ops.json")).unwrap()).unwrap();
    assert_eq!(ops[0]["uuid"], "853c80ef-3c37-49fd-aa49-938b674adae6");
    assert_eq!(ops[0]["name"], "jeb_");

    let listed = run(&server, "whitelist add jeb_").await;
    assert_eq!(listed[0]["translate"], "commands.whitelist.add.success");
    assert_eq!(session.lookups(), 1, "the second command used the cache");
    let whitelist = std::fs::read_to_string(dir.join("whitelist.json")).unwrap();
    assert!(
        whitelist.contains("853c80ef-3c37-49fd-aa49-938b674adae6"),
        "{whitelist}"
    );

    let unknown = run(&server, "ban Nobody").await;
    assert_eq!(unknown[0]["translate"], "argument.player.unknown");
    assert_eq!(unknown[0]["color"], "red");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn offline_mode_names_need_no_lookup() {
    let (session, session_url) = MockSessionServer::start().await;
    session.add_profile("jeb_", LOOKED_UP_UUID);
    let instance = TempDir::new();
    let server = tuff::Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .session_server_url(&session_url)
        .profile_api_url(&session_url)
        .online_mode(false)
        .whitelist(false)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();

    let feedback = handle.run_command("op jeb_").await.unwrap();
    assert_eq!(feedback[0].to_value()["translate"], "commands.op.success");
    let ops: Value =
        serde_json::from_str(&std::fs::read_to_string(instance.path().join("ops.json")).unwrap())
            .unwrap();
    let offline = tuff::auth::offline_uuid("jeb_");
    let hex = format!("{offline:032x}");
    assert_eq!(
        ops[0]["uuid"],
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    );
    assert_eq!(session.lookups(), 0);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn logins_past_the_per_name_limit_are_refused() {
    let (session, session_url) = MockSessionServer::start().await;
    let server = start_server_in(TempDir::new(), "127.0.0.1:0", &session_url, None).await;
    run(&server, "whitelist off").await;
    run(
        &server,
        "tuff config tuff.anticheat.login.exemptLoopback false",
    )
    .await;
    run(&server, "tuff config tuff.anticheat.login.perName 2").await;
    for _ in 0..2 {
        let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
        let compression: SetCompression = conn.expect().await.unwrap();
        assert_eq!(compression.threshold, 256);
    }
    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(
        reason["text"], "Logging in too fast, please try again in a minute",
        "{reason}"
    );
    run(&server, "tuff config tuff.anticheat.login.enabled false").await;
    let mut conn = encrypted_login(server.addr, "TestUser", Some(&session)).await;
    let _: SetCompression = conn.expect().await.unwrap();
}

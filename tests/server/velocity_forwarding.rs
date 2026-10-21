use crate::common;

use bytes::Bytes;
use common::{TempDir, connect, read_disconnect};
use tuff::auth::{GameProfile, ProfileProperty, offline_uuid};
use tuff::net::connection::Connection;
use tuff::net::velocity::{self, Forwarded, Secret};
use tuff::protocol::PROTOCOL_VERSION;
use tuff::protocol::handshake::Intent;
use tuff::protocol::login::{LoginQuery, LoginQueryAnswer, LoginStart, SetCompression};
use tuff::server::Server;
use tuff::world::messages::CoordinatorMessage;
use tuff_storage::admin::PermissionLevel;
use tuff_wire::PacketReader;

const SECRET: &str = "s3cret-forwarding-key";
const UUID: u128 = 0x069a79f444e94726a5befca90e38aaf5;

struct Running {
    server: tuff::server::ServerHandle,
    addr: std::net::SocketAddr,
    _instance: TempDir,
}

async fn start(
    instance: TempDir,
    overrides: &[(&str, &str)],
    bind: &str,
) -> anyhow::Result<Running> {
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec![bind.to_string()])
        .online_mode(false)
        .whitelist(false)
        .config_overrides(
            overrides
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    match handle.local_addrs().await {
        Ok(addrs) => Ok(Running {
            server: handle,
            addr: addrs[0],
            _instance: instance,
        }),
        Err(_) => Err(task
            .await
            .unwrap()
            .expect_err("the server stopped without an error")),
    }
}

async fn start_forwarding(instance: TempDir) -> Running {
    start(
        instance,
        &[
            (velocity::ENABLED_KEY, "true"),
            (velocity::SECRET_KEY, SECRET),
        ],
        "127.0.0.1:0",
    )
    .await
    .unwrap()
}

fn forwarded(address: &str) -> Forwarded {
    Forwarded {
        version: velocity::MAX_VERSION,
        address: address.parse().unwrap(),
        profile: GameProfile {
            id: UUID,
            name: "Notch".into(),
            properties: vec![ProfileProperty {
                name: "textures".into(),
                value: "c2tpbg==".into(),
                signature: Some("c2lnbmF0dXJl".into()),
            }],
        },
    }
}

async fn begin(addr: std::net::SocketAddr) -> (Connection, LoginQuery) {
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Login).await;
    conn.send(&LoginStart {
        name: "Notch".into(),
        uuid: 0,
    })
    .await
    .unwrap();
    let query: LoginQuery = conn.expect().await.unwrap();
    assert_eq!(query.channel, velocity::CHANNEL);
    assert_eq!(query.data.as_ref(), &[velocity::MAX_VERSION]);
    (conn, query)
}

async fn answer(addr: std::net::SocketAddr, data: Option<Bytes>) -> Connection {
    let (mut conn, query) = begin(addr).await;
    conn.send(&LoginQueryAnswer {
        transaction_id: query.transaction_id,
        data,
    })
    .await
    .unwrap();
    conn
}

fn signed(address: &str) -> Bytes {
    velocity::encode(
        &Secret::new(SECRET.as_bytes().to_vec()).unwrap(),
        &forwarded(address),
    )
}

async fn assert_refused(conn: &mut Connection) {
    let reason = read_disconnect(conn).await;
    assert!(
        reason["text"].as_str().unwrap().contains("proxy"),
        "{reason}"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_signed_answer_logs_in_with_the_forwarded_identity() {
    let server = start_forwarding(TempDir::new()).await;
    let mut conn = answer(server.addr, Some(signed("203.0.113.9"))).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    conn.enable_compression(compression.threshold as usize);
    let mut r = PacketReader::new(conn.read_packet().await.unwrap().unwrap());
    r.read_var_int().unwrap();
    assert_eq!(r.read_uuid().unwrap(), UUID);
    assert_eq!(r.read_string().unwrap(), "Notch");
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_string().unwrap(), "textures");
    assert_eq!(r.read_string().unwrap(), "c2tpbg==");
    assert!(r.read_bool().unwrap());
    assert_eq!(r.read_string().unwrap(), "c2lnbmF0dXJl");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bad_missing_and_malformed_answers_are_refused() {
    let server = start_forwarding(TempDir::new()).await;

    let wrong = velocity::encode(&Secret::new(&b"other"[..]).unwrap(), &forwarded("1.2.3.4"));
    assert_refused(&mut answer(server.addr, Some(wrong)).await).await;

    assert_refused(&mut answer(server.addr, None).await).await;

    let truncated = signed("1.2.3.4").slice(..40);
    assert_refused(&mut answer(server.addr, Some(truncated)).await).await;

    let oversized = Bytes::from(vec![7u8; velocity::MAX_PAYLOAD + 1]);
    let mut conn = answer(server.addr, Some(oversized)).await;
    let closed = conn.expect::<SetCompression>().await;
    assert!(closed.is_err(), "an oversized answer must not log in");

    let (mut conn, query) = begin(server.addr).await;
    conn.send(&LoginQueryAnswer {
        transaction_id: query.transaction_id + 1,
        data: Some(signed("1.2.3.4")),
    })
    .await
    .unwrap();
    assert_refused(&mut conn).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ip_bans_apply_to_the_forwarded_address() {
    let server = start_forwarding(TempDir::new()).await;
    let banned = server
        .server
        .run_command("ban-ip 198.51.100.4")
        .await
        .unwrap();
    assert!(
        format!("{banned:?}").contains("commands.banip.success"),
        "{banned:?}"
    );
    let mut conn = answer(server.addr, Some(signed("198.51.100.4"))).await;
    let reason = read_disconnect(&mut conn).await;
    assert_eq!(
        reason["translate"],
        "multiplayer.disconnect.banned_ip.reason"
    );

    let mut conn = answer(server.addr, Some(signed("198.51.100.5"))).await;
    let _: SetCompression = conn.expect().await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn offline_mode_on_a_public_address_needs_an_opt_in_or_forwarding() {
    let refused = start(TempDir::new(), &[], "0.0.0.0:0").await;
    let error = format!(
        "{:#}",
        refused.err().expect("public offline must be refused")
    );
    assert!(error.contains("tuff.allowPublicOffline"), "{error}");

    start(
        TempDir::new(),
        &[("tuff.allowPublicOffline", "true")],
        "0.0.0.0:0",
    )
    .await
    .unwrap();
    start(
        TempDir::new(),
        &[
            (velocity::ENABLED_KEY, "true"),
            (velocity::SECRET_KEY, SECRET),
        ],
        "0.0.0.0:0",
    )
    .await
    .unwrap();
    start(TempDir::new(), &[], "127.0.0.1:0").await.unwrap();

    let missing = start(
        TempDir::new(),
        &[(velocity::ENABLED_KEY, "true")],
        "127.0.0.1:0",
    )
    .await;
    assert!(missing.is_err());
}

async fn permission(server: &Running, uuid: u128) -> PermissionLevel {
    let shared = server.server.world_shared().await.unwrap();
    let (reply, rx) = tokio::sync::oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::PermissionOf { uuid, reply })
        .await
        .unwrap();
    rx.await.unwrap()
}

fn ops_file(uuid: u128) -> TempDir {
    let instance = TempDir::new();
    let hex = format!("{uuid:032x}");
    let dashed = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    std::fs::write(
        instance.path().join("ops.json"),
        format!(
            r#"[{{"uuid": "{dashed}", "name": "Admin", "level": 4, "bypassesPlayerLimit": false}}]"#
        ),
    )
    .unwrap();
    instance
}

#[tuff_core::watchdog]
#[tokio::test]
async fn offline_names_get_operator_rights_only_when_opted_in() {
    let uuid = offline_uuid("Admin");
    let plain = start(ops_file(uuid), &[], "127.0.0.1:0").await.unwrap();
    assert_eq!(permission(&plain, uuid).await, PermissionLevel::All);

    let opted = start(
        ops_file(uuid),
        &[("tuff.offlineOps", "true")],
        "127.0.0.1:0",
    )
    .await
    .unwrap();
    assert_eq!(permission(&opted, uuid).await, PermissionLevel::Owners);

    let proxied = start(
        ops_file(UUID),
        &[
            (velocity::ENABLED_KEY, "true"),
            (velocity::SECRET_KEY, SECRET),
        ],
        "127.0.0.1:0",
    )
    .await
    .unwrap();
    assert_eq!(permission(&proxied, UUID).await, PermissionLevel::Owners);
}

async fn offline_login(addr: std::net::SocketAddr, name: &str) -> Connection {
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Login).await;
    conn.send(&LoginStart {
        name: name.into(),
        uuid: 0,
    })
    .await
    .unwrap();
    conn
}

#[tuff_core::watchdog]
#[tokio::test]
async fn duplicate_logins_can_be_refused_instead_of_kicking() {
    let server = start(
        TempDir::new(),
        &[("tuff.refuseDuplicateLogin", "true")],
        "127.0.0.1:0",
    )
    .await
    .unwrap();
    let bot = tuff_bot::Bot::connect(
        server.addr,
        tuff_bot::BotConfig {
            view_distance: 2,
            ..tuff_bot::BotConfig::new("Steve")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    let mut second = offline_login(server.addr, "Steve").await;
    let reason = read_disconnect(&mut second).await;
    assert!(
        reason["text"]
            .as_str()
            .unwrap()
            .contains("already logged in"),
        "{reason}"
    );
    drop(bot);
}

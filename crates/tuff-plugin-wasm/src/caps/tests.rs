use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;

use super::address::Class;
use super::resolve::{self, Lookup, Resolver};
use super::*;

fn policy(requests: &[(&str, &str)], grants: &str) -> Policy {
    let requests = requests
        .iter()
        .flat_map(|(key, value)| Request::parse_line(key, value).unwrap().unwrap())
        .collect();
    let grants = Grants::parse(grants).unwrap();
    Policy::new(requests, grants.get("p"))
}

fn addrs(list: &[&str]) -> Vec<SocketAddr> {
    list.iter()
        .map(|ip| SocketAddr::new(ip.parse().unwrap(), 1))
        .collect()
}

/// Answers each lookup with the next queued list, like a name whose
/// records change between lookups.
struct Answers(Mutex<VecDeque<Vec<SocketAddr>>>);

impl Answers {
    fn new(answers: &[&[&str]]) -> Self {
        Self(Mutex::new(answers.iter().map(|list| addrs(list)).collect()))
    }
}

impl Resolver for Answers {
    fn lookup<'a>(&'a self, _name: &'a str, _port: u16) -> Lookup<'a> {
        let next = self.0.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(async move { Ok(next) })
    }
}

struct Silent;

impl Resolver for Silent {
    fn lookup<'a>(&'a self, _name: &'a str, _port: u16) -> Lookup<'a> {
        Box::pin(std::future::pending())
    }
}

#[tuff_core::watchdog]
#[test]
fn nothing_is_allowed_without_both_request_and_grant() {
    let url = "https://discord.com/api/webhooks/1";
    let ungranted = policy(&[("cap.http", "discord.com/api/webhooks/")], "");
    assert_eq!(
        ungranted.check_url(Kind::Http, url).unwrap_err(),
        Denied::NotGranted(Kind::Http, "https://discord.com:443/api/webhooks/1".into())
    );
    let unrequested = policy(&[], "[p]\nhttp = discord.com");
    assert!(matches!(
        unrequested.check_url(Kind::Http, url),
        Err(Denied::NotRequested(..))
    ));
    let both = policy(
        &[("cap.http", "discord.com/api/webhooks/")],
        "[p]\nhttp = discord.com",
    );
    assert!(both.check_url(Kind::Http, url).is_ok());
    assert!(matches!(
        both.check_url(Kind::Http, "https://discord.com/api/users/@me"),
        Err(Denied::NotRequested(..))
    ));
    assert!(matches!(
        both.check_url(Kind::Http, "http://discord.com/api/webhooks/1"),
        Err(Denied::NotRequested(..))
    ));
    assert!(matches!(
        both.check_url(Kind::Http, "https://discord.com:8443/api/webhooks/1"),
        Err(Denied::NotRequested(..))
    ));
    assert!(matches!(
        both.check_url(Kind::WebSocket, "wss://discord.com/api/webhooks/1"),
        Err(Denied::NotRequested(..))
    ));
    assert!(matches!(
        both.check_url(Kind::Http, "https://discord.com/api/webhooks/../users"),
        Err(Denied::Malformed(_))
    ));
}

#[tuff_core::watchdog]
#[test]
fn grants_narrower_than_the_request_win() {
    let p = policy(
        &[("cap.http", "*.example.com")],
        "[p]\nhttp = api.example.com/v2/",
    );
    assert!(
        p.check_url(Kind::Http, "https://api.example.com/v2/x")
            .is_ok()
    );
    assert!(matches!(
        p.check_url(Kind::Http, "https://api.example.com/v1/x"),
        Err(Denied::NotGranted(..))
    ));
    assert!(matches!(
        p.check_url(Kind::Http, "https://other.example.com/v2/x"),
        Err(Denied::NotGranted(..))
    ));
}

#[tuff_core::watchdog]
#[test]
fn public_answers_pass_and_private_ones_are_dropped() {
    let p = policy(
        &[("cap.tcp", "db.example.com:5432")],
        "[p]\ntcp = db.example.com:5432",
    );
    let approval = p.check_socket(Kind::Tcp, "db.example.com", 5432).unwrap();
    assert_eq!(
        approval
            .admit(addrs(&["93.184.216.34", "2606:2800::1"]))
            .unwrap(),
        vec![
            "93.184.216.34:5432".parse().unwrap(),
            "[2606:2800::1]:5432".parse().unwrap()
        ]
    );
    assert_eq!(
        approval
            .admit(addrs(&["10.0.0.1", "93.184.216.34", "::1"]))
            .unwrap(),
        vec!["93.184.216.34:5432".parse::<SocketAddr>().unwrap()]
    );
    for private in [
        "127.0.0.1",
        "10.1.1.1",
        "192.168.0.10",
        "169.254.1.1",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "64:ff9b::a00:1",
        "2002:7f00:1::1",
        "0.0.0.0",
        "::",
    ] {
        let err = approval.admit(addrs(&[private])).unwrap_err();
        assert!(matches!(err, Denied::Address { .. }), "{private}: {err:?}");
    }
    assert!(matches!(
        approval.admit(addrs(&[])),
        Err(Denied::Unresolved(..))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn rebinding_names_cannot_move_onto_private_addresses() {
    let p = policy(
        &[("cap.http", "rebind.example")],
        "[p]\nhttp = rebind.example",
    );
    let approval = p.check_url(Kind::Http, "https://rebind.example/").unwrap();
    let dns = Answers::new(&[
        &["93.184.216.34"],
        &["127.0.0.1"],
        &["169.254.169.254", "10.0.0.1"],
    ]);
    let timeout = Duration::from_secs(5);
    let first = resolve::resolve(&approval, &dns, timeout).await.unwrap();
    assert_eq!(
        first,
        vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]
    );
    let second = resolve::resolve(&approval, &dns, timeout)
        .await
        .unwrap_err();
    assert_eq!(
        second,
        Denied::Address {
            target: "https://rebind.example:443/".into(),
            ip: "127.0.0.1".parse().unwrap(),
            class: Class::Private("loopback"),
        }
    );
    let third = resolve::resolve(&approval, &dns, timeout)
        .await
        .unwrap_err();
    assert!(matches!(
        third,
        Denied::Address {
            class: Class::Metadata,
            ..
        }
    ));
    let silent = resolve::resolve(&approval, &Silent, Duration::from_millis(20)).await;
    assert!(matches!(silent, Err(Denied::Unresolved(..))));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ip_literals_skip_dns_but_not_the_address_checks() {
    let p = policy(&[("cap.http", "*")], "[p]\nhttp = *");
    for url in [
        "https://127.0.0.1/",
        "https://[::1]/",
        "https://[::ffff:10.0.0.1]/",
        "https://169.254.169.254/",
    ] {
        let approval = p.check_url(Kind::Http, url).unwrap();
        let denied = resolve::resolve(&approval, &Silent, Duration::from_secs(1)).await;
        assert!(
            matches!(denied, Err(Denied::Address { .. })),
            "{url}: {denied:?}"
        );
    }
    let approval = p.check_url(Kind::Http, "https://1.1.1.1/").unwrap();
    assert_eq!(
        resolve::resolve(&approval, &Silent, Duration::from_secs(1)).await,
        Ok(vec!["1.1.1.1:443".parse().unwrap()])
    );
}

#[tuff_core::watchdog]
#[test]
fn private_destinations_need_an_explicit_grant() {
    let requests = [(
        "cap.tcp",
        "db.lan:5432, [::1]:6379, 10.0.0.9:3306, 169.254.169.254:80",
    )];
    let flagged = policy(
        &requests,
        "[p]\ntcp = db.lan:5432 private, [::1]:6379, [10.0.0.0/8]:3306, [169.254.0.0/16]:80",
    );
    let db = flagged.check_socket(Kind::Tcp, "db.lan", 5432).unwrap();
    assert!(db.admit(addrs(&["10.0.0.5"])).is_ok());
    assert!(db.admit(addrs(&["::1"])).is_ok());
    assert!(matches!(
        db.admit(addrs(&["169.254.169.254"])),
        Err(Denied::Address {
            class: Class::Metadata,
            ..
        })
    ));
    assert!(matches!(
        db.admit(addrs(&["224.0.0.1"])),
        Err(Denied::Address { .. })
    ));
    let redis = flagged.check_socket(Kind::Tcp, "::1", 6379).unwrap();
    assert!(redis.admit(addrs(&["::1"])).is_ok());
    let mysql = flagged.check_socket(Kind::Tcp, "10.0.0.9", 3306).unwrap();
    assert!(mysql.admit(addrs(&["10.0.0.9"])).is_ok());
    let metadata = flagged
        .check_socket(Kind::Tcp, "169.254.169.254", 80)
        .unwrap();
    assert!(matches!(
        metadata.admit(addrs(&["169.254.169.254"])),
        Err(Denied::Address {
            class: Class::Metadata,
            ..
        })
    ));

    let plain = policy(&requests, "[p]\ntcp = db.lan:5432");
    let db = plain.check_socket(Kind::Tcp, "db.lan", 5432).unwrap();
    assert!(db.admit(addrs(&["10.0.0.5"])).is_err());

    let exact = policy(&requests, "[p]\ntcp = 169.254.169.254:80");
    let metadata = exact
        .check_socket(Kind::Tcp, "169.254.169.254", 80)
        .unwrap();
    assert!(metadata.admit(addrs(&["169.254.169.254"])).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn grants_without_private_never_reach_private_addresses() {
    let p = policy(
        &[
            ("cap.tcp", "redis.lan:6379 private -- caches balances"),
            ("cap.http", "api.example.com"),
        ],
        "[p]\ntcp = redis.lan:6379\nhttp = api.example.com",
    );
    assert!(p.check_url(Kind::Http, "https://api.example.com/").is_ok());
    let redis = p.check_socket(Kind::Tcp, "redis.lan", 6379).unwrap();
    assert!(redis.admit(addrs(&["10.0.0.2"])).is_err());
    assert_eq!(
        p.report(),
        ["requests tcp redis.lan:6379 private, non-public addresses not granted"]
    );
}

#[tuff_core::watchdog]
#[test]
fn the_report_names_missing_and_useless_grants() {
    let p = policy(
        &[
            (
                "cap.http",
                "discord.com/api/webhooks/ -- posts chat to Discord",
            ),
            ("cap.websocket", "gateway.discord.gg"),
            ("cap.files", "16 MiB"),
        ],
        "[p]\nwebsocket = gateway.discord.gg\nudp = voice.example:5000\nfiles = 8 MiB",
    );
    assert_eq!(
        p.report(),
        [
            "requests http https://discord.com/api/webhooks/ (posts chat to Discord), not granted",
            "grant udp voice.example:5000 is not requested, so it has no effect",
        ]
    );
    assert!(
        policy(&[("cap.route", "/map")], "[p]\nroute = /")
            .report()
            .is_empty()
    );
}

#[tuff_core::watchdog]
#[test]
fn file_quota_comes_from_the_owner() {
    let files = [("cap.files", "16 MiB")];
    assert_eq!(
        policy(&files, "[p]\nfiles = 8 MiB").files_quota(),
        Some(8 << 20)
    );
    assert_eq!(
        policy(&files, "[p]\nfiles = 16 MiB").files_quota(),
        Some(16 << 20)
    );
    assert_eq!(policy(&files, "[p]\nhttp = a.example").files_quota(), None);
    assert_eq!(policy(&[], "[p]\nfiles = 8 MiB").files_quota(), None);
    assert_eq!(policy(&files, "").files_quota(), None);
}

#[tuff_core::watchdog]
#[test]
fn routes_stay_inside_their_granted_prefix() {
    let p = policy(&[("cap.route", "/map, /vote")], "[p]\nroute = /map");
    assert!(p.check_route("/map").is_ok());
    assert!(p.check_route("/map/tiles/0/0.png?v=2").is_ok());
    assert!(matches!(
        p.check_route("/vote"),
        Err(Denied::NotGranted(..))
    ));
    assert!(matches!(
        p.check_route("/mapx"),
        Err(Denied::NotRequested(..))
    ));
    assert!(matches!(
        p.check_route("/map/../admin"),
        Err(Denied::Malformed(_))
    ));
    assert!(matches!(
        p.check_route("/map/%2e%2e/admin"),
        Err(Denied::Malformed(_))
    ));
}

#[tuff_core::watchdog]
#[test]
fn listeners_need_the_exact_port_and_never_get_server_ports() {
    let any = "::".parse().unwrap();
    let local = "127.0.0.1".parse().unwrap();
    let p = policy(
        &[
            ("cap.listen-udp", "19132"),
            ("cap.listen-tcp", "8192, 25565"),
        ],
        "[p]\nlisten-udp = 19132\nlisten-tcp = 8192 bind=127.0.0.1, 25565",
    );
    assert!(
        p.check_listen(Kind::ListenUdp, any, 19132, &[25565])
            .is_ok()
    );
    assert!(
        p.check_listen(Kind::ListenTcp, local, 8192, &[25565])
            .is_ok()
    );
    assert!(matches!(
        p.check_listen(Kind::ListenTcp, any, 8192, &[25565]),
        Err(Denied::NotGranted(..))
    ));
    assert!(matches!(
        p.check_listen(Kind::ListenTcp, any, 19132, &[25565]),
        Err(Denied::NotRequested(..))
    ));
    assert_eq!(
        p.check_listen(Kind::ListenTcp, any, 25565, &[25565]),
        Err(Denied::Reserved(25565))
    );
    assert!(p.check_listen(Kind::Tcp, any, 8192, &[]).is_err());
}

#[tuff_core::watchdog]
#[test]
fn dimension_entries_gate_by_id_and_namespace() {
    let p = policy(
        &[("cap.dimension", "mine:nether, other:*")],
        "[p]\ndimension = mine:nether\ndimension = other:*",
    );
    assert!(p.check_dimension("mine:nether").is_ok());
    assert!(p.check_dimension("other:anything").is_ok());
    assert!(p.check_dimension("mine:end").is_err());
    let arenas = policy(
        &[("cap.dimension", "game:arena_*")],
        "[p]\ndimension = game:arena_*",
    );
    assert!(arenas.check_dimension("game:arena_7").is_ok());
    assert!(arenas.check_dimension("game:arena_").is_ok());
    assert!(arenas.check_dimension("game:lobby").is_err());
    let narrower = policy(
        &[("cap.dimension", "game:*")],
        "[p]\ndimension = game:arena_*",
    );
    assert!(narrower.check_dimension("game:arena_1").is_ok());
    assert!(narrower.check_dimension("game:lobby").is_err());
    let none = policy(&[("cap.dimension", "mine:nether")], "");
    assert!(none.check_dimension("mine:nether").is_err());
    for bad in ["nether", "A:b", "a:", "a:b c", "a:*b", "a:b**"] {
        assert!(
            Request::parse_line("cap.dimension", bad).unwrap().is_err(),
            "{bad}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn content_entries_gate_by_namespace() {
    let p = policy(&[("cap.content", "mine, gems")], "[p]\ncontent = mine");
    assert!(p.check_content("mine:ruby").is_ok());
    assert!(p.check_content("mine").is_ok());
    assert!(p.check_content("gems:ruby").is_err());
    assert!(p.check_content("minecraft:stone").is_err());
    for bad in ["mine:ruby", "Mine", "a b", "*"] {
        assert!(
            Request::parse_line("cap.content", bad).unwrap().is_err(),
            "{bad}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn manifest_requests_parse_with_reasons() {
    let requests = Request::parse_line("cap.http", "a.example, b.example/x -- metrics export")
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].reason, "metrics export");
    assert!(Request::parse_line("description", "x").is_none());
    assert!(Request::parse_line("cap.smtp", "x").unwrap().is_err());
    assert!(Request::parse_line("cap.tcp", "noport").unwrap().is_err());
}

#[tuff_core::watchdog]
#[test]
fn capability_wit_fits_the_supported_canonical_abi_subset() {
    let schema = crate::wit::schema::Schema::parse(
        include_str!("../../wit/capabilities.wit"),
        "capabilities",
    )
    .unwrap();
    for import in [
        "granted",
        "fetch",
        "ws-connect",
        "tcp-connect",
        "listen",
        "respond",
        "close",
        "read",
        "write",
        "list-dir",
        "usage",
    ] {
        assert!(schema.import(import).is_some(), "{import}");
    }
    assert!(schema.export("on-io").is_some());
}

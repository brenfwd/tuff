use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use super::*;
use crate::caps::Grants;
use crate::caps::policy::Request;
use crate::caps::resolve::{Lookup, SystemResolver};
use crate::wit::bindings::capability_types::{Header, HttpDone, IoEvent};

type Reply = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

async fn server(reply: Reply) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let reply = reply.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let path = text.split(' ').nth(1).unwrap_or("/").to_owned();
                let out = reply(&path);
                if out.is_empty() {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let _ = stream.write_all(&out).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

fn ok(body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn redirect(to: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn gzipped(data: &[u8]) -> Vec<u8> {
    let body = gzip(data);
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(&body);
    out
}

struct Fixed(Vec<SocketAddr>);

impl Resolver for Fixed {
    fn lookup<'a>(&'a self, _name: &'a str, _port: u16) -> Lookup<'a> {
        let found = self.0.clone();
        Box::pin(async move { Ok(found) })
    }
}

pub(super) struct Rig {
    pub(super) net: Net,
    policy: watch::Sender<Arc<Policy>>,
    events: mpsc::UnboundedReceiver<Delivery>,
}

fn policy(request: &str, grant: &str) -> Arc<Policy> {
    let requests = Request::parse_line("cap.http", request).unwrap().unwrap();
    let grants = Grants::parse(&format!("[p]\n{grant}")).unwrap();
    Arc::new(Policy::new(requests, grants.get("p")))
}

pub(super) fn rig_with(request: &str, grant: &str, resolver: Arc<dyn Resolver>) -> Rig {
    let (policy, watcher) = watch::channel(policy(request, grant));
    let (queue, events) = mpsc::unbounded_channel();
    Rig {
        net: Net::new(watcher, resolver, "p", 1, queue),
        policy,
        events,
    }
}

fn rig(addr: SocketAddr, limits: &str) -> Rig {
    let host = format!("http://127.0.0.1:{}", addr.port());
    rig_with(
        &host,
        &format!("http = {host} private\n{limits}"),
        Arc::new(SystemResolver),
    )
}

pub(super) fn get(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
        timeout_ms: None,
        decompress: false,
        max_redirects: 0,
    }
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", addr.port())
}

impl Rig {
    pub(super) async fn done(&mut self) -> HttpDone {
        let delivery = tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("an event")
            .unwrap();
        match delivery.event {
            IoEvent::Http(done) => done,
            _ => panic!("not an http event"),
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn fetches_from_a_granted_private_address() {
    let addr = server(Arc::new(|_| ok(b"hello"))).await;
    let mut rig = rig(addr, "");
    let handle = rig.net.fetch(get(url(addr, "/x?y=1"))).unwrap();
    let done = rig.done().await;
    assert_eq!(done.request, handle);
    let response = done.outcome.unwrap();
    assert_eq!(
        (response.status, response.body.as_slice()),
        (200, &b"hello"[..])
    );
    assert!(response.headers.iter().any(|h| h.name == "content-length"));
    assert_eq!(rig.net.open_handles(), 0);
    assert_eq!(rig.net.shared().usage.open_outbound(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn refuses_hosts_paths_and_bad_requests_up_front() {
    let addr = server(Arc::new(|_| ok(b""))).await;
    let host = format!("http://127.0.0.1:{}/api/", addr.port());
    let rig = rig_with(
        &host,
        &format!("http = {host} private"),
        Arc::new(SystemResolver),
    );
    assert!(matches!(
        rig.net.fetch(get("http://example.org/".into())),
        Err(Denied::NotRequested(_))
    ));
    assert!(matches!(
        rig.net.fetch(get(url(addr, "/other"))),
        Err(Denied::NotRequested(_))
    ));
    assert!(rig.net.fetch(get(url(addr, "/api/ok"))).is_ok());
    let mut bad = get(url(addr, "/api/ok"));
    bad.headers.push(Header {
        name: "Host".into(),
        value: b"evil".to_vec(),
    });
    assert!(matches!(rig.net.fetch(bad), Err(Denied::Malformed(_))));
    let mut bad = get(url(addr, "/api/ok"));
    bad.method = "CONNECT".into();
    assert!(matches!(rig.net.fetch(bad), Err(Denied::Malformed(_))));

    let ungranted = rig_with(&host, "", Arc::new(SystemResolver));
    assert!(matches!(
        ungranted.net.fetch(get(url(addr, "/api/ok"))),
        Err(Denied::NotGranted(_))
    ));
    assert!(
        !ungranted
            .net
            .granted(CapabilityKind::Http, &url(addr, "/api/ok"))
    );
    assert!(rig.net.granted(CapabilityKind::Http, &url(addr, "/api/ok")));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn private_addresses_need_the_private_flag() {
    let addr = server(Arc::new(|_| ok(b"in"))).await;
    let port = addr.port();
    let resolver = || Arc::new(Fixed(vec![addr]));
    let name = format!("http://internal.test:{port}");
    let mut plain = rig_with(&name, &format!("http = {name}"), resolver());
    plain.net.fetch(get(format!("{name}/"))).unwrap();
    assert!(matches!(
        plain.done().await.outcome,
        Err(Denied::Address(_))
    ));

    let mut private = rig_with(&name, &format!("http = {name} private"), resolver());
    private.net.fetch(get(format!("{name}/"))).unwrap();
    assert_eq!(private.done().await.outcome.unwrap().body, b"in");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn redirects_are_followed_and_rechecked() {
    let addr = server(Arc::new(|path| match path {
        "/a" => redirect("/b"),
        "/b" => ok(b"final"),
        "/meta" => redirect("http://169.254.169.254/latest/"),
        "/rebind" => redirect("http://evil.test/"),
        _ => redirect("/loop"),
    }))
    .await;
    let host = format!("http://127.0.0.1:{}", addr.port());
    let grant = format!("http = {host} private, http://evil.test:80");
    let mut rig = rig_with(
        &format!("{host}, http://evil.test:80"),
        &grant,
        Arc::new(Fixed(vec!["169.254.169.254:80".parse().unwrap()])),
    );
    let mut request = get(url(addr, "/a"));
    request.max_redirects = 3;
    rig.net.fetch(request.clone()).unwrap();
    let response = rig.done().await.outcome.unwrap();
    assert_eq!(response.body, b"final");
    assert!(response.url.ends_with("/b"));

    request.max_redirects = 0;
    rig.net.fetch(request.clone()).unwrap();
    assert_eq!(rig.done().await.outcome.unwrap().status, 302);

    request.max_redirects = 3;
    request.url = url(addr, "/meta");
    rig.net.fetch(request.clone()).unwrap();
    assert!(matches!(
        rig.done().await.outcome,
        Err(Denied::NotRequested(_))
    ));

    request.url = url(addr, "/rebind");
    rig.net.fetch(request.clone()).unwrap();
    assert!(matches!(rig.done().await.outcome, Err(Denied::Address(_))));

    request.url = url(addr, "/loop");
    request.max_redirects = 2;
    rig.net.fetch(request).unwrap();
    assert!(matches!(rig.done().await.outcome, Err(Denied::Failed(_))));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn oversized_and_inflated_bodies_are_refused() {
    let addr = server(Arc::new(|path| match path {
        "/big" => ok(&[b'x'; 4096]),
        "/bomb" => gzipped(&vec![0u8; 1 << 20]),
        _ => gzipped(b"small"),
    }))
    .await;
    let mut rig = rig(addr, "limit.response-bytes = 2048");
    rig.net.fetch(get(url(addr, "/big"))).unwrap();
    assert!(matches!(rig.done().await.outcome, Err(Denied::Quota(_))));

    let mut request = get(url(addr, "/bomb"));
    request.decompress = true;
    rig.net.fetch(request).unwrap();
    assert!(matches!(rig.done().await.outcome, Err(Denied::Quota(_))));

    let mut request = get(url(addr, "/small"));
    request.decompress = true;
    rig.net.fetch(request).unwrap();
    assert_eq!(rig.done().await.outcome.unwrap().body, b"small");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn slow_servers_time_out() {
    let addr = server(Arc::new(|_| Vec::new())).await;
    let mut rig = rig(addr, "");
    let mut request = get(url(addr, "/"));
    request.timeout_ms = Some(150);
    rig.net.fetch(request).unwrap();
    assert!(matches!(rig.done().await.outcome, Err(Denied::TimedOut)));
    assert_eq!(rig.net.shared().usage.open_outbound(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn rates_and_connections_are_limited() {
    let addr = server(Arc::new(|_| Vec::new())).await;
    let rig = rig(addr, "limit.requests-per-minute = 2\nlimit.connections = 8");
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    assert!(matches!(
        rig.net.fetch(get(url(addr, "/"))),
        Err(Denied::Quota(_))
    ));

    let rig = self::rig(addr, "limit.connections = 1");
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    assert!(matches!(
        rig.net.fetch(get(url(addr, "/"))),
        Err(Denied::Quota(_))
    ));

    let rig = self::rig(addr, "limit.request-bytes = 4");
    let mut request = get(url(addr, "/"));
    request.method = "POST".into();
    request.body = vec![0; 5];
    assert!(matches!(rig.net.fetch(request), Err(Denied::Quota(_))));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn cancel_close_revoke_and_unload_stop_requests() {
    let addr = server(Arc::new(|_| Vec::new())).await;
    let mut rig = rig(addr, "");
    let handle = rig.net.fetch(get(url(addr, "/"))).unwrap();
    rig.net.close(handle);
    assert_eq!(rig.net.open_handles(), 0);

    rig.net.fetch(get(url(addr, "/"))).unwrap();
    rig.policy.send_replace(policy("example.org", ""));
    let done = rig.done().await;
    assert!(matches!(done.outcome, Err(Denied::NotRequested(_))));
    assert_eq!(rig.net.open_handles(), 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn unloading_cancels_in_flight_requests() {
    let addr = server(Arc::new(|_| Vec::new())).await;
    let mut rig = rig(addr, "");
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    let usage = rig.net.shared().usage.clone();
    assert_eq!(usage.open_outbound(), 1);
    drop(rig.net);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(usage.open_outbound(), 0);
    assert!(rig.events.try_recv().is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn pending_events_pause_delivery() {
    let addr = server(Arc::new(|_| ok(b"x"))).await;
    let mut rig = rig(addr, "limit.pending-events = 1");
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), rig.events.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(rig.events.try_recv().is_err());
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), rig.events.recv())
            .await
            .unwrap()
            .is_some()
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_guest_fetches_and_hears_the_result() {
    use crate::sandbox::{Limits, Sandbox, engine};
    use crate::wit::testing::{self, Data, RET, export, ok_unit};
    use crate::wit::value::ToValue;

    let addr = server(Arc::new(|_| ok(b"hi"))).await;
    let mut rig = rig(addr, "");
    let mut data = Data::new();
    let fetch = data.call("net.fetch", &[get(url(addr, "/")).to_value()]);
    let said = data.string("got");
    let body = format!(
        "(func {} {}) (func {} {fetch} (if (i32.ne (i32.load8_u (i32.const {RET})) (i32.const 0)) (then unreachable))) \
         (func {} (call $broadcast (i32.const {}) (i32.const {}) (i32.const {RET}))) {}",
        export("register"),
        ok_unit(),
        export("enable"),
        export("on-io"),
        said.0,
        said.1,
        data.wat()
    );
    let bytes = wat::parse_str(testing::module(&body)).unwrap();
    let mut sandbox = Sandbox::new(&engine(), &bytes, "p", Limits::default()).unwrap();
    sandbox.attach_net(rig.net);
    sandbox.register().unwrap();
    sandbox.enable().unwrap();
    let delivery = tokio::time::timeout(Duration::from_secs(10), rig.events.recv())
        .await
        .unwrap()
        .unwrap();
    let effects = sandbox.io(&delivery.event).unwrap();
    assert_eq!(effects.broadcasts, ["got"]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn limit_changes_apply_to_a_running_plugin() {
    let addr = server(Arc::new(|_| ok(b"x"))).await;
    let mut rig = rig(addr, "");
    rig.net.fetch(get(url(addr, "/"))).unwrap();
    rig.done().await.outcome.unwrap();
    let tightened = policy(
        &format!("http://127.0.0.1:{}", addr.port()),
        &format!(
            "http = http://127.0.0.1:{} private\nlimit.requests-per-minute = 1\n",
            addr.port()
        ),
    );
    rig.policy.send_replace(tightened);
    assert!(rig.net.fetch(get(url(addr, "/"))).is_ok());
    rig.done().await.outcome.ok();
    assert!(matches!(
        rig.net.fetch(get(url(addr, "/"))),
        Err(Denied::Quota(_))
    ));
    assert_eq!(rig.net.shared().limits().requests_per_minute, 1);
}

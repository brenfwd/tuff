use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn local() -> HttpConfig {
    HttpConfig {
        hosts: vec!["127.0.0.1".into()],
        port: 0,
        ..HttpConfig::default()
    }
}

async fn raw(addr: SocketAddr, request: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

fn get(path: &str, extra: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n{extra}\r\n")
}

#[tuff_core::watchdog]
#[test]
fn replacing_a_different_handler_is_reported() {
    let route = |text: &'static str| Route {
        handler: handler(move |_| async move { Response::text(text) }),
        options: RouteOptions::default(),
    };
    let (a, b) = (route("a"), route("b"));
    assert!(replacement_warning("/x", None, &a).is_none());
    assert!(replacement_warning("/x", Some(&a), &a.clone()).is_none());
    let message = replacement_warning("/x", Some(&a), &b).expect("a warning");
    assert!(message.contains("/x"), "{message}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_server_keeping_routes_and_listeners() {
    let (http, server) = start(None);
    tokio::spawn(tuff_supervise::restarting(
        "the http server",
        server,
        |server| Box::pin(server.run()),
    ));
    http.register_route("/keep", handler(|_| async { Response::text("kept") }))
        .await;
    let addrs = http.bind(MAIN_LISTENER, local()).await.unwrap();
    http.tx.send(Command::Panic).await.unwrap();
    assert_eq!(http.routes(MAIN_LISTENER).await, vec!["/keep".to_owned()]);
    let reply = raw(addrs[0], &get("/keep", "")).await;
    assert!(reply.ends_with("kept"), "{reply}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn free_registration_refuses_overlaps_and_unregisters_only_its_own() {
    let http = spawn();
    let a = handler(|_| async { Response::text("a") });
    let b = handler(|_| async { Response::text("b") });
    let opts = RouteOptions::default();
    http.register_route("/admin", a.clone()).await;
    for taken in ["/admin", "/admin/x", "/"] {
        let got = http
            .register_route_free(MAIN_LISTENER, taken, opts, b.clone())
            .await;
        assert_eq!(got, taken == "/", "{taken}");
    }
    assert!(
        http.register_route_free(MAIN_LISTENER, "/map", opts, b.clone())
            .await
    );
    assert!(
        !http
            .register_route_free(MAIN_LISTENER, "/map/deep", opts, b.clone())
            .await
    );
    http.unregister_route_if(MAIN_LISTENER, "/map", &a).await;
    assert!(
        http.routes(MAIN_LISTENER)
            .await
            .contains(&"/map".to_owned())
    );
    http.unregister_route_if(MAIN_LISTENER, "/map", &b).await;
    assert!(
        !http
            .routes(MAIN_LISTENER)
            .await
            .contains(&"/map".to_owned())
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn route_is_tracks_replacement() {
    let http = spawn();
    let a = handler(|_| async { Response::text("a") });
    let b = handler(|_| async { Response::text("b") });
    http.register_route("/x", a.clone()).await;
    assert!(http.route_is(MAIN_LISTENER, "/x", &a).await);
    http.register_route("/x", b.clone()).await;
    assert!(!http.route_is(MAIN_LISTENER, "/x", &a).await);
    assert!(http.route_is(MAIN_LISTENER, "/x", &b).await);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn plugin_routes_match_longest_prefix() {
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    http.register_route(
        "/a",
        handler(|r| async move { Response::text(format!("a:{}", r.rest)) }),
    )
    .await;
    http.register_route("/a/b", handler(|_| async { Response::text("ab") }))
        .await;
    assert!(raw(addr, &get("/a/x", "")).await.ends_with("a:/x"));
    assert!(raw(addr, &get("/a/b", "")).await.ends_with("ab"));
    assert!(raw(addr, &get("/ab", "")).await.starts_with("HTTP/1.1 404"));
    http.unregister_route(MAIN_LISTENER, "/a").await;
    assert!(
        raw(addr, &get("/a/x", ""))
            .await
            .starts_with("HTTP/1.1 404")
    );
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn meter_sees_every_served_response() {
    #[derive(Default)]
    struct Seen(std::sync::Mutex<Vec<(String, u16, usize)>>);
    impl Meter for Seen {
        fn served(&self, _: &Method, path: &str, resp: &Response) {
            let len = resp.body.bytes().map_or(0, |b| b.len());
            self.0
                .lock()
                .unwrap()
                .push((path.into(), resp.status.as_u16(), len));
        }
    }
    let seen = Arc::new(Seen::default());
    let (http, mut server) = start(Some(seen.clone()));
    tokio::spawn(async move { server.run().await });
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    http.register_route("/a", handler(|_| async { Response::text("hello") }))
        .await;
    assert!(raw(addr, &get("/a", "")).await.ends_with("hello"));
    assert!(
        raw(addr, &get("/missing", ""))
            .await
            .starts_with("HTTP/1.1 404")
    );
    let seen = seen.0.lock().unwrap().clone();
    assert_eq!(seen[0], ("/a".into(), 200, 5));
    assert_eq!(seen[1].0, "/missing");
    assert_eq!(seen[1].1, 404);
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn ranges_and_etags() {
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    http.register_route(
        "/f",
        handler(|r| async move {
            Response::conditional(&r, "text/plain", Bytes::from_static(b"0123456789"))
        }),
    )
    .await;
    let full = raw(addr, &get("/f", "")).await;
    assert!(full.ends_with("0123456789"));
    let etag = full
        .lines()
        .find_map(|l| l.strip_prefix("etag: "))
        .unwrap()
        .to_string();
    let part = raw(addr, &get("/f", "Range: bytes=2-4\r\n")).await;
    assert!(part.starts_with("HTTP/1.1 206"));
    assert!(part.contains("content-range: bytes 2-4/10"));
    assert!(part.ends_with("234"));
    let suffix = raw(addr, &get("/f", "Range: bytes=-3\r\n")).await;
    assert!(suffix.ends_with("789"));
    let bad = raw(addr, &get("/f", "Range: bytes=20-\r\n")).await;
    assert!(bad.starts_with("HTTP/1.1 416"));
    let cached = raw(addr, &get("/f", &format!("If-None-Match: {etag}\r\n"))).await;
    assert!(cached.starts_with("HTTP/1.1 304"));
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn oversized_bodies_are_refused() {
    let http = spawn();
    let mut cfg = local();
    cfg.max_body = 4;
    let addr = http.bind(MAIN_LISTENER, cfg).await.unwrap()[0];
    http.register_route(
        "/",
        handler(|r| async move { Response::text(r.body.len().to_string()) }),
    )
    .await;
    let req =
        "POST /x HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: 10\r\n\r\n0123456789";
    assert!(raw(addr, req).await.starts_with("HTTP/1.1 413"));
    let ok = "POST /x HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: 3\r\n\r\nabc";
    assert!(raw(addr, ok).await.ends_with("3"));
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn rebind_keeps_routes_and_swaps_listener() {
    let http = spawn();
    let old = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    http.register_route("/", handler(|_| async { Response::text("hi") }))
        .await;
    assert!(raw(old, &get("/", "")).await.ends_with("hi"));
    let new = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    assert_ne!(old, new);
    assert!(raw(new, &get("/", "")).await.ends_with("hi"));
    tokio::task::yield_now().await;
    assert!(TcpStream::connect(old).await.is_err());
    assert_eq!(http.addrs(MAIN_LISTENER).await, vec![new]);
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn named_listeners_have_separate_routes_and_ipv6_binds() {
    let http = spawn();
    let a = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    let admin = http.bind("admin", local()).await.unwrap()[0];
    http.register_route_on("admin", "/", handler(|_| async { Response::text("adm") }))
        .await;
    assert!(raw(admin, &get("/", "")).await.ends_with("adm"));
    assert!(raw(a, &get("/", "")).await.starts_with("HTTP/1.1 404"));
    let v6 = HttpConfig {
        hosts: vec!["::1".into()],
        port: 0,
        ..HttpConfig::default()
    };
    if let Ok(addrs) = http.bind("v6", v6).await {
        http.register_route_on("v6", "/", handler(|_| async { Response::text("six") }))
            .await;
        assert!(raw(addrs[0], &get("/", "")).await.ends_with("six"));
    }
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn connection_cap_and_forwarded_headers() {
    let http = spawn();
    let mut cfg = local();
    cfg.max_connections = 1;
    let addr = http.bind(MAIN_LISTENER, cfg).await.unwrap()[0];
    http.register_route(
        "/",
        handler(|r| async move { Response::text(r.host.unwrap_or_default()) }),
    )
    .await;
    let _held = TcpStream::connect(addr).await.unwrap();
    tokio::task::yield_now().await;
    let mut second = TcpStream::connect(addr).await.unwrap();
    let _ = second.write_all(get("/", "").as_bytes()).await;
    let mut out = Vec::new();
    let _ = second.read_to_end(&mut out).await;
    assert!(out.is_empty());

    let mut cfg = local();
    cfg.trust_forwarded = true;
    let addr = http.bind("fwd", cfg).await.unwrap()[0];
    http.register_route_on(
        "fwd",
        "/",
        handler(|r| async move { Response::text(r.host.unwrap_or_default()) }),
    )
    .await;
    assert!(
        raw(addr, &get("/", "X-Forwarded-Host: example.org\r\n"))
            .await
            .ends_with("example.org")
    );
    http.shutdown().await;
}

async fn read_until(s: &mut TcpStream, needle: &str) -> String {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = s.read(&mut buf).await.unwrap();
        assert!(
            n > 0,
            "closed before {needle:?}: {}",
            String::from_utf8_lossy(&out)
        );
        out.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&out);
        if text.contains(needle) {
            return text.into_owned();
        }
    }
}

fn body_of(resp: &str) -> &str {
    resp.split_once("\r\n\r\n").map_or("", |(_, b)| b)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn stalled_readers_are_dropped_and_producers_feel_back_pressure() {
    let http = spawn();
    let mut cfg = local();
    cfg.stream_idle_timeout = Duration::from_millis(300);
    let addr = http.bind(MAIN_LISTENER, cfg).await.unwrap()[0];
    let (done_tx, done_rx) = mpsc::channel::<usize>(1);
    http.register_route(
        "/s",
        handler(move |_| {
            let done = done_tx.clone();
            async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    let mut sent = 0;
                    while tx.send(Bytes::from(vec![b'x'; 64 * 1024])).await.is_ok() {
                        sent += 1;
                    }
                    let _ = done.send(sent).await;
                });
                Response::stream(StatusCode::OK, rx)
            }
        }),
    )
    .await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(get("/s", "").as_bytes()).await.unwrap();
    let mut done_rx = done_rx;
    let sent = done_rx.recv().await.unwrap();
    assert!(
        sent < 4096,
        "producer ran ahead of a stalled reader: {sent} chunks"
    );
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn files_stream_with_ranges_and_etags() {
    let dir = tuff_storage::test_dir::TestDir::new();
    let path = dir.path().join("f.bin");
    let data: Vec<u8> = (0..200_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    std::fs::write(&path, &data).unwrap();
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    let served = path.clone();
    http.register_route(
        "/f",
        handler(move |r| {
            let p = served.clone();
            async move { Response::file(&r, "application/octet-stream", p, "f").await }
        }),
    )
    .await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(get("/f", "").as_bytes()).await.unwrap();
    let mut full = Vec::new();
    s.read_to_end(&mut full).await.unwrap();
    let split = full.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&full[..split]).into_owned();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-length: 200000"), "{head}");
    assert_eq!(&full[split + 4..], &data[..]);
    let etag = head
        .lines()
        .find_map(|l| l.strip_prefix("etag: "))
        .unwrap()
        .to_string();
    let part = raw(addr, &get("/f", "Range: bytes=70000-70003\r\n")).await;
    assert!(part.starts_with("HTTP/1.1 206"), "{part}");
    assert!(part.contains("content-range: bytes 70000-70003/200000"));
    let want: String = data[70000..70004].iter().map(|&b| b as char).collect();
    assert_eq!(body_of(&part), want);
    let stale = raw(
        addr,
        &get("/f", "Range: bytes=0-3\r\nIf-Range: \"other\"\r\n"),
    )
    .await;
    assert!(stale.starts_with("HTTP/1.1 200"));
    let cached = raw(addr, &get("/f", &format!("If-None-Match: {etag}\r\n"))).await;
    assert!(cached.starts_with("HTTP/1.1 304"));
    let bad = raw(addr, &get("/f", "Range: bytes=300000-\r\n")).await;
    assert!(bad.starts_with("HTTP/1.1 416"));
    std::fs::remove_file(&path).unwrap();
    assert!(raw(addr, &get("/f", "")).await.starts_with("HTTP/1.1 404"));
    http.shutdown().await;
}

async fn counting_route(http: &HttpHandle, prefix: &str, options: RouteOptions) {
    http.register_route_with(
        MAIN_LISTENER,
        prefix,
        options,
        handler(|mut r| async move {
            let Some(mut body) = r.take_body_stream() else {
                return Response::text(r.body.len().to_string());
            };
            let mut n = 0;
            loop {
                match body.chunk().await {
                    Ok(Some(c)) => n += c.len(),
                    Ok(None) => return Response::text(n.to_string()),
                    Err(e) => return e.response(),
                }
            }
        }),
    )
    .await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn upload_caps_hold_against_lying_and_chunked_bodies() {
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    let stream = RouteOptions {
        max_body: Some(1000),
        stream: true,
    };
    counting_route(&http, "/up", stream).await;
    counting_route(
        &http,
        "/small",
        RouteOptions {
            max_body: Some(8),
            stream: false,
        },
    )
    .await;
    let post = |path: &str, headers: &str, body: &str| {
        format!("POST {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n{headers}\r\n{body}")
    };
    let declared = raw(addr, &post("/up", "Content-Length: 5000\r\n", "")).await;
    assert!(declared.starts_with("HTTP/1.1 413"), "{declared}");
    let mut chunked = String::new();
    for _ in 0..4 {
        chunked.push_str(&format!("1f4\r\n{}\r\n", "a".repeat(500)));
    }
    chunked.push_str("0\r\n\r\n");
    let over = raw(
        addr,
        &post("/up", "Transfer-Encoding: chunked\r\n", &chunked),
    )
    .await;
    assert!(over.starts_with("HTTP/1.1 413"), "{over}");
    let lying = raw(
        addr,
        &post("/up", "Content-Length: 10\r\n", &"b".repeat(3000)),
    )
    .await;
    assert!(lying.starts_with("HTTP/1.1 200"), "{lying}");
    assert_eq!(body_of(&lying).lines().next(), Some("10"));
    let fits = raw(
        addr,
        &post("/up", "Content-Length: 900\r\n", &"c".repeat(900)),
    )
    .await;
    assert!(fits.ends_with("900"), "{fits}");
    let small = raw(
        addr,
        &post(
            "/small",
            "Transfer-Encoding: chunked\r\n",
            "14\r\naaaaaaaaaaaaaaaaaaaa\r\n0\r\n\r\n",
        ),
    )
    .await;
    assert!(small.starts_with("HTTP/1.1 413"), "{small}");
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn idle_and_slow_uploads_time_out() {
    let http = spawn();
    let mut cfg = local();
    cfg.stream_idle_timeout = Duration::from_millis(200);
    cfg.min_upload_rate = MinRate {
        bytes_per_sec: 1_000_000,
        window: Duration::from_millis(100),
    };
    let addr = http.bind(MAIN_LISTENER, cfg).await.unwrap()[0];
    counting_route(
        &http,
        "/up",
        RouteOptions {
            max_body: Some(1 << 20),
            stream: true,
        },
    )
    .await;
    let head = "POST /up HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: 100\r\n\r\n";
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(b"0123456789").await.unwrap();
    let idle = read_until(&mut s, "\r\n\r\n").await;
    assert!(idle.starts_with("HTTP/1.1 408"), "{idle}");

    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(b"0123456789").await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    s.write_all(b"0123456789").await.unwrap();
    let slow = read_until(&mut s, "\r\n\r\n").await;
    assert!(slow.starts_with("HTTP/1.1 408"), "{slow}");
    http.shutdown().await;
}

struct HeaderGuard;

impl Guard for HeaderGuard {
    fn check<'a>(&'a self, req: &'a Request) -> GuardFuture<'a> {
        Box::pin(async move {
            match req.header("x-ok") {
                Some(who) => Ok(Some(Arc::new(who.to_string()) as AuthContext)),
                None => Err(Response::status(StatusCode::UNAUTHORIZED)),
            }
        })
    }

    fn finish(&self, _method: &Method, _path: &str, resp: &mut Response) {
        resp.set_header("x-guarded", "1");
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn guards_run_before_every_route_under_their_prefix_only() {
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    let other = http.bind("other", local()).await.unwrap()[0];
    let who = handler(|r| async move {
        Response::text(r.auth::<String>().map_or("nobody".into(), |s| (*s).clone()))
    });
    http.register_route("/admin/a", who.clone()).await;
    http.register_route("/public", who.clone()).await;
    http.register_route_on("other", "/admin/a", who).await;
    http.set_guard(MAIN_LISTENER, "/admin", Some(Arc::new(HeaderGuard)))
        .await;
    let denied = raw(addr, &get("/admin/a", "")).await;
    assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
    assert!(denied.contains("x-guarded: 1"));
    let missing = raw(addr, &get("/admin/nothing", "")).await;
    assert!(missing.starts_with("HTTP/1.1 401"), "{missing}");
    let missing = raw(addr, &get("/admin/nothing", "x-ok: alice\r\n")).await;
    assert!(missing.starts_with("HTTP/1.1 404") && missing.contains("x-guarded: 1"));
    let ok = raw(addr, &get("/admin/a", "x-ok: alice\r\n")).await;
    assert!(ok.ends_with("alice"), "{ok}");
    let public = raw(addr, &get("/public", "")).await;
    assert!(public.ends_with("nobody") && !public.contains("x-guarded"));
    let elsewhere = raw(other, &get("/admin/a", "")).await;
    assert!(elsewhere.ends_with("nobody"), "{elsewhere}");
    let adminish = raw(addr, &get("/administrator", "")).await;
    assert!(!adminish.contains("x-guarded"));
    http.set_guard(MAIN_LISTENER, "/admin", None).await;
    assert!(raw(addr, &get("/admin/a", "")).await.ends_with("nobody"));
    assert_eq!(http.routes(MAIN_LISTENER).await, ["/admin/a", "/public"]);
    http.unbind("other").await;
    tokio::task::yield_now().await;
    assert!(http.addrs("other").await.is_empty());
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn event_streams_frame_events_and_ping() {
    let http = spawn();
    let addr = http.bind(MAIN_LISTENER, local()).await.unwrap()[0];
    let (events_tx, events_rx) = mpsc::channel::<mpsc::Sender<Event>>(1);
    http.register_route(
        "/events",
        handler(move |_| {
            let events_tx = events_tx.clone();
            async move {
                let (tx, rx) = mpsc::channel(8);
                let _ = events_tx.send(tx).await;
                Response::event_stream_with(rx, Duration::from_millis(100))
            }
        }),
    )
    .await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(get("/events", "").as_bytes()).await.unwrap();
    let mut events_rx = events_rx;
    let tx = events_rx.recv().await.unwrap();
    tx.send(Event::new("logs", "one\ntwo").with_id("5"))
        .await
        .unwrap();
    let text = read_until(&mut s, "data: two\n\n").await;
    assert!(text.contains("content-type: text/event-stream"), "{text}");
    assert!(text.contains("cache-control: no-store"), "{text}");
    assert!(
        text.contains("id: 5\nevent: logs\ndata: one\ndata: two\n\n"),
        "{text}"
    );
    read_until(&mut s, ": ping\n\n").await;
    drop(tx);
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).await.unwrap();
    http.shutdown().await;
}

#[tuff_core::watchdog]
#[test]
fn query_params_decode() {
    let mut r = Request::new(Method::GET, "/x", "127.0.0.1:1".parse().unwrap());
    r.query = Some("a=1&line=say+hi%21&bad=%zz&empty".into());
    assert_eq!(r.query_param("a").as_deref(), Some("1"));
    assert_eq!(r.query_param("line").as_deref(), Some("say hi!"));
    assert_eq!(r.query_param("bad"), None);
    assert_eq!(r.query_param("empty").as_deref(), Some(""));
    assert_eq!(r.query_param("none"), None);
}

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::header::HeaderMap;
pub use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
pub use hyper::{Method, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, lookup_host};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;

mod body;
mod cookie;
mod sse;

pub use body::{Body, BodyError, BodyStream, FILE_CHUNK, FILE_QUEUE, MinRate, RouteOptions};
pub use cookie::{Cookie, CookieError, SameSite};
pub use sse::{Event, PING_INTERVAL};

pub const MAIN_LISTENER: &str = "main";

#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub hosts: Vec<String>,
    pub port: u16,
    pub max_body: usize,
    pub max_headers: usize,
    pub header_timeout: Duration,
    /// Time to produce a response head; streaming routes are exempt.
    pub request_timeout: Duration,
    /// Longest pause in a streamed body (either direction) or in writing
    /// to a peer that stopped reading.
    pub stream_idle_timeout: Duration,
    pub min_upload_rate: MinRate,
    pub max_connections: usize,
    pub trust_forwarded: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            hosts: vec!["0.0.0.0".into(), "::".into()],
            port: 8080,
            max_body: 1 << 20,
            max_headers: 64,
            header_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            stream_idle_timeout: Duration::from_secs(60),
            min_upload_rate: MinRate {
                bytes_per_sec: 16 * 1024,
                window: Duration::from_secs(30),
            },
            max_connections: 256,
            trust_forwarded: false,
        }
    }
}

/// What a [`Guard`] attached to a request; its type belongs to whoever
/// installed the guard.
pub type AuthContext = Arc<dyn Any + Send + Sync>;

pub struct Request {
    pub method: Method,
    pub path: String,
    pub rest: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    /// The collected body; empty for streaming routes (see
    /// [`Request::take_body_stream`]).
    pub body: Bytes,
    pub remote: SocketAddr,
    pub host: Option<String>,
    body_stream: Option<BodyStream>,
    auth: Option<AuthContext>,
}

impl Request {
    pub fn new(method: Method, path: impl Into<String>, remote: SocketAddr) -> Self {
        let path = path.into();
        Self {
            method,
            rest: path.clone(),
            path,
            query: None,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            remote,
            host: None,
            body_stream: None,
            auth: None,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The body of a streaming route, once.
    pub fn take_body_stream(&mut self) -> Option<BodyStream> {
        self.body_stream.take()
    }

    /// What the guard attached, if it is a `T`.
    pub fn auth<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.auth.clone()?.downcast::<T>().ok()
    }

    pub fn set_auth(&mut self, auth: Option<AuthContext>) {
        self.auth = auth;
    }

    /// The value of query parameter `name`, percent-decoded.
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query
            .as_deref()?
            .split('&')
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(k)? == name).then(|| percent_decode(v))
            })
            .next()
            .flatten()
    }
}

/// Decodes `%XX` escapes and `+` as space; `None` for a bad escape or
/// invalid UTF-8.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

pub struct Response {
    pub status: StatusCode,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Body,
}

impl Response {
    pub fn new(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Full(body.into()),
        }
    }

    pub fn status(status: StatusCode) -> Self {
        Self::new(status, Bytes::new())
    }

    pub fn text(body: impl Into<String>) -> Self {
        Self::new(StatusCode::OK, Bytes::from(body.into()))
            .header("content-type", "text/plain; charset=utf-8")
    }

    pub fn json(status: StatusCode, value: &serde_json::Value) -> Self {
        Self::new(status, Bytes::from(value.to_string())).header("content-type", "application/json")
    }

    /// A body produced chunk by chunk; the channel bound is the
    /// back-pressure, and dropping every sender ends the body.
    pub fn stream(status: StatusCode, chunks: mpsc::Receiver<Bytes>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Stream(chunks),
        }
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        if let Ok(v) = HeaderValue::from_str(value) {
            self.headers.push((HeaderName::from_static(name), v));
        }
        self
    }

    /// Sets `name` to `value`, replacing any value it had.
    pub fn set_header(&mut self, name: &'static str, value: &str) {
        self.headers.retain(|(k, _)| k != name);
        if let Ok(v) = HeaderValue::from_str(value) {
            self.headers.push((HeaderName::from_static(name), v));
        }
    }

    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.to_str().ok())
    }

    /// Serves immutable bytes with a strong ETag, conditional requests and a
    /// single byte range.
    pub fn conditional(req: &Request, content_type: &str, data: Bytes) -> Self {
        let etag = format!("\"{:016x}\"", fnv(&data));
        let len = data.len() as u64;
        match conditional_head(req, &etag, len) {
            Conditional::Done(resp) => resp,
            Conditional::Range(start, end) => base(
                Response::new(
                    StatusCode::PARTIAL_CONTENT,
                    data.slice(start as usize..=end as usize),
                )
                .header("content-type", content_type)
                .header("content-range", &format!("bytes {start}-{end}/{len}")),
                &etag,
            ),
            Conditional::Whole => base(
                Response::new(StatusCode::OK, data).header("content-type", content_type),
                &etag,
            ),
        }
    }

    /// Serves a file from disk the way [`Response::conditional`] serves
    /// bytes, streaming it in [`FILE_CHUNK`] pieces. The ETag hashes the
    /// size, modification time and `id`, so replacing the file changes it.
    pub async fn file(req: &Request, content_type: &str, path: PathBuf, id: &str) -> Self {
        let meta = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || std::fs::metadata(path)).await
        };
        let Ok(Ok(meta)) = meta else {
            return Response::status(StatusCode::NOT_FOUND);
        };
        if !meta.is_file() {
            return Response::status(StatusCode::NOT_FOUND);
        }
        let len = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let mut key = Vec::new();
        key.extend_from_slice(&len.to_le_bytes());
        key.extend_from_slice(&mtime.to_le_bytes());
        key.extend_from_slice(id.as_bytes());
        let etag = format!("\"{:016x}\"", fnv(&key));
        let file = |range| Body::File {
            path: path.clone(),
            range,
            len,
        };
        let mut resp = match conditional_head(req, &etag, len) {
            Conditional::Done(resp) => return resp,
            Conditional::Range(start, end) => {
                let mut r = Response::status(StatusCode::PARTIAL_CONTENT)
                    .header("content-range", &format!("bytes {start}-{end}/{len}"))
                    .header("content-length", &(end + 1 - start).to_string());
                r.body = file(Some((start, end)));
                r
            }
            Conditional::Whole => {
                let mut r =
                    Response::status(StatusCode::OK).header("content-length", &len.to_string());
                r.body = file(None);
                r
            }
        };
        resp = resp.header("content-type", content_type);
        base(resp, &etag)
    }
}

fn base(r: Response, etag: &str) -> Response {
    r.header("etag", etag)
        .header("accept-ranges", "bytes")
        .header("cache-control", "no-cache")
}

enum Conditional {
    Done(Response),
    Range(u64, u64),
    Whole,
}

fn conditional_head(req: &Request, etag: &str, len: u64) -> Conditional {
    if req.method != Method::GET && req.method != Method::HEAD {
        return Conditional::Done(
            Response::status(StatusCode::METHOD_NOT_ALLOWED).header("allow", "GET, HEAD"),
        );
    }
    let matched = req.header("if-none-match").is_some_and(|inm| {
        inm.split(',').any(|t| {
            let t = t.trim();
            t == "*" || t.trim_start_matches("W/") == etag
        })
    });
    if matched {
        return Conditional::Done(base(Response::status(StatusCode::NOT_MODIFIED), etag));
    }
    let range = req
        .header("range")
        .filter(|_| req.header("if-range").map(|v| v == etag).unwrap_or(true));
    match range.map(|spec| parse_range(spec, len)) {
        Some(Some(Some((start, end)))) => Conditional::Range(start, end),
        Some(Some(None)) => Conditional::Done(base(
            Response::status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("content-range", &format!("bytes */{len}")),
            etag,
        )),
        _ => Conditional::Whole,
    }
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

/// `None`: ignore the header; `Some(None)`: unsatisfiable.
fn parse_range(spec: &str, len: u64) -> Option<Option<(u64, u64)>> {
    let spec = spec.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 || len == 0 {
            return Some(None);
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() {
            u64::MAX
        } else {
            b.parse().ok()?
        };
        if end < start {
            return None;
        }
        (start, end.min(len.saturating_sub(1)))
    };
    if start >= len {
        return Some(None);
    }
    Some(Some((start, end)))
}

pub type Handler =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

pub type GuardFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<AuthContext>, Response>> + Send + 'a>>;

/// Runs before route resolution for every request under the path prefix
/// it is installed on, whether or not a route matches. The request it
/// sees has no body yet.
pub trait Guard: Send + Sync + 'static {
    /// Rejects the request with a response, or passes it on with an
    /// optional context the handler reads through [`Request::auth`].
    fn check<'a>(&'a self, req: &'a Request) -> GuardFuture<'a>;

    /// Adjusts every response under the prefix, rejections and 404s
    /// included.
    fn finish(&self, _method: &Method, _path: &str, _resp: &mut Response) {}
}

#[derive(Clone)]
struct Route {
    handler: Handler,
    options: RouteOptions,
}

/// What to log when a route replaces a different handler at the same
/// prefix, for instance a built-in registered after a plugin's alias.
fn replacement_warning(prefix: &str, existing: Option<&Route>, new: &Route) -> Option<String> {
    existing.filter(|old| !Arc::ptr_eq(&old.handler, &new.handler))?;
    Some(format!(
        "the route {prefix} was already taken; the newer registration replaces it"
    ))
}

enum Command {
    #[cfg(test)]
    Panic,
    Bind {
        listener: String,
        config: HttpConfig,
        reply: oneshot::Sender<Result<Vec<SocketAddr>>>,
    },
    Unbind {
        listener: String,
    },
    Register {
        listener: String,
        prefix: String,
        route: Route,
    },
    Unregister {
        listener: String,
        prefix: String,
    },
    RegisterFree {
        listener: String,
        prefix: String,
        route: Route,
        reply: oneshot::Sender<bool>,
    },
    UnregisterIf {
        listener: String,
        prefix: String,
        handler: Handler,
    },
    Owns {
        listener: String,
        prefix: String,
        handler: Handler,
        reply: oneshot::Sender<bool>,
    },
    SetGuard {
        listener: String,
        prefix: String,
        guard: Option<Arc<dyn Guard>>,
    },
    Resolve {
        listener: String,
        path: String,
        reply: oneshot::Sender<Resolved>,
    },
    Routes {
        listener: String,
        reply: oneshot::Sender<Vec<String>>,
    },
    Addrs {
        listener: String,
        reply: oneshot::Sender<Vec<SocketAddr>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

struct Resolved {
    guard: Option<Arc<dyn Guard>>,
    route: Option<(Route, String)>,
    meter: Option<HttpMeter>,
}

/// Observes every response the HTTP actor serves (request counters).
pub trait Meter: Send + Sync {
    fn served(&self, method: &Method, path: &str, resp: &Response);
}

type HttpMeter = Arc<dyn Meter>;

#[derive(Clone)]
pub struct HttpHandle {
    tx: mpsc::Sender<Command>,
}

impl HttpHandle {
    /// A handle with nobody receiving: every call fails or answers empty,
    /// like `/dev/null`. For test helpers that build a `Shared` outside a
    /// tokio runtime (no reactor to spawn the real actor on) and never
    /// exercise HTTP.
    pub fn detached() -> HttpHandle {
        HttpHandle {
            tx: mpsc::channel(1).0,
        }
    }

    pub async fn bind(&self, listener: &str, config: HttpConfig) -> Result<Vec<SocketAddr>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Bind {
                listener: listener.into(),
                config,
                reply,
            })
            .await
            .context("http actor stopped")?;
        rx.await.context("http actor stopped")?
    }

    /// Stops accepting on `listener`; its routes and guards stay, and
    /// connections already open finish.
    pub async fn unbind(&self, listener: &str) {
        let _ = self
            .tx
            .send(Command::Unbind {
                listener: listener.into(),
            })
            .await;
    }

    pub async fn register_route(&self, prefix: &str, handler: Handler) {
        self.register_route_on(MAIN_LISTENER, prefix, handler).await
    }

    pub async fn register_route_on(&self, listener: &str, prefix: &str, handler: Handler) {
        self.register_route_with(listener, prefix, RouteOptions::default(), handler)
            .await
    }

    pub async fn register_route_with(
        &self,
        listener: &str,
        prefix: &str,
        options: RouteOptions,
        handler: Handler,
    ) {
        let _ = self
            .tx
            .send(Command::Register {
                listener: listener.into(),
                prefix: normalize(prefix),
                route: Route { handler, options },
            })
            .await;
    }

    /// Registers `handler` at `prefix` only if no route is at, above (other
    /// than the root route) or below it; returns whether it did.
    pub async fn register_route_free(
        &self,
        listener: &str,
        prefix: &str,
        options: RouteOptions,
        handler: Handler,
    ) -> bool {
        let (reply, rx) = oneshot::channel();
        let sent = self
            .tx
            .send(Command::RegisterFree {
                listener: listener.into(),
                prefix: normalize(prefix),
                route: Route { handler, options },
                reply,
            })
            .await;
        sent.is_ok() && rx.await.unwrap_or(false)
    }

    /// Removes the route at `prefix` only if it is still `handler`.
    pub async fn unregister_route_if(&self, listener: &str, prefix: &str, handler: &Handler) {
        let _ = self
            .tx
            .send(Command::UnregisterIf {
                listener: listener.into(),
                prefix: normalize(prefix),
                handler: handler.clone(),
            })
            .await;
    }

    /// Whether the route at `prefix` is still `handler`.
    pub async fn route_is(&self, listener: &str, prefix: &str, handler: &Handler) -> bool {
        let (reply, rx) = oneshot::channel();
        let sent = self
            .tx
            .send(Command::Owns {
                listener: listener.into(),
                prefix: normalize(prefix),
                handler: handler.clone(),
                reply,
            })
            .await;
        sent.is_ok() && rx.await.unwrap_or(false)
    }

    pub async fn unregister_route(&self, listener: &str, prefix: &str) {
        let _ = self
            .tx
            .send(Command::Unregister {
                listener: listener.into(),
                prefix: normalize(prefix),
            })
            .await;
    }

    /// Installs (or with `None` removes) the guard for paths under
    /// `prefix` on `listener`; the longest matching prefix's guard runs.
    pub async fn set_guard(&self, listener: &str, prefix: &str, guard: Option<Arc<dyn Guard>>) {
        let _ = self
            .tx
            .send(Command::SetGuard {
                listener: listener.into(),
                prefix: normalize(prefix),
                guard,
            })
            .await;
    }

    /// The route prefixes registered on `listener`, sorted.
    pub async fn routes(&self, listener: &str) -> Vec<String> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::Routes {
                listener: listener.into(),
                reply,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn addrs(&self, listener: &str) -> Vec<SocketAddr> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::Addrs {
                listener: listener.into(),
                reply,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::Shutdown { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

fn normalize(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        format!("/{trimmed}")
    }
}

struct Bound {
    addrs: Vec<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Bound {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[derive(Default)]
struct Listener {
    routes: HashMap<String, Route>,
    guards: HashMap<String, Arc<dyn Guard>>,
    bound: Option<Bound>,
}

/// Spawns the HTTP actor on the current tokio runtime.
pub fn spawn() -> HttpHandle {
    let (handle, mut server) = start(None);
    tokio::spawn(async move { server.run().await });
    handle
}

/// Creates the HTTP actor without spawning it, so the caller can run it
/// under its own supervision; `meter` sees every served response. The
/// server keeps its listeners and routes in itself, so running it again
/// after a panic resumes where it stopped.
pub fn start(meter: Option<Arc<dyn Meter>>) -> (HttpHandle, Server) {
    let (tx, rx) = mpsc::channel(256);
    let server = Server {
        rx,
        tx: tx.clone(),
        meter,
        listeners: HashMap::new(),
    };
    (HttpHandle { tx }, server)
}

/// The HTTP actor's state; see [`start`].
pub struct Server {
    rx: mpsc::Receiver<Command>,
    tx: mpsc::Sender<Command>,
    meter: Option<HttpMeter>,
    listeners: HashMap<String, Listener>,
}

impl Server {
    /// Serves commands until a shutdown or every handle is gone.
    pub async fn run(&mut self) {
        let Self {
            rx,
            tx,
            meter,
            listeners,
        } = self;
        run(rx, tx, meter, listeners).await;
    }
}

async fn run(
    rx: &mut mpsc::Receiver<Command>,
    tx: &mpsc::Sender<Command>,
    meter: &Option<HttpMeter>,
    listeners: &mut HashMap<String, Listener>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            #[cfg(test)]
            Command::Panic => panic!("test panic"),
            Command::Bind {
                listener,
                config,
                reply,
            } => {
                let result = bind(&listener, config, tx).await;
                let out = result.map(|bound| {
                    let addrs = bound.addrs.clone();
                    let shown: Vec<String> = addrs.iter().map(ToString::to_string).collect();
                    log::info!("http listener {listener} listening on {}", shown.join(", "));
                    listeners.entry(listener).or_default().bound = Some(bound);
                    addrs
                });
                let _ = reply.send(out);
            }
            Command::Unbind { listener } => {
                if let Some(l) = listeners.get_mut(&listener) {
                    if l.bound.is_some() {
                        log::info!("http listener {listener} stopped");
                    }
                    l.bound = None;
                }
            }
            Command::Register {
                listener,
                prefix,
                route,
            } => {
                let routes = &mut listeners.entry(listener).or_default().routes;
                if let Some(message) = replacement_warning(&prefix, routes.get(&prefix), &route) {
                    log::warn!("{message}");
                }
                routes.insert(prefix, route);
            }
            Command::Unregister { listener, prefix } => {
                if let Some(l) = listeners.get_mut(&listener) {
                    l.routes.remove(&prefix);
                }
            }
            Command::RegisterFree {
                listener,
                prefix,
                route,
                reply,
            } => {
                let l = listeners.entry(listener).or_default();
                let free = !l.routes.keys().any(|taken| routes_overlap(taken, &prefix));
                if free {
                    l.routes.insert(prefix, route);
                }
                let _ = reply.send(free);
            }
            Command::Owns {
                listener,
                prefix,
                handler,
                reply,
            } => {
                let owns = listeners
                    .get(&listener)
                    .and_then(|l| l.routes.get(&prefix))
                    .is_some_and(|route| Arc::ptr_eq(&route.handler, &handler));
                let _ = reply.send(owns);
            }
            Command::UnregisterIf {
                listener,
                prefix,
                handler,
            } => {
                if let Some(l) = listeners.get_mut(&listener)
                    && l.routes
                        .get(&prefix)
                        .is_some_and(|route| Arc::ptr_eq(&route.handler, &handler))
                {
                    l.routes.remove(&prefix);
                }
            }
            Command::SetGuard {
                listener,
                prefix,
                guard,
            } => {
                let l = listeners.entry(listener).or_default();
                match guard {
                    Some(g) => {
                        l.guards.insert(prefix, g);
                    }
                    None => {
                        l.guards.remove(&prefix);
                    }
                }
            }
            Command::Resolve {
                listener,
                path,
                reply,
            } => {
                let found = match listeners.get(&listener) {
                    Some(l) => Resolved {
                        guard: longest(&l.guards, &path).map(|(_, g)| g.clone()),
                        route: longest(&l.routes, &path).map(|(prefix, r)| {
                            let rest = if prefix == "/" {
                                path.clone()
                            } else {
                                path[prefix.len()..].to_string()
                            };
                            (r.clone(), rest)
                        }),
                        meter: meter.clone(),
                    },
                    None => Resolved {
                        guard: None,
                        route: None,
                        meter: meter.clone(),
                    },
                };
                let _ = reply.send(found);
            }
            Command::Routes { listener, reply } => {
                let mut out: Vec<String> = listeners
                    .get(&listener)
                    .map(|l| l.routes.keys().cloned().collect())
                    .unwrap_or_default();
                out.sort();
                let _ = reply.send(out);
            }
            Command::Addrs { listener, reply } => {
                let addrs = listeners
                    .get(&listener)
                    .and_then(|l| l.bound.as_ref())
                    .map(|b| b.addrs.clone())
                    .unwrap_or_default();
                let _ = reply.send(addrs);
            }
            Command::Shutdown { reply } => {
                listeners.clear();
                let _ = reply.send(());
                return;
            }
        }
    }
}

/// The entry with the longest prefix that `path` is equal to or under.
pub fn routes_overlap(a: &str, b: &str) -> bool {
    let under = |outer: &str, inner: &str| {
        outer != "/"
            && inner.starts_with(outer)
            && matches!(inner.as_bytes().get(outer.len()), None | Some(b'/'))
    };
    a == b || under(a, b) || under(b, a)
}

fn longest<'a, T>(table: &'a HashMap<String, T>, path: &str) -> Option<(&'a str, &'a T)> {
    let mut best: Option<(&String, &T)> = None;
    for (prefix, value) in table {
        let matches = prefix == "/"
            || path == prefix
            || (path.starts_with(prefix.as_str())
                && path.as_bytes().get(prefix.len()) == Some(&b'/'));
        if matches && best.is_none_or(|(p, _)| prefix.len() > p.len()) {
            best = Some((prefix, value));
        }
    }
    best.map(|(p, v)| (p.as_str(), v))
}

async fn bind(name: &str, config: HttpConfig, tx: &mpsc::Sender<Command>) -> Result<Bound> {
    let mut sockets: Vec<TcpListener> = Vec::new();
    let mut port = config.port;
    for host in &config.hosts {
        let addrs: Vec<SocketAddr> = lookup_host((host.as_str(), port))
            .await
            .with_context(|| format!("resolving {host}"))?
            .collect();
        for addr in addrs {
            let addr = SocketAddr::new(addr.ip(), port);
            let l = bind_one(addr).with_context(|| format!("binding {addr}"))?;
            if port == 0 {
                port = l.local_addr()?.port();
            }
            sockets.push(l);
        }
    }
    anyhow::ensure!(!sockets.is_empty(), "no http bind addresses");
    let config = Arc::new(config);
    let limit = Arc::new(Semaphore::new(config.max_connections));
    let mut addrs = Vec::new();
    let mut tasks = Vec::new();
    for l in sockets {
        addrs.push(l.local_addr()?);
        tasks.push(tokio::spawn(accept_loop(
            l,
            name.to_string(),
            config.clone(),
            limit.clone(),
            tx.clone(),
        )));
    }
    Ok(Bound { addrs, tasks })
}

async fn accept_loop(
    listener: TcpListener,
    name: String,
    config: Arc<HttpConfig>,
    limit: Arc<Semaphore>,
    tx: mpsc::Sender<Command>,
) {
    loop {
        let Ok((stream, remote)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        tokio::spawn(connection(
            stream,
            remote,
            name.clone(),
            config.clone(),
            tx.clone(),
            permit,
        ));
    }
}

async fn connection(
    stream: tokio::net::TcpStream,
    remote: SocketAddr,
    name: String,
    config: Arc<HttpConfig>,
    tx: mpsc::Sender<Command>,
    _permit: OwnedSemaphorePermit,
) {
    let _ = stream.set_nodelay(true);
    let cfg = config.clone();
    let service = service_fn(move |req: hyper::Request<Incoming>| {
        let name = name.clone();
        let cfg = cfg.clone();
        let tx = tx.clone();
        async move {
            let resp = serve(req, remote, name, &cfg, tx).await;
            Ok::<_, std::convert::Infallible>(into_hyper(resp, cfg.stream_idle_timeout))
        }
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(config.header_timeout)
        .max_headers(config.max_headers)
        .max_buf_size(32 * 1024);
    let io = body::StallGuard::new(stream, config.stream_idle_timeout);
    let _ = builder.serve_connection(TokioIo::new(io), service).await;
}

fn into_hyper(resp: Response, idle: Duration) -> hyper::Response<body::OutBody> {
    let mut out = hyper::Response::new(body::OutBody::new(resp.body, idle));
    *out.status_mut() = resp.status;
    for (k, v) in resp.headers {
        out.headers_mut().append(k, v);
    }
    out
}

async fn serve(
    req: hyper::Request<Incoming>,
    mut remote: SocketAddr,
    listener: String,
    config: &HttpConfig,
    tx: mpsc::Sender<Command>,
) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    if !path.starts_with('/') || path.split('/').any(|s| s == "..") {
        return Response::status(StatusCode::BAD_REQUEST);
    }
    let mut host = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if config.trust_forwarded {
        if let Some(h) = parts
            .headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
        {
            host = Some(h.split(',').next().unwrap_or(h).trim().to_string());
        }
        if let Some(ip) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
        {
            remote.set_ip(ip);
        }
    }
    let (reply, rx) = oneshot::channel();
    if tx
        .send(Command::Resolve {
            listener,
            path: path.clone(),
            reply,
        })
        .await
        .is_err()
    {
        return Response::status(StatusCode::SERVICE_UNAVAILABLE);
    }
    let Ok(resolved) = rx.await else {
        return Response::status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let method = parts.method.clone();
    let mut request = Request {
        method: parts.method,
        rest: String::new(),
        path,
        query: parts.uri.query().map(str::to_string),
        headers: parts.headers,
        body: Bytes::new(),
        remote,
        host,
        body_stream: None,
        auth: None,
    };
    let guard = resolved.guard;
    let mut resp = match &guard {
        Some(g) => match g.check(&request).await {
            Ok(auth) => {
                request.auth = auth;
                route_request(request, resolved.route, body, config).await
            }
            Err(rejected) => rejected,
        },
        None => route_request(request, resolved.route, body, config).await,
    };
    if let Some(g) = &guard {
        g.finish(&method, parts.uri.path(), &mut resp);
    }
    if method == Method::HEAD {
        resp.body = Body::empty();
    }
    if let Some(meter) = &resolved.meter {
        meter.served(&method, parts.uri.path(), &resp);
    }
    resp
}

async fn route_request(
    mut request: Request,
    route: Option<(Route, String)>,
    body: Incoming,
    config: &HttpConfig,
) -> Response {
    let Some((route, rest)) = route else {
        return Response::status(StatusCode::NOT_FOUND);
    };
    request.rest = rest;
    let limit = route.options.max_body.unwrap_or(config.max_body);
    let declared = request
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit as u64) {
        return Response::status(StatusCode::PAYLOAD_TOO_LARGE);
    }
    if route.options.stream {
        request.body_stream = Some(BodyStream::new(
            body,
            limit,
            config.stream_idle_timeout,
            config.min_upload_rate,
        ));
        return (route.handler)(request).await;
    }
    tokio::time::timeout(config.request_timeout, async move {
        request.body = match Limited::new(body, limit).collect().await {
            Ok(c) => c.to_bytes(),
            Err(_) => return Response::status(StatusCode::PAYLOAD_TOO_LARGE),
        };
        (route.handler)(request).await
    })
    .await
    .unwrap_or_else(|_| Response::status(StatusCode::GATEWAY_TIMEOUT))
}

fn bind_one(addr: SocketAddr) -> Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    Ok(TcpListener::from_std(socket.into())?)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

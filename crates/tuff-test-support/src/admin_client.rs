//! A raw HTTP/1.1 client for the web admin panel: no browser, no client
//! library, one connection per request.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tuff_web_ui::auth::COOKIE;

#[derive(Debug, Clone)]
pub struct AdminResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// The body bytes, for binary responses.
    pub raw: Vec<u8>,
}

impl AdminResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

pub struct AdminClient {
    pub addr: SocketAddr,
    /// The `Host` header sent.
    pub host: String,
    /// The session cookie value, once logged in.
    pub session: Option<String>,
    pub csrf: Option<String>,
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(end) = rest.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size = std::str::from_utf8(&rest[..end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 || rest.len() < end + 2 + size {
            return out;
        }
        out.extend_from_slice(&rest[end + 2..end + 2 + size]);
        rest = &rest[(end + 4 + size).min(rest.len())..];
    }
}

impl AdminClient {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            host: format!("localhost:{}", addr.port()),
            session: None,
            csrf: None,
        }
    }

    /// Sends one request with the session cookie (if any) and `headers`.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> AdminResponse {
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.host
        );
        if let Some(session) = &self.session {
            head.push_str(&format!("Cookie: {COOKIE}={session}\r\n"));
        }
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(body) = body {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        head.push_str("\r\n");
        head.push_str(body.unwrap_or(""));
        self.send(head.into_bytes(), true).await
    }

    /// Sends `body` as raw bytes with `content_length` declared (`None`
    /// sends none); a body shorter than declared is ended by closing the
    /// write side.
    pub async fn request_bytes(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        content_length: Option<usize>,
    ) -> AdminResponse {
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.host
        );
        if let Some(session) = &self.session {
            head.push_str(&format!("Cookie: {COOKIE}={session}\r\n"));
        }
        if let Some(csrf) = &self.csrf {
            head.push_str(&format!("X-Tuff-CSRF: {csrf}\r\n"));
        }
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(len) = content_length {
            head.push_str(&format!("Content-Length: {len}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body);
        let complete = content_length.is_none_or(|len| body.len() >= len);
        self.send(bytes, complete).await
    }

    async fn send(&self, bytes: Vec<u8>, keep_open: bool) -> AdminResponse {
        let mut stream =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(self.addr))
                .await
                .expect("connect timed out")
                .expect("connect");
        let _ = stream.write_all(&bytes).await;
        if !keep_open {
            let _ = stream.shutdown().await;
        }
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(60), stream.read_to_end(&mut raw))
            .await
            .expect("read timed out");
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(raw.len());
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect();
        let mut body = raw.get(split + 4..).unwrap_or_default().to_vec();
        if headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
        {
            body = dechunk(&body);
        }
        AdminResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
            raw: body,
        }
    }

    pub async fn get(&self, path: &str) -> AdminResponse {
        self.request("GET", path, &[], None).await
    }

    /// A JSON request carrying the CSRF token.
    pub async fn send_json(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> AdminResponse {
        let csrf = self.csrf.clone().unwrap_or_default();
        self.request(
            method,
            path,
            &[("X-Tuff-CSRF", &csrf)],
            Some(&body.to_string()),
        )
        .await
    }

    /// Opens the panel's event stream for `topics`; check `status` before
    /// reading events.
    pub async fn events(&self, topics: &str) -> EventStream {
        let mut head = format!(
            "GET /admin/events?topics={topics} HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n",
            self.host
        );
        if let Some(session) = &self.session {
            head.push_str(&format!("Cookie: {COOKIE}={session}\r\n"));
        }
        head.push_str("\r\n");
        let mut stream =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(self.addr))
                .await
                .expect("connect timed out")
                .expect("connect");
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        let split = loop {
            if let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break split;
            }
            let mut buf = [0u8; 4096];
            let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
                .await
                .expect("read timed out")
                .unwrap();
            assert!(read > 0, "the event stream closed before its head");
            raw.extend_from_slice(&buf[..read]);
        };
        let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let chunked = head.contains("transfer-encoding: chunked");
        raw.drain(..split + 4);
        EventStream {
            status,
            stream,
            raw,
            chunked,
            decoded: Vec::new(),
        }
    }

    /// Exchanges a login code for a session, keeping its cookie and CSRF
    /// token.
    pub async fn login(&mut self, code: &str) -> AdminResponse {
        let resp = self
            .request(
                "POST",
                "/admin/api/v1/session",
                &[],
                Some(&serde_json::json!({ "code": code }).to_string()),
            )
            .await;
        if resp.status == 200 {
            self.session = resp
                .headers_named("set-cookie")
                .iter()
                .find_map(|c| c.strip_prefix(COOKIE)?.strip_prefix('='))
                .map(|c| c.split(';').next().unwrap_or_default().to_owned());
            self.csrf = resp.json()["csrf"].as_str().map(str::to_owned);
        }
        resp
    }
}

/// One server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

/// An open `text/event-stream` response, read incrementally.
pub struct EventStream {
    pub status: u16,
    stream: TcpStream,
    raw: Vec<u8>,
    chunked: bool,
    decoded: Vec<u8>,
}

impl EventStream {
    /// Moves every complete chunk of `raw` into `decoded`.
    fn dechunk_ready(&mut self) {
        if !self.chunked {
            self.decoded.append(&mut self.raw);
            return;
        }
        loop {
            let Some(end) = self.raw.windows(2).position(|w| w == b"\r\n") else {
                return;
            };
            let size = std::str::from_utf8(&self.raw[..end])
                .ok()
                .and_then(|s| {
                    usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok()
                })
                .unwrap_or(0);
            if self.raw.len() < end + 2 + size + 2 {
                return;
            }
            self.decoded
                .extend_from_slice(&self.raw[end + 2..end + 2 + size]);
            self.raw.drain(..end + 2 + size + 2);
        }
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        loop {
            let end = self.decoded.windows(2).position(|w| w == b"\n\n")?;
            let block: Vec<u8> = self.decoded.drain(..end + 2).collect();
            let text = String::from_utf8_lossy(&block);
            let mut event = SseEvent {
                event: "message".into(),
                ..SseEvent::default()
            };
            let mut data = Vec::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("data: ") {
                    data.push(v);
                } else if let Some(v) = line.strip_prefix("event: ") {
                    event.event = v.to_owned();
                } else if let Some(v) = line.strip_prefix("id: ") {
                    event.id = Some(v.to_owned());
                }
            }
            if data.is_empty() {
                continue;
            }
            event.data = data.join("\n");
            return Some(event);
        }
    }

    /// The next event (pings skipped), or `None` once `timeout` passes or
    /// the stream ends.
    pub async fn next(&mut self, timeout: Duration) -> Option<SseEvent> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.dechunk_ready();
            if let Some(event) = self.take_event() {
                return Some(event);
            }
            let mut buf = [0u8; 8192];
            let read = tokio::time::timeout_at(deadline, self.stream.read(&mut buf))
                .await
                .ok()?
                .ok()?;
            if read == 0 {
                return None;
            }
            self.raw.extend_from_slice(&buf[..read]);
        }
    }
}

/// The `XXXX-XXXX-XXXX-XXXX` code in a `/tuff web login` reply.
pub fn find_code(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .find(|w| w.len() == 19 && w.bytes().filter(|b| *b == b'-').count() == 3)
        .map(str::to_owned)
}

//! The WebSocket client executor. The socket connects only to addresses
//! the policy admitted, the host answers pings and closes, and message
//! and frame sizes are capped by the owner's `message-bytes` limit.
//! permessage-deflate is never negotiated, so nothing is inflated.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, SEC_WEBSOCKET_PROTOCOL};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error, Message};

use super::{Shared, denied};
use crate::caps::pattern::TargetHost;
use crate::caps::resolve::resolve;
use crate::caps::{Approval, Kind};
use crate::wit::bindings::capability_types::{
    Connected, Denied, Handle, Header, IoEvent, SocketClosed, WsFrame, WsMessage,
};

trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

const SEND_QUEUE: usize = 64;
const FORBIDDEN: [&str; 7] = [
    "host",
    "connection",
    "upgrade",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
];

/// Live sockets' send queues, by handle.
#[derive(Default)]
pub struct Sockets {
    queues: Mutex<HashMap<Handle, mpsc::Sender<WsMessage>>>,
}

impl Sockets {
    fn add(&self, handle: Handle, queue: mpsc::Sender<WsMessage>) {
        self.queues.lock().unwrap().insert(handle, queue);
    }

    pub fn forget(&self, handle: Handle) {
        self.queues.lock().unwrap().remove(&handle);
    }

    pub fn send(&self, shared: &Shared, handle: Handle, message: WsMessage) -> Result<(), Denied> {
        let limit = shared.limits().message_bytes;
        let len = match &message {
            WsMessage::Text(text) => text.len(),
            WsMessage::Binary(data) => data.len(),
        } as u64;
        if len > limit {
            return Err(Denied::Quota(format!("the message is over {limit} bytes")));
        }
        let queue = self.queues.lock().unwrap().get(&handle).cloned();
        match queue.ok_or(Denied::Closed)?.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(Denied::Quota("the send queue is full".into()))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.forget(handle);
                Err(Denied::Closed)
            }
        }
    }
}

pub(super) fn start(
    shared: &Arc<Shared>,
    runtime: &tokio::runtime::Handle,
    url: &str,
    headers: Vec<Header>,
    protocols: Vec<String>,
) -> Result<Handle, Denied> {
    let approval = shared
        .policy()
        .check_url(Kind::WebSocket, url)
        .map_err(denied)?;
    let request = prepare(&approval, &headers, &protocols)?;
    let slot = shared.connect_slot()?;
    let task = shared.clone();
    let (queue, outbox) = mpsc::channel(SEND_QUEUE);
    shared
        .handles
        .open(|handle| {
            task.sockets.add(handle, queue);
            runtime
                .spawn(async move {
                    let _slot = slot;
                    let current = approval.target.url_text();
                    let work = run(&task, handle, approval, request, outbox);
                    let revoked = task
                        .revoked(|policy| policy.check_url(Kind::WebSocket, &current).map(drop));
                    let closed = tokio::select! {
                        closed = work => closed,
                        why = revoked => Some(Closed::failed(why)),
                    };
                    task.sockets.forget(handle);
                    task.handles.finish(handle);
                    if let Some(closed) = closed {
                        task.events
                            .send(IoEvent::Closed(closed.event(handle)))
                            .await;
                    }
                })
                .abort_handle()
        })
        .ok_or_else(|| Denied::Quota("the plugin used all its handles".into()))
}

fn prepare(
    approval: &Approval,
    headers: &[Header],
    protocols: &[String],
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, Denied> {
    let malformed = |what: String| Denied::Malformed(what);
    let mut request = approval
        .target
        .url_text()
        .into_client_request()
        .map_err(|err| malformed(err.to_string()))?;
    for header in headers {
        let bad = |what: &str| malformed(format!("header {:?}: {what}", header.name));
        let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| bad("bad name"))?;
        if FORBIDDEN.contains(&name.as_str()) {
            return Err(bad("the host sets this header"));
        }
        let value = HeaderValue::from_bytes(&header.value).map_err(|_| bad("bad value"))?;
        request.headers_mut().append(name, value);
    }
    if !protocols.is_empty() {
        let token = |p: &String| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
        };
        if let Some(bad) = protocols.iter().find(|p| !token(p)) {
            return Err(malformed(format!("{bad:?} is not a protocol token")));
        }
        let value = HeaderValue::from_str(&protocols.join(", "))
            .map_err(|_| malformed("bad protocols".into()))?;
        request.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
    }
    Ok(request)
}

struct Closed {
    error: Option<Denied>,
    code: Option<u16>,
    reason: String,
}

impl Closed {
    fn failed(error: Denied) -> Self {
        Self {
            error: Some(error),
            code: None,
            reason: String::new(),
        }
    }

    fn orderly(frame: Option<CloseFrame>) -> Self {
        Self {
            error: None,
            code: frame.as_ref().map(|f| u16::from(f.code)),
            reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
        }
    }

    fn event(self, socket: Handle) -> SocketClosed {
        SocketClosed {
            socket,
            error: self.error,
            code: self.code,
            reason: self.reason,
        }
    }
}

async fn run(
    shared: &Shared,
    handle: Handle,
    approval: Approval,
    request: tokio_tungstenite::tungstenite::http::Request<()>,
    mut outbox: mpsc::Receiver<WsMessage>,
) -> Option<Closed> {
    let limits = shared.limits();
    let message_bytes = usize::try_from(limits.message_bytes).unwrap_or(usize::MAX);
    let config = WebSocketConfig::default()
        .max_message_size(Some(message_bytes))
        .max_frame_size(Some(message_bytes));
    let connecting = async {
        let addrs = resolve(&approval, &*shared.resolver, limits.connect_timeout)
            .await
            .map_err(denied)?;
        let stream = connect_tcp(&addrs).await?;
        let stream: Box<dyn Io> = if approval.target.scheme.name() == "wss" {
            let name = match &approval.target.host {
                TargetHost::Name(name) => name.clone(),
                TargetHost::Ip(ip) => ip.to_string(),
            };
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from(name)
                .map_err(|err| Denied::Malformed(err.to_string()))?;
            let tls = tokio_rustls::TlsConnector::from(Arc::new(super::http::tls()))
                .connect(name, stream)
                .await
                .map_err(|err| Denied::Failed(format!("tls: {err}")))?;
            Box::new(tls)
        } else {
            Box::new(stream)
        };
        tokio_tungstenite::client_async_with_config(request, stream, Some(config))
            .await
            .map_err(failed)
    };
    let connected = tokio::time::timeout(limits.connect_timeout * 2, connecting)
        .await
        .map_err(|_| Denied::TimedOut)
        .and_then(|result| result);
    let (mut socket, _) = match connected {
        Ok(connected) => connected,
        Err(err) => {
            shared
                .events
                .send(IoEvent::Connected(Connected {
                    socket: handle,
                    outcome: Err(err.clone()),
                }))
                .await;
            return None;
        }
    };
    shared
        .events
        .send(IoEvent::Connected(Connected {
            socket: handle,
            outcome: Ok(()),
        }))
        .await;
    loop {
        let step = tokio::select! {
            incoming = socket.next() => Step::In(incoming),
            out = outbox.recv() => Step::Out(out),
            () = tokio::time::sleep(limits.idle_timeout) => Step::Idle,
        };
        match step {
            Step::Idle => {
                let _ = socket.close(None).await;
                return Some(Closed::failed(Denied::TimedOut));
            }
            Step::Out(None) => return Some(Closed::failed(Denied::Closed)),
            Step::Out(Some(message)) => {
                let message = match message {
                    WsMessage::Text(text) => Message::text(text),
                    WsMessage::Binary(data) => Message::binary(data),
                };
                pause(shared.usage.sent(Instant::now(), message.len() as u64)).await;
                if let Err(err) = socket.send(message).await {
                    return Some(Closed::failed(failed(err)));
                }
            }
            Step::In(None) => return Some(Closed::orderly(None)),
            Step::In(Some(Err(err))) => return Some(Closed::failed(failed(err))),
            Step::In(Some(Ok(message))) => {
                pause(shared.usage.received(Instant::now(), message.len() as u64)).await;
                let message = match message {
                    Message::Text(text) => WsMessage::Text(text.as_str().to_owned()),
                    Message::Binary(data) => WsMessage::Binary(data.to_vec()),
                    Message::Close(frame) => {
                        let _ = socket.close(None).await;
                        return Some(Closed::orderly(frame));
                    }
                    _ => continue,
                };
                shared
                    .events
                    .send(IoEvent::WsMessage(WsFrame {
                        socket: handle,
                        message,
                    }))
                    .await;
            }
        }
    }
}

enum Step {
    In(Option<Result<Message, Error>>),
    Out(Option<WsMessage>),
    Idle,
}

async fn connect_tcp(addrs: &[std::net::SocketAddr]) -> Result<TcpStream, Denied> {
    let mut last = Denied::Failed("no address".into());
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last = Denied::Failed(format!("connect {addr}: {err}")),
        }
    }
    Err(last)
}

async fn pause(wait: std::time::Duration) {
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

fn failed(err: Error) -> Denied {
    match err {
        Error::Capacity(err) => Denied::Quota(err.to_string()),
        err => Denied::Failed(err.to_string()),
    }
}

//! The TCP client executor, optionally over TLS, plus the pieces UDP
//! reuses: the per-socket send queues and the idle clock. A socket's one
//! task connects to the addresses the policy admitted, then moves bytes
//! both ways under the plugin's byte budgets; it ends on close, idle
//! timeout, peer close, or revoked grant.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::{Shared, denied};
use crate::caps::pattern::TargetHost;
use crate::caps::resolve::resolve;
use crate::caps::{Approval, Kind};
use crate::wit::bindings::capability_types::{
    Connected, Denied, Handle, IoEvent, SocketClosed, SocketData,
};

const SEND_QUEUE: usize = 32;
const READ_CHUNK: u64 = 64 * 1024;

pub(super) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Send queues of the plugin's open sockets, by handle.
#[derive(Default)]
pub struct Sockets {
    queues: Mutex<HashMap<Handle, mpsc::Sender<Vec<u8>>>>,
}

impl Sockets {
    pub(super) fn open(&self, handle: Handle) -> mpsc::Receiver<Vec<u8>> {
        let (queue, receiver) = mpsc::channel(SEND_QUEUE);
        self.queues.lock().unwrap().insert(handle, queue);
        receiver
    }

    pub(super) fn remove(&self, handle: Handle) {
        self.queues.lock().unwrap().remove(&handle);
    }

    pub fn send(&self, shared: &Shared, handle: Handle, data: &[u8]) -> Result<(), Denied> {
        let limit = shared.limits().message_bytes;
        if data.len() as u64 > limit {
            return Err(Denied::Quota(format!("a send is over {limit} bytes")));
        }
        let queues = self.queues.lock().unwrap();
        let queue = queues.get(&handle).ok_or(Denied::Closed)?;
        queue.try_send(data.to_vec()).map_err(|why| match why {
            mpsc::error::TrySendError::Full(_) => {
                Denied::Quota("the socket's send queue is full".into())
            }
            mpsc::error::TrySendError::Closed(_) => Denied::Closed,
        })
    }
}

/// When a socket last moved data, for the idle timeout.
pub(super) struct Activity(Mutex<Instant>);

impl Activity {
    pub(super) fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    pub(super) fn touch(&self) {
        *self.0.lock().unwrap() = Instant::now();
    }

    pub(super) async fn idle(&self, limit: Duration) {
        loop {
            let due = *self.0.lock().unwrap() + limit;
            if Instant::now() >= due {
                return;
            }
            tokio::time::sleep_until(due.into()).await;
        }
    }
}

pub(super) async fn pause(wait: Duration) {
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

pub(super) fn io_failed(err: std::io::Error) -> Denied {
    Denied::Failed(err.to_string())
}

pub(super) async fn announce(shared: &Shared, socket: Handle, outcome: Result<(), Denied>) {
    shared
        .events
        .send(IoEvent::Connected(Connected { socket, outcome }))
        .await;
}

pub(super) async fn closed(shared: &Shared, socket: Handle, error: Option<Denied>) {
    shared
        .events
        .send(IoEvent::Closed(SocketClosed {
            socket,
            error,
            code: None,
            reason: String::new(),
        }))
        .await;
}

pub(super) fn start(
    shared: &Arc<Shared>,
    runtime: &tokio::runtime::Handle,
    host: &str,
    port: u16,
    tls: bool,
) -> Result<Handle, Denied> {
    let approval = shared
        .policy()
        .check_socket(Kind::Tcp, host, port)
        .map_err(denied)?;
    let slot = shared.connect_slot()?;
    let task = shared.clone();
    let name = host.to_owned();
    shared
        .handles
        .open(|handle| {
            let queue = task.streams.open(handle);
            runtime
                .spawn(async move {
                    let _slot = slot;
                    let connected = AtomicBool::new(false);
                    let work = session(&task, handle, approval, tls, queue, &connected);
                    let revoked = task
                        .revoked(|policy| policy.check_socket(Kind::Tcp, &name, port).map(drop));
                    tokio::select! {
                        () = work => {}
                        why = revoked => {
                            task.streams.remove(handle);
                            task.handles.finish(handle);
                            if connected.load(Ordering::Acquire) {
                                closed(&task, handle, Some(why)).await;
                            } else {
                                announce(&task, handle, Err(why)).await;
                            }
                            return;
                        }
                    }
                    task.streams.remove(handle);
                    task.handles.finish(handle);
                })
                .abort_handle()
        })
        .ok_or_else(|| Denied::Quota("the plugin used all its handles".into()))
}

async fn session(
    shared: &Shared,
    handle: Handle,
    approval: Approval,
    tls: bool,
    queue: mpsc::Receiver<Vec<u8>>,
    connected: &AtomicBool,
) {
    let stream = match connect(shared, &approval, tls).await {
        Ok(stream) => stream,
        Err(why) => {
            announce(shared, handle, Err(why)).await;
            return;
        }
    };
    connected.store(true, Ordering::Release);
    announce(shared, handle, Ok(())).await;
    let ending = pump(shared, handle, stream, queue).await;
    closed(shared, handle, ending).await;
}

async fn connect(
    shared: &Shared,
    approval: &Approval,
    tls: bool,
) -> Result<Box<dyn Stream>, Denied> {
    let limits = shared.limits();
    let work = async {
        let addrs = resolve(approval, &*shared.resolver, limits.connect_timeout)
            .await
            .map_err(denied)?;
        let tcp = dial(&addrs).await?;
        if !tls {
            return Ok(Box::new(tcp) as Box<dyn Stream>);
        }
        let name = match &approval.target.host {
            TargetHost::Name(name) => ServerName::try_from(name.clone())
                .map_err(|_| Denied::Malformed(format!("{name} is not a TLS server name")))?,
            TargetHost::Ip(ip) => ServerName::IpAddress((*ip).into()),
        };
        let connector = tokio_rustls::TlsConnector::from(Arc::new(super::http::tls()));
        let stream = connector.connect(name, tcp).await.map_err(io_failed)?;
        Ok(Box::new(stream) as Box<dyn Stream>)
    };
    tokio::time::timeout(limits.connect_timeout, work)
        .await
        .map_err(|_| Denied::TimedOut)?
}

async fn dial(addrs: &[SocketAddr]) -> Result<TcpStream, Denied> {
    let mut last = Denied::Failed("no address to connect to".into());
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(err) => last = io_failed(err),
        }
    }
    Err(last)
}

pub(super) async fn pump(
    shared: &Shared,
    handle: Handle,
    stream: Box<dyn Stream>,
    mut queue: mpsc::Receiver<Vec<u8>>,
) -> Option<Denied> {
    let limits = shared.limits();
    let chunk = limits.message_bytes.clamp(1, READ_CHUNK) as usize;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let activity = Activity::new();
    let read = async {
        let mut buf = vec![0u8; chunk];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => return None,
                Err(err) => return Some(io_failed(err)),
                Ok(n) => {
                    activity.touch();
                    pause(shared.usage.received(Instant::now(), n as u64)).await;
                    let event = IoEvent::Data(SocketData {
                        socket: handle,
                        data: buf[..n].to_vec(),
                    });
                    if !shared.events.send(event).await {
                        return Some(Denied::Closed);
                    }
                }
            }
        }
    };
    let write = async {
        while let Some(data) = queue.recv().await {
            pause(shared.usage.sent(Instant::now(), data.len() as u64)).await;
            if let Err(err) = writer.write_all(&data).await {
                return Some(io_failed(err));
            }
            activity.touch();
        }
        std::future::pending().await
    };
    tokio::select! {
        ending = read => ending,
        ending = write => ending,
        () = activity.idle(limits.idle_timeout) => Some(Denied::TimedOut),
    }
}

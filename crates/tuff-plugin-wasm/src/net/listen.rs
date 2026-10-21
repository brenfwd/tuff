//! Listening TCP and UDP ports. A listener is one task: it checks the
//! server's own ports at bind time, binds dual-stack unless the grant
//! names an address, reports the bound port, then accepts connections
//! (which become ordinary socket handles) or relays datagrams. Listeners
//! and their connections end when the grant is revoked.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;

use super::tcp::{Activity, closed, io_failed, pause, pump};
use super::{Shared, denied};
use crate::caps::Kind;
use crate::wit::bindings::capability_types::{
    Accepted, CapabilityKind, Datagram, Denied, Handle, IoEvent, Listening,
};

const MAX_LISTENERS: usize = 8;
const MAX_HEARD: usize = 1024;
const REPLY_QUEUE: usize = 32;
const MAX_DATAGRAM: u64 = 65_535;

/// The ports the server itself uses, read again at every bind.
pub trait Reserved: Send + Sync {
    fn ports(&self) -> Pin<Box<dyn Future<Output = Vec<u16>> + Send + '_>>;
}

type Reply = (SocketAddr, Vec<u8>);

struct Udp {
    queue: mpsc::Sender<Reply>,
    heard: Arc<Mutex<HashSet<SocketAddr>>>,
}

/// The plugin's open listeners; only UDP ones keep state here.
#[derive(Default)]
pub struct Listeners {
    reserved: Mutex<Option<Arc<dyn Reserved>>>,
    udp: Mutex<HashMap<Handle, Udp>>,
    count: Mutex<usize>,
}

impl Listeners {
    pub fn set_reserved(&self, reserved: Arc<dyn Reserved>) {
        *self.reserved.lock().unwrap() = Some(reserved);
    }

    async fn reserved_ports(&self) -> Vec<u16> {
        let reserved = self.reserved.lock().unwrap().clone();
        match reserved {
            Some(reserved) => reserved.ports().await,
            None => Vec::new(),
        }
    }

    pub(super) fn remove(&self, handle: Handle) {
        self.udp.lock().unwrap().remove(&handle);
    }

    pub fn reply(&self, handle: Handle, peer: &str, data: &[u8]) -> Result<(), Denied> {
        let peer: SocketAddr = peer
            .parse()
            .map_err(|_| Denied::Malformed(format!("{peer} is not ip:port")))?;
        let peer = canonical(peer);
        let udp = self.udp.lock().unwrap();
        let listener = udp.get(&handle).ok_or(Denied::Closed)?;
        if !listener.heard.lock().unwrap().contains(&peer) {
            return Err(Denied::NotGranted(format!(
                "{peer} has not been heard from"
            )));
        }
        listener
            .queue
            .try_send((peer, data.to_vec()))
            .map_err(|why| match why {
                mpsc::error::TrySendError::Full(_) => {
                    Denied::Quota("the listener's send queue is full".into())
                }
                mpsc::error::TrySendError::Closed(_) => Denied::Closed,
            })
    }
}

struct Count<'a>(&'a Listeners);

impl Drop for Count<'_> {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap() -= 1;
    }
}

pub(super) fn start(
    shared: &Arc<Shared>,
    runtime: &tokio::runtime::Handle,
    kind: CapabilityKind,
    bind: Option<&str>,
    port: u16,
) -> Result<Handle, Denied> {
    let kind = match kind {
        CapabilityKind::Tcp => Kind::ListenTcp,
        CapabilityKind::Udp => Kind::ListenUdp,
        _ => return Err(Denied::Malformed("only tcp and udp can listen".into())),
    };
    let ip: IpAddr = match bind {
        Some(text) => text
            .trim_matches(['[', ']'])
            .parse()
            .map_err(|_| Denied::Malformed(format!("{text} is not an address")))?,
        None => Ipv6Addr::UNSPECIFIED.into(),
    };
    shared
        .policy()
        .check_listen(kind, ip, port, &[])
        .map_err(denied)?;
    {
        let mut count = shared.listeners.count.lock().unwrap();
        if *count >= MAX_LISTENERS {
            return Err(Denied::Quota(format!(
                "a plugin may hold {MAX_LISTENERS} listeners"
            )));
        }
        *count += 1;
    }
    let task = shared.clone();
    let explicit = bind.is_some();
    let opened = shared.handles.open(|handle| {
        runtime
            .spawn(async move {
                let _count = Count(&task.listeners);
                let work = run(&task, handle, kind, ip, explicit, port);
                let revoked =
                    task.revoked(|policy| policy.check_listen(kind, ip, port, &[]).map(drop));
                tokio::select! {
                    () = work => {}
                    why = revoked => {
                        task.listeners.remove(handle);
                        task.handles.finish(handle);
                        closed(&task, handle, Some(why)).await;
                        return;
                    }
                }
                task.listeners.remove(handle);
                task.handles.finish(handle);
            })
            .abort_handle()
    });
    if opened.is_none() {
        *shared.listeners.count.lock().unwrap() -= 1;
    }
    opened.ok_or_else(|| Denied::Quota("the plugin used all its handles".into()))
}

async fn run(
    shared: &Arc<Shared>,
    handle: Handle,
    kind: Kind,
    ip: IpAddr,
    explicit: bool,
    port: u16,
) {
    let reserved = shared.listeners.reserved_ports().await;
    if let Err(why) = shared.policy().check_listen(kind, ip, port, &reserved) {
        closed(shared, handle, Some(denied(why))).await;
        return;
    }
    let ending = if kind == Kind::ListenTcp {
        tcp(shared, handle, kind, ip, explicit, port).await
    } else {
        udp(shared, handle, ip, explicit, port).await
    };
    closed(shared, handle, ending).await;
}

fn bind_socket(ip: IpAddr, explicit: bool, port: u16, ty: Type) -> std::io::Result<Socket> {
    let protocol = if ty == Type::STREAM {
        Protocol::TCP
    } else {
        Protocol::UDP
    };
    let open = |ip: IpAddr| -> std::io::Result<Socket> {
        let domain = if ip.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, ty, Some(protocol))?;
        if ip.is_ipv6() && !explicit {
            socket.set_only_v6(false)?;
        }
        if ty == Type::STREAM {
            socket.set_reuse_address(true)?;
        }
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::new(ip, port).into())?;
        Ok(socket)
    };
    match open(ip) {
        Err(_) if !explicit => open(std::net::Ipv4Addr::UNSPECIFIED.into()),
        other => other,
    }
}

async fn tcp(
    shared: &Arc<Shared>,
    handle: Handle,
    kind: Kind,
    ip: IpAddr,
    explicit: bool,
    port: u16,
) -> Option<Denied> {
    let listener = match bind_socket(ip, explicit, port, Type::STREAM).and_then(|socket| {
        socket.listen(128)?;
        TcpListener::from_std(socket.into())
    }) {
        Ok(listener) => listener,
        Err(err) => return Some(io_failed(err)),
    };
    let bound = listener.local_addr().map_or(port, |addr| addr.port());
    shared
        .events
        .send(IoEvent::Listening(Listening {
            listener: handle,
            port: bound,
        }))
        .await;
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => return Some(io_failed(err)),
        };
        let Ok(slot) = shared.usage.accept() else {
            continue;
        };
        let _ = stream.set_nodelay(true);
        let task = shared.clone();
        shared.handles.open(|socket| {
            let queue = task.streams.open(socket);
            tokio::spawn(async move {
                let _slot = slot;
                let sent = task
                    .events
                    .send(IoEvent::Accepted(Accepted {
                        listener: handle,
                        socket,
                        peer: peer.to_string(),
                    }))
                    .await;
                let work = async {
                    if sent {
                        pump(&task, socket, Box::new(stream), queue).await
                    } else {
                        Some(Denied::Closed)
                    }
                };
                let revoked =
                    task.revoked(|policy| policy.check_listen(kind, ip, port, &[]).map(drop));
                let ending = tokio::select! {
                    ending = work => ending,
                    why = revoked => Some(why),
                };
                task.streams.remove(socket);
                task.handles.finish(socket);
                closed(&task, socket, ending).await;
            })
            .abort_handle()
        });
    }
}

async fn udp(
    shared: &Arc<Shared>,
    handle: Handle,
    ip: IpAddr,
    explicit: bool,
    port: u16,
) -> Option<Denied> {
    let socket = match bind_socket(ip, explicit, port, Type::DGRAM)
        .and_then(|socket| UdpSocket::from_std(socket.into()))
    {
        Ok(socket) => socket,
        Err(err) => return Some(io_failed(err)),
    };
    let bound = socket.local_addr().map_or(port, |addr| addr.port());
    let (queue, mut replies) = mpsc::channel::<Reply>(REPLY_QUEUE);
    let heard = Arc::new(Mutex::new(HashSet::new()));
    shared.listeners.udp.lock().unwrap().insert(
        handle,
        Udp {
            queue,
            heard: heard.clone(),
        },
    );
    shared
        .events
        .send(IoEvent::Listening(Listening {
            listener: handle,
            port: bound,
        }))
        .await;
    let limits = shared.limits();
    let size = limits.message_bytes.clamp(1, MAX_DATAGRAM) as usize;
    let activity = Activity::new();
    let read = async {
        let mut buf = vec![0u8; size];
        loop {
            match socket.recv_from(&mut buf).await {
                Err(err) => return Some(io_failed(err)),
                Ok((n, peer)) => {
                    let peer = canonical(peer);
                    activity.touch();
                    pause(shared.usage.received(Instant::now(), n as u64)).await;
                    {
                        let mut heard = heard.lock().unwrap();
                        if heard.len() >= MAX_HEARD && !heard.contains(&peer) {
                            continue;
                        }
                        heard.insert(peer);
                    }
                    let event = IoEvent::Datagram(Datagram {
                        socket: handle,
                        peer: peer.to_string(),
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
        while let Some((peer, data)) = replies.recv().await {
            pause(shared.usage.sent(Instant::now(), data.len() as u64)).await;
            if let Err(err) = socket.send_to(&data, native(&socket, peer)).await {
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

fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

fn native(socket: &UdpSocket, peer: SocketAddr) -> SocketAddr {
    match (socket.local_addr(), peer.ip()) {
        (Ok(local), IpAddr::V4(v4)) if local.is_ipv6() => {
            SocketAddr::new(v4.to_ipv6_mapped().into(), peer.port())
        }
        _ => peer,
    }
}

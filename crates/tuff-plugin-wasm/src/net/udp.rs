//! The UDP client executor: a socket connected to one admitted
//! destination, so the kernel drops datagrams from anywhere else.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::tcp::{Activity, announce, closed, io_failed, pause};
use super::{Shared, denied};
use crate::caps::resolve::resolve;
use crate::caps::{Approval, Kind};
use crate::wit::bindings::capability_types::{Datagram, Denied, Handle, IoEvent};

const MAX_DATAGRAM: u64 = 65_535;

pub(super) fn start(
    shared: &Arc<Shared>,
    runtime: &tokio::runtime::Handle,
    host: &str,
    port: u16,
) -> Result<Handle, Denied> {
    let approval = shared
        .policy()
        .check_socket(Kind::Udp, host, port)
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
                    let work = session(&task, handle, approval, queue, &connected);
                    let revoked = task
                        .revoked(|policy| policy.check_socket(Kind::Udp, &name, port).map(drop));
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
    queue: mpsc::Receiver<Vec<u8>>,
    connected: &AtomicBool,
) {
    let socket = match open(shared, &approval).await {
        Ok(socket) => socket,
        Err(why) => {
            announce(shared, handle, Err(why)).await;
            return;
        }
    };
    connected.store(true, Ordering::Release);
    announce(shared, handle, Ok(())).await;
    let ending = pump(shared, handle, socket, queue).await;
    closed(shared, handle, ending).await;
}

async fn open(shared: &Shared, approval: &Approval) -> Result<(UdpSocket, SocketAddr), Denied> {
    let limits = shared.limits();
    let work = async {
        let addrs = resolve(approval, &*shared.resolver, limits.connect_timeout)
            .await
            .map_err(denied)?;
        let peer = addrs[0];
        let bind: SocketAddr = if peer.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0u16; 8], 0).into()
        };
        let socket = UdpSocket::bind(bind).await.map_err(io_failed)?;
        socket.connect(peer).await.map_err(io_failed)?;
        Ok((socket, peer))
    };
    tokio::time::timeout(limits.connect_timeout, work)
        .await
        .map_err(|_| Denied::TimedOut)?
}

async fn pump(
    shared: &Shared,
    handle: Handle,
    (socket, peer): (UdpSocket, SocketAddr),
    mut queue: mpsc::Receiver<Vec<u8>>,
) -> Option<Denied> {
    let limits = shared.limits();
    let size = limits.message_bytes.clamp(1, MAX_DATAGRAM) as usize;
    let activity = Activity::new();
    let read = async {
        let mut buf = vec![0u8; size];
        loop {
            match socket.recv(&mut buf).await {
                Err(err) => return Some(io_failed(err)),
                Ok(n) => {
                    activity.touch();
                    pause(shared.usage.received(Instant::now(), n as u64)).await;
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
        while let Some(data) = queue.recv().await {
            pause(shared.usage.sent(Instant::now(), data.len() as u64)).await;
            if let Err(err) = socket.send(&data).await {
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

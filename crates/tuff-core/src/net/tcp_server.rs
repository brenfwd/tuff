use std::net::SocketAddr;

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, lookup_host};

const LISTEN_BACKLOG: i32 = 1024;

/// Binds a listener for every resolved address of every entry in `binds`.
/// IPv6 sockets are set to IPv6-only so that `0.0.0.0` and `[::]` can be bound
/// side by side on any OS.
pub async fn bind_all(binds: &[String]) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::new();
    for bind in binds {
        let addrs = lookup_host(bind.as_str())
            .await
            .with_context(|| format!("resolving bind address {bind}"))?;
        for addr in addrs {
            let listener = bind_one(addr).with_context(|| format!("binding {addr}"))?;
            listeners.push(listener);
        }
    }
    anyhow::ensure!(!listeners.is_empty(), "no bind addresses configured");
    Ok(listeners)
}

pub(crate) fn bind_one(addr: SocketAddr) -> Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(LISTEN_BACKLOG)?;
    Ok(TcpListener::from_std(socket.into())?)
}

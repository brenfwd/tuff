//! Executors for the `net` interface: outbound HTTP here, with the pieces
//! WebSocket, TCP and UDP executors reuse. [`Net`] is one plugin's
//! network context: its live policy, quota usage, resolver, handle table
//! and event queue. Every connection resolves through
//! [`caps::resolve::resolve`] and connects only to the addresses it
//! admits; every task watches the policy and ends when the plugin loses
//! what it was doing.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, watch};

use crate::caps::policy::Denied as Refused;
use crate::caps::quota::Exceeded;
use crate::caps::resolve::Resolver;
use crate::caps::{Kind, Policy, Usage};
use crate::wit::bindings::capability_types::{
    CapabilityKind, Denied, Handle, Header, HttpRequest, WsMessage,
};

pub mod events;
pub mod handles;
mod http;
pub mod listen;
mod tcp;
mod udp;
mod ws;

pub use events::{Delivery, Events};
pub use handles::Handles;

/// What executors share; tasks hold it through an `Arc`.
pub struct Shared {
    pub policy: watch::Receiver<Arc<Policy>>,
    pub usage: Arc<Usage>,
    pub resolver: Arc<dyn Resolver>,
    pub handles: Handles,
    pub events: Events,
    pub sockets: ws::Sockets,
    pub streams: tcp::Sockets,
    pub listeners: listen::Listeners,
}

impl Shared {
    pub fn policy(&self) -> Arc<Policy> {
        self.policy.borrow().clone()
    }

    pub fn limits(&self) -> crate::caps::Limits {
        self.usage.limits()
    }

    /// Reserves an outbound slot and request token.
    pub fn connect_slot(&self) -> Result<crate::caps::quota::Slot, Denied> {
        self.usage.set_limits(*self.policy.borrow().limits());
        self.usage.connect(Instant::now()).map_err(quota)
    }

    /// Resolves once the plugin's policy no longer allows `still_allowed`
    /// (or the plugin is gone), with the reason.
    pub async fn revoked(&self, still_allowed: impl Fn(&Policy) -> Result<(), Refused>) -> Denied {
        let mut policy = self.policy.clone();
        loop {
            if policy.changed().await.is_err() {
                return Denied::Closed;
            }
            let current = policy.borrow_and_update().clone();
            if let Err(why) = still_allowed(&current) {
                return denied(why);
            }
        }
    }
}

/// One plugin's network context. Dropping it cancels everything in
/// flight.
pub struct Net {
    shared: Arc<Shared>,
    runtime: tokio::runtime::Handle,
}

impl Drop for Net {
    fn drop(&mut self) {
        self.shared.handles.cancel_all();
    }
}

impl Net {
    /// Must be called inside the tokio runtime that will run executors.
    pub fn new(
        policy: watch::Receiver<Arc<Policy>>,
        resolver: Arc<dyn Resolver>,
        plugin: &str,
        generation: u64,
        queue: mpsc::UnboundedSender<Delivery>,
    ) -> Self {
        let limits = *policy.borrow().limits();
        let usage = Usage::new(limits, Instant::now());
        let mut watching = policy.clone();
        let live = Arc::downgrade(&usage);
        tokio::spawn(async move {
            while watching.changed().await.is_ok() {
                let Some(usage) = live.upgrade() else { break };
                usage.set_limits(*watching.borrow_and_update().limits());
            }
        });
        Self {
            shared: Arc::new(Shared {
                policy,
                usage,
                resolver,
                handles: Handles::default(),
                sockets: ws::Sockets::default(),
                events: Events::new(plugin, generation, limits.pending_events, queue),
                streams: tcp::Sockets::default(),
                listeners: listen::Listeners::default(),
            }),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    pub fn open_handles(&self) -> usize {
        self.shared.handles.len()
    }

    /// Name-level check only: no DNS, no quota.
    pub fn granted(&self, kind: CapabilityKind, target: &str) -> bool {
        let policy = self.shared.policy();
        match kind {
            CapabilityKind::Http => policy.check_url(Kind::Http, target).is_ok(),
            CapabilityKind::Websocket => policy.check_url(Kind::WebSocket, target).is_ok(),
            CapabilityKind::Tcp | CapabilityKind::Udp => {
                let kind = if matches!(kind, CapabilityKind::Tcp) {
                    Kind::Tcp
                } else {
                    Kind::Udp
                };
                split_socket(target)
                    .is_some_and(|(host, port)| policy.check_socket(kind, host, port).is_ok())
            }
            _ => false,
        }
    }

    pub fn fetch(&self, request: HttpRequest) -> Result<Handle, Denied> {
        http::start(&self.shared, &self.runtime, request)
    }

    pub fn ws_connect(
        &self,
        url: &str,
        headers: Vec<Header>,
        protocols: Vec<String>,
    ) -> Result<Handle, Denied> {
        ws::start(&self.shared, &self.runtime, url, headers, protocols)
    }

    pub fn ws_send(&self, socket: Handle, message: WsMessage) -> Result<(), Denied> {
        self.shared.sockets.send(&self.shared, socket, message)
    }

    pub fn tcp_connect(&self, host: &str, port: u16, tls: bool) -> Result<Handle, Denied> {
        tcp::start(&self.shared, &self.runtime, host, port, tls)
    }

    pub fn udp_connect(&self, host: &str, port: u16) -> Result<Handle, Denied> {
        udp::start(&self.shared, &self.runtime, host, port)
    }

    /// Queues `data` on a TCP or connected UDP socket.
    pub fn send(&self, socket: Handle, data: &[u8]) -> Result<(), Denied> {
        self.shared.streams.send(&self.shared, socket, data)
    }

    pub fn set_reserved(&self, reserved: Arc<dyn listen::Reserved>) {
        self.shared.listeners.set_reserved(reserved);
    }

    /// Opens a granted TCP or UDP listener; `listening` reports its port.
    pub fn listen(
        &self,
        kind: CapabilityKind,
        bind: Option<&str>,
        port: u16,
    ) -> Result<Handle, Denied> {
        listen::start(&self.shared, &self.runtime, kind, bind, port)
    }

    pub fn reply_to(&self, listener: Handle, peer: &str, data: &[u8]) -> Result<(), Denied> {
        self.shared.listeners.reply(listener, peer, data)
    }

    /// Cancels the operation or socket behind `handle`; unknown handles
    /// are ignored.
    pub fn close(&self, handle: Handle) {
        self.shared.sockets.forget(handle);
        self.shared.streams.remove(handle);
        self.shared.listeners.remove(handle);
        self.shared.handles.cancel(handle);
    }
}

fn split_socket(target: &str) -> Option<(&str, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host, port.parse().ok()?))
}

/// The guest's view of a refusal from the policy or the resolver.
pub fn denied(why: Refused) -> Denied {
    match why {
        Refused::Malformed(why) => Denied::Malformed(why),
        Refused::NotRequested(kind, target) => {
            Denied::NotRequested(format!("{} {target}", kind.name()))
        }
        Refused::NotGranted(kind, target) => {
            Denied::NotGranted(format!("{} {target}", kind.name()))
        }
        why @ Refused::Address { .. } => Denied::Address(why.to_string()),
        why @ Refused::Unresolved(..) => Denied::Unresolved(why.to_string()),
        Refused::Reserved(port) => Denied::ReservedPort(port),
    }
}

pub fn quota(why: Exceeded) -> Denied {
    Denied::Quota(why.to_string())
}

#[cfg(test)]
mod listen_tests;
#[cfg(test)]
mod socket_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod ws_tests;

#[cfg(test)]
mod tls_tests;

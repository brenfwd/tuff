//! Name resolution for capability executors. Resolution happens once per
//! connection and the admitted addresses are the only ones connected to,
//! so a name that answers differently later (DNS rebinding) cannot move
//! an approved connection onto a private address.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use super::pattern::TargetHost;
use super::policy::{Approval, Denied};

pub type Lookup<'a> = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>>;

/// Turns a host name into addresses. Tests substitute fixed answers.
pub trait Resolver: Send + Sync {
    fn lookup<'a>(&'a self, name: &'a str, port: u16) -> Lookup<'a>;
}

/// The operating system's resolver, through tokio.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn lookup<'a>(&'a self, name: &'a str, port: u16) -> Lookup<'a> {
        Box::pin(async move { Ok(tokio::net::lookup_host((name, port)).await?.collect()) })
    }
}

/// Resolves an approved destination and admits its addresses, within
/// `timeout`. IP literals skip the resolver but face the same checks.
pub async fn resolve(
    approval: &Approval,
    resolver: &dyn Resolver,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, Denied> {
    let target = &approval.target;
    let name = match &target.host {
        TargetHost::Ip(ip) => return approval.admit([SocketAddr::new(*ip, target.port)]),
        TargetHost::Name(name) => name,
    };
    let unresolved = |why: String| Denied::Unresolved(target.to_string(), why);
    let found = tokio::time::timeout(timeout, resolver.lookup(name, target.port))
        .await
        .map_err(|_| unresolved(format!("no answer within {timeout:?}")))?
        .map_err(|err| unresolved(err.to_string()))?;
    approval.admit(found)
}

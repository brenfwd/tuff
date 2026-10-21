//! Feature gates on logins (throttles and the like). The coordinator owns
//! one gate per registered factory and asks each, in registration order,
//! once a login has passed the ban, whitelist and player-count checks.

use std::net::IpAddr;
use std::time::Instant;

use crate::world::Shared;
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;

/// One login a gate decides on.
pub struct Attempt<'a> {
    pub user: &'a NameAndId,
    pub address: IpAddr,
    pub permission: PermissionLevel,
    pub now: Instant,
    pub shared: &'a Shared,
}

/// Opens the coordinator's own gate state at world start.
pub trait LoginGates: Send + Sync {
    fn open(&self) -> Box<dyn LoginGate>;
}

/// A gate's state, owned and driven by the coordinator alone.
pub trait LoginGate: Send {
    /// `Err` refuses the login with that reason.
    fn admit(&mut self, attempt: &Attempt<'_>) -> Result<(), TextComponent>;

    /// A player who was in the world left it.
    fn left(&mut self, user: &NameAndId, address: IpAddr, now: Instant) {
        let _ = (user, address, now);
    }
}

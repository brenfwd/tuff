// SPDX-License-Identifier: MIT
//! Answering whoever started something after an `await`. The host's
//! `reply` only reaches the player the *current* callback acts for, and a
//! task resumed by an io-event or timer runs in a different callback, so a
//! [`Replier`] remembers the player and which callback it came from: in
//! that callback it replies, later it `tell`s the player (or logs, for the
//! console).

use std::cell::Cell;

use crate::bindings::tuff::plugin::host;
use crate::types::{LogLevel, Uuid};

thread_local! {
    static CALLBACK: Cell<u64> = const { Cell::new(0) };
}

/// Marks the start of a guest callback; [`crate::plugin!`] calls it
/// on every export.
#[doc(hidden)]
pub fn enter() {
    CALLBACK.with(|c| c.set(c.get().wrapping_add(1)));
}

fn current() -> u64 {
    CALLBACK.with(Cell::get)
}

/// Sends messages to one player (or the console), from the callback it was
/// made in or any later one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replier {
    to: Option<Uuid>,
    callback: u64,
}

impl Replier {
    /// Replies to the player with `uuid`, or the console for `None`, as
    /// seen from the current callback.
    pub fn to(to: Option<Uuid>) -> Self {
        Self {
            to,
            callback: current(),
        }
    }

    /// Replies to whoever the current callback acts for.
    pub fn acting() -> Self {
        Self::to(host::acting_player().map(|p| p.uuid))
    }

    /// The player this replies to; `None` for the console.
    pub fn player(&self) -> Option<Uuid> {
        self.to
    }

    /// Whether a reply now would go through `reply` (still in the callback
    /// it was made in) rather than `tell`.
    pub fn in_callback(&self) -> bool {
        self.callback == current()
    }

    /// Messages the player (a player who left is skipped silently); the
    /// console's messages go to the server log.
    pub fn reply(&self, text: &str) -> Result<(), crate::HostError> {
        match self.to {
            Some(_) if self.in_callback() => host::reply(text).map_err(Into::into),
            Some(uuid) => host::tell(uuid, text).map_err(Into::into),
            None => {
                host::log(LogLevel::Info, text);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_replier_knows_when_its_callback_ended() {
        enter();
        let replier = Replier::to(Some(Uuid::from_u128(3)));
        assert!(replier.in_callback());
        assert_eq!(replier.player(), Some(Uuid::from_u128(3)));
        enter();
        assert!(!replier.in_callback());
        assert!(Replier::to(None).in_callback());
    }
}

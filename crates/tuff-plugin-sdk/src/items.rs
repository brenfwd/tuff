// SPDX-License-Identifier: MIT
//! Static item facts.

use crate::{host, types};

pub use types::ItemDetails;

/// Stack size, durability and consumability of an item id; `None` for an
/// unknown item or air.
pub fn info(item: &str) -> Option<ItemDetails> {
    host::item_info(item)
}

/// Member item names of an item tag (`#` optional); `None` for an unknown
/// or empty tag.
pub fn tag(tag: &str) -> Option<Vec<String>> {
    host::item_tag(tag)
}

/// Items the plugin holds: a handle to stacks the server keeps for it.
/// They move only through these calls, so a plugin can make, move and use
/// up items but never copy one, and a `Stash` cannot be cloned either.
/// Dropping one leaves its items with the server until the plugin unloads,
/// when they are used up; end it with [`Stash::give`] or [`Stash::burn`].
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Stash(host::Stash);

impl Stash {
    /// A new, empty stash (at most 1024 at once).
    pub fn new() -> Result<Self, types::WorldError> {
        host::stash_new().map(Self)
    }

    /// The handle the server knows this stash by.
    pub fn handle(&self) -> u64 {
        self.0
    }

    /// Makes `count` (1 to 2304) new items into the stash.
    pub fn mint(&self, item: &str, count: u32) -> Result<(), types::WorldError> {
        host::stash_mint(self.0, item, count, None, None)
    }

    /// [`Stash::mint`] with custom data (SNBT of a compound) and a plain
    /// text name.
    pub fn mint_with(
        &self,
        item: &str,
        count: u32,
        custom_data: Option<&str>,
        custom_name: Option<&str>,
    ) -> Result<(), types::WorldError> {
        host::stash_mint(self.0, item, count, custom_data, custom_name)
    }

    /// Pictures of the stacks it holds.
    pub fn contents(&self) -> Vec<types::ItemStack> {
        host::stash_contents(self.0).unwrap_or_default()
    }

    /// How many items it holds.
    pub fn count(&self) -> u32 {
        self.contents()
            .iter()
            .map(|stack| u32::from(stack.count))
            .sum()
    }

    /// Moves up to `count` of `item` (of anything with `None`) into a new
    /// stash.
    pub fn split(&self, item: Option<&str>, count: u32) -> Result<Stash, types::WorldError> {
        host::stash_split(self.0, item, count).map(Self)
    }

    /// Moves everything in `other` into this stash.
    pub fn absorb(&self, other: Stash) -> Result<(), types::WorldError> {
        host::stash_merge(self.0, other.0)
    }

    /// Uses up everything it holds.
    pub fn burn(self) -> Result<(), types::WorldError> {
        host::stash_burn(self.0)
    }

    /// Queues moving everything it holds into an online player's
    /// inventory, dropping what does not fit.
    pub fn give(self, player: types::Uuid) -> Result<(), types::WorldError> {
        host::give_stash(player, self.0)
    }

    /// Queues taking up to `count` of `item` from an online player into
    /// this stash; the items arrive after the call.
    pub fn take_from(
        &self,
        player: types::Uuid,
        item: &str,
        count: u32,
        all_or_nothing: bool,
    ) -> Result<(), types::WorldError> {
        host::take_into_stash(player, item, count, all_or_nothing, self.0)
    }
}

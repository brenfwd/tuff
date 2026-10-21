// SPDX-License-Identifier: MIT
//! Plugin chest menus and entity interaction filters.
//!
//! [`open`] shows a menu of 1 to 6 rows to an online player; nothing a
//! player does in it moves an item, every click arrives as a
//! `menu-click` event ([`crate::events::on`] with [`crate::events::ev::MenuClick`]).
//! [`intercept_entity_interact`] cancels vanilla's reaction to right-clicks
//! on matching entities (a villager's trade screen) so they can be shop
//! NPCs, and reports them as `entity-interact` events.

use crate::{host, types, uuid_of};

pub use types::{MenuClickKind, MenuError, MenuItem};

/// A menu slot showing `count` of `item` (an id such as `minecraft:emerald`).
pub fn item(slot: u8, item: &str, count: u8) -> MenuItem {
    MenuItem {
        slot,
        item: item.to_owned(),
        count,
        name: None,
        lore: Vec::new(),
        components: None,
    }
}

impl MenuItem {
    /// Shows `name`, plain text, without the default italics.
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }

    /// Adds a plain text lore line.
    pub fn lore(mut self, line: &str) -> Self {
        self.lore.push(line.to_owned());
        self
    }

    /// Sets extra components as the SNBT of a compound keyed by component
    /// id.
    pub fn components(mut self, snbt: &str) -> Self {
        self.components = Some(snbt.to_owned());
        self
    }
}

/// Opens the menu `menu` for the online player `uuid`, replacing any
/// window they have open.
pub fn open(
    uuid: &str,
    menu: &str,
    rows: u8,
    title: &str,
    items: &[MenuItem],
) -> Result<(), MenuError> {
    let uuid = uuid_of(uuid).ok_or(MenuError::Offline)?;
    host::open_menu(uuid, menu, rows, title, items)
}

/// Sets one slot of the menu the player has open; `None` clears it.
pub fn update_slot(uuid: &str, slot: u8, item: Option<&MenuItem>) -> Result<(), MenuError> {
    let uuid = uuid_of(uuid).ok_or(MenuError::Offline)?;
    host::update_menu_slot(uuid, slot, item)
}

/// Closes the menu the player has open.
pub fn close(uuid: &str) {
    if let Some(uuid) = uuid_of(uuid) {
        host::close_menu(uuid);
    }
}

/// Cancels vanilla's reaction to right-clicks on entities of type `kind`
/// (any when `None`) that carry the scoreboard tag `tag` (any when
/// `None`). Call while registering.
pub fn intercept_entity_interact(
    kind: Option<&str>,
    tag: Option<&str>,
) -> Result<(), types::RegisterError> {
    intercept_entity_interact_scoped(kind, tag, crate::scope_everywhere())
}

/// [`intercept_entity_interact`] limited by `scope`; its area is tested
/// against the entity's position.
pub fn intercept_entity_interact_scoped(
    kind: Option<&str>,
    tag: Option<&str>,
    scope: types::FilterScope,
) -> Result<(), types::RegisterError> {
    let filter = types::EntityFilter {
        kind: kind.map(str::to_owned),
        tag: tag.map(str::to_owned),
        scope,
    };
    host::intercept_entity_interact(&filter)
}

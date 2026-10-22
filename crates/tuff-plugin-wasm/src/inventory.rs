//! Window click, drag, open and close events, and the inventory filters
//! (`intercept-inventory`) that cancel clicks.

use std::future::Future;
use std::pin::Pin;

use tuff_core::protocol::play::Serverbound;
use tuff_core::world::menu::MenuKind;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{PacketInterceptor, SessionHooks, Verdict};
use tuff_item::item::{ItemStack, ItemView};

use crate::host::WasmHost;
use crate::menus::WINDOW;
use crate::wit::bindings::types::{
    Event, EventKinds, InventoryClick, InventoryClickKind, InventoryFilter, InventoryWindow,
};

pub fn container_name(kind: Option<MenuKind>) -> &'static str {
    match kind {
        None | Some(MenuKind::Player) => "player",
        Some(MenuKind::Generic { .. }) => "chest",
        Some(MenuKind::ShulkerBox) => "shulker_box",
        Some(MenuKind::Hopper) => "hopper",
        Some(MenuKind::Generic3x3) => "dispenser",
        Some(MenuKind::Crafting) => "crafting_table",
        Some(MenuKind::Stonecutter) => "stonecutter",
        Some(MenuKind::Anvil) => "anvil",
        Some(MenuKind::Cartography) => "cartography_table",
        Some(MenuKind::Loom) => "loom",
        Some(MenuKind::Grindstone) => "grindstone",
        Some(MenuKind::Enchantment) => "enchantment",
        Some(MenuKind::Beacon) => "beacon",
        Some(MenuKind::Smithing) => "smithing_table",
        Some(MenuKind::Furnace) => "furnace",
        Some(MenuKind::BlastFurnace) => "blast_furnace",
        Some(MenuKind::Smoker) => "smoker",
        Some(MenuKind::BrewingStand) => "brewing_stand",
        Some(MenuKind::Merchant) => "merchant",
        Some(MenuKind::Crafter) => "crafter",
        Some(MenuKind::Lectern) => "lectern",
        Some(MenuKind::Mount { .. }) => "mount",
    }
}

/// The click kind for a click packet's mode, and whether it ends a drag
/// (or is not a drag at all) so it is reported.
fn click_kind(mode: i32, slot: i16) -> Option<InventoryClickKind> {
    Some(match mode {
        0 if slot == -999 => InventoryClickKind::Drop,
        0 => InventoryClickKind::Pickup,
        1 => InventoryClickKind::QuickMove,
        2 => InventoryClickKind::Swap,
        3 => InventoryClickKind::Clone,
        4 => InventoryClickKind::Drop,
        5 => InventoryClickKind::Drag,
        6 => InventoryClickKind::Double,
        _ => return None,
    })
}

fn ends_drag(button: i8) -> bool {
    matches!(button, 2 | 6 | 10)
}

pub fn matches(
    filter: &InventoryFilter,
    container: &str,
    slot: i16,
    click: InventoryClickKind,
    item: Option<&str>,
) -> bool {
    filter.container.as_deref().is_none_or(|c| c == container)
        && filter.first_slot.is_none_or(|first| slot >= first)
        && filter.last_slot.is_none_or(|last| slot <= last)
        && filter.click.is_none_or(|wanted| wanted == click)
        && filter
            .item
            .as_deref()
            .is_none_or(|wanted| Some(wanted) == item)
}

/// A filter naming only a container bars opening it.
fn bars_open(filter: &InventoryFilter, container: &str) -> bool {
    filter.container.as_deref() == Some(container)
        && filter.first_slot.is_none()
        && filter.last_slot.is_none()
        && filter.click.is_none()
        && filter.item.is_none()
}

/// A filter naming a container and the close click bars closing it.
fn bars_close(filter: &InventoryFilter, container: &str) -> bool {
    filter.container.as_deref() == Some(container)
        && filter.first_slot.is_none()
        && filter.last_slot.is_none()
        && filter.item.is_none()
        && filter.click == Some(InventoryClickKind::Close)
}

fn slot_stack(session: &PlayerSession, kind: Option<MenuKind>, slot: i16) -> Option<ItemView<'_>> {
    let slot = usize::try_from(slot).ok()?;
    let inventory = |index: usize| {
        session
            .inventory
            .slots()
            .get(index)?
            .as_ref()
            .map(ItemStack::view)
    };
    match kind {
        None => inventory(slot),
        Some(kind) => {
            let container = session.menu_container()?;
            if let Some(stack) = container.get(slot) {
                return stack.as_ref().map(ItemStack::view);
            }
            inventory(kind.inventory_slot(slot)?)
        }
    }
}

/// Reports window clicks and closes and cancels clicks a filter matches.
pub struct Clicks;

impl PacketInterceptor for Clicks {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        let verdict = match packet {
            Serverbound::ContainerClick {
                window_id,
                slot,
                button,
                mode,
                ..
            } => click(session, *window_id, *slot, *button, *mode),
            Serverbound::ContainerClose(0) if session.menu_kind().is_none() => {
                closed(session, None);
                Verdict::Allow
            }
            _ => Verdict::Allow,
        };
        Box::pin(async move { verdict })
    }
}

fn click(session: &mut PlayerSession, window: i32, slot: i16, button: i8, mode: i32) -> Verdict {
    let kind = session.menu_kind();
    let in_window = match kind {
        None => window == 0,
        Some(_) => window != WINDOW && session.window_id() == Some(window),
    };
    if !in_window {
        return Verdict::Allow;
    }
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    if !host.listens(EventKinds::INVENTORY_CLICK) {
        return Verdict::Allow;
    }
    let Some(click) = click_kind(mode, slot) else {
        return Verdict::Allow;
    };
    let container = container_name(kind);
    let stack = slot_stack(session, kind, slot)
        .or_else(|| session.inventory.carried().map(ItemStack::view));
    let item = stack.as_ref().map(|stack| stack.item().name());
    let cancel = host
        .snapshot()
        .inventory
        .iter()
        .any(|filter| matches(filter, container, slot, click, item));
    if click != InventoryClickKind::Drag || ends_drag(button) {
        let who = crate::bridge::player(session);
        host.event(
            Event::InventoryClick(InventoryClick {
                player: who.to_wit(),
                container: container.to_owned(),
                slot,
                click,
                item: item.unwrap_or_default().to_owned(),
                count: stack.as_ref().map_or(0, |stack| stack.count()),
                cancelled: cancel,
            }),
            Some(who),
        );
    }
    if !cancel {
        return Verdict::Allow;
    }
    if kind.is_none() {
        session.send_inventory();
    } else {
        session.send_menu_content();
    }
    Verdict::Cancel
}

fn announce(session: &PlayerSession, kind: Option<MenuKind>, open: bool) {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return;
    };
    let flag = if open {
        EventKinds::INVENTORY_OPEN
    } else {
        EventKinds::INVENTORY_CLOSE
    };
    if !host.listens(flag) {
        return;
    }
    let who = crate::bridge::player(session);
    let window = InventoryWindow {
        player: who.to_wit(),
        container: container_name(kind).to_owned(),
    };
    let event = if open {
        Event::InventoryOpen(window)
    } else {
        Event::InventoryClose(window)
    };
    host.event(event, Some(who));
}

fn closed(session: &PlayerSession, kind: Option<MenuKind>) {
    announce(session, kind, false);
}

/// Reports container windows opening and closing.
pub struct Windows;

impl SessionHooks for Windows {
    fn allows_menu_open(&self, session: &PlayerSession, kind: MenuKind) -> bool {
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return true;
        };
        let container = container_name(Some(kind));
        !host
            .snapshot()
            .inventory
            .iter()
            .any(|filter| bars_open(filter, container))
    }

    fn allows_menu_close(&self, session: &PlayerSession, kind: MenuKind) -> bool {
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return true;
        };
        let container = container_name(Some(kind));
        !host
            .snapshot()
            .inventory
            .iter()
            .any(|filter| bars_close(filter, container))
    }

    fn menu_opened(&self, session: &PlayerSession, kind: MenuKind) {
        announce(session, Some(kind), true);
    }

    fn menu_closed(&self, session: &PlayerSession, kind: MenuKind) {
        closed(session, Some(kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> InventoryFilter {
        InventoryFilter {
            container: Some("chest".into()),
            first_slot: Some(2),
            last_slot: Some(4),
            item: Some("minecraft:stick".into()),
            click: Some(InventoryClickKind::QuickMove),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_field_narrows_the_match() {
        let stick = Some("minecraft:stick");
        let quick = InventoryClickKind::QuickMove;
        assert!(matches(&filter(), "chest", 3, quick, stick));
        assert!(!matches(&filter(), "hopper", 3, quick, stick));
        assert!(!matches(&filter(), "chest", 5, quick, stick));
        assert!(!matches(&filter(), "chest", 1, quick, stick));
        assert!(!matches(
            &filter(),
            "chest",
            3,
            InventoryClickKind::Swap,
            stick
        ));
        assert!(!matches(&filter(), "chest", 3, quick, None));
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_container_wide_filters_bar_opening() {
        let bare = InventoryFilter {
            container: Some("hopper".into()),
            first_slot: None,
            last_slot: None,
            item: None,
            click: None,
        };
        assert!(bars_open(&bare, "hopper"));
        assert!(!bars_open(&bare, "chest"));
        assert!(!bars_open(&filter(), "chest"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn modes_map_to_click_kinds_and_drags_end_on_release() {
        assert_eq!(click_kind(0, -999), Some(InventoryClickKind::Drop));
        assert_eq!(click_kind(2, 3), Some(InventoryClickKind::Swap));
        assert_eq!(click_kind(7, 3), None);
        assert!(ends_drag(2) && ends_drag(6) && ends_drag(10));
        assert!(!ends_drag(1));
    }
}

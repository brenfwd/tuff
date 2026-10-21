//! Plugin chest menus and entity interaction filters.
//!
//! A plugin opens a menu for a player; the session shows it on its next
//! tick, every click on it is cancelled and reported, and nothing in it
//! ever moves. The open menus are a small table the session hooks read and
//! the host writes under a short lock, never held across an await.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tuff_core::custom::CustomContent;
use tuff_core::custom::catalog::{self, Catalog, CatalogClick};
use tuff_core::protocol::play::{self, Serverbound};
use tuff_core::world::commands::text;
use tuff_core::world::menu::MenuKind;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{
    HookFuture, PacketInterceptor, SessionHooks, SessionTick, Verdict,
};
use tuff_item::item::{BurnReason, ItemSnapshot, ItemStack, ItemView, MintReason};
use tuff_nbt::{Compound, Tag};

use crate::abi;
use crate::bridge::{entity_of, player};
use crate::host::WasmHost;
use crate::wit::bindings::types::{
    EntityInteract, Event, EventKinds, MenuClick, MenuClickKind, MenuClose, MenuItem,
};

/// The window id of a plugin menu: past the 1 to 100 the server cycles.
pub const WINDOW: i32 = 101;
pub const CATALOG_MENU: &str = "tuff:catalog";
const MAX_ROWS: u8 = 6;
const PLAYER_SLOTS: std::ops::Range<usize> = 9..45;

/// What a plugin asked of a player's menu.
#[derive(Debug, PartialEq)]
pub enum MenuOp {
    Open {
        player: u128,
        menu: String,
        rows: u8,
        title: String,
        slots: Vec<(u8, ItemSnapshot)>,
    },
    Set {
        player: u128,
        slot: u8,
        stack: Option<ItemSnapshot>,
    },
    Close {
        player: u128,
    },
}

/// A right-click filter a plugin declared.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityFilter {
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub scope: crate::combat::Scope,
}

impl EntityFilter {
    fn matches(&self, session: &PlayerSession, kind: &str, tags: &[String], at: [f64; 3]) -> bool {
        self.scope.applies(session, at)
            && self.kind.as_deref().is_none_or(|wanted| wanted == kind)
            && self
                .tag
                .as_deref()
                .is_none_or(|wanted| tags.iter().any(|tag| tag == wanted))
    }
}

#[derive(Debug)]
struct Open {
    menu: String,
    rows: u8,
    slots: Vec<Option<ItemSnapshot>>,
    state_id: i32,
    shown: bool,
    title: String,
    dirty: BTreeSet<u8>,
    catalog: Option<Catalog>,
}

#[derive(Debug, Default)]
struct Entry {
    open: Option<Open>,
    close: Option<String>,
}

/// The menus players have open.
#[derive(Default)]
pub struct Menus {
    entries: Mutex<HashMap<u128, Entry>>,
    active: AtomicUsize,
}

impl Menus {
    pub fn apply(&self, op: MenuOp) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match op {
            MenuOp::Open {
                player,
                menu,
                rows,
                title,
                slots,
            } => {
                let mut cells: Vec<Option<ItemSnapshot>> =
                    (0..usize::from(rows) * 9).map(|_| None).collect();
                for (slot, stack) in slots {
                    cells[usize::from(slot)] = Some(stack);
                }
                let entry = entries.entry(player).or_default();
                entry.close = None;
                entry.open = Some(Open {
                    menu,
                    rows,
                    slots: cells,
                    state_id: 0,
                    shown: false,
                    title,
                    dirty: BTreeSet::new(),
                    catalog: None,
                });
            }
            MenuOp::Set {
                player,
                slot,
                stack,
            } => {
                if let Some(open) = entries.get_mut(&player).and_then(|e| e.open.as_mut())
                    && let Some(cell) = open.slots.get_mut(usize::from(slot))
                {
                    *cell = stack;
                    open.dirty.insert(slot);
                }
            }
            MenuOp::Close { player } => {
                if let Some(entry) = entries.get_mut(&player)
                    && let Some(open) = entry.open.take()
                {
                    entry.close = Some(open.menu);
                }
            }
        }
        self.active.store(entries.len(), Ordering::Relaxed);
    }

    /// Opens the custom item catalog on `page` for a player.
    pub fn open_catalog(&self, player: u128, page: usize, content: &CustomContent) {
        let catalog = Catalog { page };
        let mut cells: Vec<Option<ItemSnapshot>> =
            (0..usize::from(catalog::ROWS) * 9).map(|_| None).collect();
        for (slot, stack) in catalog.slots(content) {
            cells[usize::from(slot)] = Some(ItemSnapshot::of(&stack));
            stack.burn(BurnReason::Template);
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(player).or_default();
        entry.close = None;
        let state_id = entry.open.as_ref().map_or(0, |open| open.state_id);
        entry.open = Some(Open {
            menu: CATALOG_MENU.to_owned(),
            rows: catalog::ROWS,
            slots: cells,
            state_id,
            shown: false,
            title: catalog.title(content),
            dirty: BTreeSet::new(),
            catalog: Some(catalog),
        });
        self.active.store(entries.len(), Ordering::Relaxed);
    }

    /// Resolves a click on the player's open catalog, if it is one: paging
    /// reopens it on the new page.
    pub fn catalog_click(
        &self,
        player: u128,
        slot: u8,
        content: &CustomContent,
    ) -> Option<CatalogClick> {
        let catalog = {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.get(&player)?.open.as_ref()?.catalog?
        };
        let result = catalog.click(content, slot);
        if let CatalogClick::Page(page) = result {
            self.open_catalog(player, page, content);
        }
        Some(result)
    }

    fn quiet(&self) -> bool {
        self.active.load(Ordering::Relaxed) == 0
    }

    pub fn open_menu(&self, player: u128) -> Option<(String, u8)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let open = entries.get(&player)?.open.as_ref()?;
        Some((open.menu.clone(), open.rows))
    }

    /// The contents to resend after a click: the menu's slots then the
    /// player's. Bumps the state id.
    fn contents(&self, player: u128) -> Option<(i32, Vec<Option<ItemSnapshot>>)> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let open = entries.get_mut(&player)?.open.as_mut()?;
        open.state_id += 1;
        Some((open.state_id, open.slots.clone()))
    }

    /// Takes a player's closed menu out of the table.
    fn closed(&self, player: u128) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get_mut(&player)?;
        let menu = entry.open.take().map(|open| open.menu);
        if entry.close.is_none() {
            entries.remove(&player);
        }
        self.active.store(entries.len(), Ordering::Relaxed);
        menu
    }

    /// What the session must send now: the packets of a fresh menu, slot
    /// updates, or a close.
    fn due(&self, player: u128) -> Due {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.get_mut(&player) else {
            return Due::Nothing;
        };
        if let Some(menu) = entry.close.take() {
            if entry.open.is_none() {
                entries.remove(&player);
            }
            self.active.store(entries.len(), Ordering::Relaxed);
            return Due::Close(menu);
        }
        let Some(open) = entry.open.as_mut() else {
            return Due::Nothing;
        };
        if !open.shown {
            open.shown = true;
            open.dirty.clear();
            open.state_id += 1;
            return Due::Show {
                rows: open.rows,
                title: open.title.clone(),
                state_id: open.state_id,
                slots: open.slots.clone(),
            };
        }
        if open.dirty.is_empty() {
            return Due::Nothing;
        }
        open.state_id += 1;
        let changed = std::mem::take(&mut open.dirty)
            .into_iter()
            .map(|slot| (slot, open.slots[usize::from(slot)].clone()))
            .collect();
        Due::Update {
            state_id: open.state_id,
            changed,
        }
    }

    fn forget(&self, player: u128) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(&player);
        self.active.store(entries.len(), Ordering::Relaxed);
    }
}

enum Due {
    Nothing,
    Show {
        rows: u8,
        title: String,
        state_id: i32,
        slots: Vec<Option<ItemSnapshot>>,
    },
    Update {
        state_id: i32,
        changed: Vec<(u8, Option<ItemSnapshot>)>,
    },
    Close(String),
}

fn component(text: &str) -> Tag {
    let mut compound = Compound::new();
    compound.insert("text", Tag::String(text.to_owned()));
    compound.insert("italic", Tag::Byte(0));
    Tag::Compound(compound)
}

/// The stack a menu item describes, or `None` for an unknown item, bad
/// SNBT or oversized text.
pub fn build_stack(item: &MenuItem) -> Option<ItemSnapshot> {
    let too_long = |text: &str| text.len() > abi::MAX_GIVE_TEXT;
    if item.count == 0
        || item.lore.len() > abi::MAX_MENU_LORE
        || item.name.as_deref().is_some_and(too_long)
        || item.lore.iter().any(|line| too_long(line))
        || item.components.as_deref().is_some_and(too_long)
    {
        return None;
    }
    let kind =
        <tuff_vanilla_data::Item as tuff_vanilla_data::registry::Registry>::from_name(&item.item)
            .filter(|kind| *kind != tuff_vanilla_data::Item::Air)?;
    let mut components = Compound::new();
    if let Some(name) = &item.name {
        components.insert("minecraft:custom_name", component(name));
    }
    if !item.lore.is_empty() {
        let lines = item.lore.iter().map(|line| component(line)).collect();
        components.insert("minecraft:lore", Tag::List(lines));
    }
    if let Some(extra) = &item.components {
        let Ok(Tag::Compound(extra)) = Tag::from_snbt(extra) else {
            return None;
        };
        for (key, value) in extra.iter() {
            components.insert(key, value.clone());
        }
    }
    let mut nbt = Compound::new();
    nbt.insert("id", Tag::String(kind.name().to_owned()));
    nbt.insert("count", Tag::Int(i32::from(item.count)));
    nbt.insert("components", Tag::Compound(components));
    let stack = ItemStack::mint_from_nbt(&nbt, MintReason::Template)?;
    let shown = ItemSnapshot::of(&stack);
    stack.burn(BurnReason::Template);
    Some(shown)
}

fn click_kind(mode: i32, button: i8) -> Option<MenuClickKind> {
    Some(match (mode, button) {
        (0, 0) => MenuClickKind::Left,
        (0, 1) => MenuClickKind::Right,
        (1, 0) => MenuClickKind::ShiftLeft,
        (1, 1) => MenuClickKind::ShiftRight,
        (2, _) => MenuClickKind::HotbarSwap,
        (3, _) => MenuClickKind::Middle,
        (4, 0) => MenuClickKind::Drop,
        (4, 1) => MenuClickKind::DropAll,
        (5, _) => MenuClickKind::Drag,
        (6, _) => MenuClickKind::Double,
        _ => return None,
    })
}

fn send_contents(session: &mut PlayerSession, state_id: i32, menu: Vec<Option<ItemSnapshot>>) {
    let slots: Vec<Option<ItemView<'_>>> = menu
        .iter()
        .map(|slot| slot.as_ref().map(ItemSnapshot::view))
        .chain(
            session.inventory.slots()[PLAYER_SLOTS]
                .iter()
                .map(|slot| slot.as_ref().map(ItemStack::view)),
        )
        .collect();
    let packet = play::ContainerSetContent {
        window_id: WINDOW,
        state_id,
        slots: slots.as_slice().into(),
        carried: None,
    };
    let bytes = tuff_core::world::framed(&packet);
    session.send_raw(bytes);
}

/// Shows, updates and closes the menus plugins opened, once per session
/// tick.
pub struct MenuSessions;

impl SessionHooks for MenuSessions {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase != SessionTick::Use {
                return;
            }
            let shared = session.shared.clone();
            let Some(host) = shared.services.get::<WasmHost>() else {
                return;
            };
            let menus = host.menus();
            if menus.quiet() {
                return;
            }
            let uuid = session.profile.id;
            match menus.due(uuid) {
                Due::Nothing => {}
                Due::Show {
                    rows,
                    title,
                    state_id,
                    slots,
                } => {
                    let Some(kind) = (MenuKind::Generic { rows }).menu_type() else {
                        return;
                    };
                    session.send(&play::OpenScreen {
                        window_id: WINDOW,
                        menu_type: kind.id(),
                        title: &text(title),
                    });
                    send_contents(session, state_id, slots);
                }
                Due::Update { state_id, changed } => {
                    for (slot, item) in changed {
                        session.send(&play::ContainerSetSlot {
                            window_id: WINDOW,
                            state_id,
                            slot: i16::from(slot),
                            item: item.as_ref().map(ItemSnapshot::view),
                        });
                    }
                }
                Due::Close(menu) => {
                    session.send(&play::ContainerClose(WINDOW));
                    if host.listens(EventKinds::MENU_CLOSE) {
                        let who = player(session);
                        host.event(
                            Event::MenuClose(MenuClose {
                                player: who.to_wit(),
                                menu,
                            }),
                            Some(who),
                        );
                    }
                }
            }
        })
    }

    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            if let Some(host) = session.shared.services.get::<WasmHost>() {
                host.menus().forget(session.profile.id);
            }
        })
    }
}

/// Cancels every click on a plugin menu and reports it, and reports and
/// optionally cancels right-clicks on entities.
pub struct MenuClicks;

impl PacketInterceptor for MenuClicks {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        Box::pin(async move {
            match packet {
                Serverbound::ContainerClick {
                    window_id: WINDOW,
                    slot,
                    button,
                    mode,
                    ..
                } => click(session, *slot, *button, *mode).await,
                Serverbound::ContainerClose(WINDOW) => close(session),
                Serverbound::Interact {
                    entity, off_hand, ..
                } => interact(session, *entity, *off_hand).await,
                _ => Verdict::Allow,
            }
        })
    }
}

async fn click(session: &mut PlayerSession, slot: i16, button: i8, mode: i32) -> Verdict {
    let shared = session.shared.clone();
    let Some(host) = shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    let uuid = session.profile.id;
    let Some((menu, rows)) = host.menus().open_menu(uuid) else {
        return Verdict::Allow;
    };
    if let Ok(slot) = u8::try_from(slot)
        && let Some(result) = host.menus().catalog_click(uuid, slot, &shared.custom)
        && click_kind(mode, button) == Some(MenuClickKind::Left)
        && let CatalogClick::Take(stack) = result
    {
        let message = shared
            .custom
            .item_of(&stack)
            .map(|def| Catalog::taken_message(&shared.custom, &def.id));
        if let Some(extra) = session.inventory.add(stack) {
            session.throw_item(extra).await;
        }
        session.send_inventory();
        if let Some(message) = message {
            session.system_message(&text(message));
        }
    }
    if let Some((state_id, slots)) = host.menus().contents(uuid) {
        send_contents(session, state_id, slots);
    }
    session.send(&play::ContainerSetSlot {
        window_id: -1,
        state_id: 0,
        slot: -1,
        item: None,
    });
    let inside = usize::try_from(slot).is_ok_and(|slot| slot < usize::from(rows) * 9);
    if let (true, Some(kind)) = (inside, click_kind(mode, button))
        && host.listens(EventKinds::MENU_CLICK)
    {
        let who = player(session);
        host.event(
            Event::MenuClick(MenuClick {
                player: who.to_wit(),
                menu,
                slot: slot as u8,
                click: kind,
            }),
            Some(who),
        );
    }
    Verdict::Cancel
}

fn close(session: &mut PlayerSession) -> Verdict {
    let shared = session.shared.clone();
    let Some(host) = shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    if let Some(menu) = host.menus().closed(session.profile.id)
        && host.listens(EventKinds::MENU_CLOSE)
    {
        let who = player(session);
        host.event(
            Event::MenuClose(MenuClose {
                player: who.to_wit(),
                menu,
            }),
            Some(who),
        );
    }
    Verdict::Cancel
}

async fn interact(session: &mut PlayerSession, target: i32, off_hand: bool) -> Verdict {
    let shared = session.shared.clone();
    let Some(host) = shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    let filters = host.snapshot().entity_filters.clone();
    let reporting = host.listens(EventKinds::ENTITY_INTERACT);
    if filters.is_empty() && !reporting {
        return Verdict::Allow;
    }
    let Some((kind, chunk, at)) = session.view.entities.visible_entity(target) else {
        return Verdict::Allow;
    };
    let may_match = filters
        .iter()
        .any(|filter| filter.kind.as_deref().is_none_or(|k| k == kind.name()));
    if !may_match && !(reporting && !off_hand) {
        return Verdict::Allow;
    }
    let Some(region) = session.region(chunk.region()).await else {
        return Verdict::Allow;
    };
    let Some(Some((entity, tags))) = region
        .ask(move |world| {
            world.entity(target).map(|tracked| {
                (
                    entity_of(&*world, &*tracked.entity),
                    tracked.entity.base().tags.clone(),
                )
            })
        })
        .await
    else {
        return Verdict::Allow;
    };
    let cancelled = filters
        .iter()
        .any(|filter| filter.matches(session, &entity.kind, &tags, at));
    if reporting && !off_hand {
        let who = player(session);
        host.event(
            Event::EntityInteract(EntityInteract {
                player: who.to_wit(),
                entity: entity.uuid,
                kind: entity.kind,
                cancelled,
            }),
            Some(who),
        );
    }
    if cancelled {
        Verdict::Cancel
    } else {
        Verdict::Allow
    }
}

pub fn valid_rows(rows: u8) -> bool {
    (1..=MAX_ROWS).contains(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::custom::{CustomItemDef, Definitions};
    use tuff_vanilla_data::Item;

    fn content(items: usize) -> std::sync::Arc<CustomContent> {
        let mut defs = Definitions::default();
        for i in 0..items {
            let id = format!("ex:i{i:03}");
            defs.items
                .register("ex", &id, CustomItemDef::new(&id, Item::Paper))
                .unwrap();
        }
        defs.resolve().unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn catalog_opens_and_pages() {
        let content = content(50);
        let menus = Menus::default();
        menus.open_catalog(7, 0, &content);
        assert_eq!(menus.open_menu(7), Some((CATALOG_MENU.to_owned(), 6)));
        assert!(matches!(
            menus.catalog_click(7, 0, &content),
            Some(CatalogClick::Take(_))
        ));
        assert_eq!(
            menus.catalog_click(7, catalog::NEXT_SLOT, &content),
            Some(CatalogClick::Page(1))
        );
        assert_eq!(
            menus.catalog_click(7, catalog::PREVIOUS_SLOT, &content),
            Some(CatalogClick::Page(0))
        );
        assert_eq!(menus.catalog_click(8, 0, &content), None);
    }
}

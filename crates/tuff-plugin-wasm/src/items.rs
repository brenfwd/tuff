//! Item drop, pickup, swap-hands and crafting events, and the item
//! filters (`intercept-item`) that cancel them.

use std::future::Future;
use std::pin::Pin;
use tuff_item::item::ItemSnapshot;

use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_core::world::inventory::Inventory;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{PacketInterceptor, SessionHooks, Verdict};

use crate::host::WasmHost;
use crate::wit::bindings::types::{Event, EventKinds, ItemAction, ItemActionKind, ItemsTaken};

pub fn kind_flag(kind: ItemActionKind) -> EventKinds {
    match kind {
        ItemActionKind::Drop => EventKinds::ITEM_DROP,
        ItemActionKind::Pickup => EventKinds::ITEM_PICKUP,
        ItemActionKind::Swap => EventKinds::ITEM_SWAP,
        ItemActionKind::Craft => EventKinds::ITEM_CRAFT,
    }
}

fn has_tag(stack: tuff_item::item::ItemView<'_>, tag: &str) -> bool {
    stack
        .components
        .other
        .get("minecraft:custom_data")
        .and_then(|data| data.as_compound())
        .is_some_and(|data| data.get(tag).is_some())
}

/// Reports the action when plugins listen; whether a filter cancels it.
fn act(
    session: &PlayerSession,
    kind: ItemActionKind,
    stack: tuff_item::item::ItemView<'_>,
    count: u8,
) -> bool {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return false;
    };
    let flag = kind_flag(kind);
    if !host.listens(flag) {
        return false;
    }
    let item = stack.item.name();
    let cancel = host.snapshot().items.iter().any(|filter| {
        filter.kind == kind
            && filter.item.as_deref().is_none_or(|wanted| wanted == item)
            && filter.tag.as_deref().is_none_or(|tag| has_tag(stack, tag))
            && filter.scope.applies_to_player(session)
    });
    let who = crate::bridge::player(session);
    let action = ItemAction {
        player: who.to_wit(),
        item: item.to_owned(),
        count,
        dimension: session.shared.dimensions.get(session.dimension).key.clone(),
    };
    let event = match kind {
        ItemActionKind::Drop => Event::ItemDrop(action),
        ItemActionKind::Pickup => Event::ItemPickup(action),
        ItemActionKind::Swap => Event::ItemSwap(action),
        ItemActionKind::Craft => Event::ItemCraft(action),
    };
    host.event(event, Some(who));
    cancel
}

/// Reports and cancels drops, hand swaps and result takes from every
/// recipe station (crafting grids, stonecutter, smithing, furnaces,
/// cartography, anvil, merchant).
pub struct Actions;

impl PacketInterceptor for Actions {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        let verdict = match packet {
            Serverbound::PlayerAction { status, .. } => hand_action(session, *status),
            Serverbound::ContainerClick {
                window_id, slot, ..
            } => craft(session, *window_id, *slot),
            _ => Verdict::Allow,
        };
        Box::pin(async move { verdict })
    }
}

fn hand_action(session: &mut PlayerSession, status: PlayerActionStatus) -> Verdict {
    let main = session.inventory.slots()[Inventory::hotbar_slot(session.inventory.selected())]
        .as_ref()
        .map(ItemSnapshot::of);
    let cancel = match status {
        PlayerActionStatus::DropItem | PlayerActionStatus::DropAllItems => {
            let Some(stack) = main else {
                return Verdict::Allow;
            };
            let count = if status == PlayerActionStatus::DropItem {
                1
            } else {
                stack.count()
            };
            act(session, ItemActionKind::Drop, stack.view(), count)
        }
        PlayerActionStatus::SwapItemWithOffhand => {
            let Some(stack) = main.or_else(|| session.inventory.hand(true).map(ItemSnapshot::of))
            else {
                return Verdict::Allow;
            };
            act(session, ItemActionKind::Swap, stack.view(), stack.count())
        }
        _ => return Verdict::Allow,
    };
    if !cancel {
        return Verdict::Allow;
    }
    session.send_inventory();
    Verdict::Cancel
}

fn craft(session: &mut PlayerSession, window: i32, slot: i16) -> Verdict {
    let result = match session.menu_kind() {
        None if window == 0 && slot == 0 => session.inventory.slots()[0].as_ref().map(ItemSnapshot::of),
        Some(kind)
            if session.window_id() == Some(window)
                && kind.result_slot().is_some_and(|r| r as i16 == slot) =>
        {
            session.menu_container().and_then(|container| {
                container
                    .get(slot as usize)
                    .and_then(Option::as_ref)
                    .map(ItemSnapshot::of)
            })
        }
        _ => return Verdict::Allow,
    };
    let Some(stack) = result else {
        return Verdict::Allow;
    };
    if !act(session, ItemActionKind::Craft, stack.view(), stack.count()) {
        return Verdict::Allow;
    }
    if window == 0 {
        session.send_inventory();
    } else {
        session.send_menu_content();
    }
    Verdict::Cancel
}

/// Reports and cancels pickups of dropped items. The event fires when the
/// pickup is offered and room exists, so a plugin that allows it sees the
/// event just before the item is taken; cancelling is only possible then.
pub struct Pickups;

impl SessionHooks for Pickups {
    fn items_taken(
        &self,
        session: &PlayerSession,
        item: tuff_vanilla_data::Item,
        requested: u32,
        removed: u32,
    ) {
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.listens(EventKinds::ITEMS_TAKEN) {
            return;
        }
        let who = crate::bridge::player(session);
        host.event(
            Event::ItemsTaken(ItemsTaken {
                player: who.to_wit(),
                item: item.name().to_owned(),
                requested,
                removed,
                dimension: session.shared.dimensions.get(session.dimension).key.clone(),
            }),
            Some(who),
        );
    }

    fn allows_item_pickup(
        &self,
        session: &PlayerSession,
        stack: tuff_item::item::ItemView<'_>,
    ) -> bool {
        if session.inventory.room_for(stack.get()) == 0 {
            return true;
        }
        !act(session, ItemActionKind::Pickup, stack, stack.count())
    }
}

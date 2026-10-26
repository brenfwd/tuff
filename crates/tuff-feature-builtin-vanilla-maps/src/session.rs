//! The map behaviour of a player session (carrying maps, making a map from
//! an empty one) and of regions (banner clicks).

use tuff_core::protocol::play::GameMode;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::messages::{ActionOutcome, HandEffect, Place};
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_core::world::stats::Stat;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{SoundEvent, StatType};
use tuff_vanilla_data::{BlockState, Item};

use tuff_core::world::menu::cartography;
use tuff_core::world::session::Station;

use crate::crafted::{cartography_result, extends, post_processing, take_post_processing};
use crate::service::{DECORATIONS, StaticMarker};
use crate::service::{Derivation, Hold, MapService, banner_at, banner_color};

pub struct MapSessionHooks;

impl SessionHooks for MapSessionHooks {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase == SessionTick::Late {
                fill_cartography(session).await;
                check_extending(session).await;
                post_process(session).await;
                report_carried(session);
            }
        })
    }

    fn use_item<'a>(&'a self, session: &'a mut PlayerSession, off_hand: bool) -> HookFuture<'a> {
        Box::pin(use_empty_map(session, off_hand))
    }
}

fn hides_marker(session: &PlayerSession) -> bool {
    [
        EquipmentSlot::Head,
        EquipmentSlot::Chest,
        EquipmentSlot::Legs,
        EquipmentSlot::Feet,
    ]
    .into_iter()
    .any(|slot| {
        session.inventory.equipped(slot).is_some_and(|stack| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::item::key::MAP_INVISIBILITY_EQUIPMENT,
                stack.item(),
            )
        })
    })
}

fn report_carried(session: &mut PlayerSession) {
    let shared = session.shared.clone();
    let Some(service) = shared.services.get::<MapService>() else {
        return;
    };
    let selected = Inventory::hotbar_slot(session.inventory.selected());
    let mut carried: Vec<(u32, bool, Vec<StaticMarker>)> = Vec::new();
    for (slot, stack) in session.inventory.slots().iter().enumerate() {
        let Some(id) = stack
            .as_ref()
            .filter(|stack| is_map(stack))
            .and_then(|stack| stack.components.map_id)
        else {
            continue;
        };
        let in_hand = slot == selected || slot == Inventory::OFFHAND_SLOT;
        match carried.iter_mut().find(|(known, _, _)| *known == id) {
            Some((_, hand, _)) => *hand |= in_hand,
            None => {
                let decorations = stack
                    .as_ref()
                    .and_then(|stack| stack.components.other.get(DECORATIONS))
                    .and_then(|tag| tag.as_compound())
                    .map(StaticMarker::read_all)
                    .unwrap_or_default();
                carried.push((id, in_hand, decorations))
            }
        }
    }
    if carried.is_empty() {
        return;
    }
    let hidden = hides_marker(session);
    for (id, render, decorations) in carried {
        service.hold(Hold {
            id,
            player: session.id,
            name: session.profile.name.clone(),
            outbound: session.conn.outbound.clone(),
            dimension: session.dimension,
            position: session.pose.position,
            yaw: session.pose.yaw,
            render,
            hidden,
            decorations,
        });
    }
}

pub fn is_map(stack: &ItemStack) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::item::key::CLONABLE_MAPS,
        stack.item(),
    )
}

/// The cartography table's result, worked out again after its inputs
/// changed.
async fn fill_cartography(session: &mut PlayerSession) {
    let Some(menu) = session.menus.menu.as_mut() else {
        return;
    };
    let window = menu.window_id;
    let Some(Station::Cartography { inputs, stale }) = &mut menu.station else {
        return;
    };
    if !std::mem::take(stale) {
        return;
    }
    let [Some(map), Some(additional)] = (**inputs).clone() else {
        return;
    };
    let Some(id) = map.components.map_id else {
        return;
    };
    let shared = session.shared.clone();
    let Some(service) = shared.services.get::<MapService>() else {
        return;
    };
    let Some(data) = service.data(id).await else {
        return;
    };
    let result = cartography_result(&map, &additional, &data);
    let Some(menu) = session.menus.menu.as_mut() else {
        return;
    };
    let current = matches!(
        &menu.station,
        Some(Station::Cartography { inputs, stale: false })
            if inputs[0].as_ref() == Some(&map) && inputs[1].as_ref() == Some(&additional)
    );
    if menu.window_id != window || !current || menu.container[cartography::RESULT] == result {
        if let Some(result) = result {
            result.burn(tuff_item::item::BurnReason::Preview);
        }
        return;
    }
    if let Some(old) = std::mem::replace(&mut menu.container[cartography::RESULT], result) {
        old.burn(tuff_item::item::BurnReason::Preview);
    }
    session.send_menu_slot(cartography::RESULT);
}

#[derive(Default)]
struct Extending {
    checked: Option<tuff_item::item::ItemSnapshot>,
}

/// Clears a crafting table's map extending result whose map cannot grow,
/// which the recipe alone cannot see.
async fn check_extending(session: &mut PlayerSession) {
    let Some(menu) = session.menus.menu.as_ref() else {
        return;
    };
    if !matches!(menu.station, Some(Station::Crafting)) {
        return;
    }
    let window = menu.window_id;
    let Some(result) = menu.container[0]
        .as_ref()
        .filter(|stack| post_processing(stack) == Some(Derivation::ZoomOut))
        .map(tuff_item::item::ItemSnapshot::of)
    else {
        return;
    };
    if session.ext.entry::<Extending>().checked.as_ref() == Some(&result) {
        return;
    }
    let shared = session.shared.clone();
    let Some(service) = shared.services.get::<MapService>() else {
        return;
    };
    let data = match result.components.map_id {
        Some(id) => service.data(id).await,
        None => None,
    };
    let Some(menu) = session.menus.menu.as_mut() else {
        return;
    };
    if menu.window_id != window
        || !tuff_item::item::same_slot(menu.container[0].as_ref(), Some(&*result))
    {
        return;
    }
    if extends(data.as_ref()) {
        session.ext.entry::<Extending>().checked = Some(result);
    } else {
        if let Some(old) = menu.container[0].take() {
            old.burn(tuff_item::item::BurnReason::Preview);
        }
        session.send_menu_slot(0);
    }
}

/// Turns the maps a player took from a table into the locked or zoomed
/// out maps they stand for.
async fn post_process(session: &mut PlayerSession) {
    let pending = |stack: &Option<ItemStack>| {
        stack
            .as_ref()
            .is_some_and(|stack| post_processing(stack).is_some())
    };
    let slots: Vec<usize> = (1..session.inventory.slots().len())
        .filter(|&slot| pending(&session.inventory.slots()[slot]))
        .collect();
    let carried = session
        .inventory
        .carried()
        .is_some_and(|stack| post_processing(stack).is_some());
    if slots.is_empty() && !carried {
        return;
    }
    let shared = session.shared.clone();
    let Some(service) = shared.services.get::<MapService>() else {
        return;
    };
    let targets = slots.into_iter().map(Some).chain(carried.then_some(None));
    let mut jobs = Vec::new();
    for target in targets {
        let held = match target {
            Some(slot) => session.inventory.slot_mut(slot),
            None => session.inventory.carried_mut(),
        };
        if let Some(stack) = held
            && let Some(how) = take_post_processing(stack)
            && let Some(id) = stack.components.map_id
        {
            jobs.push((target, id, how));
        }
    }
    for (target, id, how) in jobs {
        let Some(new) = service.derive(id, how).await else {
            continue;
        };
        let held = match target {
            Some(slot) => session.inventory.slot_mut(slot),
            None => session.inventory.carried_mut(),
        };
        if let Some(stack) = held.as_mut().filter(|s| s.components.map_id == Some(id)) {
            stack.components.map_id = Some(new);
        }
    }
    if session.menus.menu.is_some() {
        session.send_menu_content();
    } else {
        session.send_inventory();
    }
}

async fn use_empty_map(session: &mut PlayerSession, off_hand: bool) {
    if session.is_dead() || session.mode.game_mode == GameMode::Spectator {
        return;
    }
    if session.inventory.hand(off_hand).map(|stack| stack.item()) != Some(Item::Map) {
        return;
    }
    let shared = session.shared.clone();
    let Some(service) = shared.services.get::<MapService>() else {
        return;
    };
    let dimension = shared.dimensions.get(session.dimension).key.clone();
    let [x, _, z] = session.pose.position;
    let Some(id) = service
        .create(&dimension, x.floor(), z.floor(), 0, true, false)
        .await
    else {
        return;
    };
    if !session.mode.abilities.instabuild {
        session.inventory.consume_from_hand(off_hand, 1);
    }
    session
        .progress
        .stats
        .award(Stat::item(StatType::Used, Item::Map), 1);
    session
        .player_sound(SoundEvent::UiCartographyTableTakeResult, 1.0, 1.0)
        .await;
    #[expect(
        clippy::disallowed_methods,
        reason = "item turning into another (use_empty_map)"
    )]
    let mut map = ItemStack::mint(Item::FilledMap, 1, MintReason::Transform);
    map.components.map_id = Some(id);
    if session.inventory.hand(off_hand).is_none() {
        let slot = if off_hand {
            Inventory::OFFHAND_SLOT
        } else {
            Inventory::hotbar_slot(session.inventory.selected())
        };
        session.inventory.set_slot(slot, Some(map));
    } else if let Some(extra) = session.inventory.add(map) {
        session.throw_item(extra).await;
    }
    session.send_inventory();
}

pub struct MapRegionHooks;

impl RegionHooks for MapRegionHooks {
    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        if place.held != Some(Item::FilledMap) || banner_color(state).is_none() {
            return None;
        }
        let id = place.components.map_id?;
        let banner = banner_at(&*world, place.target)?;
        world
            .shared()
            .services
            .get::<MapService>()?
            .toggle_banner(id, banner);
        Some(ActionOutcome::Used {
            off_hand: place.off_hand,
            item: Item::FilledMap,
            effect: HandEffect::None,
        })
    }
}

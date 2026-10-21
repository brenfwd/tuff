//! Lecterns: opening the book view, turning pages, taking the book and the
//! redstone pulse a page turn gives.

use tuff_core::world::block_entity::analog;
use tuff_core::world::menu::MenuKind;
use tuff_core::world::messages::{
    ActionOutcome, HandEffect, MenuOpen, MenuSource, Place, PlayerEvent, PlayerHandle,
};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::registry::{CustomStat, SoundEvent};
use tuff_vanilla_data::tag::item as item_tags;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

const BUTTON_PREVIOUS: i32 = 1;
const BUTTON_NEXT: i32 = 2;
const BUTTON_TAKE: i32 = 3;
const BUTTON_PAGE: i32 = 100;
const PULSE_TICKS: u32 = 2;

pub struct Lecterns;

fn book(world: &dyn RegionWorld, pos: BlockPos) -> Option<(ItemStack, i32)> {
    let nbt = world.raw_block_entity(pos)?;
    let book = nbt.get_compound("Book").and_then(ItemStack::from_nbt)?;
    Some((book, nbt.get_int("Page").unwrap_or(0)))
}

fn update_below(world: &mut dyn RegionWorld, pos: BlockPos) {
    world.update_neighbors_at(BlockPos::new(pos.x, pos.y - 1, pos.z), Block::Lectern);
}

fn open(world: &mut dyn RegionWorld, place: &Place) -> bool {
    let pos = place.target;
    let Some((book, page)) = book(world, pos) else {
        return false;
    };
    if !world.is_spectator(place.player.id) {
        world.add_menu_viewer(pos, place.player.clone());
    }
    place.player.send(PlayerEvent::MenuOpen(MenuOpen {
        dimension: world.dimension_id(),
        region: world.handle(),
        kind: MenuKind::Lectern,
        title: TextComponent::translate("container.lectern", []),
        source: MenuSource::Blocks {
            parts: vec![(pos, 1)],
            contents: vec![Some(book)],
            data: vec![page],
            remote: None,
        },
        stat: Some(CustomStat::InteractWithLectern),
    }));
    true
}

fn take(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    book: ItemStack,
    player: &PlayerHandle,
) {
    let Some(next) = state
        .with("has_book", "false")
        .and_then(|s| s.with("powered", "false"))
    else {
        return;
    };
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        nbt.remove("Book");
        nbt.remove("Page");
    }
    world.close_menu_viewers(pos);
    world.put_block(pos, next);
    update_below(world, pos);
    player.send(PlayerEvent::ItemGranted {
        stack: book,
        picked_up: false,
    });
}

fn set_page(world: &mut dyn RegionWorld, pos: BlockPos, page: i32, book: &ItemStack) {
    let pages = analog::page_count(book) as i32;
    let page = page.clamp(0, (pages - 1).max(0));
    let current = world
        .raw_block_entity(pos)
        .and_then(|nbt| nbt.get_int("Page"))
        .unwrap_or(0);
    if page == current {
        return;
    }
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        nbt.insert("Page", page);
    }
    let dimension = world.dimension_id();
    for viewer in world.menu_viewers(pos) {
        viewer.send(PlayerEvent::ContainerData {
            dimension,
            pos,
            data: vec![(0, page)],
        });
    }
    pulse(world, pos);
}

fn pulse(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(powered) = world.block(pos).and_then(|s| s.with("powered", "true")) else {
        return;
    };
    world.set_block_flags(pos, powered, UPDATE_ALL);
    update_below(world, pos);
    world.schedule_tick(pos, Block::Lectern, PULSE_TICKS);
}

fn place_book(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState, book: ItemStack) {
    let Some(next) = state.with("has_book", "true") else {
        return;
    };
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        nbt.insert("Book", book.to_nbt());
        nbt.insert("Page", 0i32);
    }
    world.put_block(pos, next);
    update_below(world, pos);
}

impl RegionHooks for Lecterns {
    fn use_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        if state.block() != Block::Lectern {
            return None;
        }
        if state.get("has_book") != Some("true") {
            let item = place
                .held
                .filter(|item| item_tags::LECTERN_BOOKS.contains(item))?;
            let book = ItemStack::new(item, 1).with_components(place.components.clone());
            place_book(world, place.target, state, book);
            world.play_sound(SoundEvent::ItemBookPut, place.target, 1.0, 1.0, None);
            return Some(ActionOutcome::Used {
                off_hand: place.off_hand,
                item,
                effect: HandEffect::Consume(1),
            });
        }
        open(world, place).then_some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        })
    }

    fn menu_button(
        &self,
        world: &mut dyn RegionWorld,
        player: &PlayerHandle,
        pos: BlockPos,
        button: i32,
        may_build: bool,
    ) -> bool {
        let Some(state) = world.block(pos).filter(|s| s.block() == Block::Lectern) else {
            return false;
        };
        let Some((book, page)) = book(world, pos) else {
            return true;
        };
        match button {
            BUTTON_TAKE if !may_build => {}
            BUTTON_TAKE => take(world, pos, state, book, player),
            BUTTON_PREVIOUS => set_page(world, pos, page - 1, &book),
            BUTTON_NEXT => set_page(world, pos, page + 1, &book),
            b if b >= BUTTON_PAGE => set_page(world, pos, b - BUTTON_PAGE, &book),
            _ => {}
        }
        true
    }
}

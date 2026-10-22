//! Chiseled bookshelves and shelves: books and single items go in and out
//! of the slot the click lands on, and a powered shelf swaps whole hotbar
//! rows.

use tuff_core::world::block_entity::{read_items, write_items};
use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::inventory::Inventory;
use tuff_core::world::messages::{ActionOutcome, HandEffect, Place, PlayerEvent};
use tuff_core::world::region_api::RegionWorld;
use tuff_item::item::ItemStack;
use tuff_vanilla_common::placement;
use tuff_vanilla_common::side_chain;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::tag::item as item_tags;
use tuff_vanilla_data::{Block, BlockState, Item};
use tuff_world_model::{BlockPos, Face};

const BOOKSHELF_SLOTS: usize = 6;
const BOOKSHELF_ROWS: usize = 2;
const BOOKSHELF_COLUMNS: usize = 3;
const SHELF_SLOTS: usize = 3;
const HOTBAR: i32 = 9;
const LAST_INTERACTED_SLOT: &str = "last_interacted_slot";

fn section(coordinate: f32, sections: usize) -> usize {
    let size = 16.0 / sections as f32;
    ((coordinate * 16.0 / size).floor() as i64).clamp(0, sections as i64 - 1) as usize
}

/// The slot of a `rows` by `columns` grid on the `facing` side of a block
/// that a click on `face` at `cursor` lands in; `None` off that side.
pub fn hit_slot(
    face: Face,
    cursor: [f32; 3],
    facing: Facing,
    rows: usize,
    columns: usize,
) -> Option<usize> {
    if placement::facing(face) != facing {
        return None;
    }
    let x = match face {
        Face::North => 1.0 - cursor[0],
        Face::South => cursor[0],
        Face::West => cursor[2],
        Face::East => 1.0 - cursor[2],
        Face::Up | Face::Down => return None,
    };
    let row = section(1.0 - cursor[1], rows);
    Some(section(x, columns) + row * columns)
}

fn is_enchanted(stack: &ItemStack) -> bool {
    stack.item == Item::EnchantedBook
}

/// Shelf logic over a region: the block entity edits and the sounds and
/// game events that go with them.
pub trait Shelves: RegionWorld {
    /// A click on a chiseled bookshelf: a book goes into an empty slot, a
    /// book in the slot comes out. `None` lets the click fall through.
    fn use_chiseled_bookshelf(
        &mut self,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        tuff_item::item::conservation::releasing(|| self.use_bookshelf_slots(place, state))
    }

    /// The shelf's slots are a working copy of its saved NBT.
    fn use_bookshelf_slots(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let pos = place.target;
        let facing = state.facing()?;
        let interacted = ActionOutcome::Interacted {
            off_hand: place.off_hand,
        };
        let mut items = read_items(self.raw_block_entity(pos)?, BOOKSHELF_SLOTS);
        let slot = hit_slot(
            place.face,
            place.cursor,
            facing,
            BOOKSHELF_ROWS,
            BOOKSHELF_COLUMNS,
        )?;
        let occupied = state.get(&format!("slot_{slot}_occupied")) == Some("true");
        let book = place
            .held
            .filter(|item| item_tags::BOOKSHELF_BOOKS.contains(item));
        if let Some(item) = book.filter(|_| !occupied)
            && let Some(stack) = place.offer.keep_one(place.hand_slot)
        {
            let sound = if is_enchanted(&stack) {
                SoundEvent::BlockChiseledBookshelfInsertEnchanted
            } else {
                SoundEvent::BlockChiseledBookshelfInsert
            };
            items[slot] = Some(stack);
            self.update_chiseled_bookshelf(pos, state, &items, slot);
            self.play_sound(sound, pos, 1.0, 1.0, None);
            return Some(ActionOutcome::Used {
                off_hand: place.off_hand,
                item,
                effect: HandEffect::None,
            });
        }
        if !occupied {
            return Some(interacted);
        }
        let removed = items[slot].take();
        self.update_chiseled_bookshelf(pos, state, &items, slot);
        if let Some(stack) = removed {
            let sound = if is_enchanted(&stack) {
                SoundEvent::BlockChiseledBookshelfPickupEnchanted
            } else {
                SoundEvent::BlockChiseledBookshelfPickup
            };
            self.play_sound(sound, pos, 1.0, 1.0, None);
            place.player.send(PlayerEvent::ItemGranted {
                stack,
                picked_up: false,
            });
        }
        let after = self.block(pos).unwrap_or(state);
        self.game_event(
            GameEvent::BlockChange,
            pos.center(),
            Context {
                source: Some(Source::player(place.player.id)),
                affected: Some(after),
            },
        );
        Some(interacted)
    }

    fn update_chiseled_bookshelf(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        items: &[Option<ItemStack>],
        slot: usize,
    ) {
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            write_items(nbt, items);
            nbt.insert(LAST_INTERACTED_SLOT, slot as i32);
        }
        let mut next = state;
        for (index, item) in items.iter().enumerate() {
            let value = if item.is_some() { "true" } else { "false" };
            next = next
                .with(&format!("slot_{index}_occupied"), value)
                .unwrap_or(next);
        }
        self.put_block(pos, next);
        self.block_entity_changed(pos);
        self.update_output_signal(pos, Block::ChiseledBookshelf);
        self.game_event(GameEvent::BlockChange, pos.center(), Context::block(next));
    }

    /// A main-hand click on a shelf: an unpowered one swaps the clicked
    /// slot with the hand; a powered one swaps its slots, and those of the
    /// shelves chained to it, with the hotbar.
    fn use_shelf(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        if place.off_hand {
            return None;
        }
        let pos = place.target;
        let facing = state.facing()?;
        let slot = hit_slot(place.face, place.cursor, facing, 1, SHELF_SLOTS)?;
        self.raw_block_entity(pos)?;
        tuff_item::item::conservation::releasing(|| {
            if state.get("powered") != Some("true") {
                return self.swap_single_item(place, state, slot);
            }
            self.swap_hotbar(place, pos)
        })
    }

    fn swap_single_item(
        &mut self,
        place: &Place,
        state: BlockState,
        slot: usize,
    ) -> Option<ActionOutcome> {
        let pos = place.target;
        let mut items = read_items(self.raw_block_entity(pos)?, SHELF_SLOTS);
        let held = if items[slot].is_some() {
            place.offer.take_all(place.hand_slot)
        } else {
            place.offer.keep_all(place.hand_slot)
        };
        let holding = held.is_some();
        let removed = std::mem::replace(&mut items[slot], held);
        let sound = match (&removed, holding) {
            (Some(_), false) => SoundEvent::BlockShelfTakeItem,
            (Some(_), true) => SoundEvent::BlockShelfSingleSwap,
            (None, true) => SoundEvent::BlockShelfPlaceItem,
            (None, false) => return None,
        };
        self.store_shelf(pos, items);
        self.play_sound(sound, pos, 1.0, 1.0, None);
        let vibrates = removed.as_ref().is_none_or(interact_vibrations);
        if vibrates {
            self.game_event(
                GameEvent::ItemInteractFinish,
                pos.center(),
                Context::block(state),
            );
        }
        if let Some(removed) = removed {
            place.offer.give(place.hand_slot, removed);
        }
        Some(ActionOutcome::Interacted { off_hand: false })
    }

    fn swap_hotbar(&mut self, place: &Place, pos: BlockPos) -> Option<ActionOutcome> {
        let chain = side_chain::connected(&|at| self.block(at), pos);
        let count = chain.len() as i32;
        let mut changed = false;
        for (index, part) in chain.iter().enumerate() {
            let Some(nbt) = self.raw_block_entity(*part) else {
                continue;
            };
            let mut items = read_items(nbt, SHELF_SLOTS);
            for (slot, entry) in items.iter_mut().enumerate() {
                let inventory = HOTBAR - (count - index as i32) * SHELF_SLOTS as i32 + slot as i32;
                if !(0..HOTBAR).contains(&inventory) {
                    continue;
                }
                let hotbar_slot = Inventory::hotbar_slot(inventory as usize);
                let placed = place.offer.take_all(hotbar_slot);
                let removed = entry.take();
                changed |= placed.is_some() || removed.is_some();
                *entry = placed;
                if let Some(removed) = removed {
                    place.offer.give(hotbar_slot, removed);
                }
            }
            self.store_shelf(*part, items);
            let affected = self.block(*part);
            self.game_event(
                GameEvent::EntityInteract,
                part.center(),
                Context {
                    source: None,
                    affected,
                },
            );
        }
        if changed {
            self.play_sound(SoundEvent::BlockShelfMultiSwap, pos, 1.0, 1.0, None);
        }
        Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        })
    }

    fn store_shelf(&mut self, pos: BlockPos, items: Vec<Option<ItemStack>>) {
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            write_items(nbt, &items);
        }
        tuff_item::item::conservation::release(items);
        self.block_entity_changed(pos);
        self.update_output_signal(pos, self.block(pos).map_or(Block::Air, |s| s.block()));
    }
}

fn interact_vibrations(stack: &ItemStack) -> bool {
    stack.use_effects().interact_vibrations
}

impl<T: RegionWorld + ?Sized> Shelves for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_slot_follows_the_side_and_the_cursor() {
        let south = |x, y| hit_slot(Face::South, [x, y, 1.0], Facing::South, 2, 3);
        assert_eq!(south(0.1, 0.9), Some(0));
        assert_eq!(south(0.5, 0.9), Some(1));
        assert_eq!(south(0.9, 0.1), Some(5));
        let north = hit_slot(Face::North, [0.1, 0.9, 0.0], Facing::North, 2, 3);
        assert_eq!(north, Some(2));
        let east = hit_slot(Face::East, [1.0, 0.1, 0.1], Facing::East, 1, 3);
        assert_eq!(east, Some(2));
        let west = hit_slot(Face::West, [0.0, 0.1, 0.1], Facing::West, 1, 3);
        assert_eq!(west, Some(0));
        assert_eq!(hit_slot(Face::Up, [0.5; 3], Facing::South, 1, 3), None);
        assert_eq!(hit_slot(Face::North, [0.5; 3], Facing::South, 1, 3), None);
    }
}

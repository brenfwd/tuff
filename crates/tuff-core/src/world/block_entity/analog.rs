//! Comparator readings of blocks whose value lives in block entity data
//! this server keeps as saved NBT: lecterns, jukeboxes, chiseled
//! bookshelves, decorated pots, crafters, command blocks and sculk
//! sensors.

use tuff_vanilla_data::{Block, BlockState};

use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;

/// Comparator readings features compute from runtime state (a creaking
/// heart's distance to its creaking), keyed by position; they win over the
/// saved block entity reading.
#[derive(Default)]
pub struct RuntimeSignals(pub std::collections::HashMap<tuff_world_model::BlockPos, u8>);

const JUKEBOX_PLAYABLE: &str = "minecraft:jukebox_playable";
const CRAFTER_SLOTS: usize = 9;

/// The comparator reading of the block `state` whose saved block entity is
/// `nbt`; 0 for blocks this does not cover.
/// The comparator reading of a block with no block entity, from its state.
pub fn state_signal(state: BlockState) -> u8 {
    let bulb = state.block().name().ends_with("copper_bulb");
    if bulb && state.get("lit") == Some("true") {
        15
    } else {
        0
    }
}

pub fn block_entity_signal(
    state: BlockState,
    nbt: &Compound,
    direction: tuff_vanilla_data::block::Facing,
) -> u8 {
    if state.block().name().ends_with("_shelf") {
        return shelf(state, nbt, direction);
    }
    match state.block() {
        Block::Lectern => lectern(state, nbt),
        Block::Jukebox => nbt
            .get_compound("RecordItem")
            .and_then(ItemStack::picture_from_nbt)
            .map_or(0, |record| song_output(&record)),
        Block::ChiseledBookshelf => nbt
            .get_int("last_interacted_slot")
            .map_or(0, |slot| (slot + 1).clamp(0, 15) as u8),
        Block::DecoratedPot => nbt
            .get_compound("item")
            .and_then(ItemStack::picture_from_nbt)
            .map_or(0, |stack| fullness(&[Some(stack)])),
        Block::Crafter => crafter(nbt),
        Block::CommandBlock | Block::ChainCommandBlock | Block::RepeatingCommandBlock => nbt
            .get_int("SuccessCount")
            .map_or(0, |count| count.clamp(0, 15) as u8),
        Block::SculkSensor | Block::CalibratedSculkSensor => {
            if state.get("sculk_sensor_phase") != Some("active") {
                return 0;
            }
            nbt.get_int("last_vibration_frequency")
                .map_or(0, |frequency| frequency.clamp(0, 15) as u8)
        }
        _ => 0,
    }
}

/// How far through its book a lectern is open, 1 on the first page to 15
/// on the last (15 for a book of one page or none).
fn lectern(state: BlockState, nbt: &Compound) -> u8 {
    if state.get("has_book") != Some("true") {
        return 0;
    }
    let Some(book) = nbt
        .get_compound("Book")
        .and_then(ItemStack::picture_from_nbt)
    else {
        return 0;
    };
    let pages = page_count(&book);
    let progress = if pages > 1 {
        let page = nbt.get_int("Page").unwrap_or(0).clamp(0, pages as i32 - 1);
        page as f32 / (pages as f32 - 1.0)
    } else {
        1.0
    };
    (progress * 14.0).floor() as u8 + 1
}

pub fn page_count(book: &ItemStack) -> usize {
    let components = &book.components;
    if let Some(written) = &components.written_book {
        return written.pages.len();
    }
    components.writable_book.as_ref().map_or(0, Vec::len)
}

/// The comparator output of the song a record plays, 0 if it plays none.
pub fn song_output(record: &ItemStack) -> u8 {
    let components = &record.components;
    if components.is_removed(JUKEBOX_PLAYABLE) {
        return 0;
    }
    match components.other.get_str(JUKEBOX_PLAYABLE) {
        Some(song) => tuff_vanilla_data::item::jukebox_song_output(song).unwrap_or(0),
        None => record.item().jukebox_output(),
    }
}

/// The `jukebox_song` registry id of the song a record plays, if any.
pub fn song_id(record: &ItemStack) -> Option<u32> {
    let components = &record.components;
    if components.is_removed(JUKEBOX_PLAYABLE) {
        return None;
    }
    match components.other.get_str(JUKEBOX_PLAYABLE) {
        Some(song) => tuff_vanilla_data::item::jukebox_song(song).map(|(id, _)| id),
        None => record.item().jukebox_song_id(),
    }
}

/// A shelf read from behind gives one bit per occupied slot.
fn shelf(state: BlockState, nbt: &Compound, direction: tuff_vanilla_data::block::Facing) -> u8 {
    if state
        .facing()
        .map(tuff_vanilla_data::block::Facing::opposite)
        != Some(direction)
    {
        return 0;
    }
    let mut bits = 0;
    for entry in nbt.get_list("Items").unwrap_or(&[]) {
        let Some(entry) = entry.as_compound() else {
            continue;
        };
        let slot = entry.get_byte("Slot").map_or(-1, i32::from);
        if (0..3).contains(&slot) && ItemStack::mint_from_nbt(entry, MintReason::Template).is_some()
        {
            bits |= 1 << slot;
        }
    }
    bits
}

/// A crafter counts its slots that hold an item or are disabled.
fn crafter(nbt: &Compound) -> u8 {
    let mut filled = [false; CRAFTER_SLOTS];
    for entry in nbt.get_list("Items").unwrap_or(&[]) {
        let Some(entry) = entry.as_compound() else {
            continue;
        };
        let slot = entry.get_byte("Slot").map_or(-1, i32::from);
        if (0..CRAFTER_SLOTS as i32).contains(&slot)
            && ItemStack::mint_from_nbt(entry, MintReason::Template).is_some()
        {
            filled[slot as usize] = true;
        }
    }
    for slot in nbt.get_int_array("disabled_slots").unwrap_or(&[]) {
        if (0..CRAFTER_SLOTS as i32).contains(slot) {
            filled[*slot as usize] = true;
        }
    }
    filled.iter().filter(|filled| **filled).count() as u8
}

/// How full a container with these slots is, 0 to 15.
fn fullness(slots: &[Option<ItemStack>]) -> u8 {
    let total: f32 = slots
        .iter()
        .flatten()
        .map(|stack| f32::from(stack.count()) / f32::from(stack.max_stack_size().max(1)))
        .sum();
    let share = total / slots.len() as f32;
    (share * 14.0).floor() as u8 + u8::from(share > 0.0)
}

#[cfg(test)]
mod tests {
    fn read(state: BlockState, nbt: &Compound) -> u8 {
        super::block_entity_signal(state, nbt, tuff_vanilla_data::block::Facing::North)
    }

    #[tuff_core::watchdog]
    #[test]
    fn shelves_read_their_slots_as_bits_from_behind() {
        let shelf = state("minecraft:oak_shelf[facing=south]");
        let items = Compound::new().with(
            "Items",
            tuff_nbt::Tag::List(vec![
                tuff_nbt::Tag::Compound(
                    stack("minecraft:stone").with("Slot", tuff_nbt::Tag::Byte(0)),
                ),
                tuff_nbt::Tag::Compound(
                    stack("minecraft:stone").with("Slot", tuff_nbt::Tag::Byte(2)),
                ),
            ]),
        );
        assert_eq!(read(shelf, &items), 5);
        assert_eq!(
            super::block_entity_signal(shelf, &items, tuff_vanilla_data::block::Facing::East),
            0
        );
    }

    use super::*;
    use tuff_nbt::Tag;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn lit_copper_bulbs_read_full_strength() {
        let bulb = Block::WaxedExposedCopperBulb.default_state();
        let lit = bulb.with("lit", "true").unwrap();
        assert_eq!(state_signal(bulb.with("lit", "false").unwrap()), 0);
        assert_eq!(state_signal(lit), 15);
        assert_eq!(state_signal(Block::Stone.default_state()), 0);
    }

    fn state(text: &str) -> BlockState {
        text.parse().unwrap()
    }

    fn stack(item: &str) -> Compound {
        Compound::new().with("id", item).with("count", Tag::Int(1))
    }

    fn book(pages: usize) -> Compound {
        let pages = vec![Tag::String("page".into()); pages];
        stack("minecraft:writable_book").with(
            "components",
            Compound::new().with(
                "minecraft:writable_book_content",
                Compound::new().with("pages", Tag::List(pages)),
            ),
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn lecterns_read_how_far_through_the_book_they_are() {
        let lectern = state("minecraft:lectern[has_book=true]");
        let open = |pages, page| {
            let nbt = Compound::new()
                .with("Book", book(pages))
                .with("Page", Tag::Int(page));
            read(lectern, &nbt)
        };
        assert_eq!(open(3, 0), 1);
        assert_eq!(open(3, 1), 8);
        assert_eq!(open(3, 2), 15);
        assert_eq!(open(3, 9), 15, "the page is clamped to the book");
        assert_eq!(open(1, 0), 15, "a one page book is always at its end");
        assert_eq!(open(0, 0), 15);
        let nbt = Compound::new().with("Book", book(3));
        assert_eq!(read(state("minecraft:lectern[has_book=false]"), &nbt), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn jukeboxes_read_their_records_song() {
        let jukebox = state("minecraft:jukebox[has_record=true]");
        let playing = |record: Compound| read(jukebox, &Compound::new().with("RecordItem", record));
        assert_eq!(playing(stack("minecraft:music_disc_13")), 1);
        assert_eq!(playing(stack("minecraft:music_disc_11")), 11);
        assert_eq!(playing(stack("minecraft:music_disc_5")), 15);
        assert_eq!(playing(stack("minecraft:stone")), 0);
        let patched = stack("minecraft:stone").with(
            "components",
            Compound::new().with(JUKEBOX_PLAYABLE, "minecraft:pigstep"),
        );
        assert_eq!(playing(patched), 13);
        let removed = stack("minecraft:music_disc_cat").with(
            "components",
            Compound::new().with(format!("!{JUKEBOX_PLAYABLE}"), Compound::new()),
        );
        assert_eq!(playing(removed), 0);
        assert_eq!(Item::MusicDiscCat.jukebox_output(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn other_block_entities_read_their_own_counters() {
        let shelf = state("minecraft:chiseled_bookshelf");
        let slot = |n| Compound::new().with("last_interacted_slot", Tag::Int(n));
        assert_eq!(read(shelf, &slot(-1)), 0);
        assert_eq!(read(shelf, &slot(2)), 3);
        assert_eq!(read(shelf, &Compound::new()), 0);

        let pot = state("minecraft:decorated_pot");
        let holding = |count| {
            let item = stack("minecraft:stone").with("count", Tag::Int(count));
            read(pot, &Compound::new().with("item", item))
        };
        assert_eq!(holding(1), 1);
        assert_eq!(holding(32), 8);
        assert_eq!(holding(64), 15);

        let items = vec![
            Tag::Compound(stack("minecraft:stone").with("Slot", Tag::Byte(0))),
            Tag::Compound(stack("minecraft:stone").with("Slot", Tag::Byte(4))),
        ];
        let crafter = Compound::new()
            .with("Items", Tag::List(items))
            .with("disabled_slots", Tag::IntArray(vec![4, 7, 8]));
        assert_eq!(read(state("minecraft:crafter"), &crafter), 4);

        let command = Compound::new().with("SuccessCount", Tag::Int(3));
        assert_eq!(read(state("minecraft:command_block"), &command), 3);

        let sensor = Compound::new().with("last_vibration_frequency", Tag::Int(6));
        let phase = |phase: &str| {
            let text = format!("minecraft:sculk_sensor[sculk_sensor_phase={phase}]");
            read(state(&text), &sensor)
        };
        assert_eq!(phase("active"), 6);
        assert_eq!(phase("cooldown"), 0);
        assert_eq!(phase("inactive"), 0);
    }
}

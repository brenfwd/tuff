//! Item stacks shown to other players drop contents they do not render, and
//! oversized block entity data stays out of chunk packets.

use super::*;
use tuff_core::protocol::codec::WriteExt;
use tuff_core::protocol::encode_packet;
use tuff_core::protocol::play::{MetadataValue, SetEntityData, SetEquipment, equipment_slot};
use tuff_core::storage::chunk_nbt::CLIENT_BLOCK_ENTITY_MAX_BYTES;
use tuff_item::item::decor::{Filterable, WrittenBook};
use tuff_item::item::{ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;
use tuff_wire::PacketWriter;

fn heavy_shulker() -> ItemStack {
    let mut book = ItemStack::mint(Item::WrittenBook, 1, MintReason::Test);
    book.components.written_book = Some(Box::new(WrittenBook {
        title: Filterable::plain("t".into()),
        author: "a".into(),
        generation: 0,
        pages: (0..100)
            .map(|_| Filterable::plain(Tag::String("x".repeat(30_000))))
            .collect(),
        resolved: false,
    }));
    let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
    shulker.components.container = Some(vec![Some(book)]);
    shulker
}

#[tuff_core::watchdog]
#[test]
fn outbound_items_equipment_is_stripped_for_viewers() {
    let shulker = heavy_shulker();
    let snapshot = ItemSnapshot::of(&shulker);
    let items = [(equipment_slot::MAIN_HAND, Some(snapshot.any()))];
    let packet = encode_packet(&SetEquipment {
        entity_id: 1,
        items: &items,
        obfuscate: false,
    });
    assert!(
        packet.len() < 64,
        "stripped packet is {} bytes",
        packet.len()
    );
    let values = [(8u8, MetadataValue::Slot(Some(snapshot)))];
    let packet = encode_packet(&SetEntityData {
        entity_id: 2,
        values: &values,
    });
    assert!(
        packet.len() < 64,
        "stripped metadata is {} bytes",
        packet.len()
    );
}

#[tuff_core::watchdog]
#[test]
fn outbound_items_owner_slot_keeps_full_components() {
    let shulker = heavy_shulker();
    let mut w = PacketWriter::new();
    w.write_slot(Some(shulker.view()));
    assert!(w.len() > 1_000_000, "owner slot is {} bytes", w.len());
}

#[tuff_core::watchdog]
#[test]
fn outbound_items_small_stacks_are_unchanged() {
    let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    sword.set_damage(5);
    let (mut owner, mut viewer) = (PacketWriter::new(), PacketWriter::new());
    owner.write_slot(Some(sword.view()));
    viewer.write_viewer_slot(Some(sword.view()));
    assert_eq!(owner.into_bytes(), viewer.into_bytes());
}

#[tuff_core::watchdog]
#[test]
fn outbound_items_oversized_block_entity_data_is_left_out() {
    use tuff_core::storage::chunk_nbt::bounded_client_data;

    let mut big = Compound::new();
    big.insert("huge", "x".repeat(CLIENT_BLOCK_ENTITY_MAX_BYTES * 2));
    assert_eq!(bounded_client_data(big), Compound::new());
    let small = Compound::new().with("a", 1);
    assert_eq!(bounded_client_data(small.clone()), small);
}

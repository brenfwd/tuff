//! Optional hiding of other players' health and equipment details.

use super::*;
use tuff_core::config::{ConfigStore, Value};
use tuff_core::protocol::encode_packet;
use tuff_core::protocol::play::{SetEquipment, equipment_slot};
use tuff_core::world::hide_details::{HEALTH_KEY, ITEMS_KEY, hide_items, shown_health};
use tuff_item::item::{Enchantments, ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::Tag;
use tuff_vanilla_data::Item;

fn detailed_sword() -> ItemStack {
    let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    sword.set_damage(40);
    sword.components.enchantments =
        Some(Enchantments::new([("minecraft:sharpness".to_owned(), 5)]));
    sword
}

fn equipment_bytes(stack: &ItemStack, obfuscate: bool) -> Vec<u8> {
    let snapshot = ItemSnapshot::of(stack);
    let items = [(equipment_slot::MAIN_HAND, Some(snapshot.any()))];
    encode_packet(&SetEquipment {
        entity_id: 1,
        items: &items,
        obfuscate,
    })
    .into_bytes()
    .to_vec()
}

#[tuff_core::watchdog]
#[test]
fn hide_player_health_full_and_alive_modes() {
    let config = ConfigStore::new();
    assert_eq!(shown_health(&config, 7.5), 7.5);
    config
        .set(HEALTH_KEY, Value::String("full".into()))
        .unwrap();
    assert_eq!(shown_health(&config, 7.5), 20.0);
    config
        .set(HEALTH_KEY, Value::String("alive".into()))
        .unwrap();
    assert_eq!(shown_health(&config, 7.5), 1.0);
    assert_eq!(shown_health(&config, 0.0), 0.0);
    assert!(
        config
            .set(HEALTH_KEY, Value::String("bogus".into()))
            .is_err()
    );
}

#[tuff_core::watchdog]
#[test]
fn hide_player_items_strips_enchantments_damage_and_custom_data_keeps_glint() {
    let sword = detailed_sword();
    let mut expected = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    expected.components.other =
        tuff_nbt::Compound::new().with("minecraft:enchantment_glint_override", Tag::Byte(1));
    let hidden = equipment_bytes(&sword, true);
    assert_eq!(hidden, equipment_bytes(&expected, false));
    assert_ne!(hidden, equipment_bytes(&sword, false));
}

#[tuff_core::watchdog]
#[test]
fn hide_player_items_leaves_unenchanted_items_without_glint() {
    let plain = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    assert_eq!(
        equipment_bytes(&plain, true),
        equipment_bytes(&plain, false)
    );
}

#[tuff_core::watchdog]
#[test]
fn hide_player_defaults_leave_packets_unchanged() {
    let config = ConfigStore::new();
    assert!(!hide_items(&config));
    assert_eq!(shown_health(&config, 3.0), 3.0);
    config.set(ITEMS_KEY, Value::Bool(true)).unwrap();
    assert!(hide_items(&config));
}

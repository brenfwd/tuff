use tuff_item::item::BurnReason;
use tuff_item::item::MintReason;
use tuff_vanilla_data::item::EquipmentSlot;

use tuff_core::world::region_api::RegionWorld;
use tuff_nbt::{Compound, Tag};

fn float_of(tag: &Tag) -> Option<f32> {
    match tag {
        Tag::Float(v) => Some(*v),
        Tag::Double(v) => Some(*v as f32),
        Tag::Int(v) => Some(*v as f32),
        _ => None,
    }
}

const SLOT_NAMES: [(EquipmentSlot, &str); 8] = [
    (EquipmentSlot::Mainhand, "mainhand"),
    (EquipmentSlot::Offhand, "offhand"),
    (EquipmentSlot::Feet, "feet"),
    (EquipmentSlot::Legs, "legs"),
    (EquipmentSlot::Chest, "chest"),
    (EquipmentSlot::Head, "head"),
    (EquipmentSlot::Body, "body"),
    (EquipmentSlot::Saddle, "saddle"),
];

/// Rolls an equipment table into the entity's saved `equipment` and
/// `drop_chances`, so the mob loads already dressed.
pub fn equip(world: &mut dyn RegionWorld, table: &Compound, saved: &mut Compound) {
    let Some(name) = table.get_str("loot_table") else {
        return;
    };
    let chances = table.get("slot_drop_chances");
    let mut used: Vec<EquipmentSlot> = Vec::new();
    let mut equipment = saved.get_compound("equipment").cloned().unwrap_or_default();
    let mut drop_chances = saved
        .get_compound("drop_chances")
        .cloned()
        .unwrap_or_default();
    for mut stack in world.shared().block_loot().roll(name, world.rng()) {
        let slot = stack
            .item()
            .equippable()
            .map_or(EquipmentSlot::Mainhand, |e| e.slot);
        let Some((_, key)) = SLOT_NAMES
            .iter()
            .find(|(s, _)| *s == slot)
            .filter(|_| !used.contains(&slot))
        else {
            stack.burn(BurnReason::Loot);
            continue;
        };
        if !matches!(slot, EquipmentSlot::Mainhand | EquipmentSlot::Offhand) {
            stack.resize(1, MintReason::Loot, BurnReason::Loot);
        }
        equipment.insert(*key, stack.to_storage());
        tuff_item::item::conservation::release(stack);
        let chance = match chances {
            Some(Tag::Compound(per_slot)) => per_slot.get(key).and_then(float_of),
            Some(single) => float_of(single),
            None => None,
        };
        if let Some(chance) = chance {
            drop_chances.insert(*key, chance);
        }
        used.push(slot);
    }
    if !equipment.is_empty() {
        saved.insert("equipment", equipment);
    }
    if !drop_chances.is_empty() {
        saved.insert("drop_chances", drop_chances);
    }
}
